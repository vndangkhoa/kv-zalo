//! Streaming E2EE / Zalo Cloud (`pcloud`) media decryption (`SecureStream`) and
//! `new-download-channel` download manager (`DownloadManager`).
//!
//! Port of `DownloadManager.swift`:
//! - Fail-closed `SecureSpec::parse` (`Invalid` -> error `14998` `INS_OF_ERROR_ERR`, never saving raw ciphertext).
//! - Streaming decryption into `<dest>.part` + `sync_all()` + atomic `rename`:
//!   1. `Aes256CtrLinear` (continuous `Ctr128BE<Aes256>`)
//!   2. `Aes256CtrBlock` (per-`block_size` cipher reset with same `(key, iv)`, trailing `< block_size` bytes passed through verbatim via `skipOverflow`)
//!   3. `Aes256GcmLinear` (O(1)-memory 1 MiB chunked `GHash` + `Ctr128BE<Aes256>` from `IV || 0x00000002` for 12-byte IVs with constant-time tag verification and `.part` deletion on mismatch; bounded 256 MiB `aes_gcm` one-shot for non-12-byte IVs).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes::cipher::{BlockEncrypt, InnerIvInit, KeyInit, KeyIvInit, StreamCipher, StreamCipherCore};
use aes::Aes256;
use aes_gcm::aead::{consts::U16, AeadInPlace};
use aes_gcm::AesGcm;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine as _;
use ctr::Ctr128BE;
use dashmap::DashMap;
use ghash::universal_hash::UniversalHash;
use ghash::GHash;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::sync::Notify;
use url::Url;
use uuid::Uuid;

use crate::media_loader::{
    MediaLoadError, MediaLoader, DEFAULT_IMAGE_DEADLINE, MAX_MEDIA_FILE_BYTES, MAX_PHOTO_BYTES,
};

/// IPC channel name registered on the main bridge (`DownloadManager.channel`).
pub const DOWNLOAD_IPC_CHANNEL: &str = "new-download-channel";

/// Sub-channel initiating a download (`DownloadManager.Request.download`).
pub const DOWNLOAD_REQ_START: &str = "new-download-file";

/// Sub-channel aborting an in-flight download (`DownloadManager.Request.abort`).
pub const DOWNLOAD_REQ_ABORT: &str = "new-download-file-abort";

/// Sub-channel carrying download completion payloads (`DownloadManager.responseChannel`).
pub const DOWNLOAD_RESP_CHANNEL: &str = "new-download-file-resp";

/// Streaming buffer chunk size (`1 MiB`).
pub const STREAM_CHUNK_BYTES: usize = 1 << 20;

/// Maximum buffered payload size for non-12-byte-IV AES-GCM (`256 MiB`).
pub const GCM_ONE_SHOT_CAP: usize = 256 << 20;

/// Negative cache TTL for HTTP 404 URLs (`DownloadManager.notFoundTTL` = 600s).
pub const NOT_FOUND_CACHE_TTL: Duration = Duration::from_secs(600);

/// Maximum entries retained in the 404 negative URL cache (`500`).
pub const NOT_FOUND_CACHE_CAPACITY: usize = 500;

type Ctr32BE<'a> = ctr::CtrCore<&'a Aes256, ctr::flavors::Ctr32BE>;

/// Error returned when `SecureStream` decryption or authentication fails.
#[derive(Debug, Error)]
pub enum DecryptError {
    #[error("invalid cryptographic parameters")]
    InvalidParameters,
    #[error("AES-GCM authentication tag mismatch")]
    TagMismatch,
    #[error("non-standard GCM payload exceeds 256 MiB cap")]
    PayloadTooLarge,
    #[error("I/O error during stream decryption: {0}")]
    Io(#[from] std::io::Error),
}

/// Normalized error wire payload matching Zalo PC's `DownloadErrors` / `normalizeError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadFailure {
    pub name: String,
    pub code: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

impl DownloadFailure {
    pub fn aborted() -> Self {
        Self {
            name: "GENERAL_MAIN_CONTROLLER_OBJ_DOWNLOAD_ABORT".to_owned(),
            code: 1009,
            message: String::new(),
        }
    }

    pub fn invalid_url() -> Self {
        Self {
            name: "GENERAL_UI_OBJ_DOWNLOAD_INVALID_URL".to_owned(),
            code: 9,
            message: String::new(),
        }
    }

    pub fn save_dir_invalid() -> Self {
        Self {
            name: "GENERAL_MAIN_CONTROLLER_OBJ_DOWNLOAD_SAVE_DIR_INVALID".to_owned(),
            code: 1012,
            message: String::new(),
        }
    }

    pub fn copy_src_not_found() -> Self {
        Self {
            name: "COPY_SERVICE_SRC_NOT_FOUND".to_owned(),
            code: 1013,
            message: String::new(),
        }
    }

    pub fn empty_body() -> Self {
        Self {
            name: "DOWNLOAD_SERVICE_SINGLE_INVALID_CONTENT_LENGTH".to_owned(),
            code: 75000 + 5002,
            message: String::new(),
        }
    }

    pub fn ttfb_timeout() -> Self {
        Self {
            name: "DOWNLOAD_SERVICE_SINGLE_MAX_TTFB_TIMEOUT".to_owned(),
            code: 75000 + 5027,
            message: String::new(),
        }
    }

    pub fn decrypt_failed() -> Self {
        Self {
            name: "INS_OF_ERROR_ERR".to_owned(),
            code: 14998,
            message: "File secure decrypt failed".to_owned(),
        }
    }

    pub fn too_large() -> Self {
        Self {
            name: "DOWNLOAD_SERVICE_SINGLE_FILE_SIZE_LIMIT_EXCEED".to_owned(),
            code: 75000 + 5026,
            message: String::new(),
        }
    }

    pub fn unknown() -> Self {
        Self {
            name: "DOWNLOAD_SERVICE_SINGLE_DOWNLOAD_ON_EMPTY_ERR".to_owned(),
            code: 75000 + 5025,
            message: String::new(),
        }
    }

    pub fn http(status: u16) -> Self {
        Self {
            name: format!("DOWNLOAD_SERVICE_NET_{status}"),
            code: 75000 + i64::from(status),
            message: String::new(),
        }
    }

    pub fn net(name: &str, number: i64) -> Self {
        Self {
            name: name.to_owned(),
            code: 30000 + number,
            message: String::new(),
        }
    }

    /// Maps a [`MediaLoadError`] to Zalo PC's exact main-process error code.
    pub fn from_media_error(err: &MediaLoadError) -> Self {
        match err {
            MediaLoadError::BadUrl => Self::invalid_url(),
            MediaLoadError::Http(status) => Self::http(*status),
            MediaLoadError::HostNotFound => Self::net("ERR_NAME_NOT_RESOLVED", 34),
            MediaLoadError::TtfbTimeout => Self::ttfb_timeout(),
            MediaLoadError::TimedOut => Self::net("ERR_CONNECTION_TIMED_OUT", 47),
            MediaLoadError::TooLarge => Self::too_large(),
            MediaLoadError::TooManyRedirects => Self::net("ERR_TOO_MANY_REDIRECTS", 310),
            MediaLoadError::ConnectionRefused(_) => Self::net("ERR_CONNECTION_REFUSED", 31),
            MediaLoadError::TlsError(_) => Self::net("ERR_SSL_PROTOCOL_ERROR", 36),
            MediaLoadError::Disconnected(_) => Self::net("ERR_INTERNET_DISCONNECTED", 35),
            MediaLoadError::Network(_) => Self::net("ERR_CONNECTION_FAILED", 33),
            MediaLoadError::Io(_) => Self::unknown(),
        }
    }

    /// Serializes to `{ name, code, normalize: true, message? }`.
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("name".to_owned(), Value::String(self.name.clone()));
        obj.insert("code".to_owned(), Value::Number(self.code.into()));
        obj.insert("normalize".to_owned(), Value::Bool(true));
        if !self.message.is_empty() {
            obj.insert("message".to_owned(), Value::String(self.message.clone()));
        }
        Value::Object(obj)
    }
}

/// Supported Zalo E2EE / `pcloud` stream decryption modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecureMode {
    /// Continuous AES-256-CTR (`ctr::Ctr128BE<Aes256>`) over the entire stream.
    Aes256CtrLinear { key: [u8; 32], iv: [u8; 16] },
    /// Block-resetting AES-256-CTR: re-initializes `Ctr128BE<Aes256>` with `(key, iv)` every
    /// `block_size` bytes and passes any `< block_size` EOF tail through verbatim (`skipOverflow`).
    Aes256CtrBlock {
        key: [u8; 32],
        iv: [u8; 16],
        block_size: usize,
    },
    /// Authenticated AES-256-GCM over the entire stream (`iv.len() >= 12`, 16-byte `tag`).
    Aes256GcmLinear {
        key: [u8; 32],
        iv: Vec<u8>,
        tag: [u8; 16],
    },
}

pub use SecureMode::{Aes256CtrBlock, Aes256CtrLinear, Aes256GcmLinear};

/// Parsed `secure` descriptor from a download request (`SecureSpec::parse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecureSpec {
    /// No decryption requested (`secure` absent or `type` is neither `"e2ee"` nor `"pcloud"`).
    None,
    /// Valid decryption specification.
    Stream(SecureStream),
    /// `secure.type` is `"e2ee"` or `"pcloud"` with unknown/malformed parameters — MUST fail closed (`14998`).
    Invalid,
}

impl SecureSpec {
    /// Parses a `secure` JSON payload (`{ type: "e2ee" | "pcloud", cryptography: { ... } }`).
    ///
    /// Fails closed ([`SecureSpec::Invalid`]) whenever `type` is `"e2ee"` or `"pcloud"` and any
    /// cryptographic parameter is unknown, missing, or malformed.
    pub fn parse(value: &Value) -> Self {
        let Some(dict) = value.as_object() else {
            return Self::None;
        };
        let Some(sec_type) = dict.get("type").and_then(|v| v.as_str()) else {
            return Self::None;
        };
        if sec_type != "e2ee" && sec_type != "pcloud" {
            return Self::None;
        }

        let Some(crypto) = dict.get("cryptography").and_then(|v| v.as_object()) else {
            return Self::Invalid;
        };
        let Some(alg) = crypto.get("alg").and_then(|v| v.as_str()) else {
            return Self::Invalid;
        };

        let Some(key_vec) = parse_crypto_bytes(crypto.get("key")) else {
            return Self::Invalid;
        };
        let Ok(key): std::result::Result<[u8; 32], _> = key_vec.try_into() else {
            return Self::Invalid;
        };

        let Some(iv_vec) = parse_crypto_bytes(crypto.get("iv")) else {
            return Self::Invalid;
        };
        if iv_vec.is_empty() {
            return Self::Invalid;
        }

        let is_block = match crypto.get("type") {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) if s == "linear" => false,
            Some(Value::String(s)) if s == "block" => true,
            _ => return Self::Invalid,
        };

        match alg {
            "aes-256-ctr" => {
                let Ok(iv): std::result::Result<[u8; 16], _> = iv_vec.try_into() else {
                    return Self::Invalid;
                };
                if is_block {
                    let block_size = match crypto.get("blockSize") {
                        None | Some(Value::Null) => 4096usize,
                        Some(v) => {
                            let Some(raw) = v.as_i64() else {
                                return Self::Invalid;
                            };
                            raw.max(16) as usize
                        }
                    };
                    Self::Stream(SecureStream::new(SecureMode::Aes256CtrBlock {
                        key,
                        iv,
                        block_size,
                    }))
                } else {
                    Self::Stream(SecureStream::new(SecureMode::Aes256CtrLinear { key, iv }))
                }
            }
            "aes-256-gcm" => {
                if is_block || iv_vec.len() < 12 {
                    return Self::Invalid;
                }
                let Some(tag_vec) = parse_crypto_bytes(crypto.get("tag")) else {
                    return Self::Invalid;
                };
                let Ok(tag): std::result::Result<[u8; 16], _> = tag_vec.try_into() else {
                    return Self::Invalid;
                };
                Self::Stream(SecureStream::new(SecureMode::Aes256GcmLinear {
                    key,
                    iv: iv_vec,
                    tag,
                }))
            }
            _ => Self::Invalid,
        }
    }

    pub fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid)
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub fn stream(&self) -> Option<&SecureStream> {
        match self {
            Self::Stream(s) => Some(s),
            _ => None,
        }
    }
}

/// Streaming decryptor executing [`SecureMode`] transformations into `<dest>.part` + `sync_all()` + `rename`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecureStream {
    mode: SecureMode,
}

impl SecureStream {
    pub fn new(mode: SecureMode) -> Self {
        Self { mode }
    }

    pub fn ctr_linear(key: [u8; 32], iv: [u8; 16]) -> Self {
        Self::new(SecureMode::Aes256CtrLinear { key, iv })
    }

    pub fn ctr_block(key: [u8; 32], iv: [u8; 16], block_size: usize) -> Self {
        Self::new(SecureMode::Aes256CtrBlock {
            key,
            iv,
            block_size: block_size.max(16),
        })
    }

    pub fn gcm_linear(key: [u8; 32], iv: Vec<u8>, tag: [u8; 16]) -> Self {
        Self::new(SecureMode::Aes256GcmLinear { key, iv, tag })
    }

    pub fn mode(&self) -> &SecureMode {
        &self.mode
    }

    /// Returns the deterministic `.part` staging path (`<destination>.part`) used during decryption.
    pub fn part_path_for(destination: &Path) -> PathBuf {
        let mut os = destination.as_os_str().to_os_string();
        os.push(".part");
        PathBuf::from(os)
    }

    /// Decrypts an in-memory buffer (bounded photos or test vectors).
    pub fn decrypt_bytes(&self, ciphertext: &[u8]) -> std::result::Result<Vec<u8>, DecryptError> {
        match &self.mode {
            SecureMode::Aes256CtrLinear { key, iv } => {
                let mut out = ciphertext.to_vec();
                let mut cipher = Ctr128BE::<Aes256>::new(key.into(), iv.into());
                cipher.apply_keystream(&mut out);
                Ok(out)
            }
            SecureMode::Aes256CtrBlock {
                key,
                iv,
                block_size,
            } => {
                let bs = (*block_size).max(16);
                let mut out = ciphertext.to_vec();
                let full_blocks = out.len() / bs;
                for i in 0..full_blocks {
                    let start = i * bs;
                    let end = start + bs;
                    let mut cipher = Ctr128BE::<Aes256>::new(key.into(), iv.into());
                    cipher.apply_keystream(&mut out[start..end]);
                }
                // Trailing `< bs` bytes remain verbatim (`skipOverflow`).
                Ok(out)
            }
            SecureMode::Aes256GcmLinear { key, iv, tag } => {
                if iv.len() < 12 {
                    return Err(DecryptError::InvalidParameters);
                }
                if iv.len() == 12 {
                    let mut out = Vec::with_capacity(ciphertext.len());
                    let mut gcm = StreamingGcm96::new(key, iv[..12].try_into().unwrap());
                    gcm.decrypt_chunk(ciphertext, &mut out);
                    if !gcm.verify(tag) {
                        return Err(DecryptError::TagMismatch);
                    }
                    Ok(out)
                } else {
                    decrypt_gcm_non96_oneshot(key, iv, tag, ciphertext)
                }
            }
        }
    }

    /// Streams decryption from `source` into `<destination>.part`, verifies integrity, `sync_all()`s,
    /// and atomically renames to `destination`. Deletes `<destination>.part` on any error.
    pub fn decrypt_file(
        &self,
        source: &Path,
        destination: &Path,
    ) -> std::result::Result<u64, DecryptError> {
        let input = File::open(source)?;
        self.decrypt_reader_to_path(input, destination)
    }

    /// Streams decryption from any [`Read`] into `<destination>.part` + `sync_all()` + atomic `rename`.
    ///
    /// On any I/O error or GCM authentication tag mismatch, `<destination>.part` is deleted and
    /// `destination` is never created.
    pub fn decrypt_reader_to_path<R: Read>(
        &self,
        mut reader: R,
        destination: &Path,
    ) -> std::result::Result<u64, DecryptError> {
        if let Some(parent) = destination.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let part_path = Self::part_path_for(destination);
        let _ = fs::remove_file(&part_path);

        let result = (|| -> std::result::Result<u64, DecryptError> {
            let mut out_file = File::create(&part_path)?;
            let mut total_written: u64 = 0;

            match &self.mode {
                SecureMode::Aes256CtrLinear { key, iv } => {
                    let mut cipher = Ctr128BE::<Aes256>::new(key.into(), iv.into());
                    let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
                    loop {
                        let n = reader.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        cipher.apply_keystream(&mut buf[..n]);
                        out_file.write_all(&buf[..n])?;
                        total_written += n as u64;
                    }
                }
                SecureMode::Aes256CtrBlock {
                    key,
                    iv,
                    block_size,
                } => {
                    let bs = (*block_size).max(16);
                    let chunk_cap = bs * (STREAM_CHUNK_BYTES / bs).max(1);
                    let mut carry = Vec::with_capacity(chunk_cap + bs);
                    let mut read_buf = vec![0u8; chunk_cap];

                    loop {
                        let n = reader.read(&mut read_buf)?;
                        if n == 0 {
                            break;
                        }
                        carry.extend_from_slice(&read_buf[..n]);

                        let full_len = (carry.len() / bs) * bs;
                        if full_len > 0 {
                            for block_slice in carry[..full_len].chunks_exact_mut(bs) {
                                let mut cipher = Ctr128BE::<Aes256>::new(key.into(), iv.into());
                                cipher.apply_keystream(block_slice);
                            }
                            out_file.write_all(&carry[..full_len])?;
                            total_written += full_len as u64;
                            carry.drain(..full_len);
                        }
                    }

                    // At true EOF, write any trailing `< block_size` overflow verbatim (`skipOverflow`).
                    if !carry.is_empty() {
                        out_file.write_all(&carry)?;
                        total_written += carry.len() as u64;
                    }
                }
                SecureMode::Aes256GcmLinear { key, iv, tag } => {
                    if iv.len() < 12 {
                        return Err(DecryptError::InvalidParameters);
                    }
                    if iv.len() == 12 {
                        let iv12: &[u8; 12] = iv[..12].try_into().unwrap();
                        let mut gcm = StreamingGcm96::new(key, iv12);
                        let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
                        let mut plain_chunk = Vec::with_capacity(STREAM_CHUNK_BYTES);

                        loop {
                            let n = reader.read(&mut buf)?;
                            if n == 0 {
                                break;
                            }
                            plain_chunk.clear();
                            gcm.decrypt_chunk(&buf[..n], &mut plain_chunk);
                            out_file.write_all(&plain_chunk)?;
                            total_written += plain_chunk.len() as u64;
                        }

                        if !gcm.verify(tag) {
                            return Err(DecryptError::TagMismatch);
                        }
                    } else {
                        let mut cipher_buf = Vec::new();
                        let mut limited =
                            (&mut reader).take((GCM_ONE_SHOT_CAP as u64).saturating_add(1));
                        limited.read_to_end(&mut cipher_buf)?;
                        if cipher_buf.len() > GCM_ONE_SHOT_CAP {
                            return Err(DecryptError::PayloadTooLarge);
                        }
                        let plain = decrypt_gcm_non96_oneshot(key, iv, tag, &cipher_buf)?;
                        out_file.write_all(&plain)?;
                        total_written = plain.len() as u64;
                    }
                }
            }

            out_file.sync_all()?;
            drop(out_file);
            fs::rename(&part_path, destination)?;
            Ok(total_written)
        })();

        if result.is_err() {
            let _ = fs::remove_file(&part_path);
        }
        result
    }
}

/// Constant-memory streaming AES-256-GCM decryptor + GHASH authenticator for 96-bit (12-byte) nonces.
struct StreamingGcm96 {
    ctr: Ctr128BE<Aes256>,
    ghash: GHash,
    tag_mask: [u8; 16],
    total_bytes: u64,
    pending: [u8; 16],
    pending_len: usize,
}

impl StreamingGcm96 {
    fn new(key: &[u8; 32], iv: &[u8; 12]) -> Self {
        let aes_cipher = Aes256::new(key.into());

        // H = AES_ECB(K, 0^128)
        let mut h_block = aes::Block::default();
        aes_cipher.encrypt_block(&mut h_block);
        let ghash = GHash::new(&h_block);

        // J0 = IV || 0x00000001
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(iv);
        j0[15] = 1;

        // tag_mask = AES_ECB(K, J0)
        let mut mask_block = aes::Block::from(j0);
        aes_cipher.encrypt_block(&mut mask_block);
        let mut tag_mask = [0u8; 16];
        tag_mask.copy_from_slice(&mask_block);

        // Counter starts at J0 + 1 = IV || 0x00000002
        let mut initial_ctr = j0;
        initial_ctr[15] = 2;
        let ctr = Ctr128BE::<Aes256>::new(key.into(), (&initial_ctr).into());

        Self {
            ctr,
            ghash,
            tag_mask,
            total_bytes: 0,
            pending: [0u8; 16],
            pending_len: 0,
        }
    }

    fn decrypt_chunk(&mut self, ciphertext: &[u8], out_plain: &mut Vec<u8>) {
        self.total_bytes = self.total_bytes.wrapping_add(ciphertext.len() as u64);
        self.absorb_ciphertext(ciphertext);

        let start = out_plain.len();
        out_plain.extend_from_slice(ciphertext);
        self.ctr.apply_keystream(&mut out_plain[start..]);
    }

    fn absorb_ciphertext(&mut self, mut data: &[u8]) {
        if self.pending_len > 0 {
            let needed = 16 - self.pending_len;
            if data.len() < needed {
                self.pending[self.pending_len..self.pending_len + data.len()]
                    .copy_from_slice(data);
                self.pending_len += data.len();
                return;
            }
            self.pending[self.pending_len..16].copy_from_slice(&data[..needed]);
            let block = ghash::Block::from(self.pending);
            self.ghash.update(std::slice::from_ref(&block));
            self.pending_len = 0;
            data = &data[needed..];
        }

        let full_blocks_len = (data.len() / 16) * 16;
        for chunk16 in data[..full_blocks_len].chunks_exact(16) {
            let block = ghash::Block::clone_from_slice(chunk16);
            self.ghash.update(std::slice::from_ref(&block));
        }

        let rem = &data[full_blocks_len..];
        if !rem.is_empty() {
            self.pending[..rem.len()].copy_from_slice(rem);
            self.pending_len = rem.len();
        }
    }

    fn verify(mut self, expected_tag: &[u8; 16]) -> bool {
        if self.pending_len > 0 {
            self.ghash.update_padded(&self.pending[..self.pending_len]);
            self.pending_len = 0;
        }

        // Length block: 64-bit AAD bit-length (0) || 64-bit ciphertext bit-length (BE).
        let mut len_block = ghash::Block::default();
        let cipher_bits = self.total_bytes.wrapping_mul(8);
        len_block[8..16].copy_from_slice(&cipher_bits.to_be_bytes());
        self.ghash.update(std::slice::from_ref(&len_block));

        let digest = self.ghash.finalize();
        let mut computed_tag = [0u8; 16];
        for i in 0..16 {
            computed_tag[i] = digest[i] ^ self.tag_mask[i];
        }

        bool::from(computed_tag.ct_eq(expected_tag))
    }
}

fn decrypt_gcm_non96_oneshot(
    key: &[u8; 32],
    iv: &[u8],
    tag: &[u8; 16],
    ciphertext: &[u8],
) -> std::result::Result<Vec<u8>, DecryptError> {
    if ciphertext.len() > GCM_ONE_SHOT_CAP {
        return Err(DecryptError::PayloadTooLarge);
    }

    if iv.len() == 16 {
        let mut buf = ciphertext.to_vec();
        let aead = AesGcm::<Aes256, U16>::new(key.into());
        aead.decrypt_in_place_detached(iv.into(), b"", &mut buf, tag.into())
            .map_err(|_| DecryptError::TagMismatch)?;
        return Ok(buf);
    }

    // General NIST SP 800-38D §7.2 non-96-bit IV path (`iv.len() >= 12`).
    let aes_cipher = Aes256::new(key.into());
    let mut h_block = aes::Block::default();
    aes_cipher.encrypt_block(&mut h_block);
    let base_ghash = GHash::new(&h_block);

    let mut j0_hash = base_ghash.clone();
    j0_hash.update_padded(iv);
    let mut iv_len_block = ghash::Block::default();
    iv_len_block[8..16].copy_from_slice(&((iv.len() as u64) * 8).to_be_bytes());
    j0_hash.update(std::slice::from_ref(&iv_len_block));
    let j0 = j0_hash.finalize();

    let mut ctr = Ctr32BE::inner_iv_init(&aes_cipher, &j0);
    let mut tag_mask = aes::Block::default();
    ctr.write_keystream_block(&mut tag_mask);

    let mut tag_hash = base_ghash;
    tag_hash.update_padded(ciphertext);
    let mut c_len_block = ghash::Block::default();
    c_len_block[8..16].copy_from_slice(&((ciphertext.len() as u64) * 8).to_be_bytes());
    tag_hash.update(std::slice::from_ref(&c_len_block));
    let digest = tag_hash.finalize();

    let mut computed_tag = [0u8; 16];
    for i in 0..16 {
        computed_tag[i] = digest[i] ^ tag_mask[i];
    }
    if !bool::from(computed_tag.ct_eq(tag)) {
        return Err(DecryptError::TagMismatch);
    }

    let mut plain = ciphertext.to_vec();
    ctr.apply_keystream_partial(plain.as_mut_slice().into());
    Ok(plain)
}

fn parse_crypto_bytes(value: Option<&Value>) -> Option<Vec<u8>> {
    let val = value?;
    match val {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Some(Vec::new());
            }
            STANDARD
                .decode(trimmed)
                .or_else(|_| STANDARD_NO_PAD.decode(trimmed))
                .ok()
        }
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                if let Some(u) = item.as_u64() {
                    if u > 255 {
                        return None;
                    }
                    out.push(u as u8);
                } else if let Some(i) = item.as_i64() {
                    if !(-128..=255).contains(&i) {
                        return None;
                    }
                    out.push(i as u8);
                } else {
                    return None;
                }
            }
            Some(out)
        }
        Value::Object(obj) => {
            if let Some(b64) = obj.get("__bytes").and_then(|v| v.as_str()) {
                return parse_crypto_bytes(Some(&Value::String(b64.to_owned())));
            }
            if obj.get("type").and_then(|v| v.as_str()) == Some("Buffer") {
                if let Some(data) = obj.get("data") {
                    return parse_crypto_bytes(Some(data));
                }
            }
            None
        }
        _ => None,
    }
}

#[derive(Clone, Default)]
struct CancelToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancelToken {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// Outcome of a single `new-download-file` transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub save_path: Option<String>,
    pub failure: Option<DownloadFailure>,
    pub is_cached: bool,
}

type PathAuthorizer = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// Main-process download manager handling `new-download-channel` (`new-download-file` & `abort`).
#[derive(Clone)]
pub struct DownloadManager {
    loader: MediaLoader,
    temp_dir: PathBuf,
    allowed_save_roots: Option<Vec<PathBuf>>,
    save_authorizer: Option<PathAuthorizer>,
    read_authorizer: Option<PathAuthorizer>,
    active_tokens: Arc<DashMap<String, HashMap<Uuid, CancelToken>>>,
    not_found: Arc<DashMap<String, Instant>>,
}

impl std::fmt::Debug for DownloadManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadManager")
            .field("temp_dir", &self.temp_dir)
            .field("active_keys", &self.active_tokens.len())
            .field("not_found_count", &self.not_found.len())
            .finish()
    }
}

impl DownloadManager {
    /// Creates a `DownloadManager` backed by `loader` and optional `allowed_save_roots`.
    pub fn new(
        loader: MediaLoader,
        temp_dir: impl Into<PathBuf>,
        allowed_save_roots: Option<Vec<PathBuf>>,
    ) -> Self {
        let temp_dir = temp_dir.into();
        let _ = fs::create_dir_all(&temp_dir);
        Self {
            loader,
            temp_dir,
            allowed_save_roots,
            save_authorizer: None,
            read_authorizer: None,
            active_tokens: Arc::new(DashMap::new()),
            not_found: Arc::new(DashMap::new()),
        }
    }

    /// Attaches a dynamic save-destination authorizer (e.g. checking `ZaloPlatform::is_writable`
    /// so user-chosen `showSaveDialog` destinations outside static roots are honored).
    pub fn with_save_authorizer<F>(mut self, authorizer: F) -> Self
    where
        F: Fn(&Path) -> bool + Send + Sync + 'static,
    {
        self.save_authorizer = Some(Arc::new(authorizer));
        self
    }

    /// Attaches a dynamic local-source read authorizer for `options.srcPath` (`*Copy` transfers).
    pub fn with_read_authorizer<F>(mut self, authorizer: F) -> Self
    where
        F: Fn(&Path) -> bool + Send + Sync + 'static,
    {
        self.read_authorizer = Some(Arc::new(authorizer));
        self
    }

    /// Alias for [`Self::with_save_authorizer`] validating runtime portal-approved save targets.
    pub fn with_writable_validator<F>(self, validator: F) -> Self
    where
        F: Fn(&Path) -> bool + Send + Sync + 'static,
    {
        self.with_save_authorizer(validator)
    }

    /// Cancels all in-flight downloads and clears the 404 negative URL cache (e.g. on logout).
    pub fn reset(&self) {
        for entry in self.active_tokens.iter() {
            for token in entry.value().values() {
                token.cancel();
            }
        }
        self.active_tokens.clear();
        self.not_found.clear();
    }

    /// Returns the number of cached 404 URLs.
    pub fn not_found_count(&self) -> usize {
        self.not_found.len()
    }

    /// Aborts in-flight download(s) matching `(download_from, key)`.
    pub fn abort(&self, download_from: Option<&str>, key: &str) {
        if let Some(from) = download_from {
            let composite = format!("{from}|{key}");
            if let Some(entry) = self.active_tokens.get(&composite) {
                for token in entry.value().values() {
                    token.cancel();
                }
            }
        } else {
            let suffix = format!("|{key}");
            for entry in self.active_tokens.iter() {
                if entry.key().ends_with(&suffix) {
                    for token in entry.value().values() {
                        token.cancel();
                    }
                }
            }
        }
    }

    /// Processes a `{ channel, data }` message on `new-download-channel`.
    ///
    /// - `"new-download-file"`: executes the transfer (cancellable via `"new-download-file-abort"` / `"abort"`)
    ///   and returns `Some({ channel: "new-download-file-resp", data: { downloadError, savePath, downloadFrom, key, type, isCached } })`.
    /// - `"new-download-file-abort"` / `"abort"`: cancels matching transfers and returns `None`.
    pub async fn handle_message(&self, request: Value) -> Option<Value> {
        let obj = request.as_object()?;
        let channel = obj.get("channel").and_then(|v| v.as_str())?;
        let data = obj.get("data").cloned().unwrap_or_else(|| json!({}));

        match channel {
            DOWNLOAD_REQ_START => {
                let data_obj = data.as_object()?;
                let key_val = data_obj.get("key")?.clone();
                let key_str = value_to_key_string(&key_val)?;
                let download_from = data_obj
                    .get("downloadFrom")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let dl_type = data_obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();

                let composite_id = format!("{download_from}|{key_str}");
                let token_id = Uuid::new_v4();
                let cancel_token = CancelToken::default();
                self.active_tokens
                    .entry(composite_id.clone())
                    .or_default()
                    .insert(token_id, cancel_token.clone());

                let outcome = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => DownloadOutcome {
                        save_path: None,
                        failure: Some(DownloadFailure::aborted()),
                        is_cached: false,
                    },
                    res = self.perform(data_obj, &cancel_token) => res,
                };

                if let Some(mut map_entry) = self.active_tokens.get_mut(&composite_id) {
                    map_entry.remove(&token_id);
                    let empty = map_entry.is_empty();
                    drop(map_entry);
                    if empty {
                        self.active_tokens.remove(&composite_id);
                    }
                }

                let err_json = outcome
                    .failure
                    .as_ref()
                    .map(DownloadFailure::to_json)
                    .unwrap_or(Value::Null);
                let total_size = outcome
                    .save_path
                    .as_deref()
                    .and_then(|p| fs::metadata(p).ok().map(|m| m.len()))
                    .unwrap_or(0);
                let save_path_json = outcome
                    .save_path
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                let p_key = data_obj.get("pKey").cloned();

                let mut resp_data = json!({
                    "downloadError": err_json,
                    "savePath": save_path_json,
                    "downloadFrom": download_from,
                    "key": key_val,
                    "type": dl_type,
                    "isCached": outcome.is_cached,
                    "totalSize": total_size,
                });
                if let (Some(pk), Some(map)) = (p_key, resp_data.as_object_mut()) {
                    map.insert("pKey".to_owned(), pk);
                }

                Some(json!({
                    "channel": DOWNLOAD_RESP_CHANNEL,
                    "data": resp_data,
                }))
            }
            DOWNLOAD_REQ_ABORT | "abort" => {
                if let Some(data_obj) = data.as_object() {
                    if let Some(key_str) = data_obj.get("key").and_then(value_to_key_string) {
                        let from = data_obj.get("downloadFrom").and_then(|v| v.as_str());
                        self.abort(from, &key_str);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Spawns a fire-and-forget `new-download-channel` message and invokes `responder` when a
    /// `new-download-file-resp` payload is ready.
    pub fn spawn_request<F>(&self, request: Value, responder: F)
    where
        F: FnOnce(Value) + Send + 'static,
    {
        let this = self.clone();
        tokio::spawn(async move {
            if let Some(resp) = this.handle_message(request).await {
                responder(resp);
            }
        });
    }

    async fn perform(
        &self,
        request: &serde_json::Map<String, Value>,
        cancel_token: &CancelToken,
    ) -> DownloadOutcome {
        let data = request.get("data").and_then(|v| v.as_object());
        let options = request.get("options").and_then(|v| v.as_object());
        let dl_type = request
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_photo = dl_type.starts_with("Photo");
        let is_copy_type = dl_type.contains("Copy")
            || request
                .get("entryFlow")
                .and_then(|v| v.as_str())
                .is_some_and(|f| f.eq_ignore_ascii_case("copyResource"));
        let duplicate = options
            .and_then(|o| o.get("duplicate"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let want_noise = options
            .and_then(|o| o.get("noiseDownload"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || dl_type.ends_with("Noise");
        let local_src_path = options
            .and_then(|o| o.get("srcPath"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                options
                    .and_then(|o| o.get("manualResourcePath"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| data.and_then(|d| d.get("localPath")).and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|s| s.starts_with('/') && !s.contains('\0'))
            .map(PathBuf::from);

        let save_dir = options
            .and_then(|o| o.get("saveDir"))
            .and_then(|v| v.as_str());
        let file_name = data
            .and_then(|d| d.get("fileName"))
            .and_then(|v| v.as_str());
        let Some(initial_destination) = resolve_destination(save_dir, file_name) else {
            return DownloadOutcome {
                save_path: None,
                failure: Some(DownloadFailure::save_dir_invalid()),
                is_cached: false,
            };
        };

        if !self.is_save_target_allowed(&initial_destination) {
            return DownloadOutcome {
                save_path: None,
                failure: Some(DownloadFailure::save_dir_invalid()),
                is_cached: false,
            };
        }

        let destination = if duplicate {
            deduplicate_destination(&initial_destination)
        } else {
            initial_destination
        };

        let secure = SecureSpec::parse(request.get("secure").unwrap_or(&Value::Null));
        // Fail closed BEFORE touching local disk or network: malformed e2ee/pcloud specs must NEVER save ciphertext!
        if secure.is_invalid() {
            return DownloadOutcome {
                save_path: None,
                failure: Some(DownloadFailure::decrypt_failed()),
                is_cached: false,
            };
        }

        // Local `*Copy*` / `srcPath` fast-path (e.g. `FileCopy`, `VideoCopy`, `PhotoCopy`,
        // `FileCopyOneOneNoise`, `FileCopyGroupNoise`, or `copyResourceToZPC`), where Zalo
        // copies an already-cached local file (denoise when saving to user dir, or apply
        // `fileNoise` when caching into `ZaloDownloads/.../fileNoise/`) even when `data.url`
        // is empty or expired.
        if let Some(ref src) = local_src_path {
            let resolved_src = if src.is_dir() {
                options
                    .and_then(|o| o.get("manualFileName"))
                    .and_then(|v| v.as_str())
                    .or(file_name)
                    .map(|name| src.join(Path::new(name).file_name().unwrap_or_default()))
                    .unwrap_or_else(|| src.clone())
            } else {
                src.clone()
            };
            if is_usable_file(&resolved_src) && self.is_read_path_allowed(&resolved_src) {
                if cancel_token.is_cancelled() {
                    return DownloadOutcome {
                        save_path: None,
                        failure: Some(DownloadFailure::aborted()),
                        is_cached: false,
                    };
                }
                if !duplicate && is_usable_file(&destination) {
                    return DownloadOutcome {
                        save_path: Some(destination.to_string_lossy().into_owned()),
                        failure: None,
                        is_cached: true,
                    };
                }
                if matches!(secure, SecureSpec::Stream(_)) {
                    return self
                        .finalize_source_async(
                            &destination,
                            FetchedSource::File {
                                path: resolved_src,
                                owned: false,
                            },
                            secure,
                            want_noise,
                        )
                        .await;
                }
                let dest = destination.clone();
                let copy_res = tokio::task::spawn_blocking(move || {
                    copy_local_with_noise_transform(&resolved_src, &dest, want_noise)
                })
                .await;
                return match copy_res {
                    Ok(Ok(())) if is_usable_file(&destination) => DownloadOutcome {
                        save_path: Some(destination.to_string_lossy().into_owned()),
                        failure: None,
                        is_cached: false,
                    },
                    Ok(Err(failure)) => DownloadOutcome {
                        save_path: None,
                        failure: Some(failure),
                        is_cached: false,
                    },
                    _ => DownloadOutcome {
                        save_path: None,
                        failure: Some(DownloadFailure::unknown()),
                        is_cached: false,
                    },
                };
            }
        }

        let raw_url = data.and_then(|d| d.get("url")).and_then(|v| v.as_str());
        let valid_http_url = raw_url.and_then(|u| {
            let parsed = Url::parse(u).ok()?;
            (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some())
                .then_some(u)
        });
        let Some(url_str) = valid_http_url else {
            let failure = if is_copy_type && local_src_path.is_some() {
                DownloadFailure::copy_src_not_found()
            } else {
                DownloadFailure::invalid_url()
            };
            return DownloadOutcome {
                save_path: None,
                failure: Some(failure),
                is_cached: false,
            };
        };

        let manual = options
            .and_then(|o| o.get("srcAction"))
            .and_then(|v| v.as_str())
            == Some("Manual");
        let headers = extract_request_headers(request);

        if !duplicate && is_usable_file(&destination) {
            return DownloadOutcome {
                save_path: Some(destination.to_string_lossy().into_owned()),
                failure: None,
                is_cached: true,
            };
        }

        if !manual {
            if let Some(until) = self.not_found.get(url_str) {
                if *until > Instant::now() {
                    return DownloadOutcome {
                        save_path: None,
                        failure: Some(DownloadFailure::http(404)),
                        is_cached: false,
                    };
                }
                drop(until);
                self.not_found.remove(url_str);
            }
        }

        let ttfb = options
            .and_then(|o| o.get("maxTTFB"))
            .and_then(|v| v.as_f64())
            .filter(|&ms| ms > 0.0)
            .map(|ms| Duration::from_secs_f64(ms / 1000.0));

        let outcome = self
            .execute_transfer(
                url_str,
                &destination,
                is_photo,
                &headers,
                secure,
                want_noise,
                ttfb,
                cancel_token,
            )
            .await;

        if outcome
            .failure
            .as_ref()
            .is_some_and(|f| f.code == 75404)
        {
            self.remember_not_found(url_str);
        }

        outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_transfer(
        &self,
        url_str: &str,
        destination: &Path,
        is_photo: bool,
        headers: &HashMap<String, String>,
        secure: SecureSpec,
        want_noise: bool,
        ttfb: Option<Duration>,
        cancel_token: &CancelToken,
    ) -> DownloadOutcome {
        let fetched = if headers.is_empty() && is_photo {
            let deadline = ttfb.map_or(DEFAULT_IMAGE_DEADLINE, |t| t.max(DEFAULT_IMAGE_DEADLINE));
            match self.loader.fetch_bytes(url_str, deadline).await {
                Ok(b) => FetchedSource::Bytes(b),
                Err(err) => {
                    return DownloadOutcome {
                        save_path: None,
                        failure: Some(DownloadFailure::from_media_error(&err)),
                        is_cached: false,
                    };
                }
            }
        } else if headers.is_empty() {
            match self.loader.local_file(url_str).await {
                Ok(path) => FetchedSource::File { path, owned: false },
                Err(err) => {
                    return DownloadOutcome {
                        save_path: None,
                        failure: Some(DownloadFailure::from_media_error(&err)),
                        is_cached: false,
                    };
                }
            }
        } else {
            let max_bytes = if is_photo {
                MAX_PHOTO_BYTES
            } else {
                MAX_MEDIA_FILE_BYTES
            };
            match self
                .loader
                .download_with_headers(url_str, headers, max_bytes, ttfb)
                .await
            {
                Ok(path) => FetchedSource::File { path, owned: true },
                Err(err) => {
                    return DownloadOutcome {
                        save_path: None,
                        failure: Some(DownloadFailure::from_media_error(&err)),
                        is_cached: false,
                    };
                }
            }
        };

        if cancel_token.is_cancelled() {
            if let FetchedSource::File { path, owned: true } = fetched {
                let _ = fs::remove_file(path);
            }
            return DownloadOutcome {
                save_path: None,
                failure: Some(DownloadFailure::aborted()),
                is_cached: false,
            };
        }

        self.finalize_source_async(destination, fetched, secure, want_noise)
            .await
    }

    async fn finalize_source_async(
        &self,
        destination: &Path,
        fetched: FetchedSource,
        secure: SecureSpec,
        want_noise: bool,
    ) -> DownloadOutcome {
        let dest = destination.to_path_buf();
        let temp_dir = self.temp_dir.clone();
        let finalize_res = tokio::task::spawn_blocking(move || -> DownloadOutcome {
            let result = finalize_fetched_to_dest(&dest, &temp_dir, fetched, secure, want_noise);
            match result {
                Ok(()) => {
                    if !is_usable_file(&dest) {
                        let _ = fs::remove_file(&dest);
                        DownloadOutcome {
                            save_path: None,
                            failure: Some(DownloadFailure::empty_body()),
                            is_cached: false,
                        }
                    } else {
                        DownloadOutcome {
                            save_path: Some(dest.to_string_lossy().into_owned()),
                            failure: None,
                            is_cached: false,
                        }
                    }
                }
                Err(failure) => DownloadOutcome {
                    save_path: None,
                    failure: Some(failure),
                    is_cached: false,
                },
            }
        })
        .await;

        match finalize_res {
            Ok(outcome) => outcome,
            Err(_) => DownloadOutcome {
                save_path: None,
                failure: Some(DownloadFailure::unknown()),
                is_cached: false,
            },
        }
    }

    fn remember_not_found(&self, url: &str) {
        let now = Instant::now();
        if self.not_found.len() >= NOT_FOUND_CACHE_CAPACITY {
            self.not_found.retain(|_, exp| *exp > now);
            if self.not_found.len() >= NOT_FOUND_CACHE_CAPACITY {
                let mut oldest: Vec<(String, Instant)> = self
                    .not_found
                    .iter()
                    .map(|e| (e.key().clone(), *e.value()))
                    .collect();
                oldest.sort_by_key(|(_, exp)| *exp);
                for (k, _) in oldest.into_iter().take(NOT_FOUND_CACHE_CAPACITY / 4) {
                    self.not_found.remove(&k);
                }
            }
        }
        self.not_found
            .insert(url.to_owned(), now + NOT_FOUND_CACHE_TTL);
    }

    fn is_save_target_allowed(&self, destination: &Path) -> bool {
        if let Some(ref auth) = self.save_authorizer {
            if auth(destination) {
                return true;
            }
        }
        let Some(parent) = destination.parent() else {
            return false;
        };
        if let Some(ref auth) = self.save_authorizer {
            if auth(parent) {
                return true;
            }
        }
        if let Some(ref roots) = self.allowed_save_roots {
            let canon_parent = canonicalize_longest_ancestor(parent);
            return roots.iter().any(|root| {
                let canon_root = canonicalize_longest_ancestor(root);
                canon_parent == canon_root || canon_parent.starts_with(&canon_root)
            });
        }
        true
    }

    fn is_read_path_allowed(&self, src: &Path) -> bool {
        if let Some(ref auth) = self.read_authorizer {
            return auth(src);
        }
        if let Some(ref roots) = self.allowed_save_roots {
            let canon_src = canonicalize_longest_ancestor(src);
            return roots.iter().any(|root| {
                let canon_root = canonicalize_longest_ancestor(root);
                canon_src != canon_root && canon_src.starts_with(&canon_root)
            });
        }
        true
    }
}

const ZALO_NOISE_SIGN: [u8; 6] = [0x07, 0x7a, 0x41, 0x6c, 0x4f, 0x07];
const ZALO_NOISE_XOR_LEN: u64 = 100;
const ZALO_NOISE_XOR_KEY: u8 = 0x93;

fn has_zalo_noise_trailer(file: &mut File, raw_len: u64) -> bool {
    if raw_len <= ZALO_NOISE_SIGN.len() as u64 {
        return false;
    }
    let sig_off = raw_len - ZALO_NOISE_SIGN.len() as u64;
    let mut tail = [0u8; 6];
    file.seek(SeekFrom::Start(sig_off)).is_ok()
        && file.read_exact(&mut tail).is_ok()
        && tail == ZALO_NOISE_SIGN
}

fn xor_noise_window(offset: u64, buf: &mut [u8]) {
    if offset < ZALO_NOISE_XOR_LEN && !buf.is_empty() {
        let end = ((ZALO_NOISE_XOR_LEN - offset) as usize).min(buf.len());
        for b in &mut buf[..end] {
            *b ^= ZALO_NOISE_XOR_KEY;
        }
    }
}

fn copy_local_with_noise_transform(
    src: &Path,
    destination: &Path,
    want_noise: bool,
) -> std::result::Result<(), DownloadFailure> {
    if src == destination {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        if !parent.is_dir() {
            fs::create_dir_all(parent).map_err(|_| DownloadFailure::save_dir_invalid())?;
        }
    }

    let mut fin = File::open(src).map_err(|_| DownloadFailure::copy_src_not_found())?;
    let raw_len = fin
        .metadata()
        .map(|m| m.len())
        .map_err(|_| DownloadFailure::copy_src_not_found())?;
    if raw_len == 0 {
        return Err(DownloadFailure::empty_body());
    }
    let src_has_noise = has_zalo_noise_trailer(&mut fin, raw_len);
    fin.seek(SeekFrom::Start(0))
        .map_err(|_| DownloadFailure::unknown())?;

    let logical_len = if src_has_noise {
        raw_len - ZALO_NOISE_SIGN.len() as u64
    } else {
        raw_len
    };
    if logical_len == 0 {
        return Err(DownloadFailure::empty_body());
    }

    let write_to = |fout: &mut File, fin: &mut File| -> std::io::Result<()> {
        let read_limit = if src_has_noise && !want_noise {
            logical_len
        } else {
            raw_len
        };
        let need_xor = src_has_noise != want_noise;
        let mut remaining = read_limit;
        let mut offset = 0u64;
        let mut chunk = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let to_read = (remaining as usize).min(chunk.len());
            let n = fin.read(&mut chunk[..to_read])?;
            if n == 0 {
                break;
            }
            if need_xor {
                xor_noise_window(offset, &mut chunk[..n]);
            }
            fout.write_all(&chunk[..n])?;
            offset += n as u64;
            remaining -= n as u64;
        }
        if !src_has_noise && want_noise {
            fout.write_all(&ZALO_NOISE_SIGN)?;
        }
        fout.flush()
    };

    let part_path = SecureStream::part_path_for(destination);
    let _ = fs::remove_file(&part_path);
    match File::create(&part_path) {
        Ok(mut fout) => {
            if write_to(&mut fout, &mut fin)
                .and_then(|_| fout.sync_all())
                .and_then(|_| {
                    drop(fout);
                    fs::rename(&part_path, destination)
                })
                .is_err()
            {
                let _ = fs::remove_file(&part_path);
                return Err(DownloadFailure::unknown());
            }
            Ok(())
        }
        Err(_) => {
            // Direct-open fallback for XDG document-portal FUSE mounts where sibling `.part` files are denied.
            fin.seek(SeekFrom::Start(0))
                .map_err(|_| DownloadFailure::unknown())?;
            let mut fout = File::create(destination).map_err(|_| DownloadFailure::unknown())?;
            write_to(&mut fout, &mut fin).map_err(|_| DownloadFailure::unknown())
        }
    }
}

fn apply_noise_in_place(path: &Path) -> std::io::Result<()> {
    let mut f = fs::OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    if has_zalo_noise_trailer(&mut f, len) {
        return Ok(());
    }
    let head_len = (len as usize).min(ZALO_NOISE_XOR_LEN as usize);
    if head_len > 0 {
        f.seek(SeekFrom::Start(0))?;
        let mut head = vec![0u8; head_len];
        f.read_exact(&mut head)?;
        xor_noise_window(0, &mut head);
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&head)?;
    }
    f.seek(SeekFrom::End(0))?;
    f.write_all(&ZALO_NOISE_SIGN)?;
    f.sync_all()
}

fn deduplicate_destination(initial: &Path) -> PathBuf {
    if !initial.exists() {
        return initial.to_path_buf();
    }
    let Some(parent) = initial.parent() else {
        return initial.to_path_buf();
    };
    let stem = initial
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("download");
    let ext = initial
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty());
    for i in 1..10_000 {
        let candidate_name = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = parent.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    initial.to_path_buf()
}

enum FetchedSource {
    Bytes(bytes::Bytes),
    File { path: PathBuf, owned: bool },
}

fn finalize_fetched_to_dest(
    destination: &Path,
    temp_dir: &Path,
    source: FetchedSource,
    secure: SecureSpec,
    want_noise: bool,
) -> std::result::Result<(), DownloadFailure> {
    let cleanup_owned = |src: &FetchedSource| {
        if let FetchedSource::File { path, owned: true } = src {
            let _ = fs::remove_file(path);
        }
    };

    let stream_opt = match secure {
        SecureSpec::None => None,
        SecureSpec::Stream(s) => Some(s),
        SecureSpec::Invalid => {
            cleanup_owned(&source);
            return Err(DownloadFailure::decrypt_failed());
        }
    };

    if let Some(parent) = destination.parent() {
        if !parent.is_dir() {
            fs::create_dir_all(parent).map_err(|_| {
                cleanup_owned(&source);
                DownloadFailure::save_dir_invalid()
            })?;
        }
    }
    let _ = fs::create_dir_all(temp_dir);

    let res: std::result::Result<(), DownloadFailure> = match (source, stream_opt) {
        (FetchedSource::Bytes(bytes), Some(stream)) => {
            if bytes.is_empty() {
                return Err(DownloadFailure::empty_body());
            }
            if bytes.len() as u64 > MAX_PHOTO_BYTES {
                return Err(DownloadFailure::too_large());
            }
            let written = stream
                .decrypt_reader_to_path(Cursor::new(&bytes[..]), destination)
                .map_err(|_| DownloadFailure::decrypt_failed())?;
            if written == 0 {
                let _ = fs::remove_file(destination);
                return Err(DownloadFailure::empty_body());
            }
            Ok(())
        }
        (FetchedSource::Bytes(bytes), None) => {
            if bytes.is_empty() {
                return Err(DownloadFailure::empty_body());
            }
            if bytes.len() as u64 > MAX_PHOTO_BYTES {
                return Err(DownloadFailure::too_large());
            }
            write_atomic_part(destination, &bytes).map_err(|_| DownloadFailure::unknown())?;
            Ok(())
        }
        (FetchedSource::File { path, owned }, Some(stream)) => {
            let dec_res = stream.decrypt_file(&path, destination);
            if owned {
                let _ = fs::remove_file(&path);
            }
            let written = dec_res.map_err(|_| DownloadFailure::decrypt_failed())?;
            if written == 0 {
                let _ = fs::remove_file(destination);
                return Err(DownloadFailure::empty_body());
            }
            Ok(())
        }
        (FetchedSource::File { path, owned }, None) => {
            let copy_res = copy_local_with_noise_transform(&path, destination, want_noise);
            if owned {
                let _ = fs::remove_file(&path);
            }
            copy_res
        }
    };

    res?;
    if want_noise {
        apply_noise_in_place(destination).map_err(|_| {
            let _ = fs::remove_file(destination);
            DownloadFailure::unknown()
        })?;
    }
    Ok(())
}

fn write_atomic_part(destination: &Path, data: &[u8]) -> std::io::Result<()> {
    let part_path = SecureStream::part_path_for(destination);
    let _ = fs::remove_file(&part_path);
    match File::create(&part_path) {
        Ok(mut f) => {
            let res = (|| -> std::io::Result<()> {
                f.write_all(data)?;
                f.sync_all()?;
                drop(f);
                fs::rename(&part_path, destination)?;
                Ok(())
            })();
            if res.is_err() {
                let _ = fs::remove_file(&part_path);
            }
            res
        }
        Err(_) => {
            let mut f = File::create(destination)?;
            f.write_all(data)?;
            f.flush()
        }
    }
}

fn resolve_destination(save_dir: Option<&str>, file_name: Option<&str>) -> Option<PathBuf> {
    let save_dir = save_dir?;
    if !save_dir.starts_with('/') || save_dir.contains('\0') {
        return None;
    }
    let cleaned = file_name.unwrap_or("").replace('\0', "");
    let base = Path::new(&cleaned)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let final_name = if base.is_empty() || base == "." || base == ".." {
        "no_name"
    } else {
        base
    };
    Some(PathBuf::from(save_dir).join(final_name))
}

fn extract_request_headers(request: &serde_json::Map<String, Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(arr) = request
        .get("options")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("headers"))
        .and_then(|v| v.as_array())
    {
        for entry in arr {
            if let Some(obj) = entry.as_object() {
                if let (Some(name), Some(val)) = (
                    obj.get("name").and_then(|v| v.as_str()),
                    obj.get("value").and_then(|v| v.as_str()),
                ) {
                    if !name.is_empty() {
                        out.insert(name.to_owned(), val.to_owned());
                    }
                }
            }
        }
    }
    if let Some(extra) = request.get("extraHeader").and_then(|v| v.as_object()) {
        for (name, val) in extra {
            if let Some(val_str) = val.as_str() {
                if !name.is_empty() {
                    out.insert(name.clone(), val_str.to_owned());
                }
            }
        }
    }
    out
}

fn is_usable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn value_to_key_string(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn canonicalize_longest_ancestor(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    let mut tail = Vec::new();
    while !cur.exists() {
        let Some(name) = cur.file_name().map(|n| n.to_os_string()) else {
            break;
        };
        tail.push(name);
        if !cur.pop() {
            break;
        }
    }
    let mut resolved = fs::canonicalize(&cur).unwrap_or(cur);
    for comp in tail.into_iter().rev() {
        resolved.push(comp);
    }
    resolved
}
