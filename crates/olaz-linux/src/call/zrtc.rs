//! Zalo ZRTC (`InitZRTP` handshake, connectivity probes, single-socket multi-relay sweep,
//! dynamic relay + `flow_token` retargeting, ZRTC outer framing, and inner RTP + `0xBEDE` framing).

use crate::call::srtp::{SrtpContext, SrtpError, SrtpMode};
use crate::call::voip_log::{hex_preview, voip_log};
use parking_lot::Mutex;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// Default UDP port used by Zalo 1-1 ZRTC media relays.
pub const DEFAULT_RELAY_PORT: u16 = 4200;
/// Minimum valid ASCII `sessId` length (first 30 bytes form SRTP master key+salt).
pub const MIN_SESS_ID_LEN: usize = 30;

/// ZRTC packet type bytes.
pub const ZRTC_INIT_REQ: u8 = 0x01;
pub const ZRTC_INIT_RESP_OR_EXT_GROUP: u8 = 0x02;
pub const ZRTC_AUDIO_RTP_OUT: u8 = 0x03;
pub const ZRTC_AUDIO_RTP_IN_SHORT: u8 = 0x04;
pub const ZRTC_AUDIO_RTCP: u8 = 0x05;
pub const ZRTC_VIDEO_RTP_OUT: u8 = 0x0D;
pub const ZRTC_VIDEO_RTP_IN_SHORT: u8 = 0x0E;
pub const ZRTC_VIDEO_RTCP: u8 = 0x0F;
pub const ZRTC_GROUP_COMPACT: u8 = 0x7F;

/// Inner Audio RTP constants (`opus/16000/1`, `PT=112`, `20ms` = `320` samples, `0xBEDE` ext).
pub const OPUS_RTP_PAYLOAD_TYPE: u8 = 112;
pub const AUDIO_SAMPLES_PER_20MS: u32 = 320;
pub const RTP_ONE_BYTE_EXT_PROFILE: u16 = 0xBEDE;

/// Errors returned by ZRTC packet builders/parsers and media sessions.
#[derive(Debug, Error)]
pub enum ZrtcError {
    #[error("sessId must be at least 30 ASCII bytes (got {0})")]
    ShortSessId(usize),
    #[error("InitZRTP response too short ({0} bytes, need >= 35)")]
    ResponseTooShort(usize),
    #[error("unexpected InitZRTP response header [{0:#04x}, {1:#04x}] (expected [0x02, 0x7e])")]
    InvalidResponseMagic(u8, u8),
    #[error("InitZRTP relay returned error status {0}")]
    RelayRejected(i32),
    #[error("no valid relay candidates provided")]
    NoCandidates,
    #[error("no relay responded to InitZRTP sweep")]
    SweepNoResponse,
    #[error("RTP packet too short ({0} bytes)")]
    RtpTooShort(usize),
    #[error("unsupported RTP version {0}")]
    InvalidRtpVersion(u8),
    #[error("truncated RTP header/extension (need {needed}, got {actual})")]
    TruncatedRtp { needed: usize, actual: usize },
    #[error("SRTP error: {0}")]
    Srtp(#[from] SrtpError),
    #[error("UDP I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Optional reflexive `"ip|port"` string echoed in an `InitZRTP` `0x02` response (`offset 35..`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflexiveAddress {
    pub ip: String,
    pub port: String,
}

/// Parsed `InitZRTP` `0x02, 0x7e` response packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitZrtpResponse {
    pub pkt_type: u8,
    pub subtype: u8,
    pub from_id: u32,
    pub result_code: i32,
    pub nonce: [u8; 4],
    pub flow_token: [u8; 4],
    pub reflexive_addr: Option<ReflexiveAddress>,
}

/// Builds a 1-1 `InitZRTP` request (`31 + sess_id.len()` bytes):
/// - `[0..2] = [0x01, 0x7e]`
/// - `[2..10] = 0`
/// - `[10..14] = from_id` (`u32` LE)
/// - `[14..18] = 0`
/// - `[18] = 0x0b` (caller) or `0x0c` (callee)
/// - `[19..21] = [0x00, 0x02]`
/// - `[21..25] = call_id` (`u32` LE)
/// - `[25..29] = to_id` (`u32` LE)
/// - `[29..31] = sess_id.len()` (`u16` LE)
/// - `[31..] = sess_id` (ASCII)
pub fn build_init_zrtp_request(
    from_id: u32,
    to_id: u32,
    call_id: u32,
    sess_id: &str,
    is_callee: bool,
) -> Result<Vec<u8>, ZrtcError> {
    let sess_bytes = sess_id.as_bytes();
    if sess_bytes.len() < MIN_SESS_ID_LEN {
        return Err(ZrtcError::ShortSessId(sess_bytes.len()));
    }
    let mut buf = vec![0u8; 31 + sess_bytes.len()];
    buf[0] = ZRTC_INIT_REQ;
    buf[1] = 0x7e;
    buf[10..14].copy_from_slice(&from_id.to_le_bytes());
    buf[18] = if is_callee { 0x0c } else { 0x0b };
    buf[19] = 0x00;
    buf[20] = 0x02;
    buf[21..25].copy_from_slice(&call_id.to_le_bytes());
    buf[25..29].copy_from_slice(&to_id.to_le_bytes());
    buf[29..31].copy_from_slice(&(sess_bytes.len() as u16).to_le_bytes());
    buf[31..].copy_from_slice(sess_bytes);
    Ok(buf)
}

/// Builds a Group SFU `InitZRTP` (`0x0b` / `sub_cmd = 0x04`) request packet.
pub fn build_group_init_zrtp_request(
    from_id: u32,
    group_id: u32,
    call_id: u32,
    sess_id: &str,
) -> Result<Vec<u8>, ZrtcError> {
    let sess_bytes = sess_id.as_bytes();
    if sess_bytes.is_empty() {
        return Err(ZrtcError::ShortSessId(0));
    }
    let mut buf = Vec::with_capacity(31 + sess_bytes.len() + 6);
    buf.extend_from_slice(&[ZRTC_INIT_REQ, 0x7e]);
    buf.extend_from_slice(&[0u8; 8]);
    buf.extend_from_slice(&from_id.to_le_bytes());
    buf.extend_from_slice(&[0u8; 4]);
    buf.extend_from_slice(&[0x0b, 0x00, 0x04]);
    buf.extend_from_slice(&call_id.to_le_bytes());
    buf.extend_from_slice(&group_id.to_le_bytes());
    buf.extend_from_slice(&(sess_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(sess_bytes);
    buf.extend_from_slice(&4u16.to_le_bytes());
    buf.extend_from_slice(&[0x00, 0x03, 0x01, 0x01]);
    Ok(buf)
}

/// Builds a Group SFU `InitZRTP_v6` (`0x0c` / `sub_cmd = 0x06`) request packet.
pub fn build_group_init_zrtp_v6_request(
    from_id: u32,
    call_id: u32,
    sess_id: &str,
) -> Result<Vec<u8>, ZrtcError> {
    let sess_bytes = sess_id.as_bytes();
    if sess_bytes.is_empty() {
        return Err(ZrtcError::ShortSessId(0));
    }
    let mut buf = Vec::with_capacity(30 + sess_bytes.len() + 6);
    buf.extend_from_slice(&[ZRTC_INIT_REQ, 0x7e]);
    buf.extend_from_slice(&[0u8; 8]);
    buf.extend_from_slice(&from_id.to_le_bytes());
    buf.extend_from_slice(&[0u8; 4]);
    buf.extend_from_slice(&[0x0c, 0x00, 0x06]);
    buf.extend_from_slice(&call_id.to_le_bytes());
    buf.extend_from_slice(&[0x00, 0x03, 0x00]);
    buf.extend_from_slice(&(sess_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(sess_bytes);
    buf.extend_from_slice(&4u16.to_le_bytes());
    buf.extend_from_slice(&[0x00, 0x03, 0x01, 0x01]);
    Ok(buf)
}

/// Builds a 25-byte connectivity probe (`[0x01, 0x7e, ..., from_id LE, nonce, 0x03, 0, 0, call_id LE]`).
pub fn build_probe(from_id: u32, call_id: u32, probe_nonce: Option<[u8; 4]>) -> [u8; 25] {
    let mut buf = [0u8; 25];
    buf[0] = ZRTC_INIT_REQ;
    buf[1] = 0x7e;
    buf[10..14].copy_from_slice(&from_id.to_le_bytes());
    let nonce = probe_nonce.unwrap_or_else(|| {
        let mut n = [0u8; 4];
        rand::thread_rng().fill_bytes(&mut n);
        n
    });
    buf[14..18].copy_from_slice(&nonce);
    buf[18] = 0x03;
    buf[19] = 0x00;
    buf[20] = 0x00;
    buf[21..25].copy_from_slice(&call_id.to_le_bytes());
    buf
}

/// Parses an `InitZRTP` response (`>= 35` bytes starting with `[0x02, 0x7e]`),
/// extracting `flow_token` at `29..33` and optional reflexive `"ip|port"` at `35..35+addr_len`.
pub fn parse_init_zrtp_response(buf: &[u8]) -> Result<InitZrtpResponse, ZrtcError> {
    if buf.len() < 35 {
        return Err(ZrtcError::ResponseTooShort(buf.len()));
    }
    if buf[0] != 0x02 || buf[1] != 0x7e {
        return Err(ZrtcError::InvalidResponseMagic(buf[0], buf[1]));
    }
    let from_id = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]);
    let subtype = buf[18];
    let result_code = i32::from_le_bytes([buf[21], buf[22], buf[23], buf[24]]);
    if result_code < 0 {
        return Err(ZrtcError::RelayRejected(result_code));
    }
    let mut nonce = [0u8; 4];
    nonce.copy_from_slice(&buf[25..29]);
    let mut flow_token = [0u8; 4];
    flow_token.copy_from_slice(&buf[29..33]);

    let addr_len = u16::from_le_bytes([buf[33], buf[34]]) as usize;
    let reflexive_addr = if addr_len > 0 && buf.len() >= 35 + addr_len {
        let raw_str = std::str::from_utf8(&buf[35..35 + addr_len]).unwrap_or("");
        if let Some((ip, port)) = raw_str.split_once('|') {
            Some(ReflexiveAddress {
                ip: ip.to_string(),
                port: port.to_string(),
            })
        } else if !raw_str.is_empty() {
            Some(ReflexiveAddress {
                ip: raw_str.to_string(),
                port: String::new(),
            })
        } else {
            None
        }
    } else {
        None
    };

    Ok(InitZrtpResponse {
        pkt_type: buf[0],
        subtype,
        from_id,
        result_code,
        nonce,
        flow_token,
        reflexive_addr,
    })
}

/// Classification of inner payload carried inside a ZRTC UDP datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZrtcPacketKind {
    AudioRtp,
    AudioRtcp,
    VideoRtp,
    VideoRtcp,
}

/// Unwrapped ZRTC media/RTCP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnwrappedZrtc<'a> {
    pub outer_type: u8,
    pub kind: ZrtcPacketKind,
    pub flow_token: Option<[u8; 4]>,
    pub group_peer_uid: Option<u32>,
    pub inner: &'a [u8],
}

/// Wraps an inner SRTP/SRTCP packet with the 5-byte outbound ZRTC prefix `[type_byte, flow_token[0..4]]`.
pub fn wrap_zrtc(type_byte: u8, flow_token: &[u8; 4], inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + inner.len());
    out.push(type_byte);
    out.extend_from_slice(flow_token);
    out.extend_from_slice(inner);
    out
}

/// Wraps a Group SFU compact `0x7F` packet (`9`-byte header: `[0x7f, 0x00, cmd, 0x00, 0x00, uid_le]`).
pub fn wrap_group_zrtc(cmd: u8, sender_uid: u32, inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + inner.len());
    out.extend_from_slice(&[ZRTC_GROUP_COMPACT, 0x00, cmd, 0x00, 0x00]);
    out.extend_from_slice(&sender_uid.to_le_bytes());
    out.extend_from_slice(inner);
    out
}

/// Unwraps an inbound ZRTC UDP datagram across all supported 1-1 and Group SFU wire formats:
/// - `0x04`: 1-byte prefix Audio RTP
/// - `0x0E`: 1-byte prefix Video RTP
/// - `0x03` / `0x05` / `0x0D` / `0x0F`: 5-byte prefix (`[type, flow_token[0..4], inner..]`)
/// - `0x7F`: 9-byte Group SFU header (`cmd=6` audio / `cmd=8` video, `peer_uid` at `5..9`)
/// - `0x02` with `cmd == 0x0032`: 25-byte Group SFU extended header (`peer_uid` at `10..14`, `sub` at `18`)
pub fn unwrap_zrtc(buf: &[u8]) -> Option<UnwrappedZrtc<'_>> {
    if buf.len() < 8 {
        return None;
    }
    match buf[0] {
        ZRTC_AUDIO_RTP_IN_SHORT => {
            if buf.len() < 1 + 12 || (buf[1] >> 6) != 2 {
                return None;
            }
            Some(UnwrappedZrtc {
                outer_type: ZRTC_AUDIO_RTP_IN_SHORT,
                kind: ZrtcPacketKind::AudioRtp,
                flow_token: None,
                group_peer_uid: None,
                inner: &buf[1..],
            })
        }
        ZRTC_VIDEO_RTP_IN_SHORT => {
            if buf.len() < 1 + 12 || (buf[1] >> 6) != 2 {
                return None;
            }
            Some(UnwrappedZrtc {
                outer_type: ZRTC_VIDEO_RTP_IN_SHORT,
                kind: ZrtcPacketKind::VideoRtp,
                flow_token: None,
                group_peer_uid: None,
                inner: &buf[1..],
            })
        }
        ZRTC_AUDIO_RTP_OUT | ZRTC_VIDEO_RTP_OUT => {
            if buf.len() < 5 + 12 || (buf[5] >> 6) != 2 {
                return None;
            }
            let mut ft = [0u8; 4];
            ft.copy_from_slice(&buf[1..5]);
            let kind = if buf[0] == ZRTC_AUDIO_RTP_OUT {
                ZrtcPacketKind::AudioRtp
            } else {
                ZrtcPacketKind::VideoRtp
            };
            Some(UnwrappedZrtc {
                outer_type: buf[0],
                kind,
                flow_token: Some(ft),
                group_peer_uid: None,
                inner: &buf[5..],
            })
        }
        ZRTC_AUDIO_RTCP | ZRTC_VIDEO_RTCP => {
            if buf.len() < 5 + 8 || (buf[5] >> 6) != 2 {
                return None;
            }
            let mut ft = [0u8; 4];
            ft.copy_from_slice(&buf[1..5]);
            let kind = if buf[0] == ZRTC_AUDIO_RTCP {
                ZrtcPacketKind::AudioRtcp
            } else {
                ZrtcPacketKind::VideoRtcp
            };
            Some(UnwrappedZrtc {
                outer_type: buf[0],
                kind,
                flow_token: Some(ft),
                group_peer_uid: None,
                inner: &buf[5..],
            })
        }
        ZRTC_GROUP_COMPACT => {
            if buf.len() < 9 + 12 || (buf[9] >> 6) != 2 {
                return None;
            }
            let cmd = buf[2];
            let peer_uid = u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]);
            let inner = &buf[9..];
            let pt = inner[1] & 0x7f;
            let kind = if cmd == 0x08 || (cmd != 0x06 && (96..=103).contains(&pt)) {
                ZrtcPacketKind::VideoRtp
            } else {
                ZrtcPacketKind::AudioRtp
            };
            Some(UnwrappedZrtc {
                outer_type: ZRTC_GROUP_COMPACT,
                kind,
                flow_token: None,
                group_peer_uid: Some(peer_uid),
                inner,
            })
        }
        ZRTC_INIT_RESP_OR_EXT_GROUP => {
            // `0x02, 0x7e` is an InitZRTP response, NOT Group SFU media (`u16 LE @ 1..3 == 0x0032`).
            if buf.len() < 25 + 12 || buf[1] == 0x7e {
                return None;
            }
            let cmd = u16::from_le_bytes([buf[1], buf[2]]);
            if cmd != 0x0032 || (buf[25] >> 6) != 2 {
                return None;
            }
            let peer_uid = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]);
            let sub = buf[18];
            let inner = &buf[25..];
            let pt = inner[1] & 0x7f;
            let kind = if sub == 0x08 || (96..=103).contains(&pt) {
                ZrtcPacketKind::VideoRtp
            } else {
                ZrtcPacketKind::AudioRtp
            };
            Some(UnwrappedZrtc {
                outer_type: ZRTC_INIT_RESP_OR_EXT_GROUP,
                kind,
                flow_token: None,
                group_peer_uid: Some(peer_uid),
                inner,
            })
        }
        _ => None,
    }
}

/// Parsed inner RTP packet with dynamic CSRC and header extension lengths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRtpPacket<'a> {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub csrc_count: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub seq: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub header_len: usize,
    pub extension_profile: Option<u16>,
    pub extension_data: &'a [u8],
    pub payload: &'a [u8],
}

/// Builds an outbound 20-byte Zalo Audio RTP header + Opus payload:
/// `[0x90, 0x70, seq_be, ts_be, ssrc_be, 0xBE, 0xDE, 0x00, 0x01, 0, 0, 0, 0, payload..]`.
pub fn build_audio_rtp(seq: u16, timestamp: u32, ssrc: u32, opus_payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + opus_payload.len());
    out.push(0x90); // V=2, P=0, X=1, CC=0
    out.push(OPUS_RTP_PAYLOAD_TYPE); // M=0, PT=112 (0x70)
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&timestamp.to_be_bytes());
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(&RTP_ONE_BYTE_EXT_PROFILE.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(opus_payload);
    out
}

/// Parses any RFC 3550 RTP packet, dynamically skipping CSRCs, any header extension,
/// and optional trailing RTP padding bytes.
pub fn parse_rtp_packet(buf: &[u8]) -> Result<ParsedRtpPacket<'_>, ZrtcError> {
    if buf.len() < 12 {
        return Err(ZrtcError::RtpTooShort(buf.len()));
    }
    let version = buf[0] >> 6;
    if version != 2 {
        return Err(ZrtcError::InvalidRtpVersion(version));
    }
    let padding = (buf[0] & 0x20) != 0;
    let extension = (buf[0] & 0x10) != 0;
    let csrc_count = buf[0] & 0x0f;
    let marker = (buf[1] & 0x80) != 0;
    let payload_type = buf[1] & 0x7f;
    let seq = u16::from_be_bytes([buf[2], buf[3]]);
    let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

    let mut hdr_len = 12 + (csrc_count as usize) * 4;
    if buf.len() < hdr_len {
        return Err(ZrtcError::TruncatedRtp {
            needed: hdr_len,
            actual: buf.len(),
        });
    }

    let mut extension_profile = None;
    let mut extension_data: &[u8] = &[];
    if extension {
        if buf.len() < hdr_len + 4 {
            return Err(ZrtcError::TruncatedRtp {
                needed: hdr_len + 4,
                actual: buf.len(),
            });
        }
        let profile = u16::from_be_bytes([buf[hdr_len], buf[hdr_len + 1]]);
        let ext_words = u16::from_be_bytes([buf[hdr_len + 2], buf[hdr_len + 3]]) as usize;
        let ext_bytes = ext_words * 4;
        let ext_start = hdr_len + 4;
        let ext_end = ext_start + ext_bytes;
        if buf.len() < ext_end {
            return Err(ZrtcError::TruncatedRtp {
                needed: ext_end,
                actual: buf.len(),
            });
        }
        extension_profile = Some(profile);
        extension_data = &buf[ext_start..ext_end];
        hdr_len = ext_end;
    }

    let mut payload_end = buf.len();
    if padding && payload_end > hdr_len {
        let pad_len = buf[payload_end - 1] as usize;
        if pad_len > 0 && pad_len <= (payload_end - hdr_len) {
            payload_end -= pad_len;
        }
    }

    Ok(ParsedRtpPacket {
        version,
        padding,
        extension,
        csrc_count,
        marker,
        payload_type,
        seq,
        timestamp,
        ssrc,
        header_len: hdr_len,
        extension_profile,
        extension_data,
        payload: &buf[hdr_len..payload_end],
    })
}

/// Parses `"host:port"` or bare `"host"` (defaulting to `:4200`) into a `SocketAddr`.
pub fn parse_relay_endpoint(endpoint: &str, default_port: u16) -> Option<SocketAddr> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(sa) = trimmed.parse::<SocketAddr>() {
        if sa.port() == 0 {
            return Some(SocketAddr::new(sa.ip(), default_port));
        }
        return Some(sa);
    }
    if let Some((h, p)) = trimmed.rsplit_once(':') {
        if let (Ok(ip), Ok(port)) = (h.trim().parse::<IpAddr>(), p.trim().parse::<u16>()) {
            return Some(SocketAddr::new(
                ip,
                if port == 0 { default_port } else { port },
            ));
        }
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, default_port));
    }
    None
}

/// Per-relay `InitZRTP` sweep result used to populate `extendData.serverResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaySweepEntry {
    pub addr: SocketAddr,
    pub recv: u32,
    pub rtt_ms: u32,
    pub flow_token: [u8; 4],
    pub reflexive_addr: Option<ReflexiveAddress>,
}

/// Configuration for `ZrtcMediaSession::bind_and_sweep`.
#[derive(Debug, Clone)]
pub struct ZrtcSweepConfig {
    pub bind_addr: SocketAddr,
    pub candidates: Vec<SocketAddr>,
    pub prefer_host: Option<IpAddr>,
    pub prefer_addr: Option<SocketAddr>,
    pub from_id: u32,
    pub to_id: u32,
    pub call_id: u32,
    pub sess_id: String,
    pub is_callee: bool,
    pub is_group: bool,
    pub probe_count: usize,
    pub sweep_timeout: Duration,
    pub early_exit_min: Duration,
}

impl ZrtcSweepConfig {
    /// Standard 1-1 VoIP sweep config (`0.0.0.0:0`, `6` probes, `1800ms` max sweep, `350ms` early exit).
    pub fn new_one_to_one(
        candidates: Vec<SocketAddr>,
        prefer_endpoint: Option<&str>,
        from_id: u32,
        to_id: u32,
        call_id: u32,
        sess_id: String,
        is_callee: bool,
    ) -> Self {
        let prefer_addr = prefer_endpoint.and_then(|s| parse_relay_endpoint(s, DEFAULT_RELAY_PORT));
        let prefer_host = prefer_addr.map(|a| a.ip()).or_else(|| {
            prefer_endpoint.and_then(|s| s.split(':').next()?.trim().parse::<IpAddr>().ok())
        });
        Self {
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            candidates,
            prefer_host,
            prefer_addr,
            from_id,
            to_id,
            call_id,
            sess_id,
            is_callee,
            is_group: false,
            probe_count: 6,
            sweep_timeout: Duration::from_millis(1800),
            early_exit_min: Duration::from_millis(350),
        }
    }
}

/// Summary returned once `InitZRTP` multi-relay sweep completes.
#[derive(Debug, Clone)]
pub struct ZrtcSweepOutcome {
    pub chosen_relay: SocketAddr,
    pub chosen_flow_token: [u8; 4],
    pub local_addr: SocketAddr,
    pub results: Vec<RelaySweepEntry>,
}

/// Decrypted inbound media/RTCP event emitted by `ZrtcMediaSession`.
#[derive(Debug, Clone)]
pub enum InboundZrtcEvent {
    AudioRtp {
        outer_type: u8,
        from_relay: SocketAddr,
        flow_token: Option<[u8; 4]>,
        group_peer_uid: Option<u32>,
        seq: u16,
        timestamp: u32,
        ssrc: u32,
        opus_payload: Vec<u8>,
    },
    VideoRtp {
        outer_type: u8,
        from_relay: SocketAddr,
        flow_token: Option<[u8; 4]>,
        group_peer_uid: Option<u32>,
        rtp_packet: Vec<u8>,
    },
    AudioRtcp {
        from_relay: SocketAddr,
        rtcp_packet: Vec<u8>,
    },
    VideoRtcp {
        from_relay: SocketAddr,
        rtcp_packet: Vec<u8>,
    },
    RelayRetargeted {
        old_relay: SocketAddr,
        new_relay: SocketAddr,
        new_flow_token: [u8; 4],
    },
}

#[derive(Debug, Clone)]
struct ActiveRelayTarget {
    addr: SocketAddr,
    flow_token: [u8; 4],
}

/// Single-socket ZRTC UDP + SRTP session handling multi-relay `InitZRTP` sweep,
/// outbound audio/video/RTCP transmission, inbound SRTP decryption, and dynamic relay retargeting.
pub struct ZrtcMediaSession {
    socket: Arc<UdpSocket>,
    ssrc: u32,
    is_group: bool,
    closed: AtomicBool,
    audio_seq: AtomicU16,
    audio_ts: AtomicU32,
    target: Mutex<ActiveRelayTarget>,
    ft_by_addr: HashMap<SocketAddr, [u8; 4]>,
    ft_by_ip: HashMap<IpAddr, (SocketAddr, [u8; 4])>,
    srtp: Mutex<SrtpContext>,
}

impl ZrtcMediaSession {
    /// Binds a **single** `tokio::net::UdpSocket`, performs the `InitZRTP` + 6-probe sweep across all
    /// deduplicated relay candidates, selects the preferred/best responder, and spawns the receive loop
    /// on that exact same socket (preserving NAT/relay port mapping and dynamically retargeting if media
    /// arrives from another known relay).
    pub async fn bind_and_sweep(
        cfg: ZrtcSweepConfig,
        srtp_mode: SrtpMode,
    ) -> Result<
        (
            Arc<Self>,
            ZrtcSweepOutcome,
            mpsc::UnboundedReceiver<InboundZrtcEvent>,
        ),
        ZrtcError,
    > {
        // Deduplicate candidates while preserving order.
        let mut deduped: Vec<SocketAddr> = Vec::with_capacity(cfg.candidates.len());
        for cand in &cfg.candidates {
            let normalized = if cand.port() == 0 {
                SocketAddr::new(cand.ip(), DEFAULT_RELAY_PORT)
            } else {
                *cand
            };
            if !deduped.contains(&normalized) {
                deduped.push(normalized);
            }
        }
        if deduped.is_empty() {
            return Err(ZrtcError::NoCandidates);
        }

        let srtp = SrtpContext::from_sess_id(&cfg.sess_id, srtp_mode)?;
        let socket = Arc::new(UdpSocket::bind(cfg.bind_addr).await?);
        let local_addr = socket.local_addr()?;

        let req_pkt = if cfg.is_group {
            build_group_init_zrtp_request(cfg.from_id, cfg.to_id, cfg.call_id, &cfg.sess_id)?
        } else {
            build_init_zrtp_request(
                cfg.from_id,
                cfg.to_id,
                cfg.call_id,
                &cfg.sess_id,
                cfg.is_callee,
            )?
        };
        let v6_pkt = if cfg.is_group {
            Some(build_group_init_zrtp_v6_request(
                cfg.from_id,
                cfg.call_id,
                &cfg.sess_id,
            )?)
        } else {
            None
        };

        let start = Instant::now();
        let mut sent_at: HashMap<SocketAddr, Instant> = HashMap::with_capacity(deduped.len());
        for &addr in &deduped {
            sent_at.insert(addr, Instant::now());
            let _ = socket.send_to(&req_pkt, addr).await;
            if let Some(ref p6) = v6_pkt {
                let _ = socket.send_to(p6, addr).await;
            }
            for _ in 0..cfg.probe_count {
                let probe = build_probe(cfg.from_id, cfg.call_id, None);
                let _ = socket.send_to(&probe, addr).await;
            }
        }

        voip_log(
            "zrtc",
            &format!(
                "InitZRTP sweep started on {local_addr} -> {} candidate(s), role={}",
                deduped.len(),
                if cfg.is_callee { "callee(0x0c)" } else { "caller(0x0b)" }
            ),
        );

        let mut entries: HashMap<SocketAddr, RelaySweepEntry> = HashMap::new();
        let mut buffered_early_media: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
        let mut retried_init = false;
        let mut recv_buf = vec![0u8; 2048];
        let deadline = start + cfg.sweep_timeout;
        let early_min = cfg.early_exit_min.min(cfg.sweep_timeout);

        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let elapsed = now.duration_since(start);

            // Re-send InitZRTP once around 450ms if no relay has replied yet.
            if !retried_init
                && entries.is_empty()
                && cfg.sweep_timeout >= Duration::from_millis(600)
                && elapsed >= Duration::from_millis(450)
            {
                retried_init = true;
                for &addr in &deduped {
                    let _ = socket.send_to(&req_pkt, addr).await;
                }
            }

            // Early exit check once minimum observation window has passed.
            if elapsed >= early_min && !entries.is_empty() {
                let all_replied = deduped.iter().all(|c| entries.contains_key(c));
                let preferred_replied = match (cfg.prefer_addr, cfg.prefer_host) {
                    (Some(pa), _) => entries.contains_key(&pa),
                    (None, Some(ph)) => entries.keys().any(|a| a.ip() == ph),
                    (None, None) => true,
                };
                if all_replied || (preferred_replied && elapsed >= early_min) {
                    break;
                }
            }

            let step_wait = (deadline - now).min(Duration::from_millis(40));
            match tokio::time::timeout(step_wait, socket.recv_from(&mut recv_buf)).await {
                Ok(Ok((n, src))) => {
                    let pkt = &recv_buf[..n];
                    if n >= 35 && pkt[0] == 0x02 && pkt[1] == 0x7e {
                        if let Ok(parsed) = parse_init_zrtp_response(pkt) {
                            let rtt_ms = sent_at
                                .get(&src)
                                .or_else(|| {
                                    sent_at
                                        .iter()
                                        .find(|(k, _)| k.ip() == src.ip())
                                        .map(|(_, v)| v)
                                })
                                .map(|t0| now.duration_since(*t0).as_millis() as u32)
                                .unwrap_or(elapsed.as_millis() as u32);
                            let entry = entries.entry(src).or_insert(RelaySweepEntry {
                                addr: src,
                                recv: 0,
                                rtt_ms,
                                flow_token: parsed.flow_token,
                                reflexive_addr: parsed.reflexive_addr,
                            });
                            entry.recv += 1;
                            entry.flow_token = parsed.flow_token;
                        }
                    } else if unwrap_zrtc(pkt).is_some() {
                        // Preserve any media packet arriving mid-sweep!
                        buffered_early_media.push((src, pkt.to_vec()));
                    }
                }
                Ok(Err(e)) => return Err(ZrtcError::Io(e)),
                Err(_) => continue,
            }
        }

        if entries.is_empty() {
            return Err(ZrtcError::SweepNoResponse);
        }

        let mut results: Vec<RelaySweepEntry> = Vec::with_capacity(entries.len());
        for cand in &deduped {
            if let Some(e) = entries.remove(cand) {
                results.push(e);
            }
        }
        for (_, e) in entries {
            results.push(e);
        }

        // Choose preferred relay if it responded; otherwise highest `recv`, then lowest `rtt_ms`.
        let chosen = cfg
            .prefer_addr
            .and_then(|pa| results.iter().find(|r| r.addr == pa))
            .or_else(|| {
                cfg.prefer_host
                    .and_then(|ph| results.iter().find(|r| r.addr.ip() == ph))
            })
            .or_else(|| {
                results
                    .iter()
                    .max_by(|a, b| a.recv.cmp(&b.recv).then_with(|| b.rtt_ms.cmp(&a.rtt_ms)))
            })
            .cloned()
            .ok_or(ZrtcError::SweepNoResponse)?;

        let mut ft_by_addr = HashMap::with_capacity(results.len());
        let mut ft_by_ip = HashMap::with_capacity(results.len());
        for r in &results {
            ft_by_addr.insert(r.addr, r.flow_token);
            ft_by_ip.entry(r.addr.ip()).or_insert((r.addr, r.flow_token));
        }

        voip_log(
            "zrtc",
            &format!(
                "InitZRTP sweep selected relay={} ft={} ({} responder(s))",
                chosen.addr,
                hex::encode(chosen.flow_token),
                results.len()
            ),
        );

        let initial_seq = (rand::thread_rng().next_u32() & 0x7fff) as u16;
        let initial_ts = rand::thread_rng().next_u32() & 0x0fff_ffff;

        let session = Arc::new(Self {
            socket,
            ssrc: cfg.from_id,
            is_group: cfg.is_group,
            closed: AtomicBool::new(false),
            audio_seq: AtomicU16::new(initial_seq),
            audio_ts: AtomicU32::new(initial_ts),
            target: Mutex::new(ActiveRelayTarget {
                addr: chosen.addr,
                flow_token: chosen.flow_token,
            }),
            ft_by_addr,
            ft_by_ip,
            srtp: Mutex::new(srtp),
        });

        let (tx_ev, rx_ev) = mpsc::unbounded_channel();
        for (src, raw) in buffered_early_media {
            session.process_inbound_datagram(src, &raw, &tx_ev);
        }

        let session_rx = Arc::clone(&session);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while !session_rx.closed.load(Ordering::Relaxed) {
                match session_rx.socket.recv_from(&mut buf).await {
                    Ok((n, src)) => {
                        if session_rx.closed.load(Ordering::Relaxed) {
                            break;
                        }
                        session_rx.process_inbound_datagram(src, &buf[..n], &tx_ev);
                    }
                    Err(_) => {
                        if session_rx.closed.load(Ordering::Relaxed) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }
            }
        });

        let outcome = ZrtcSweepOutcome {
            chosen_relay: chosen.addr,
            chosen_flow_token: chosen.flow_token,
            local_addr,
            results,
        };

        Ok((session, outcome, rx_ev))
    }

    /// Creates an already-configured `ZrtcMediaSession` without running a handshake sweep
    /// (useful when relay + `flow_token` are pre-established).
    pub async fn bind_direct(
        bind_addr: SocketAddr,
        relay_addr: SocketAddr,
        flow_token: [u8; 4],
        ssrc: u32,
        srtp: SrtpContext,
        known_relays: HashMap<SocketAddr, [u8; 4]>,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<InboundZrtcEvent>), ZrtcError> {
        let socket = Arc::new(UdpSocket::bind(bind_addr).await?);
        let mut ft_by_addr = known_relays;
        ft_by_addr.entry(relay_addr).or_insert(flow_token);
        let mut ft_by_ip = HashMap::new();
        for (&addr, &ft) in &ft_by_addr {
            ft_by_ip.entry(addr.ip()).or_insert((addr, ft));
        }

        let session = Arc::new(Self {
            socket,
            ssrc,
            is_group: false,
            closed: AtomicBool::new(false),
            audio_seq: AtomicU16::new(1),
            audio_ts: AtomicU32::new(AUDIO_SAMPLES_PER_20MS),
            target: Mutex::new(ActiveRelayTarget {
                addr: relay_addr,
                flow_token,
            }),
            ft_by_addr,
            ft_by_ip,
            srtp: Mutex::new(srtp),
        });

        let (tx_ev, rx_ev) = mpsc::unbounded_channel();
        let session_rx = Arc::clone(&session);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while !session_rx.closed.load(Ordering::Relaxed) {
                match session_rx.socket.recv_from(&mut buf).await {
                    Ok((n, src)) => {
                        if session_rx.closed.load(Ordering::Relaxed) {
                            break;
                        }
                        session_rx.process_inbound_datagram(src, &buf[..n], &tx_ev);
                    }
                    Err(_) => break,
                }
            }
        });
        Ok((session, rx_ev))
    }

    /// Local bound socket address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Outbound SSRC (`from_id`).
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Currently active outbound relay address.
    pub fn active_relay(&self) -> SocketAddr {
        self.target.lock().addr
    }

    /// Currently active 4-byte outbound `flow_token`.
    pub fn active_flow_token(&self) -> [u8; 4] {
        self.target.lock().flow_token
    }

    /// Encapsulates a 20ms Opus frame into RTP (`PT=112` + `0xBEDE`), SRTP-protects it,
    /// wraps with `0x03` + `flow_token` (or `0x7F` in Group SFU mode), and sends it on the single socket.
    pub async fn send_audio_opus(&self, opus_frame: &[u8]) -> Result<(), ZrtcError> {
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let seq = self.audio_seq.fetch_add(1, Ordering::Relaxed);
        let ts = self
            .audio_ts
            .fetch_add(AUDIO_SAMPLES_PER_20MS, Ordering::Relaxed);
        let rtp = build_audio_rtp(seq, ts, self.ssrc, opus_frame);
        let srtp_pkt = self.srtp.lock().protect(&rtp)?;
        let target = self.target.lock().clone();
        let wire = if self.is_group {
            wrap_group_zrtc(0x06, self.ssrc, &srtp_pkt)
        } else {
            wrap_zrtc(ZRTC_AUDIO_RTP_OUT, &target.flow_token, &srtp_pkt)
        };
        self.socket.send_to(&wire, target.addr).await?;
        Ok(())
    }

    /// SRTP-protects pre-packetized Video RTP packets (e.g. from `packetize_zalo_h264_au`),
    /// wraps with `0x0D` + `flow_token` (or `0x7F` cmd=8 in Group mode), and transmits them.
    pub async fn send_video_rtp_packets(&self, rtp_packets: &[Vec<u8>]) -> Result<(), ZrtcError> {
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let target = self.target.lock().clone();
        for rtp in rtp_packets {
            let srtp_pkt = self.srtp.lock().protect(rtp)?;
            let wire = if self.is_group {
                wrap_group_zrtc(0x08, self.ssrc, &srtp_pkt)
            } else {
                wrap_zrtc(ZRTC_VIDEO_RTP_OUT, &target.flow_token, &srtp_pkt)
            };
            self.socket.send_to(&wire, target.addr).await?;
        }
        Ok(())
    }

    /// Sends an Audio RTCP control packet wrapped with `[0x05, flow_token[0..4]]`.
    pub async fn send_audio_rtcp(&self, rtcp: &[u8]) -> Result<(), ZrtcError> {
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let target = self.target.lock().clone();
        let wire = wrap_zrtc(ZRTC_AUDIO_RTCP, &target.flow_token, rtcp);
        self.socket.send_to(&wire, target.addr).await?;
        Ok(())
    }

    /// Sends a Video RTCP control packet (SR/REMB/PLI) wrapped with `[0x0F, flow_token[0..4]]`.
    pub async fn send_video_rtcp(&self, rtcp: &[u8]) -> Result<(), ZrtcError> {
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let target = self.target.lock().clone();
        let wire = wrap_zrtc(ZRTC_VIDEO_RTCP, &target.flow_token, rtcp);
        self.socket.send_to(&wire, target.addr).await?;
        Ok(())
    }

    /// Closes the media session and stops the background UDP receiver task.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    fn maybe_retarget(&self, src: SocketAddr, tx_ev: &mpsc::UnboundedSender<InboundZrtcEvent>) {
        let candidate = self
            .ft_by_addr
            .get(&src)
            .copied()
            .map(|ft| (src, ft))
            .or_else(|| self.ft_by_ip.get(&src.ip()).copied());

        if let Some((new_addr, new_ft)) = candidate {
            let mut guard = self.target.lock();
            if guard.addr != new_addr {
                let old_addr = guard.addr;
                guard.addr = new_addr;
                guard.flow_token = new_ft;
                drop(guard);
                voip_log(
                    "zrtc",
                    &format!(
                        "retargeted outbound relay {old_addr} -> {new_addr} ft={}",
                        hex::encode(new_ft)
                    ),
                );
                let _ = tx_ev.send(InboundZrtcEvent::RelayRetargeted {
                    old_relay: old_addr,
                    new_relay: new_addr,
                    new_flow_token: new_ft,
                });
            }
        }
    }

    fn process_inbound_datagram(
        &self,
        src: SocketAddr,
        datagram: &[u8],
        tx_ev: &mpsc::UnboundedSender<InboundZrtcEvent>,
    ) {
        let Some(unwrapped) = unwrap_zrtc(datagram) else {
            return;
        };

        // Dynamically retarget `active_relay` + `flow_token` when media arrives from another known relay!
        self.maybe_retarget(src, tx_ev);

        match unwrapped.kind {
            ZrtcPacketKind::AudioRtp => {
                let plain = match self.srtp.lock().unprotect(unwrapped.inner) {
                    Ok(p) => p,
                    Err(err) => {
                        voip_log(
                            "srtp",
                            &format!(
                                "audio unprotect failed from {src}: {err} raw={}",
                                hex_preview(datagram, 24)
                            ),
                        );
                        return;
                    }
                };
                if let Ok(rtp) = parse_rtp_packet(&plain) {
                    let _ = tx_ev.send(InboundZrtcEvent::AudioRtp {
                        outer_type: unwrapped.outer_type,
                        from_relay: src,
                        flow_token: unwrapped.flow_token,
                        group_peer_uid: unwrapped.group_peer_uid,
                        seq: rtp.seq,
                        timestamp: rtp.timestamp,
                        ssrc: rtp.ssrc,
                        opus_payload: rtp.payload.to_vec(),
                    });
                }
            }
            ZrtcPacketKind::VideoRtp => {
                let plain = match self.srtp.lock().unprotect(unwrapped.inner) {
                    Ok(p) => p,
                    Err(err) => {
                        voip_log(
                            "srtp",
                            &format!(
                                "video unprotect failed from {src}: {err} raw={}",
                                hex_preview(datagram, 24)
                            ),
                        );
                        return;
                    }
                };
                let _ = tx_ev.send(InboundZrtcEvent::VideoRtp {
                    outer_type: unwrapped.outer_type,
                    from_relay: src,
                    flow_token: unwrapped.flow_token,
                    group_peer_uid: unwrapped.group_peer_uid,
                    rtp_packet: plain,
                });
            }
            ZrtcPacketKind::AudioRtcp => {
                let _ = tx_ev.send(InboundZrtcEvent::AudioRtcp {
                    from_relay: src,
                    rtcp_packet: unwrapped.inner.to_vec(),
                });
            }
            ZrtcPacketKind::VideoRtcp => {
                let _ = tx_ev.send(InboundZrtcEvent::VideoRtcp {
                    from_relay: src,
                    rtcp_packet: unwrapped.inner.to_vec(),
                });
            }
        }
    }
}

impl Drop for ZrtcMediaSession {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}
