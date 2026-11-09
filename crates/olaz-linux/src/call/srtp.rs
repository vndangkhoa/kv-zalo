//! Pure-Rust SRTP (`AES_CM_128_HMAC_SHA1_80` + `ZaviNullAuthNoSeq`) implementation
//! per RFC 3711 §3.3.1 / §4.1.1 / §4.3.1 for Zalo 1-1 and Group SFU VoIP calls.

use aes::cipher::{KeyIvInit, StreamCipher};
use aes::Aes128;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::collections::HashMap;
use subtle::ConstantTimeEq;
use thiserror::Error;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;
type HmacSha1 = Hmac<Sha1>;

/// Required master material length (`16` master key bytes + `14` master salt bytes).
pub const SRTP_MASTER_MATERIAL_LEN: usize = 30;
/// 80-bit (`10`-byte) authentication tag length for `AES_CM_128_HMAC_SHA1_80`.
pub const SRTP_HMAC_SHA1_80_TAG_LEN: usize = 10;
/// Replay protection sliding window size in packets.
pub const SRTP_REPLAY_WINDOW_BITS: u64 = 64;

/// SRTP protection profile negotiated in `extendData.srtpMode` or SFU port selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SrtpMode {
    /// `srtpMode = 0`: unencrypted RTP pass-through.
    None,
    /// `srtpMode = 1`: standard RFC 3711 `AES_CM_128_HMAC_SHA1_80` (default for 1-1 calls).
    AesCm128HmacSha1_80,
    /// Group SFU (`:3000` / `:3500`) `ZaviNullAuthNoSeq` mode:
    /// AES-128-CTR keyed from RFC 3711 KDF with `no_seq = true` (`iv = salt XOR ssrc`) and 0-byte tag.
    ZaviNullAuthNoSeq,
}

impl SrtpMode {
    /// Maps Zalo's integer `srtpMode` field (`0` -> `None`, `2` -> `ZaviNullAuthNoSeq`, else `1`).
    pub fn from_wire_code(code: i64) -> Self {
        match code {
            0 => Self::None,
            2 => Self::ZaviNullAuthNoSeq,
            _ => Self::AesCm128HmacSha1_80,
        }
    }

    /// Returns the integer `srtpMode` value serialized in `extendData`.
    pub fn to_wire_code(self) -> i32 {
        match self {
            Self::None => 0,
            Self::AesCm128HmacSha1_80 => 1,
            Self::ZaviNullAuthNoSeq => 2,
        }
    }
}

/// Errors produced during SRTP key extraction, protection, or unprotection.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SrtpError {
    #[error("SRTP master material requires at least 30 bytes, got {0}")]
    ShortMasterMaterial(usize),
    #[error("RTP/SRTP packet too short: {0} bytes")]
    PacketTooShort(usize),
    #[error("unsupported RTP version {0} (expected 2)")]
    InvalidRtpVersion(u8),
    #[error("truncated RTP header or header extension (need {needed}, got {actual})")]
    TruncatedHeader { needed: usize, actual: usize },
    #[error("SRTP replay window rejected old packet (index={index}, max={max_index})")]
    ReplayTooOld { index: u64, max_index: u64 },
    #[error("SRTP replay window rejected duplicate packet (index={index})")]
    ReplayDuplicate { index: u64 },
    #[error("SRTP HMAC-SHA1-80 authentication failed")]
    AuthFailed,
}

/// Derived RFC 3711 §4.3.1 session keys for a single SRTP master key + salt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtpSessionKeys {
    pub cipher_key: [u8; 16],
    pub auth_key: [u8; 20],
    pub cipher_salt: [u8; 14],
}

impl SrtpSessionKeys {
    /// Extracts `master_key = sess_id[0..16]` and `master_salt = sess_id[16..30]`
    /// from Zalo's ASCII `sessId` and derives RFC 3711 session keys.
    pub fn from_sess_id(sess_id: &str) -> Result<Self, SrtpError> {
        let (master_key, master_salt) = extract_master_material(sess_id)?;
        Ok(Self::derive(&master_key, &master_salt))
    }

    /// Extracts `master_key = raw[0..16]` and `master_salt = raw[16..30]` from a 30+ byte slice.
    pub fn from_raw_master(raw: &[u8]) -> Result<Self, SrtpError> {
        if raw.len() < SRTP_MASTER_MATERIAL_LEN {
            return Err(SrtpError::ShortMasterMaterial(raw.len()));
        }
        let mut master_key = [0u8; 16];
        let mut master_salt = [0u8; 14];
        master_key.copy_from_slice(&raw[0..16]);
        master_salt.copy_from_slice(&raw[16..30]);
        Ok(Self::derive(&master_key, &master_salt))
    }

    /// RFC 3711 §4.3.1 AES-128-CTR key derivation (`key_derivation_rate = 0`, `r = 0`).
    ///
    /// `iv[0..14] = master_salt`, `iv[7] ^= label`, `iv[14..16] = 0`, encrypting zero bytes:
    /// - `label = 0x00` -> 16-byte `cipher_key`
    /// - `label = 0x01` -> 20-byte `auth_key`
    /// - `label = 0x02` -> 14-byte `cipher_salt`
    pub fn derive(master_key: &[u8; 16], master_salt: &[u8; 14]) -> Self {
        let mut cipher_key = [0u8; 16];
        kdf_prf(master_key, master_salt, 0x00, &mut cipher_key);

        let mut auth_key = [0u8; 20];
        kdf_prf(master_key, master_salt, 0x01, &mut auth_key);

        let mut cipher_salt = [0u8; 14];
        kdf_prf(master_key, master_salt, 0x02, &mut cipher_salt);

        Self {
            cipher_key,
            auth_key,
            cipher_salt,
        }
    }
}

/// Extracts `(master_key, master_salt)` from `&sess_id.as_bytes()[0..30]`.
pub fn extract_master_material(sess_id: &str) -> Result<([u8; 16], [u8; 14]), SrtpError> {
    let raw = sess_id.as_bytes();
    if raw.len() < SRTP_MASTER_MATERIAL_LEN {
        return Err(SrtpError::ShortMasterMaterial(raw.len()));
    }
    let mut master_key = [0u8; 16];
    let mut master_salt = [0u8; 14];
    master_key.copy_from_slice(&raw[0..16]);
    master_salt.copy_from_slice(&raw[16..30]);
    Ok((master_key, master_salt))
}

fn kdf_prf(master_key: &[u8; 16], master_salt: &[u8; 14], label: u8, out_zeros: &mut [u8]) {
    let mut iv = [0u8; 16];
    iv[0..14].copy_from_slice(master_salt);
    iv[7] ^= label;
    let mut cipher = Aes128Ctr::new(master_key.into(), (&iv).into());
    cipher.apply_keystream(out_zeros);
}

/// Builds the 16-byte AES-128-CTR initial counter block per RFC 3711 §4.1.1:
/// `IV = (k_s * 2^16) XOR (SSRC * 2^64) XOR (i * 2^16)`.
pub fn build_srtp_iv(cipher_salt: &[u8; 14], ssrc: u32, packet_index_48: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[0..14].copy_from_slice(cipher_salt);
    let ssrc_be = ssrc.to_be_bytes();
    for j in 0..4 {
        iv[4 + j] ^= ssrc_be[j];
    }
    let idx_be = packet_index_48.to_be_bytes();
    // Low 48 bits live in `idx_be[2..8]` and XOR into `iv[8..14]`.
    for j in 0..6 {
        iv[8 + j] ^= idx_be[2 + j];
    }
    iv
}

/// Builds the 16-byte AES-128-CTR IV for `SrtpMode::ZaviNullAuthNoSeq` (`no_seq = true`):
/// XORs only `ssrc` (`u32` BE) into `iv[4..8]`, leaving sequence/ROC out of the counter block.
pub fn build_zavi_no_seq_iv(cipher_salt: &[u8; 14], ssrc: u32) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[0..14].copy_from_slice(cipher_salt);
    let ssrc_be = ssrc.to_be_bytes();
    for j in 0..4 {
        iv[4 + j] ^= ssrc_be[j];
    }
    iv
}

/// Computes the exact unencrypted RTP header byte length (including CSRCs and any `0xBEDE` / RFC 3550
/// header extension) together with `(seq, ssrc)`.
pub fn rtp_header_layout(pkt: &[u8]) -> Result<(usize, u16, u32), SrtpError> {
    if pkt.len() < 12 {
        return Err(SrtpError::PacketTooShort(pkt.len()));
    }
    let version = pkt[0] >> 6;
    if version != 2 {
        return Err(SrtpError::InvalidRtpVersion(version));
    }
    let cc = (pkt[0] & 0x0f) as usize;
    let has_ext = (pkt[0] & 0x10) != 0;
    let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
    let ssrc = u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]);

    let mut hdr_len = 12 + cc * 4;
    if pkt.len() < hdr_len {
        return Err(SrtpError::TruncatedHeader {
            needed: hdr_len,
            actual: pkt.len(),
        });
    }
    if has_ext {
        if pkt.len() < hdr_len + 4 {
            return Err(SrtpError::TruncatedHeader {
                needed: hdr_len + 4,
                actual: pkt.len(),
            });
        }
        let ext_words = u16::from_be_bytes([pkt[hdr_len + 2], pkt[hdr_len + 3]]) as usize;
        hdr_len += 4 + ext_words * 4;
        if pkt.len() < hdr_len {
            return Err(SrtpError::TruncatedHeader {
                needed: hdr_len,
                actual: pkt.len(),
            });
        }
    }
    Ok((hdr_len, seq, ssrc))
}

#[derive(Debug, Clone, Default)]
struct TxStreamState {
    initialized: bool,
    last_seq: u16,
    roc: u32,
}

impl TxStreamState {
    fn next_packet_index(&mut self, seq: u16) -> (u32, u64) {
        if !self.initialized {
            self.initialized = true;
            self.last_seq = seq;
        } else {
            // Detect 16-bit sequence rollover on outbound stream.
            if seq < self.last_seq && (self.last_seq - seq) > 32_768 {
                self.roc = self.roc.wrapping_add(1);
            }
            self.last_seq = seq;
        }
        let index = ((self.roc as u64) << 16) | (seq as u64);
        (self.roc, index)
    }
}

#[derive(Debug, Clone, Default)]
struct RxStreamState {
    initialized: bool,
    s_l: u16,
    roc: u32,
    max_index: u64,
    replay_bitmap: u64,
}

impl RxStreamState {
    /// RFC 3711 §3.3.1 rollover counter estimation without mutating receiver state.
    fn estimate_roc(&self, seq: u16) -> (u32, u64) {
        if !self.initialized {
            let v = self.roc;
            let idx = ((v as u64) << 16) | (seq as u64);
            return (v, idx);
        }
        let v = if self.s_l < 32_768 {
            if (seq as i32) - (self.s_l as i32) > 32_768 {
                self.roc.saturating_sub(1)
            } else {
                self.roc
            }
        } else if (self.s_l as i32) - 32_768 > (seq as i32) {
            self.roc.wrapping_add(1)
        } else {
            self.roc
        };
        let idx = ((v as u64) << 16) | (seq as u64);
        (v, idx)
    }

    fn check_replay(&self, index: u64) -> Result<(), SrtpError> {
        if !self.initialized || index > self.max_index {
            return Ok(());
        }
        let delta = self.max_index - index;
        if delta >= SRTP_REPLAY_WINDOW_BITS {
            return Err(SrtpError::ReplayTooOld {
                index,
                max_index: self.max_index,
            });
        }
        if ((self.replay_bitmap >> delta) & 1) != 0 {
            return Err(SrtpError::ReplayDuplicate { index });
        }
        Ok(())
    }

    fn commit_authenticated(&mut self, seq: u16, candidate_roc: u32, index: u64) {
        if !self.initialized {
            self.initialized = true;
            self.s_l = seq;
            self.roc = candidate_roc;
            self.max_index = index;
            self.replay_bitmap = 1;
            return;
        }
        if index > self.max_index {
            let shift = index - self.max_index;
            self.replay_bitmap = if shift >= SRTP_REPLAY_WINDOW_BITS {
                1
            } else {
                (self.replay_bitmap << shift) | 1
            };
            self.max_index = index;
            self.roc = candidate_roc;
            self.s_l = seq;
        } else {
            let delta = self.max_index - index;
            self.replay_bitmap |= 1u64 << delta;
        }
    }
}

/// Bidirectional SRTP context with separate outbound (`tx`) and per-SSRC inbound (`rx`) state.
#[derive(Debug, Clone)]
pub struct SrtpContext {
    mode: SrtpMode,
    keys: Option<SrtpSessionKeys>,
    tx_streams: HashMap<u32, TxStreamState>,
    rx_streams: HashMap<u32, RxStreamState>,
}

impl SrtpContext {
    /// Creates an unencrypted pass-through SRTP context (`SrtpMode::None`).
    pub fn pass_through() -> Self {
        Self {
            mode: SrtpMode::None,
            keys: None,
            tx_streams: HashMap::new(),
            rx_streams: HashMap::new(),
        }
    }

    /// Creates an SRTP context from Zalo's `sessId` and selected `SrtpMode`.
    pub fn from_sess_id(sess_id: &str, mode: SrtpMode) -> Result<Self, SrtpError> {
        if mode == SrtpMode::None {
            return Ok(Self::pass_through());
        }
        let keys = SrtpSessionKeys::from_sess_id(sess_id)?;
        Ok(Self::from_keys(keys, mode))
    }

    /// Creates an SRTP context from a raw 30-byte master key+salt buffer.
    pub fn from_raw_master(raw: &[u8], mode: SrtpMode) -> Result<Self, SrtpError> {
        if mode == SrtpMode::None {
            return Ok(Self::pass_through());
        }
        let keys = SrtpSessionKeys::from_raw_master(raw)?;
        Ok(Self::from_keys(keys, mode))
    }

    /// Creates an SRTP context from pre-derived session keys.
    pub fn from_keys(keys: SrtpSessionKeys, mode: SrtpMode) -> Self {
        Self {
            mode,
            keys: Some(keys),
            tx_streams: HashMap::new(),
            rx_streams: HashMap::new(),
        }
    }

    /// Active SRTP protection mode.
    pub fn mode(&self) -> SrtpMode {
        self.mode
    }

    /// Reference to derived session keys (if `mode != SrtpMode::None`).
    pub fn keys(&self) -> Option<&SrtpSessionKeys> {
        self.keys.as_ref()
    }

    /// Current inbound ROC for `ssrc` (useful for RFC 3711 rollover tests/diagnostics).
    pub fn rx_roc(&self, ssrc: u32) -> u32 {
        self.rx_streams.get(&ssrc).map(|s| s.roc).unwrap_or(0)
    }

    /// Current outbound ROC for `ssrc`.
    pub fn tx_roc(&self, ssrc: u32) -> u32 {
        self.tx_streams.get(&ssrc).map(|s| s.roc).unwrap_or(0)
    }

    /// Seeds outbound ROC for `ssrc` (primarily for testing rollover boundaries).
    pub fn set_tx_roc_for_test(&mut self, ssrc: u32, roc: u32, last_seq: u16) {
        self.tx_streams.insert(
            ssrc,
            TxStreamState {
                initialized: true,
                last_seq,
                roc,
            },
        );
    }

    /// Protects an outbound RTP packet (`header + extension` stays cleartext, payload encrypted,
    /// 10-byte HMAC-SHA1 tag appended in `AesCm128HmacSha1_80`).
    pub fn protect(&mut self, rtp: &[u8]) -> Result<Vec<u8>, SrtpError> {
        let (hdr_len, seq, ssrc) = rtp_header_layout(rtp)?;
        match self.mode {
            SrtpMode::None => Ok(rtp.to_vec()),
            SrtpMode::ZaviNullAuthNoSeq => {
                let keys = self
                    .keys
                    .as_ref()
                    .ok_or(SrtpError::ShortMasterMaterial(0))?;
                let mut out = rtp.to_vec();
                let iv = build_zavi_no_seq_iv(&keys.cipher_salt, ssrc);
                let mut cipher = Aes128Ctr::new((&keys.cipher_key).into(), (&iv).into());
                cipher.apply_keystream(&mut out[hdr_len..]);
                Ok(out)
            }
            SrtpMode::AesCm128HmacSha1_80 => {
                let keys = self
                    .keys
                    .as_ref()
                    .ok_or(SrtpError::ShortMasterMaterial(0))?;
                let tx = self.tx_streams.entry(ssrc).or_default();
                let (roc, packet_index) = tx.next_packet_index(seq);

                let mut out = Vec::with_capacity(rtp.len() + SRTP_HMAC_SHA1_80_TAG_LEN);
                out.extend_from_slice(rtp);

                let iv = build_srtp_iv(&keys.cipher_salt, ssrc, packet_index);
                let mut cipher = Aes128Ctr::new((&keys.cipher_key).into(), (&iv).into());
                cipher.apply_keystream(&mut out[hdr_len..]);

                let tag = compute_srtp_hmac_tag(&keys.auth_key, &out, roc);
                out.extend_from_slice(&tag);
                Ok(out)
            }
        }
    }

    /// Authenticates, checks replay protection, and decrypts an inbound SRTP packet into RTP.
    pub fn unprotect(&mut self, srtp: &[u8]) -> Result<Vec<u8>, SrtpError> {
        let (hdr_len, seq, ssrc) = rtp_header_layout(srtp)?;
        match self.mode {
            SrtpMode::None => Ok(srtp.to_vec()),
            SrtpMode::ZaviNullAuthNoSeq => {
                let keys = self
                    .keys
                    .as_ref()
                    .ok_or(SrtpError::ShortMasterMaterial(0))?;
                let payload = &srtp[hdr_len..];
                // Group SFU occasionally forwards unencrypted Annex-B keyframes directly; preserve if already clear.
                if is_plaintext_zalo_h264(payload) {
                    return Ok(srtp.to_vec());
                }
                let mut out = srtp.to_vec();
                let iv = build_zavi_no_seq_iv(&keys.cipher_salt, ssrc);
                let mut cipher = Aes128Ctr::new((&keys.cipher_key).into(), (&iv).into());
                cipher.apply_keystream(&mut out[hdr_len..]);
                Ok(out)
            }
            SrtpMode::AesCm128HmacSha1_80 => {
                if srtp.len() < hdr_len + SRTP_HMAC_SHA1_80_TAG_LEN {
                    return Err(SrtpError::PacketTooShort(srtp.len()));
                }
                let keys = self
                    .keys
                    .as_ref()
                    .ok_or(SrtpError::ShortMasterMaterial(0))?;
                let body_end = srtp.len() - SRTP_HMAC_SHA1_80_TAG_LEN;
                let authenticated_portion = &srtp[..body_end];
                let received_tag = &srtp[body_end..];

                let rx_entry = self.rx_streams.entry(ssrc).or_default();
                let (candidate_roc, packet_index) = rx_entry.estimate_roc(seq);
                rx_entry.check_replay(packet_index)?;

                let expected_tag =
                    compute_srtp_hmac_tag(&keys.auth_key, authenticated_portion, candidate_roc);
                if !bool::from(received_tag.ct_eq(&expected_tag)) {
                    return Err(SrtpError::AuthFailed);
                }

                rx_entry.commit_authenticated(seq, candidate_roc, packet_index);

                let mut out = authenticated_portion.to_vec();
                let iv = build_srtp_iv(&keys.cipher_salt, ssrc, packet_index);
                let mut cipher = Aes128Ctr::new((&keys.cipher_key).into(), (&iv).into());
                cipher.apply_keystream(&mut out[hdr_len..]);
                Ok(out)
            }
        }
    }
}

fn compute_srtp_hmac_tag(
    auth_key: &[u8; 20],
    authenticated_portion: &[u8],
    roc: u32,
) -> [u8; SRTP_HMAC_SHA1_80_TAG_LEN] {
    let mut mac = HmacSha1::new_from_slice(auth_key).expect("20-byte SRTP HMAC key is valid");
    mac.update(authenticated_portion);
    mac.update(&roc.to_be_bytes());
    let full = mac.finalize().into_bytes();
    let mut tag = [0u8; SRTP_HMAC_SHA1_80_TAG_LEN];
    tag.copy_from_slice(&full[..SRTP_HMAC_SHA1_80_TAG_LEN]);
    tag
}

fn is_plaintext_zalo_h264(payload: &[u8]) -> bool {
    if payload.len() >= 5 && payload[0..4] == [0x00, 0x00, 0x00, 0x01] {
        let nal = payload[4] & 0x1f;
        return matches!(nal, 1 | 5 | 6 | 7 | 8 | 9);
    }
    if payload.len() >= 7
        && payload[0] == 0x1c
        && matches!(payload[1], 0x80 | 0x00 | 0x40)
        && payload[2..6] == [0x00, 0x00, 0x00, 0x01]
    {
        let nal = payload[6] & 0x1f;
        return matches!(nal, 1 | 5 | 6 | 7 | 8 | 9);
    }
    false
}
