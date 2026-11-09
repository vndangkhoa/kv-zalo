//! Full-duplex Linux VoIP Call Recorder (`WAV` crash-safe streaming + RFC 7845 `Ogg/Opus`).
//!
//! Runs a dedicated `"olaz-call-recorder"` writer thread fed by a bounded wait-free
//! `sync_channel::<AudioTapPacket>(512)` from `AudioIoSession`, aligning independent
//! `TxMic` and `RxRemote` `16,000 Hz` `20ms` (`320`-sample) streams without ever blocking
//! real-time audio threads on disk I/O.

use crate::call::audio_io::{AudioTapPacket, TapSource, AUDIO_FRAME_SAMPLES, AUDIO_SAMPLE_RATE_HZ};
use crate::call::opus_ffi::{OpusPcmEncoder, OPUS_RECORDING_DEFAULT_BITRATE};
use crate::call::voip_log::voip_log;
use directories::BaseDirs;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

/// Bounded wait-free tap channel capacity (`512` packets = ~5.1s stereo tap headroom).
pub const RECORDER_CHANNEL_CAPACITY: usize = 512;
/// Samples per 20ms VoIP frame at `16,000 Hz` (`320`).
pub const ALIGNER_FRAME_SAMPLES: usize = AUDIO_FRAME_SAMPLES;
/// Drift threshold (`960` samples = `60ms`) at which a lagging side is zero-padded.
pub const ALIGNER_LAG_THRESHOLD_SAMPLES: usize = 960;
/// Maximum buffered samples per side (`3,200` samples = `200ms`) to bound clock skew.
pub const ALIGNER_MAX_QUEUE_SAMPLES: usize = 3_200;
/// Standard PCM RIFF/WAVE header byte size (`44` bytes).
pub const WAV_HEADER_BYTES: u64 = 44;
/// Interval (`250` frames = `5.0s`) at which WAV `RIFF` and `data` chunk sizes are patched on disk.
pub const WAV_SYNC_INTERVAL_FRAMES: u64 = 250;
/// RFC 7845 Opus pre-skip at `48 kHz` granule scale (`312` samples = `6.5ms`).
pub const OGG_OPUS_PRE_SKIP: u16 = 312;
/// RFC 7845 granule position increment per 20ms frame (`960` samples @ `48 kHz`).
pub const OGG_OPUS_GRANULE_STEP_20MS: u64 = 960;
/// Maximum 20ms Opus frames packed into a single Ogg audio page (`25` frames = `500ms`).
const OGG_MAX_FRAMES_PER_PAGE: usize = 25;

const OGG_FLAG_BOS: u8 = 0x02;
const OGG_FLAG_EOS: u8 = 0x04;

/// Output container/codec format for VoIP call recordings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RecordingFormat {
    #[default]
    Wav,
    #[serde(alias = "opus", alias = "ogg", alias = "ogg_opus")]
    OggOpus,
}

impl RecordingFormat {
    /// File extension (without leading dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::OggOpus => "opus",
        }
    }

    /// Canonical lowercase format identifier (`"wav"` or `"opus"`).
    pub fn as_str(self) -> &'static str {
        self.extension()
    }

    /// Parses optional UI/IPC format string (`"wav"`, `"opus"`, `"ogg"`, `"oggopus"`).
    pub fn from_opt_str(raw: Option<&str>) -> Option<Self> {
        let clean = raw?.trim().to_ascii_lowercase();
        match clean.as_str() {
            "wav" | "wave" | "pcm" => Some(Self::Wav),
            "opus" | "ogg" | "oggopus" | "ogg_opus" | "ogg-opus" => Some(Self::OggOpus),
            _ => None,
        }
    }
}

/// Channel mapping layout for VoIP call recordings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RecordingChannelLayout {
    /// 2-channel stereo split: Left = local mic (`TxMic`), Right = remote peer (`RxRemote`).
    #[default]
    #[serde(alias = "stereo", alias = "stereo_split")]
    StereoSplit,
    /// 1-channel mono soft-clipped sum of `TxMic + RxRemote`.
    #[serde(alias = "mono", alias = "mono_mix")]
    MonoMix,
}

impl RecordingChannelLayout {
    /// Number of output audio channels (`2` for `StereoSplit`, `1` for `MonoMix`).
    pub fn channels(self) -> u8 {
        match self {
            Self::StereoSplit => 2,
            Self::MonoMix => 1,
        }
    }

    /// Canonical lowercase layout identifier (`"stereo"` or `"mono"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StereoSplit => "stereo",
            Self::MonoMix => "mono",
        }
    }

    /// Parses optional UI/IPC layout string (`"stereo"`, `"mono"`).
    pub fn from_opt_str(raw: Option<&str>) -> Option<Self> {
        let clean = raw?.trim().to_ascii_lowercase();
        match clean.as_str() {
            "stereo" | "stereosplit" | "stereo_split" | "split" => Some(Self::StereoSplit),
            "mono" | "monomix" | "mono_mix" | "mix" => Some(Self::MonoMix),
            _ => None,
        }
    }
}

/// Finalized metadata returned when a `CallRecorderHandle` stops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub format: RecordingFormat,
    pub layout: RecordingChannelLayout,
    pub duration_ms: u64,
    pub frames_written: u64,
    pub dropped_frames: u64,
    pub bytes_written: u64,
}

/// Returns the default recordings directory (`~/.local/share/olaz/recordings` via `BaseDirs`).
pub fn default_recordings_dir() -> PathBuf {
    if let Some(base) = BaseDirs::new() {
        return base.data_local_dir().join("olaz").join("recordings");
    }
    std::env::temp_dir().join("olaz").join("recordings")
}

/// Sanitizes a call partner display name into a safe filesystem slug (`[a-z0-9-]`, max 40 chars).
pub fn sanitize_partner_slug(partner_name: &str) -> String {
    let mut slug = String::with_capacity(partner_name.len().min(48));
    let mut prev_dash = false;
    for ch in partner_name.nfd() {
        let mapped = match ch {
            'đ' | 'Đ' => Some('d'),
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            // Skip combining diacritical marks so `"Nguyễn"` -> `"nguyen"` rather than `"nguye-n"`.
            c if ('\u{0300}'..='\u{036f}').contains(&c) => continue,
            _ => None,
        };
        if let Some(valid) = mapped {
            slug.push(valid);
            prev_dash = false;
            if slug.len() >= 40 {
                break;
            }
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "peer".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Builds a unique `<recordings_dir>/call-<YYYYMMDD-HHMMSS>-<safe_partner>.wav|.opus` path.
pub fn build_recording_file_path(
    recordings_dir: &Path,
    partner_name: &str,
    format: RecordingFormat,
) -> PathBuf {
    let ts = format_local_timestamp_ymdhms(SystemTime::now());
    let slug = sanitize_partner_slug(partner_name);
    let ext = format.extension();
    let base = recordings_dir.join(format!("call-{ts}-{slug}.{ext}"));
    if !base.exists() {
        return base;
    }
    for suffix in 2u32..10_000 {
        let candidate = recordings_dir.join(format!("call-{ts}-{slug}-{suffix}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

/// Soft-clips the `i32` sum `tx + rx` into `i16`:
/// - Exact linear `tx + rx` within `[-24_576, 24_576]` so single-speaker speech keeps full gain.
/// - Smooth $C^1$ rational knee compression above `24_576` asymptotically bounded by `±32_767`.
#[inline]
pub fn soft_clip_mono_sum(tx: i16, rx: i16) -> i16 {
    const KNEE: i32 = 24_576;
    const HEADROOM: f32 = (i16::MAX as i32 - KNEE) as f32; // 8,191.0
    let sum = (tx as i32) + (rx as i32);
    let abs = sum.abs();
    if abs <= KNEE {
        return sum as i16;
    }
    let excess = ((abs - KNEE) as f32) / HEADROOM;
    let compressed = (KNEE as f32) + HEADROOM * (excess / (1.0 + excess));
    let mag = (compressed.round() as i32).clamp(0, i16::MAX as i32);
    if sum < 0 {
        (-mag) as i16
    } else {
        mag as i16
    }
}

/// Drift-tolerant `16,000 Hz` stream aligner pairing `TxMic` and `RxRemote` `320`-sample frames.
///
/// - Pops `320` samples from each queue when both have `>= 320`.
/// - When one queue reaches `>= 960` (`60ms`) while the other has `< 320`, pads the lagging side
///   with `0i16` so recording never stalls if a microphone or playback device re-opens.
/// - Caps each queue at `3,200` samples (`200ms`) to bound clock skew.
#[derive(Debug)]
pub struct DriftAligner {
    tx_buf: VecDeque<i16>,
    rx_buf: VecDeque<i16>,
    zero_padded_frames: u64,
}

impl Default for DriftAligner {
    fn default() -> Self {
        Self::new()
    }
}

impl DriftAligner {
    pub fn new() -> Self {
        Self {
            tx_buf: VecDeque::with_capacity(ALIGNER_MAX_QUEUE_SAMPLES + ALIGNER_FRAME_SAMPLES),
            rx_buf: VecDeque::with_capacity(ALIGNER_MAX_QUEUE_SAMPLES + ALIGNER_FRAME_SAMPLES),
            zero_padded_frames: 0,
        }
    }

    pub fn tx_len(&self) -> usize {
        self.tx_buf.len()
    }

    pub fn rx_len(&self) -> usize {
        self.rx_buf.len()
    }

    pub fn zero_padded_frames(&self) -> u64 {
        self.zero_padded_frames
    }

    /// Appends a 20ms tap packet, enforcing the `3,200`-sample (`200ms`) clock-skew cap.
    pub fn push(&mut self, packet: AudioTapPacket) {
        self.push_samples(packet.source, &packet.samples);
    }

    /// Appends raw mono `16 kHz` samples to `source`, capping queue depth at `3,200` samples.
    pub fn push_samples(&mut self, source: TapSource, samples: &[i16]) {
        let target = match source {
            TapSource::TxMic => &mut self.tx_buf,
            TapSource::RxRemote => &mut self.rx_buf,
        };
        target.extend(samples.iter().copied());
        if target.len() > ALIGNER_MAX_QUEUE_SAMPLES {
            let drop_n = target.len() - ALIGNER_MAX_QUEUE_SAMPLES;
            target.drain(..drop_n);
        }
    }

    /// Pops the next aligned `(tx_320, rx_320)` pair if available or if one side reached `960` samples.
    pub fn pop_aligned_frame(
        &mut self,
    ) -> Option<([i16; ALIGNER_FRAME_SAMPLES], [i16; ALIGNER_FRAME_SAMPLES])> {
        if self.tx_buf.len() >= ALIGNER_FRAME_SAMPLES && self.rx_buf.len() >= ALIGNER_FRAME_SAMPLES
        {
            let tx = pop_320_padded(&mut self.tx_buf);
            let rx = pop_320_padded(&mut self.rx_buf);
            return Some((tx, rx));
        }
        if self.tx_buf.len() >= ALIGNER_LAG_THRESHOLD_SAMPLES
            && self.rx_buf.len() < ALIGNER_FRAME_SAMPLES
        {
            let tx = pop_320_padded(&mut self.tx_buf);
            let rx = pop_320_padded(&mut self.rx_buf);
            self.zero_padded_frames += 1;
            return Some((tx, rx));
        }
        if self.rx_buf.len() >= ALIGNER_LAG_THRESHOLD_SAMPLES
            && self.tx_buf.len() < ALIGNER_FRAME_SAMPLES
        {
            let tx = pop_320_padded(&mut self.tx_buf);
            let rx = pop_320_padded(&mut self.rx_buf);
            self.zero_padded_frames += 1;
            return Some((tx, rx));
        }
        None
    }

    /// Drains any remaining buffered tail samples on `stop()`, zero-padding the shorter side.
    pub fn pop_flush_frame(
        &mut self,
    ) -> Option<([i16; ALIGNER_FRAME_SAMPLES], [i16; ALIGNER_FRAME_SAMPLES])> {
        if self.tx_buf.is_empty() && self.rx_buf.is_empty() {
            return None;
        }
        let padded = self.tx_buf.len() < ALIGNER_FRAME_SAMPLES
            || self.rx_buf.len() < ALIGNER_FRAME_SAMPLES;
        let tx = pop_320_padded(&mut self.tx_buf);
        let rx = pop_320_padded(&mut self.rx_buf);
        if padded {
            self.zero_padded_frames += 1;
        }
        Some((tx, rx))
    }
}

fn pop_320_padded(buf: &mut VecDeque<i16>) -> [i16; ALIGNER_FRAME_SAMPLES] {
    let mut out = [0i16; ALIGNER_FRAME_SAMPLES];
    for slot in out.iter_mut() {
        *slot = buf.pop_front().unwrap_or(0);
    }
    out
}

const fn build_ogg_crc_table() -> [u32; 256] {
    const POLY: u32 = 0x04C1_1DB7;
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            if (r & 0x8000_0000) != 0 {
                r = (r << 1) ^ POLY;
            } else {
                r <<= 1;
            }
            bit += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
}

const OGG_CRC_TABLE: [u32; 256] = build_ogg_crc_table();

/// Computes the standard RFC 3533 Ogg CRC-32 (`poly = 0x04C11DB7`, direct/unreflected, init `0`, xorout `0`).
pub fn ogg_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &b in data {
        let idx = (((crc >> 24) as u8) ^ b) as usize;
        crc = (crc << 8) ^ OGG_CRC_TABLE[idx];
    }
    crc
}

/// Builds a complete RFC 3533 Ogg page with segment lacing table and valid `ogg_crc32` checksum.
fn build_ogg_page(
    header_type_flag: u8,
    granule_pos: u64,
    serial: u32,
    page_seq: u32,
    packets: &[Vec<u8>],
) -> Vec<u8> {
    let mut seg_table: Vec<u8> = Vec::with_capacity(packets.len() * 2);
    let mut body_len = 0usize;
    for pkt in packets {
        body_len += pkt.len();
        let mut rem = pkt.len();
        while rem >= 255 {
            seg_table.push(255);
            rem -= 255;
        }
        seg_table.push(rem as u8);
    }
    debug_assert!(seg_table.len() <= 255);

    let header_len = 27 + seg_table.len();
    let mut page = Vec::with_capacity(header_len + body_len);
    page.extend_from_slice(b"OggS");
    page.push(0); // stream_structure_version = 0
    page.push(header_type_flag);
    page.extend_from_slice(&granule_pos.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&page_seq.to_le_bytes());
    page.extend_from_slice(&[0u8; 4]); // CRC placeholder at 22..26
    page.push(seg_table.len() as u8);
    page.extend_from_slice(&seg_table);
    for pkt in packets {
        page.extend_from_slice(pkt);
    }

    let crc = ogg_crc32(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

fn build_opus_head_packet(channels: u8) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(19);
    pkt.extend_from_slice(b"OpusHead");
    pkt.push(1); // version = 1
    pkt.push(channels);
    pkt.extend_from_slice(&OGG_OPUS_PRE_SKIP.to_le_bytes());
    pkt.extend_from_slice(&AUDIO_SAMPLE_RATE_HZ.to_le_bytes()); // 16,000 Hz original input rate
    pkt.extend_from_slice(&0i16.to_le_bytes()); // output_gain = 0 dB
    pkt.push(0); // channel_mapping_family = 0 (RTP mono/stereo)
    pkt
}

fn build_opus_tags_packet() -> Vec<u8> {
    const VENDOR: &[u8] = b"olaz-linux";
    let mut pkt = Vec::with_capacity(8 + 4 + VENDOR.len() + 4);
    pkt.extend_from_slice(b"OpusTags");
    pkt.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    pkt.extend_from_slice(VENDOR);
    pkt.extend_from_slice(&0u32.to_le_bytes()); // 0 user comments
    pkt
}

fn build_wav_header(channels: u8, data_bytes: u64) -> [u8; 44] {
    let ch = channels as u16;
    let block_align: u16 = ch * 2;
    let byte_rate: u32 = AUDIO_SAMPLE_RATE_HZ * (block_align as u32);
    let data_u32 = data_bytes.min((u32::MAX - 36) as u64) as u32;
    let riff_u32 = 36u32.saturating_add(data_u32);

    let mut hdr = [0u8; 44];
    hdr[0..4].copy_from_slice(b"RIFF");
    hdr[4..8].copy_from_slice(&riff_u32.to_le_bytes());
    hdr[8..12].copy_from_slice(b"WAVE");
    hdr[12..16].copy_from_slice(b"fmt ");
    hdr[16..20].copy_from_slice(&16u32.to_le_bytes());
    hdr[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM = 1
    hdr[22..24].copy_from_slice(&ch.to_le_bytes());
    hdr[24..28].copy_from_slice(&AUDIO_SAMPLE_RATE_HZ.to_le_bytes());
    hdr[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    hdr[32..34].copy_from_slice(&block_align.to_le_bytes());
    hdr[34..36].copy_from_slice(&16u16.to_le_bytes());
    hdr[36..40].copy_from_slice(b"data");
    hdr[40..44].copy_from_slice(&data_u32.to_le_bytes());
    hdr
}

struct WavStreamWriter {
    writer: BufWriter<File>,
    layout: RecordingChannelLayout,
    data_bytes: u64,
    frames_written: u64,
    finalized: bool,
    byte_scratch: Vec<u8>,
}

impl WavStreamWriter {
    fn create(file: File, layout: RecordingChannelLayout) -> Result<Self, String> {
        let mut writer = BufWriter::with_capacity(16 * 1024, file);
        let hdr = build_wav_header(layout.channels(), 0);
        writer
            .write_all(&hdr)
            .and_then(|_| writer.flush())
            .map_err(|e| format!("failed writing initial WAV header: {e}"))?;
        let frame_bytes = ALIGNER_FRAME_SAMPLES * (layout.channels() as usize) * 2;
        Ok(Self {
            writer,
            layout,
            data_bytes: 0,
            frames_written: 0,
            finalized: false,
            byte_scratch: Vec::with_capacity(frame_bytes),
        })
    }

    fn write_aligned_frame(
        &mut self,
        tx: &[i16; ALIGNER_FRAME_SAMPLES],
        rx: &[i16; ALIGNER_FRAME_SAMPLES],
    ) -> Result<(), String> {
        self.byte_scratch.clear();
        match self.layout {
            RecordingChannelLayout::StereoSplit => {
                for i in 0..ALIGNER_FRAME_SAMPLES {
                    self.byte_scratch.extend_from_slice(&tx[i].to_le_bytes());
                    self.byte_scratch.extend_from_slice(&rx[i].to_le_bytes());
                }
            }
            RecordingChannelLayout::MonoMix => {
                for i in 0..ALIGNER_FRAME_SAMPLES {
                    let mixed = soft_clip_mono_sum(tx[i], rx[i]);
                    self.byte_scratch.extend_from_slice(&mixed.to_le_bytes());
                }
            }
        }
        self.writer
            .write_all(&self.byte_scratch)
            .map_err(|e| format!("WAV PCM write error: {e}"))?;
        self.data_bytes += self.byte_scratch.len() as u64;
        self.frames_written += 1;

        if self.frames_written % WAV_SYNC_INTERVAL_FRAMES == 0 {
            self.patch_sizes()?;
        }
        Ok(())
    }

    fn patch_sizes(&mut self) -> Result<(), String> {
        self.writer
            .flush()
            .map_err(|e| format!("WAV buffer flush error: {e}"))?;
        let data_u32 = self.data_bytes.min((u32::MAX - 36) as u64) as u32;
        let riff_u32 = 36u32.saturating_add(data_u32);

        let file = self.writer.get_mut();
        file.seek(SeekFrom::Start(4))
            .and_then(|_| file.write_all(&riff_u32.to_le_bytes()))
            .and_then(|_| file.seek(SeekFrom::Start(40)))
            .and_then(|_| file.write_all(&data_u32.to_le_bytes()))
            .and_then(|_| file.seek(SeekFrom::End(0)))
            .and_then(|_| file.flush())
            .map_err(|e| format!("WAV header patch error: {e}"))
    }

    fn finalize(&mut self) -> Result<(u64, u64), String> {
        if !self.finalized {
            self.patch_sizes()?;
            let _ = self.writer.get_mut().sync_data();
            self.finalized = true;
        }
        Ok((self.frames_written, WAV_HEADER_BYTES + self.data_bytes))
    }
}

impl Drop for WavStreamWriter {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = self.patch_sizes();
            self.finalized = true;
        }
    }
}

struct OggOpusStreamWriter {
    writer: BufWriter<File>,
    encoder: OpusPcmEncoder,
    layout: RecordingChannelLayout,
    serial: u32,
    next_page_seq: u32,
    granule_pos: u64,
    frames_written: u64,
    bytes_written: u64,
    pending_packets: Vec<Vec<u8>>,
    pending_segments: usize,
    pcm_scratch: Vec<i16>,
    finalized: bool,
}

impl OggOpusStreamWriter {
    fn create(
        file: File,
        encoder: OpusPcmEncoder,
        layout: RecordingChannelLayout,
    ) -> Result<Self, String> {
        let mut writer = BufWriter::with_capacity(16 * 1024, file);
        let mut serial: u32 = rand::thread_rng().gen();
        if serial == 0 {
            serial = 0x4F4C_415A; // "OLAZ"
        }

        let head_page = build_ogg_page(
            OGG_FLAG_BOS,
            0,
            serial,
            0,
            &[build_opus_head_packet(layout.channels())],
        );
        let tags_page = build_ogg_page(0, 0, serial, 1, &[build_opus_tags_packet()]);

        writer
            .write_all(&head_page)
            .and_then(|_| writer.write_all(&tags_page))
            .and_then(|_| writer.flush())
            .map_err(|e| format!("failed writing Ogg/Opus headers: {e}"))?;

        let bytes_written = (head_page.len() + tags_page.len()) as u64;
        let samples_per_frame = ALIGNER_FRAME_SAMPLES * (layout.channels() as usize);
        Ok(Self {
            writer,
            encoder,
            layout,
            serial,
            next_page_seq: 2,
            granule_pos: 0,
            frames_written: 0,
            bytes_written,
            pending_packets: Vec::with_capacity(OGG_MAX_FRAMES_PER_PAGE),
            pending_segments: 0,
            pcm_scratch: Vec::with_capacity(samples_per_frame),
            finalized: false,
        })
    }

    fn flush_pending_page(&mut self, eos: bool) -> Result<(), String> {
        if self.pending_packets.is_empty() && !eos {
            return Ok(());
        }
        let flag = if eos { OGG_FLAG_EOS } else { 0 };
        let page = build_ogg_page(
            flag,
            self.granule_pos,
            self.serial,
            self.next_page_seq,
            &self.pending_packets,
        );
        self.next_page_seq = self.next_page_seq.wrapping_add(1);
        self.pending_packets.clear();
        self.pending_segments = 0;

        self.writer
            .write_all(&page)
            .map_err(|e| format!("Ogg/Opus page write error: {e}"))?;
        self.bytes_written += page.len() as u64;
        Ok(())
    }

    fn write_aligned_frame(
        &mut self,
        tx: &[i16; ALIGNER_FRAME_SAMPLES],
        rx: &[i16; ALIGNER_FRAME_SAMPLES],
    ) -> Result<(), String> {
        self.pcm_scratch.clear();
        match self.layout {
            RecordingChannelLayout::StereoSplit => {
                for i in 0..ALIGNER_FRAME_SAMPLES {
                    self.pcm_scratch.push(tx[i]);
                    self.pcm_scratch.push(rx[i]);
                }
            }
            RecordingChannelLayout::MonoMix => {
                for i in 0..ALIGNER_FRAME_SAMPLES {
                    self.pcm_scratch.push(soft_clip_mono_sum(tx[i], rx[i]));
                }
            }
        }

        let opus_pkt = self
            .encoder
            .encode_20ms(&self.pcm_scratch)
            .map_err(|e| format!("Opus recording encode failed: {e}"))?;
        let needed_segs = (opus_pkt.len() / 255) + 1;

        // Flush previous buffered packets before appending if this page reached 25 frames (500ms)
        // or 255 lacing segments, ensuring at least 1 packet remains to carry `EOS` on `finalize()`.
        if !self.pending_packets.is_empty()
            && (self.pending_packets.len() >= OGG_MAX_FRAMES_PER_PAGE
                || self.pending_segments + needed_segs > 255)
        {
            self.flush_pending_page(false)?;
            let _ = self.writer.flush();
        }

        self.pending_packets.push(opus_pkt);
        self.pending_segments += needed_segs;
        self.frames_written += 1;
        self.granule_pos += OGG_OPUS_GRANULE_STEP_20MS;
        Ok(())
    }

    fn finalize(&mut self) -> Result<(u64, u64), String> {
        if !self.finalized {
            self.flush_pending_page(true)?;
            self.writer
                .flush()
                .map_err(|e| format!("Ogg/Opus final flush error: {e}"))?;
            let _ = self.writer.get_mut().sync_data();
            self.finalized = true;
        }
        Ok((self.frames_written, self.bytes_written))
    }
}

impl Drop for OggOpusStreamWriter {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = self.flush_pending_page(true);
            let _ = self.writer.flush();
            self.finalized = true;
        }
    }
}

enum ActiveContainerWriter {
    Wav(WavStreamWriter),
    OggOpus(OggOpusStreamWriter),
}

impl ActiveContainerWriter {
    fn write_aligned_frame(
        &mut self,
        tx: &[i16; ALIGNER_FRAME_SAMPLES],
        rx: &[i16; ALIGNER_FRAME_SAMPLES],
    ) -> Result<(), String> {
        match self {
            Self::Wav(w) => w.write_aligned_frame(tx, rx),
            Self::OggOpus(w) => w.write_aligned_frame(tx, rx),
        }
    }

    fn finalize(&mut self) -> Result<(u64, u64), String> {
        match self {
            Self::Wav(w) => w.finalize(),
            Self::OggOpus(w) => w.finalize(),
        }
    }
}

/// Active background VoIP call recorder handle managing the `"olaz-call-recorder"` thread.
///
/// Automatically flushes and finalizes the recording file on both `stop()` and `Drop`.
pub struct CallRecorderHandle {
    path: PathBuf,
    format: RecordingFormat,
    layout: RecordingChannelLayout,
    started_at_ms: u64,
    stop_flag: Arc<AtomicBool>,
    tap_tx: Option<SyncSender<AudioTapPacket>>,
    worker: Option<JoinHandle<Result<(u64, u64), String>>>,
}

impl CallRecorderHandle {
    /// Starts a new call recorder writing to `target_path` (automatically falling back to `.wav`
    /// if `RecordingFormat::OggOpus` is requested and `libopus` is unavailable).
    pub fn start(
        target_path: PathBuf,
        requested_format: RecordingFormat,
        layout: RecordingChannelLayout,
    ) -> Result<Self, String> {
        Self::start_internal(target_path, requested_format, layout, true)
    }

    /// Starts a call recorder with an explicit `allow_opus` toggle (used by unit tests to verify
    /// deterministic `.opus -> .wav` fallback even when system `libopus.so.0` is installed).
    pub fn start_with_opus_availability(
        target_path: PathBuf,
        requested_format: RecordingFormat,
        layout: RecordingChannelLayout,
        allow_opus: bool,
    ) -> Result<Self, String> {
        Self::start_internal(target_path, requested_format, layout, allow_opus)
    }

    fn start_internal(
        target_path: PathBuf,
        requested_format: RecordingFormat,
        layout: RecordingChannelLayout,
        allow_opus: bool,
    ) -> Result<Self, String> {
        let (effective_path, effective_format, maybe_opus_enc) = match requested_format {
            RecordingFormat::Wav => (target_path, RecordingFormat::Wav, None),
            RecordingFormat::OggOpus => {
                let enc_res = if allow_opus {
                    OpusPcmEncoder::new(layout.channels(), OPUS_RECORDING_DEFAULT_BITRATE)
                } else {
                    Err(crate::call::opus_ffi::OpusError::LibraryUnavailable(
                        "disabled for fallback test".to_string(),
                    ))
                };
                match enc_res {
                    Ok(enc) => (target_path, RecordingFormat::OggOpus, Some(enc)),
                    Err(err) => {
                        let fallback_path = target_path.with_extension("wav");
                        voip_log(
                            "recorder",
                            &format!(
                                "Ogg/Opus unavailable ({err}); falling back to WAV at {}",
                                fallback_path.display()
                            ),
                        );
                        (fallback_path, RecordingFormat::Wav, None)
                    }
                }
            }
        };

        let file = create_secure_recording_file(&effective_path)?;
        let container = match (effective_format, maybe_opus_enc) {
            (RecordingFormat::OggOpus, Some(enc)) => {
                ActiveContainerWriter::OggOpus(OggOpusStreamWriter::create(file, enc, layout)?)
            }
            _ => ActiveContainerWriter::Wav(WavStreamWriter::create(file, layout)?),
        };

        let (tx, rx) = mpsc::sync_channel::<AudioTapPacket>(RECORDER_CHANNEL_CAPACITY);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_worker = Arc::clone(&stop_flag);

        let worker = thread::Builder::new()
            .name("olaz-call-recorder".to_string())
            .spawn(move || run_recorder_writer_loop(rx, stop_worker, container))
            .map_err(|e| format!("failed to spawn olaz-call-recorder thread: {e}"))?;

        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        voip_log(
            "recorder",
            &format!(
                "started recording format={} layout={} path={}",
                effective_format.as_str(),
                layout.as_str(),
                effective_path.display()
            ),
        );

        Ok(Self {
            path: effective_path,
            format: effective_format,
            layout,
            started_at_ms,
            stop_flag,
            tap_tx: Some(tx),
            worker: Some(worker),
        })
    }

    /// Clones the bounded `SyncSender<AudioTapPacket>` (`capacity = 512`) to attach to `AudioIoSession`.
    pub fn tap_sender(&self) -> SyncSender<AudioTapPacket> {
        self.tap_tx
            .as_ref()
            .expect("CallRecorderHandle tap_tx alive before stop")
            .clone()
    }

    /// Output file path on disk (reflecting `.wav` extension if fallback occurred).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Effective recording format (`Wav` or `OggOpus`).
    pub fn format(&self) -> RecordingFormat {
        self.format
    }

    /// Active channel layout (`StereoSplit` or `MonoMix`).
    pub fn layout(&self) -> RecordingChannelLayout {
        self.layout
    }

    /// Unix epoch timestamp (`ms`) when recording started.
    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    /// Stops the recorder, flushes remaining aligned audio, patches final WAV/Ogg headers, and returns `RecordingSummary`.
    pub fn stop(self) -> Result<RecordingSummary, String> {
        self.stop_with_dropped(0)
    }

    /// Stops the recorder while recording `dropped_frames` reported by `AudioIoSession`.
    pub fn stop_with_dropped(mut self, dropped_frames: u64) -> Result<RecordingSummary, String> {
        self.finalize_inner(dropped_frames)
    }

    fn finalize_inner(&mut self, dropped_frames: u64) -> Result<RecordingSummary, String> {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = self.tap_tx.take();
        let Some(handle) = self.worker.take() else {
            return Err("recorder already stopped".to_string());
        };
        let (frames_written, bytes_written) = handle
            .join()
            .map_err(|_| "olaz-call-recorder thread panicked".to_string())??;
        let duration_ms = frames_written * 20;
        voip_log(
            "recorder",
            &format!(
                "finalized recording path={} frames={} duration_ms={} bytes={} dropped={}",
                self.path.display(),
                frames_written,
                duration_ms,
                bytes_written,
                dropped_frames
            ),
        );
        Ok(RecordingSummary {
            path: self.path.clone(),
            format: self.format,
            layout: self.layout,
            duration_ms,
            frames_written,
            dropped_frames,
            bytes_written,
        })
    }
}

impl Drop for CallRecorderHandle {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.finalize_inner(0);
        }
    }
}

fn run_recorder_writer_loop(
    rx: Receiver<AudioTapPacket>,
    stop_flag: Arc<AtomicBool>,
    mut container: ActiveContainerWriter,
) -> Result<(u64, u64), String> {
    let mut aligner = DriftAligner::new();

    while !stop_flag.load(Ordering::Acquire) {
        match rx.recv_timeout(Duration::from_millis(15)) {
            Ok(pkt) => {
                aligner.push(pkt);
                while let Some((tx_f, rx_f)) = aligner.pop_aligned_frame() {
                    container.write_aligned_frame(&tx_f, &rx_f)?;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    // Drain any remaining queued packets sent right before `stop()` was invoked.
    while let Ok(pkt) = rx.try_recv() {
        aligner.push(pkt);
        while let Some((tx_f, rx_f)) = aligner.pop_aligned_frame() {
            container.write_aligned_frame(&tx_f, &rx_f)?;
        }
    }

    // Flush remaining sub-threshold tail samples so hangup never truncates the final 20-40ms.
    while let Some((tx_f, rx_f)) = aligner.pop_flush_frame() {
        container.write_aligned_frame(&tx_f, &rx_f)?;
    }

    container.finalize()
}

fn create_secure_recording_file(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
                let mut db = std::fs::DirBuilder::new();
                db.recursive(true).mode(0o700);
                db.create(parent).map_err(|e| {
                    format!("failed to create recordings dir {}: {e}", parent.display())
                })?;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
            #[cfg(not(unix))]
            {
                std::fs::create_dir_all(parent).map_err(|e| {
                    format!("failed to create recordings dir {}: {e}", parent.display())
                })?;
            }
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("failed to open recording file {}: {e}", path.display()))?;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(path)
            .map_err(|e| format!("failed to open recording file {}: {e}", path.display()))
    }
}

fn format_local_timestamp_ymdhms(when: SystemTime) -> String {
    let secs = when
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let res = unsafe { libc::localtime_r(&t, &mut tm) };
        if !res.is_null() {
            let year = tm.tm_year + 1900;
            let month = (tm.tm_mon + 1).clamp(1, 12);
            let day = tm.tm_mday.clamp(1, 31);
            let hour = tm.tm_hour.clamp(0, 23);
            let min = tm.tm_min.clamp(0, 59);
            let sec = tm.tm_sec.clamp(0, 60);
            return format!("{year:04}{month:02}{day:02}-{hour:02}{min:02}{sec:02}");
        }
    }

    // Pure-Rust UTC fallback (Howard Hinnant civil-from-days).
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    format!("{year:04}{m:02}{d:02}-{hour:02}{min:02}{sec:02}")
}
