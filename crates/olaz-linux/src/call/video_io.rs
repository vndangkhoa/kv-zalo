//! Pure-Rust Zalo H.264 video call + screen-share packetizer, depacketizer, RTCP control
//! (`RR`, `SR+SDES+REMB`, `PLI`), and `ffmpeg` V4L2 / X11 capture + low-latency RGBA decoder.

use crate::call::voip_log::voip_log;
use crate::call::zrtc::{parse_rtp_packet, RTP_ONE_BYTE_EXT_PROFILE};
use parking_lot::Mutex;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

/// Zalo H.264 non-IDR P-frame RTP payload type (`97` / `0x61`).
pub const ZALO_H264_PT_PFRAME: u8 = 97;
/// Zalo H.264 IDR keyframe AU (`SPS+PPS+IDR`) RTP payload type (`98` / `0x62`).
pub const ZALO_H264_PT_IDR: u8 = 98;
/// Maximum raw Annex-B AU bytes carried in a single unfragmented Zalo video RTP packet (`1200`).
pub const ZALO_H264_MAX_SINGLE_AU: usize = 1200;
/// Zalo custom H.264 fragment header indicator byte (`0x1C`).
pub const ZALO_H264_FRAG_INDICATOR: u8 = 0x1C;
/// Video RTP clock rate (`90,000 Hz`).
pub const VIDEO_RTP_CLOCK_HZ: u32 = 90_000;
/// Video RTP timestamp step at `15 fps` (`6,000` ticks).
pub const VIDEO_TS_STEP_15FPS: u32 = 6_000;
/// Annex-B Access Unit Delimiter (`AUD`) flushed after every AU so `ffmpeg` emits RGBA immediately.
pub const H264_AUD_ANNEX_B: [u8; 6] = [0x00, 0x00, 0x00, 0x01, 0x09, 0xF0];

/// Capture width/height for outbound H.264 video stream.
pub const VIDEO_ENC_WIDTH: u32 = 320;
pub const VIDEO_ENC_HEIGHT: u32 = 240;
/// Local PiP preview RGBA dimensions.
pub const LOCAL_PREVIEW_WIDTH: u32 = 240;
pub const LOCAL_PREVIEW_HEIGHT: u32 = 180;

/// Splits an Annex-B byte stream into individual NAL units (without their `00 00 01` / `00 00 00 01` start codes).
pub fn split_annex_b_nals(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annex_b.len() {
        if i + 4 <= annex_b.len() && annex_b[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
            starts.push((i, i + 4));
            i += 4;
        } else if annex_b[i..i + 3] == [0x00, 0x00, 0x01] {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (idx, &(_, payload_start)) in starts.iter().enumerate() {
        let payload_end = starts
            .get(idx + 1)
            .map(|&(next_sc, _)| next_sc)
            .unwrap_or(annex_b.len());
        if payload_start < payload_end {
            out.push(&annex_b[payload_start..payload_end]);
        }
    }
    out
}

/// Returns `true` if `annex_b_au` contains an H.264 IDR slice (`nal_type == 5`) or SPS (`nal_type == 7`).
pub fn is_h264_idr_au(annex_b_au: &[u8]) -> bool {
    for nal in split_annex_b_nals(annex_b_au) {
        if let Some(&hdr) = nal.first() {
            let nal_type = hdr & 0x1f;
            if nal_type == 5 || nal_type == 7 {
                return true;
            }
        }
    }
    false
}

fn build_video_rtp_packet(
    marker: bool,
    payload_type: u8,
    seq: u16,
    timestamp_90k: u32,
    ssrc: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(20 + payload.len());
    pkt.push(0x90); // V=2, X=1 (`0xBEDE` 1-word extension)
    pkt.push((if marker { 0x80 } else { 0x00 }) | (payload_type & 0x7f));
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(&timestamp_90k.to_be_bytes());
    pkt.extend_from_slice(&ssrc.to_be_bytes());
    pkt.extend_from_slice(&RTP_ONE_BYTE_EXT_PROFILE.to_be_bytes());
    pkt.extend_from_slice(&1u16.to_be_bytes());
    pkt.extend_from_slice(&[0u8; 4]);
    pkt.extend_from_slice(payload);
    pkt
}

/// Packetizes a single Annex-B H.264 Access Unit using Zalo's ZRTC video profile:
/// - `PT 98` (`0x62`) for every IDR AU (`SPS+PPS+IDR`, even single-packet!), `PT 97` (`0x61`) for non-IDR AUs.
/// - `<= 1200` bytes: 1 RTP packet with raw Annex-B payload and `M = 1`.
/// - `> 1200` bytes: split into even chunks prefixed with `[0x1C, 0x80 | 0x00 | 0x40]` (`M = 1` on last).
pub fn packetize_zalo_h264_au(
    annex_b_au: &[u8],
    seq: &mut u16,
    timestamp_90k: u32,
    ssrc: u32,
) -> Vec<Vec<u8>> {
    if annex_b_au.is_empty() {
        return Vec::new();
    }
    let pt = if is_h264_idr_au(annex_b_au) {
        ZALO_H264_PT_IDR
    } else {
        ZALO_H264_PT_PFRAME
    };

    if annex_b_au.len() <= ZALO_H264_MAX_SINGLE_AU {
        let cur_seq = *seq;
        *seq = seq.wrapping_add(1);
        return vec![build_video_rtp_packet(
            true,
            pt,
            cur_seq,
            timestamp_90k,
            ssrc,
            annex_b_au,
        )];
    }

    let total_len = annex_b_au.len();
    let num_chunks = total_len.div_ceil(ZALO_H264_MAX_SINGLE_AU);
    let base_chunk = total_len / num_chunks;
    let remainder = total_len % num_chunks;

    let mut packets = Vec::with_capacity(num_chunks);
    let mut offset = 0usize;
    for idx in 0..num_chunks {
        let chunk_len = base_chunk + usize::from(idx < remainder);
        let slice = &annex_b_au[offset..offset + chunk_len];
        offset += chunk_len;

        let is_first = idx == 0;
        let is_last = idx + 1 == num_chunks;
        let frag_flag = if is_first {
            0x80
        } else if is_last {
            0x40
        } else {
            0x00
        };

        let mut payload = Vec::with_capacity(2 + slice.len());
        payload.push(ZALO_H264_FRAG_INDICATOR);
        payload.push(frag_flag);
        payload.extend_from_slice(slice);

        let cur_seq = *seq;
        *seq = seq.wrapping_add(1);
        packets.push(build_video_rtp_packet(
            is_last,
            pt,
            cur_seq,
            timestamp_90k,
            ssrc,
            &payload,
        ));
    }
    packets
}

/// Multi-format H.264 RTP depacketizer supporting:
/// 1. Zalo `0x1C` fragments (`[0x1C, 0x80 | 0x00 | 0x40, raw_bytes..]`)
/// 2. Raw Annex-B single-AU packets (`00 00 00 01 ...` or `00 00 01 ...`)
/// 3. Single RFC 6184 NAL units (`nal_type` `1..=23`)
/// 4. RFC 6184 STAP-A (`nal_type == 24`)
/// 5. RFC 6184 FU-A (`nal_type == 28`, distinguishing `payload[1] & 0x1f != 0` from Zalo `0x1C`)
#[derive(Debug, Default)]
pub struct H264Depacketizer {
    frag_buf: Vec<u8>,
    frag_ts: Option<u32>,
    in_zalo_frag: bool,
    in_fua_frag: bool,
    last_seq: Option<u16>,
    remote_ssrc: Option<u32>,
    seen_sps_or_idr: bool,
    needs_pli: bool,
}

impl H264Depacketizer {
    pub fn new() -> Self {
        Self {
            needs_pli: true,
            ..Self::default()
        }
    }

    /// Remote video SSRC observed from RTP packets.
    pub fn remote_ssrc(&self) -> Option<u32> {
        self.remote_ssrc
    }

    /// Returns `true` once at least one SPS/IDR keyframe AU has been reassembled cleanly.
    pub fn has_keyframe_sync(&self) -> bool {
        self.seen_sps_or_idr
    }

    /// Returns `true` (and clears sticky gap state) if a PLI keyframe request should be sent.
    pub fn should_send_pli(&mut self) -> bool {
        if !self.seen_sps_or_idr {
            return true;
        }
        let n = self.needs_pli;
        self.needs_pli = false;
        n
    }

    /// Feeds one decrypted Video RTP packet; returns a complete Annex-B AU when ready.
    pub fn push_rtp_packet(&mut self, rtp_bytes: &[u8]) -> Option<Vec<u8>> {
        let rtp = parse_rtp_packet(rtp_bytes).ok()?;
        self.remote_ssrc = Some(rtp.ssrc);

        if let Some(prev_seq) = self.last_seq {
            let expected = prev_seq.wrapping_add(1);
            if rtp.seq != expected {
                // Sequence discontinuity: drop partial fragment and request PLI recovery.
                self.frag_buf.clear();
                self.in_zalo_frag = false;
                self.in_fua_frag = false;
                self.needs_pli = true;
            }
        }
        self.last_seq = Some(rtp.seq);

        let payload = rtp.payload;
        if payload.is_empty() {
            return None;
        }

        // Case 1: Raw Annex-B (`00 00 00 01` or `00 00 01`).
        if (payload.len() >= 4 && payload[0..4] == [0x00, 0x00, 0x00, 0x01])
            || (payload.len() >= 3 && payload[0..3] == [0x00, 0x00, 0x01])
        {
            self.in_zalo_frag = false;
            self.in_fua_frag = false;
            self.frag_buf.clear();
            return self.finish_au(payload.to_vec());
        }

        // Distinguish Zalo `0x1C` (`payload[0] == 0x1C` and `payload[1]` in `{0x80, 0x00, 0x40}`,
        // whose low 5 bits `nal_type` are `0` — invalid in RFC 6184 FU-A!) from RFC 6184 FU-A (`nal_type` in `1..=23`).
        if payload.len() >= 2
            && payload[0] == ZALO_H264_FRAG_INDICATOR
            && (payload[1] & 0x1f) == 0
            && matches!(payload[1], 0x80 | 0x00 | 0x40)
        {
            let flag = payload[1];
            let body = &payload[2..];
            if flag == 0x80 {
                self.frag_buf.clear();
                self.frag_buf.extend_from_slice(body);
                self.frag_ts = Some(rtp.timestamp);
                self.in_zalo_frag = true;
                self.in_fua_frag = false;
                if rtp.marker {
                    self.in_zalo_frag = false;
                    let raw = std::mem::take(&mut self.frag_buf);
                    return self.finish_au(raw);
                }
                return None;
            }
            if !self.in_zalo_frag || self.frag_ts != Some(rtp.timestamp) {
                self.frag_buf.clear();
                self.in_zalo_frag = false;
                self.needs_pli = true;
                return None;
            }
            self.frag_buf.extend_from_slice(body);
            if flag == 0x40 || rtp.marker {
                self.in_zalo_frag = false;
                let raw = std::mem::take(&mut self.frag_buf);
                return self.finish_au(raw);
            }
            return None;
        }

        let nal_type = payload[0] & 0x1f;
        match nal_type {
            1..=23 => {
                let mut au = Vec::with_capacity(4 + payload.len());
                au.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                au.extend_from_slice(payload);
                self.finish_au(au)
            }
            24 => {
                // RFC 6184 STAP-A: `[hdr(1), [u16_be len, nal_bytes]...]`.
                let mut offset = 1usize;
                let mut au = Vec::new();
                while offset + 2 <= payload.len() {
                    let nlen = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
                    offset += 2;
                    if nlen == 0 || offset + nlen > payload.len() {
                        break;
                    }
                    au.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    au.extend_from_slice(&payload[offset..offset + nlen]);
                    offset += nlen;
                }
                if au.is_empty() {
                    None
                } else {
                    self.finish_au(au)
                }
            }
            28 => {
                // RFC 6184 FU-A: `[fu_indicator, fu_header, rbsp..]`.
                if payload.len() < 2 {
                    return None;
                }
                let fu_ind = payload[0];
                let fu_hdr = payload[1];
                let start_bit = (fu_hdr & 0x80) != 0;
                let end_bit = (fu_hdr & 0x40) != 0;
                let orig_nal_hdr = (fu_ind & 0xe0) | (fu_hdr & 0x1f);

                if start_bit {
                    self.frag_buf.clear();
                    self.frag_buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, orig_nal_hdr]);
                    self.frag_buf.extend_from_slice(&payload[2..]);
                    self.frag_ts = Some(rtp.timestamp);
                    self.in_fua_frag = true;
                    self.in_zalo_frag = false;
                    if end_bit || rtp.marker {
                        self.in_fua_frag = false;
                        let raw = std::mem::take(&mut self.frag_buf);
                        return self.finish_au(raw);
                    }
                    return None;
                }
                if !self.in_fua_frag || self.frag_ts != Some(rtp.timestamp) {
                    self.frag_buf.clear();
                    self.in_fua_frag = false;
                    self.needs_pli = true;
                    return None;
                }
                self.frag_buf.extend_from_slice(&payload[2..]);
                if end_bit || rtp.marker {
                    self.in_fua_frag = false;
                    let raw = std::mem::take(&mut self.frag_buf);
                    return self.finish_au(raw);
                }
                None
            }
            _ => None,
        }
    }

    fn finish_au(&mut self, au: Vec<u8>) -> Option<Vec<u8>> {
        if au.is_empty() {
            return None;
        }
        if is_h264_idr_au(&au) {
            self.seen_sps_or_idr = true;
            self.needs_pli = false;
        }
        Some(au)
    }
}

/// Builds a 32-byte RTCP Receiver Report (`PT=201`, `RC=1`) for Audio (`0x05`) or Video (`0x0F`).
pub fn build_rtcp_rr(
    sender_ssrc: u32,
    media_ssrc: u32,
    highest_seq_ext: u32,
    jitter: u32,
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(32);
    pkt.extend_from_slice(&[0x81, 201, 0x00, 0x07]); // V=2, RC=1, PT=201, length=7 words (32B)
    pkt.extend_from_slice(&sender_ssrc.to_be_bytes());
    pkt.extend_from_slice(&media_ssrc.to_be_bytes());
    pkt.extend_from_slice(&[0u8; 4]); // fraction lost = 0, cum lost = 0
    pkt.extend_from_slice(&highest_seq_ext.to_be_bytes());
    pkt.extend_from_slice(&jitter.to_be_bytes());
    pkt.extend_from_slice(&[0u8; 8]); // LSR=0, DLSR=0
    pkt
}

/// Builds a 12-byte RTCP Picture Loss Indication (`PT=206`, `FMT=1`) requesting an immediate keyframe.
pub fn build_rtcp_pli(sender_ssrc: u32, media_ssrc: u32) -> [u8; 12] {
    let mut pkt = [0u8; 12];
    pkt[0] = 0x81; // V=2, FMT=1
    pkt[1] = 206; // PSFB
    pkt[2] = 0x00;
    pkt[3] = 0x02; // length = 2 words (12 bytes)
    pkt[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    pkt[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    pkt
}

/// Builds a compound Video RTCP packet (`SR PT=200` + `SDES PT=202` + `REMB PT=206, FMT=15`) for `0x0F`.
pub fn build_rtcp_sr_sdes_remb(
    sender_ssrc: u32,
    remote_video_ssrc: u32,
    rtp_timestamp: u32,
    packet_count: u32,
    octet_count: u32,
    bitrate_bps: u32,
    cname: &str,
) -> Vec<u8> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ntp_sec = (now.as_secs() + 2_208_988_800) as u32;
    let ntp_frac = (((now.subsec_nanos() as u64) << 32) / 1_000_000_000) as u32;

    let mut out = Vec::with_capacity(96);

    // 1. Sender Report (28 bytes = 7 words -> length=6)
    out.extend_from_slice(&[0x80, 200, 0x00, 0x06]);
    out.extend_from_slice(&sender_ssrc.to_be_bytes());
    out.extend_from_slice(&ntp_sec.to_be_bytes());
    out.extend_from_slice(&ntp_frac.to_be_bytes());
    out.extend_from_slice(&rtp_timestamp.to_be_bytes());
    out.extend_from_slice(&packet_count.to_be_bytes());
    out.extend_from_slice(&octet_count.to_be_bytes());

    // 2. SDES CNAME
    let cname_bytes = cname.as_bytes();
    let cname_len = cname_bytes.len().min(64);
    let mut sdes_chunk = Vec::with_capacity(4 + 2 + cname_len + 4);
    sdes_chunk.extend_from_slice(&sender_ssrc.to_be_bytes());
    sdes_chunk.push(0x01); // CNAME item
    sdes_chunk.push(cname_len as u8);
    sdes_chunk.extend_from_slice(&cname_bytes[..cname_len]);
    sdes_chunk.push(0x00); // END item
    while sdes_chunk.len() % 4 != 0 {
        sdes_chunk.push(0x00);
    }
    let sdes_words = (sdes_chunk.len() / 4) as u16;
    out.extend_from_slice(&[0x81, 202]);
    out.extend_from_slice(&sdes_words.to_be_bytes());
    out.extend_from_slice(&sdes_chunk);

    // 3. Google REMB (`PT=206`, `FMT=15`, 24 bytes = 6 words -> length=5)
    let mut mantissa = bitrate_bps;
    let mut exp: u8 = 0;
    while mantissa > 0x3ffff && exp < 63 {
        mantissa >>= 1;
        exp += 1;
    }
    out.extend_from_slice(&[0x8f, 206, 0x00, 0x05]);
    out.extend_from_slice(&sender_ssrc.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"REMB");
    out.push(1); // num SSRC = 1
    out.push((exp << 2) | (((mantissa >> 16) & 0x03) as u8));
    out.push(((mantissa >> 8) & 0xff) as u8);
    out.push((mantissa & 0xff) as u8);
    out.extend_from_slice(&remote_video_ssrc.to_be_bytes());

    out
}

/// Walks an inbound (compound) RTCP packet and returns `true` if any subpacket requests
/// a keyframe (`PSFB PLI PT=206 FMT=1`, `PSFB FIR PT=206 FMT=4`, or `PT=192`).
pub fn rtcp_requests_keyframe(rtcp: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset + 4 <= rtcp.len() {
        let b0 = rtcp[offset];
        if (b0 >> 6) != 2 {
            break;
        }
        let fmt = b0 & 0x1f;
        let pt = rtcp[offset + 1];
        let words = u16::from_be_bytes([rtcp[offset + 2], rtcp[offset + 3]]) as usize;
        let pkt_bytes = (words + 1) * 4;
        if pt == 192 || (pt == 206 && (fmt == 1 || fmt == 4)) {
            return true;
        }
        if pkt_bytes == 0 || offset + pkt_bytes > rtcp.len() {
            break;
        }
        offset += pkt_bytes;
    }
    false
}

/// Video capture source mode (`Camera` V4L2 `/dev/video0..7` or `ScreenShare` desktop capture).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCaptureMode {
    Camera,
    ScreenShare,
}

/// Probes `/dev/video0..7` for the first existing V4L2 camera device node.
pub fn find_v4l2_camera_device() -> Option<String> {
    for idx in 0..=7 {
        let path = format!("/dev/video{idx}");
        if Path::new(&path).exists() {
            return Some(path);
        }
    }
    None
}

/// Spawns `ffmpeg` to capture either a V4L2 webcam (`/dev/video0..7`, falling back to `testsrc2`)
/// or X11 screen (`x11grab`, falling back to `testsrc2`), producing:
/// 1. `320x240@15fps` Baseline H.264 Annex-B Access Units (`on_h264_au`)
/// 2. `240x180` RGBA local preview frames (`on_local_rgba`) via `pipe:3`
pub struct FfmpegVideoCapture {
    running: Arc<AtomicBool>,
    force_idr: Arc<AtomicBool>,
    child: Mutex<Option<Child>>,
    h264_thread: Mutex<Option<JoinHandle<()>>>,
    preview_thread: Mutex<Option<JoinHandle<()>>>,
}

impl FfmpegVideoCapture {
    pub fn start<FAu, FPrev>(
        mode: VideoCaptureMode,
        on_h264_au: FAu,
        on_local_rgba: FPrev,
    ) -> Result<Self, String>
    where
        FAu: Fn(Vec<u8>) + Send + Sync + 'static,
        FPrev: Fn(u32, u32, Vec<u8>) + Send + Sync + 'static,
    {
        let mut pipe_fds = [0i32; 2];
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            return Err("libc::pipe failed for local video preview".to_string());
        }
        let preview_read_fd = pipe_fds[0];
        let preview_write_fd = pipe_fds[1];

        // Clear FD_CLOEXEC on read end stays in parent; write end will be dup2'd to fd 3 in child.
        unsafe {
            let flags = libc::fcntl(preview_read_fd, libc::F_GETFD);
            if flags >= 0 {
                let _ = libc::fcntl(preview_read_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-loglevel", "error"]);

        match mode {
            VideoCaptureMode::Camera => {
                if let Some(dev) = find_v4l2_camera_device() {
                    voip_log("video", &format!("starting V4L2 camera capture on {dev}"));
                    cmd.args([
                        "-f",
                        "v4l2",
                        "-framerate",
                        "15",
                        "-video_size",
                        "320x240",
                        "-i",
                        &dev,
                    ]);
                } else {
                    voip_log(
                        "video",
                        "no /dev/video0..7 found; using ffmpeg testsrc2 synthetic camera",
                    );
                    cmd.args([
                        "-re",
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=320x240:rate=15",
                    ]);
                }
            }
            VideoCaptureMode::ScreenShare => {
                if let Ok(display) = std::env::var("DISPLAY") {
                    if !display.trim().is_empty() {
                        voip_log(
                            "video",
                            &format!("starting X11 screen-share capture on DISPLAY={display}"),
                        );
                        cmd.args([
                            "-f",
                            "x11grab",
                            "-framerate",
                            "15",
                            "-i",
                            &display,
                        ]);
                    } else {
                        cmd.args([
                            "-re",
                            "-f",
                            "lavfi",
                            "-i",
                            "testsrc2=size=640x360:rate=15",
                        ]);
                    }
                } else {
                    cmd.args([
                        "-re",
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=640x360:rate=15",
                    ]);
                }
            }
        }

        // Output 1 (stdout): 320x240@15fps H.264 Baseline Annex-B with frequent 1-second IDR (`-g 15`)
        cmd.args([
            "-map",
            "0:v:0",
            "-vf",
            "scale=320:240,format=yuv420p",
            "-r",
            "15",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "baseline",
            "-bf",
            "0",
            "-g",
            "15",
            "-keyint_min",
            "15",
            "-sc_threshold",
            "0",
            "-b:v",
            "260k",
            "-maxrate",
            "320k",
            "-bufsize",
            "320k",
            "-f",
            "h264",
            "pipe:1",
            // Output 2 (`pipe:3`): 240x180 RGBA local preview
            "-map",
            "0:v:0",
            "-vf",
            "scale=240:180",
            "-r",
            "15",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:3",
        ]);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        unsafe {
            cmd.pre_exec(move || {
                if preview_write_fd != 3 {
                    if libc::dup2(preview_write_fd, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let _ = libc::close(preview_write_fd);
                } else {
                    let flags = libc::fcntl(3, libc::F_GETFD);
                    if flags >= 0 {
                        let _ = libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    }
                }
                Ok(())
            });
        }

        let spawn_res = cmd.spawn();
        unsafe {
            let _ = libc::close(preview_write_fd);
        }
        let mut child = match spawn_res {
            Ok(c) => c,
            Err(e) => {
                unsafe {
                    let _ = libc::close(preview_read_fd);
                }
                return Err(format!("failed to spawn ffmpeg video capture: {e}"));
            }
        };

        let Some(mut stdout) = child.stdout.take() else {
            unsafe {
                let _ = libc::close(preview_read_fd);
            }
            let _ = child.kill();
            return Err("ffmpeg stdout unavailable".to_string());
        };

        let running = Arc::new(AtomicBool::new(true));
        let force_idr = Arc::new(AtomicBool::new(true));

        let run_h264 = Arc::clone(&running);
        let force_h264 = Arc::clone(&force_idr);
        let h264_thread = thread::Builder::new()
            .name("olaz-video-h264-cap".to_string())
            .spawn(move || {
                let mut read_buf = [0u8; 8192];
                let mut stream_acc: Vec<u8> = Vec::with_capacity(65536);
                let mut cached_sps_pps: Vec<u8> = Vec::new();
                let mut cached_last_idr: Vec<u8> = Vec::new();

                while run_h264.load(Ordering::Relaxed) {
                    match stdout.read(&mut read_buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            stream_acc.extend_from_slice(&read_buf[..n]);
                            while let Some(au) = pop_next_h264_access_unit(&mut stream_acc) {
                                update_cached_parameter_sets(
                                    &au,
                                    &mut cached_sps_pps,
                                    &mut cached_last_idr,
                                );
                                if force_h264.swap(false, Ordering::Relaxed)
                                    && !is_h264_idr_au(&au)
                                    && !cached_last_idr.is_empty()
                                {
                                    on_h264_au(cached_last_idr.clone());
                                }
                                on_h264_au(au);
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .ok();

        let run_prev = Arc::clone(&running);
        let mut preview_file = unsafe { File::from_raw_fd(preview_read_fd) };
        let preview_thread = thread::Builder::new()
            .name("olaz-video-preview".to_string())
            .spawn(move || {
                let frame_bytes = (LOCAL_PREVIEW_WIDTH * LOCAL_PREVIEW_HEIGHT * 4) as usize;
                let mut frame = vec![0u8; frame_bytes];
                while run_prev.load(Ordering::Relaxed) {
                    if preview_file.read_exact(&mut frame).is_err() {
                        break;
                    }
                    on_local_rgba(LOCAL_PREVIEW_WIDTH, LOCAL_PREVIEW_HEIGHT, frame.clone());
                }
            })
            .ok();

        Ok(Self {
            running,
            force_idr,
            child: Mutex::new(Some(child)),
            h264_thread: Mutex::new(h264_thread),
            preview_thread: Mutex::new(preview_thread),
        })
    }

    /// Forces immediate `SPS + PPS + IDR` re-transmission on incoming RTCP PLI/FIR.
    pub fn request_keyframe(&self) {
        self.force_idr.store(true, Ordering::SeqCst);
    }

    /// Terminates the `ffmpeg` capture process and joins background reader threads.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(h) = self.h264_thread.lock().take() {
            let _ = h.join();
        }
        if let Some(h) = self.preview_thread.lock().take() {
            let _ = h.join();
        }
    }
}

impl Drop for FfmpegVideoCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pop_next_h264_access_unit(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    // Locate NAL boundaries; emit an AU once we have a slice NAL (`type 1` or `type 5`) followed
    // by the start code of the next AU (`SPS=7`, `AUD=9`, or another slice with `first_mb_in_slice == 0`).
    let mut nal_offsets: Vec<(usize, usize, u8)> = Vec::new();
    let mut i = 0usize;
    while i + 4 < buf.len() {
        if buf[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
            nal_offsets.push((i, i + 4, buf[i + 4] & 0x1f));
            i += 4;
        } else if buf[i..i + 3] == [0x00, 0x00, 0x01] {
            nal_offsets.push((i, i + 3, buf[i + 3] & 0x1f));
            i += 3;
        } else {
            i += 1;
        }
    }
    if nal_offsets.len() < 2 {
        return None;
    }
    let mut saw_vcl = false;
    for &(sc_pos, hdr_pos, nal_type) in &nal_offsets {
        if saw_vcl {
            let starts_new_au = matches!(nal_type, 6 | 7 | 8 | 9)
                || (matches!(nal_type, 1 | 5)
                    && hdr_pos + 1 < buf.len()
                    && (buf[hdr_pos + 1] & 0x80) != 0);
            if starts_new_au && sc_pos > 0 {
                let au = buf[..sc_pos].to_vec();
                buf.drain(..sc_pos);
                return Some(au);
            }
        }
        if matches!(nal_type, 1 | 5) {
            saw_vcl = true;
        }
    }
    None
}

fn update_cached_parameter_sets(
    au: &[u8],
    cached_sps_pps: &mut Vec<u8>,
    cached_last_idr: &mut Vec<u8>,
) {
    let mut new_params = Vec::new();
    let mut has_idr = false;
    for nal in split_annex_b_nals(au) {
        if let Some(&hdr) = nal.first() {
            match hdr & 0x1f {
                7 | 8 => {
                    new_params.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    new_params.extend_from_slice(nal);
                }
                5 => has_idr = true,
                _ => {}
            }
        }
    }
    if !new_params.is_empty() {
        *cached_sps_pps = new_params;
    }
    if has_idr {
        if cached_sps_pps.is_empty() || au.windows(5).any(|w| w[0..4] == [0, 0, 0, 1] && (w[4] & 0x1f) == 7) {
            *cached_last_idr = au.to_vec();
        } else {
            let mut full = cached_sps_pps.clone();
            full.extend_from_slice(au);
            *cached_last_idr = full;
        }
    }
}

/// Normalizes an Annex-B Access Unit (`00 00 01` or `00 00 00 01` start codes) to 4-byte
/// `00 00 00 01` start codes while stripping redundant SEI (`6`) and AUD (`9`) NAL units.
fn normalize_annex_b_4byte(au: &[u8]) -> Vec<u8> {
    let nals = split_annex_b_nals(au);
    if nals.is_empty() {
        return au.to_vec();
    }
    let mut out = Vec::with_capacity(au.len() + nals.len() * 2);
    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1f;
        if nal_type == 6 || nal_type == 9 {
            continue;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(nal);
    }
    out
}

/// Low-latency `ffmpeg` H.264 -> `320x240` RGBA decoder using single-thread slice decoding,
/// `-fps_mode passthrough`, and the `H264_AUD` (`00 00 00 01 09 f0`) Access Unit Delimiter flush trick
/// so `libavcodec` delivers every frame immediately without multi-thread buffering lag.
pub struct FfmpegH264Decoder {
    running: Arc<AtomicBool>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
}

impl FfmpegH264Decoder {
    pub fn start<F>(on_remote_rgba: F) -> Result<Self, String>
    where
        F: Fn(u32, u32, Vec<u8>) + Send + Sync + 'static,
    {
        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-threads",
                "1",
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-vf",
                "scale=320:240",
                "-fps_mode",
                "passthrough",
                "-avioflags",
                "direct",
                "-flush_packets",
                "1",
                "-pix_fmt",
                "rgba",
                "-f",
                "rawvideo",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn ffmpeg H.264 decoder: {e}"))?;

        let stdin = child.stdin.take();
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            return Err("ffmpeg decoder stdout unavailable".to_string());
        };

        let running = Arc::new(AtomicBool::new(true));
        let run_rd = Arc::clone(&running);
        let reader_thread = thread::Builder::new()
            .name("olaz-video-h264-dec".to_string())
            .spawn(move || {
                let frame_bytes = (VIDEO_ENC_WIDTH * VIDEO_ENC_HEIGHT * 4) as usize;
                let mut frame = vec![0u8; frame_bytes];
                while run_rd.load(Ordering::Relaxed) {
                    if stdout.read_exact(&mut frame).is_err() {
                        break;
                    }
                    on_remote_rgba(VIDEO_ENC_WIDTH, VIDEO_ENC_HEIGHT, frame.clone());
                }
            })
            .ok();

        Ok(Self {
            running,
            stdin: Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            reader_thread: Mutex::new(reader_thread),
        })
    }

    /// Writes a normalized 4-byte Annex-B Access Unit followed immediately by `H264_AUD_ANNEX_B`
    /// (`00 00 00 01 09 f0`) and flushes `stdin` so `ffmpeg` emits the RGBA frame immediately.
    pub fn push_access_unit(&self, annex_b_au: &[u8]) {
        if annex_b_au.is_empty() || !self.running.load(Ordering::Relaxed) {
            return;
        }
        let norm = normalize_annex_b_4byte(annex_b_au);
        if norm.is_empty() {
            return;
        }
        let mut guard = self.stdin.lock();
        if let Some(ref mut pipe) = *guard {
            if pipe.write_all(&norm).is_ok() && pipe.write_all(&H264_AUD_ANNEX_B).is_ok() {
                let _ = pipe.flush();
            }
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = self.stdin.lock().take();
        if let Some(mut c) = self.child.lock().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(h) = self.reader_thread.lock().take() {
            let _ = h.join();
        }
    }
}

impl Drop for FfmpegH264Decoder {
    fn drop(&mut self) {
        self.stop();
    }
}
