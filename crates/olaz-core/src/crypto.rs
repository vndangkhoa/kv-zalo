//! Exact `$znode.crypto` Node.js parity for Olaz.
//!
//! Port of `Sources/Olaz/ZaloRuntime/CryptoBridge.swift` and the stateful handle
//! protocol in `Sources/Olaz/HostScripts/local-modules.js`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use aes::cipher::consts::{U12, U16};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::{Aes128, Aes192, Aes256};
use aes_gcm::{AeadInPlace, AesGcm, Tag};
use base64::engine::general_purpose::{STANDARD as B64_STANDARD, URL_SAFE_NO_PAD as B64_URL_NO_PAD};
use base64::Engine as _;
use ctr::Ctr128BE;
use digest::Digest;
use md5::Md5;
use parking_lot::Mutex;
use rand::RngCore;
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};

use crate::wire;

const AES_BLOCK_LEN: usize = 16;

#[derive(Clone)]
enum AesBlockCipher {
    Aes128(Aes128),
    Aes192(Aes192),
    Aes256(Aes256),
}

impl AesBlockCipher {
    fn new(bits: usize, key: &[u8]) -> Result<Self, String> {
        let expected = bits / 8;
        if key.len() != expected {
            return Err("Invalid key length".to_string());
        }
        match bits {
            128 => Aes128::new_from_slice(key)
                .map(Self::Aes128)
                .map_err(|_| "Invalid key length".to_string()),
            192 => Aes192::new_from_slice(key)
                .map(Self::Aes192)
                .map_err(|_| "Invalid key length".to_string()),
            256 => Aes256::new_from_slice(key)
                .map(Self::Aes256)
                .map_err(|_| "Invalid key length".to_string()),
            _ => Err(format!("Unsupported AES key size: {bits}")),
        }
    }

    fn encrypt_block(&self, block: &mut [u8; AES_BLOCK_LEN]) {
        let ga = GenericArray::from_mut_slice(block);
        match self {
            Self::Aes128(c) => c.encrypt_block(ga),
            Self::Aes192(c) => c.encrypt_block(ga),
            Self::Aes256(c) => c.encrypt_block(ga),
        }
    }

    fn decrypt_block(&self, block: &mut [u8; AES_BLOCK_LEN]) {
        let ga = GenericArray::from_mut_slice(block);
        match self {
            Self::Aes128(c) => c.decrypt_block(ga),
            Self::Aes192(c) => c.decrypt_block(ga),
            Self::Aes256(c) => c.decrypt_block(ga),
        }
    }
}

enum CtrState {
    Aes128(Ctr128BE<Aes128>),
    Aes192(Ctr128BE<Aes192>),
    Aes256(Ctr128BE<Aes256>),
}

impl CtrState {
    fn new(bits: usize, key: &[u8], iv: &[u8]) -> Result<Self, String> {
        if key.len() != bits / 8 {
            return Err("Invalid key length".to_string());
        }
        if iv.len() != AES_BLOCK_LEN {
            return Err("Invalid initialization vector".to_string());
        }
        match bits {
            128 => Ctr128BE::<Aes128>::new_from_slices(key, iv)
                .map(Self::Aes128)
                .map_err(|_| "Invalid key or IV".to_string()),
            192 => Ctr128BE::<Aes192>::new_from_slices(key, iv)
                .map(Self::Aes192)
                .map_err(|_| "Invalid key or IV".to_string()),
            256 => Ctr128BE::<Aes256>::new_from_slices(key, iv)
                .map(Self::Aes256)
                .map_err(|_| "Invalid key or IV".to_string()),
            _ => Err(format!("Unsupported AES key size: {bits}")),
        }
    }

    fn apply(&mut self, data: &mut [u8]) {
        match self {
            Self::Aes128(c) => c.apply_keystream(data),
            Self::Aes192(c) => c.apply_keystream(data),
            Self::Aes256(c) => c.apply_keystream(data),
        }
    }
}

enum CipherKind {
    Block {
        cipher: AesBlockCipher,
        cbc_iv: Option<[u8; AES_BLOCK_LEN]>,
        auto_padding: bool,
        pending: Vec<u8>,
    },
    Ctr(CtrState),
    Gcm {
        bits: usize,
        key: Vec<u8>,
        iv: Vec<u8>,
        aad: Vec<u8>,
        buffered: Vec<u8>,
        auth_tag: Option<[u8; 16]>,
    },
}

struct CipherSession {
    encrypting: bool,
    finalized: bool,
    kind: CipherKind,
}

impl CipherSession {
    fn new(encrypting: bool, algorithm: &str, key: &[u8], iv: &[u8]) -> Result<Self, String> {
        let alg = algorithm.trim().to_ascii_lowercase();
        let (bits, mode) = parse_aes_algorithm(&alg)?;

        let kind = match mode {
            "cbc" => {
                let cipher = AesBlockCipher::new(bits, key)?;
                if iv.len() != AES_BLOCK_LEN {
                    return Err("Invalid initialization vector".to_string());
                }
                let mut cbc_iv = [0u8; AES_BLOCK_LEN];
                cbc_iv.copy_from_slice(iv);
                CipherKind::Block {
                    cipher,
                    cbc_iv: Some(cbc_iv),
                    auto_padding: true,
                    pending: Vec::new(),
                }
            }
            "ecb" => {
                let cipher = AesBlockCipher::new(bits, key)?;
                CipherKind::Block {
                    cipher,
                    cbc_iv: None,
                    auto_padding: true,
                    pending: Vec::new(),
                }
            }
            "ctr" => CipherKind::Ctr(CtrState::new(bits, key, iv)?),
            "gcm" => {
                if key.len() != bits / 8 {
                    return Err("Invalid key length".to_string());
                }
                if iv.len() != 12 && iv.len() != 16 {
                    return Err("Invalid initialization vector".to_string());
                }
                CipherKind::Gcm {
                    bits,
                    key: key.to_vec(),
                    iv: iv.to_vec(),
                    aad: Vec::new(),
                    buffered: Vec::new(),
                    auth_tag: None,
                }
            }
            _ => return Err(format!("Unsupported cipher {algorithm}")),
        };

        Ok(Self {
            encrypting,
            finalized: false,
            kind,
        })
    }

    fn is_gcm(&self) -> bool {
        matches!(self.kind, CipherKind::Gcm { .. })
    }

    fn set_auto_padding(&mut self, auto_padding: bool) -> Result<(), String> {
        if self.finalized {
            return Err("cipher released".to_string());
        }
        if let CipherKind::Block {
            auto_padding: ap, ..
        } = &mut self.kind
        {
            *ap = auto_padding;
        }
        Ok(())
    }

    fn set_aad(&mut self, aad_bytes: &[u8]) -> Result<(), String> {
        if self.finalized {
            return Err("cipher released".to_string());
        }
        match &mut self.kind {
            CipherKind::Gcm { aad, .. } => {
                *aad = aad_bytes.to_vec();
                Ok(())
            }
            _ => Err("Invalid state: setAAD is only supported on AEAD ciphers".to_string()),
        }
    }

    fn set_auth_tag(&mut self, tag_bytes: &[u8]) -> Result<(), String> {
        if self.finalized {
            return Err("cipher released".to_string());
        }
        match &mut self.kind {
            CipherKind::Gcm { auth_tag, .. } if !self.encrypting => {
                let arr: [u8; 16] = tag_bytes
                    .try_into()
                    .map_err(|_| format!("Invalid authentication tag length: {}", tag_bytes.len()))?;
                *auth_tag = Some(arr);
                Ok(())
            }
            _ => Err("Invalid state: setAuthTag requires an active GCM decipher".to_string()),
        }
    }

    fn get_auth_tag(&self) -> Result<Option<Vec<u8>>, String> {
        match &self.kind {
            CipherKind::Gcm { auth_tag, .. } if self.encrypting => {
                Ok(auth_tag.map(|t| t.to_vec()))
            }
            _ => Ok(None),
        }
    }

    fn update(&mut self, input: &[u8]) -> Result<Vec<u8>, String> {
        if self.finalized {
            return Err("cipher released".to_string());
        }
        let encrypting = self.encrypting;
        match &mut self.kind {
            CipherKind::Ctr(ctr) => {
                let mut out = input.to_vec();
                ctr.apply(&mut out);
                Ok(out)
            }
            CipherKind::Gcm { buffered, .. } => {
                buffered.extend_from_slice(input);
                Ok(Vec::new())
            }
            CipherKind::Block {
                cipher,
                cbc_iv,
                auto_padding,
                pending,
            } => {
                pending.extend_from_slice(input);
                if encrypting {
                    let full_len = (pending.len() / AES_BLOCK_LEN) * AES_BLOCK_LEN;
                    let mut out = Vec::with_capacity(full_len);
                    for chunk in pending[..full_len].chunks_exact(AES_BLOCK_LEN) {
                        let blk = encrypt_single_block(cipher, cbc_iv.as_mut(), chunk);
                        out.extend_from_slice(&blk);
                    }
                    pending.drain(..full_len);
                    Ok(out)
                } else if *auto_padding {
                    // Hold back at least 1 byte (up to 1 full block) until `final()` so PKCS#7
                    // padding on the final block can be strictly validated.
                    if pending.len() <= AES_BLOCK_LEN {
                        return Ok(Vec::new());
                    }
                    let emit_blocks = (pending.len() - 1) / AES_BLOCK_LEN;
                    let emit_len = emit_blocks * AES_BLOCK_LEN;
                    let mut out = Vec::with_capacity(emit_len);
                    for chunk in pending[..emit_len].chunks_exact(AES_BLOCK_LEN) {
                        let blk = decrypt_single_block(cipher, cbc_iv.as_mut(), chunk);
                        out.extend_from_slice(&blk);
                    }
                    pending.drain(..emit_len);
                    Ok(out)
                } else {
                    let full_len = (pending.len() / AES_BLOCK_LEN) * AES_BLOCK_LEN;
                    let mut out = Vec::with_capacity(full_len);
                    for chunk in pending[..full_len].chunks_exact(AES_BLOCK_LEN) {
                        let blk = decrypt_single_block(cipher, cbc_iv.as_mut(), chunk);
                        out.extend_from_slice(&blk);
                    }
                    pending.drain(..full_len);
                    Ok(out)
                }
            }
        }
    }

    fn finish(&mut self) -> Result<Vec<u8>, String> {
        if self.finalized {
            return Err("cipher released".to_string());
        }
        self.finalized = true;
        let encrypting = self.encrypting;

        match &mut self.kind {
            CipherKind::Ctr(_) => Ok(Vec::new()),
            CipherKind::Gcm {
                bits,
                key,
                iv,
                aad,
                buffered,
                auth_tag,
            } => {
                let mut buf = std::mem::take(buffered);
                if encrypting {
                    let tag = gcm_encrypt(*bits, key, iv, aad, &mut buf)?;
                    *auth_tag = Some(tag);
                    Ok(buf)
                } else {
                    let tag = auth_tag.ok_or_else(|| {
                        "Unsupported state or unable to authenticate data".to_string()
                    })?;
                    gcm_decrypt(*bits, key, iv, aad, &mut buf, &tag)?;
                    Ok(buf)
                }
            }
            CipherKind::Block {
                cipher,
                cbc_iv,
                auto_padding,
                pending,
            } => {
                let rem = std::mem::take(pending);
                if encrypting {
                    if *auto_padding {
                        debug_assert!(rem.len() < AES_BLOCK_LEN);
                        let pad = AES_BLOCK_LEN - rem.len();
                        let mut final_blk = [pad as u8; AES_BLOCK_LEN];
                        final_blk[..rem.len()].copy_from_slice(&rem);
                        let enc = encrypt_single_block(cipher, cbc_iv.as_mut(), &final_blk);
                        Ok(enc.to_vec())
                    } else if rem.is_empty() {
                        Ok(Vec::new())
                    } else {
                        Err("wrong final block length".to_string())
                    }
                } else if *auto_padding {
                    if rem.len() != AES_BLOCK_LEN {
                        return Err("bad decrypt".to_string());
                    }
                    let blk = decrypt_single_block(cipher, cbc_iv.as_mut(), &rem);
                    let pad = blk[AES_BLOCK_LEN - 1] as usize;
                    if pad == 0
                        || pad > AES_BLOCK_LEN
                        || !blk[AES_BLOCK_LEN - pad..].iter().all(|&b| b as usize == pad)
                    {
                        return Err("bad decrypt".to_string());
                    }
                    Ok(blk[..AES_BLOCK_LEN - pad].to_vec())
                } else if rem.is_empty() {
                    Ok(Vec::new())
                } else if rem.len() == AES_BLOCK_LEN {
                    let blk = decrypt_single_block(cipher, cbc_iv.as_mut(), &rem);
                    Ok(blk.to_vec())
                } else {
                    Err("bad decrypt".to_string())
                }
            }
        }
    }
}

fn parse_aes_algorithm(alg: &str) -> Result<(usize, &str), String> {
    let parts: Vec<&str> = alg.split('-').collect();
    if parts.len() != 3 || parts[0] != "aes" {
        return Err(format!("Unsupported cipher {alg}"));
    }
    let bits: usize = match parts[1] {
        "128" => 128,
        "192" => 192,
        "256" => 256,
        _ => return Err(format!("Unsupported cipher {alg}")),
    };
    Ok((bits, parts[2]))
}

fn encrypt_single_block(
    cipher: &AesBlockCipher,
    cbc_iv: Option<&mut [u8; AES_BLOCK_LEN]>,
    plain: &[u8],
) -> [u8; AES_BLOCK_LEN] {
    let mut blk = [0u8; AES_BLOCK_LEN];
    blk.copy_from_slice(plain);
    if let Some(iv) = cbc_iv {
        for (b, &v) in blk.iter_mut().zip(iv.iter()) {
            *b ^= v;
        }
        cipher.encrypt_block(&mut blk);
        *iv = blk;
    } else {
        cipher.encrypt_block(&mut blk);
    }
    blk
}

fn decrypt_single_block(
    cipher: &AesBlockCipher,
    cbc_iv: Option<&mut [u8; AES_BLOCK_LEN]>,
    ct: &[u8],
) -> [u8; AES_BLOCK_LEN] {
    let mut blk = [0u8; AES_BLOCK_LEN];
    blk.copy_from_slice(ct);
    if let Some(iv) = cbc_iv {
        let mut next_iv = [0u8; AES_BLOCK_LEN];
        next_iv.copy_from_slice(ct);
        cipher.decrypt_block(&mut blk);
        for (b, &v) in blk.iter_mut().zip(iv.iter()) {
            *b ^= v;
        }
        *iv = next_iv;
    } else {
        cipher.decrypt_block(&mut blk);
    }
    blk
}

fn gcm_encrypt(
    bits: usize,
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    buf: &mut [u8],
) -> Result<[u8; 16], String> {
    fn seal<C, N>(key: &[u8], iv: &[u8], aad: &[u8], buf: &mut [u8]) -> Result<[u8; 16], String>
    where
        C: KeyInit + aes::cipher::BlockCipher<BlockSize = U16> + BlockEncrypt,
        N: aes::cipher::ArrayLength<u8>,
    {
        let aead = AesGcm::<C, N>::new_from_slice(key).map_err(|_| "Invalid key length".to_string())?;
        let nonce = GenericArray::<u8, N>::from_slice(iv);
        let tag = aead
            .encrypt_in_place_detached(nonce, aad, buf)
            .map_err(|_| "GCM encryption failed".to_string())?;
        let mut out = [0u8; 16];
        out.copy_from_slice(tag.as_slice());
        Ok(out)
    }

    match (bits, iv.len()) {
        (128, 12) => seal::<Aes128, U12>(key, iv, aad, buf),
        (128, 16) => seal::<Aes128, U16>(key, iv, aad, buf),
        (192, 12) => seal::<Aes192, U12>(key, iv, aad, buf),
        (192, 16) => seal::<Aes192, U16>(key, iv, aad, buf),
        (256, 12) => seal::<Aes256, U12>(key, iv, aad, buf),
        (256, 16) => seal::<Aes256, U16>(key, iv, aad, buf),
        _ => Err("Invalid initialization vector".to_string()),
    }
}

fn gcm_decrypt(
    bits: usize,
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    buf: &mut [u8],
    tag_bytes: &[u8; 16],
) -> Result<(), String> {
    fn open<C, N>(
        key: &[u8],
        iv: &[u8],
        aad: &[u8],
        buf: &mut [u8],
        tag_bytes: &[u8; 16],
    ) -> Result<(), String>
    where
        C: KeyInit + aes::cipher::BlockCipher<BlockSize = U16> + BlockEncrypt,
        N: aes::cipher::ArrayLength<u8>,
    {
        let aead = AesGcm::<C, N>::new_from_slice(key).map_err(|_| "Invalid key length".to_string())?;
        let nonce = GenericArray::<u8, N>::from_slice(iv);
        let tag = Tag::from_slice(tag_bytes);
        aead.decrypt_in_place_detached(nonce, aad, buf, tag)
            .map_err(|_| "Unsupported state or unable to authenticate data".to_string())
    }

    match (bits, iv.len()) {
        (128, 12) => open::<Aes128, U12>(key, iv, aad, buf, tag_bytes),
        (128, 16) => open::<Aes128, U16>(key, iv, aad, buf, tag_bytes),
        (192, 12) => open::<Aes192, U12>(key, iv, aad, buf, tag_bytes),
        (192, 16) => open::<Aes192, U16>(key, iv, aad, buf, tag_bytes),
        (256, 12) => open::<Aes256, U12>(key, iv, aad, buf, tag_bytes),
        (256, 16) => open::<Aes256, U16>(key, iv, aad, buf, tag_bytes),
        _ => Err("Invalid initialization vector".to_string()),
    }
}

enum HashState {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
}

impl HashState {
    fn new(algorithm: &str) -> Result<Self, String> {
        let norm = algorithm.trim().to_ascii_lowercase().replace('-', "");
        match norm.as_str() {
            "md5" => Ok(Self::Md5(Md5::new())),
            "sha1" => Ok(Self::Sha1(Sha1::new())),
            "sha256" => Ok(Self::Sha256(Sha256::new())),
            "sha384" => Ok(Self::Sha384(Sha384::new())),
            "sha512" => Ok(Self::Sha512(Sha512::new())),
            _ => Err(format!("Digest method not supported: {algorithm}")),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md5(h) => h.update(data),
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
            Self::Sha384(h) => h.update(data),
            Self::Sha512(h) => h.update(data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            Self::Md5(h) => h.finalize().to_vec(),
            Self::Sha1(h) => h.finalize().to_vec(),
            Self::Sha256(h) => h.finalize().to_vec(),
            Self::Sha384(h) => h.finalize().to_vec(),
            Self::Sha512(h) => h.finalize().to_vec(),
        }
    }
}

/// Shared AES-256-CBC PKCS#7 helper used by `SafeStorage` and `KeychainCipher`.
pub(crate) fn aes256_cbc_pkcs7(
    input: &[u8],
    key: &[u8; 32],
    iv: &[u8; 16],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let mut session = CipherSession::new(encrypt, "aes-256-cbc", key, iv)?;
    let mut out = session.update(input)?;
    out.extend_from_slice(&session.finish()?);
    Ok(out)
}

/// Thread-safe (`Send + Sync`) Node.js `$znode.crypto` bridge with stateful cipher/hash handles.
pub struct CryptoBridge {
    next_handle: AtomicU64,
    ciphers: Mutex<HashMap<u64, CipherSession>>,
    hashes: Mutex<HashMap<u64, HashState>>,
}

impl Default for CryptoBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CryptoBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptoBridge")
            .field("active_ciphers", &self.ciphers.lock().len())
            .field("active_hashes", &self.hashes.lock().len())
            .finish()
    }
}

impl CryptoBridge {
    pub fn new() -> Self {
        Self {
            next_handle: AtomicU64::new(1),
            ciphers: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
        }
    }

    fn alloc_handle(&self) -> u64 {
        self.next_handle.fetch_add(1, Ordering::Relaxed)
    }

    // ---- Node Buffer input/output encodings ----

    /// Decodes a JS wire value into bytes according to Node `Buffer.from(val, encoding)` rules.
    pub fn decode_input(value: Option<&Value>, encoding: Option<&str>) -> Result<Vec<u8>, String> {
        let Some(v) = value else {
            return Ok(Vec::new());
        };
        if wire::is_null_or_undefined(v) {
            return Ok(Vec::new());
        }
        if let Some(bytes) = wire::decode_bytes(v) {
            return Ok(bytes);
        }
        match v {
            Value::String(s) => Self::decode_string(s, encoding.unwrap_or("utf8")),
            other => Err(format!("Unsupported crypto input: {other}")),
        }
    }

    /// Decodes a string using a Node.js buffer encoding name.
    pub fn decode_string(s: &str, encoding: &str) -> Result<Vec<u8>, String> {
        match encoding.trim().to_ascii_lowercase().as_str() {
            "" | "utf8" | "utf-8" => Ok(s.as_bytes().to_vec()),
            "hex" => {
                let bytes = s.as_bytes();
                let mut out = Vec::with_capacity(bytes.len() / 2);
                let mut i = 0;
                while i + 1 < bytes.len() {
                    match (hex_nibble(bytes[i]), hex_nibble(bytes[i + 1])) {
                        (Some(hi), Some(lo)) => {
                            out.push((hi << 4) | lo);
                            i += 2;
                        }
                        _ => break,
                    }
                }
                Ok(out)
            }
            "base64" | "base64url" => Ok(wire::decode_base64_forgiving(s)),
            "binary" | "latin1" | "ascii" => {
                Ok(s.chars().map(|c| (c as u32 & 0xFF) as u8).collect())
            }
            "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => {
                Ok(s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect())
            }
            other => Err(format!("Unknown encoding: {other}")),
        }
    }

    /// Encodes output bytes as either a JS `ArrayBuffer` wire envelope (when `encoding` is `None`)
    /// or a JSON `String` according to Node `buf.toString(encoding)` rules.
    pub fn encode_output(bytes: &[u8], encoding: Option<&str>) -> Value {
        let Some(enc) = encoding.filter(|e| !e.is_empty()) else {
            return wire::encode_array_buffer(bytes);
        };
        let s = match enc.trim().to_ascii_lowercase().as_str() {
            "hex" => hex::encode(bytes),
            "base64" => B64_STANDARD.encode(bytes),
            "base64url" => B64_URL_NO_PAD.encode(bytes),
            "binary" | "latin1" | "ascii" => bytes.iter().map(|&b| b as char).collect(),
            "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => String::from_utf8_lossy(bytes).into_owned(),
        };
        Value::String(s)
    }

    // ---- One-shot operations ----

    /// One-shot encryption (`[0u8; 16]` default IV when `iv` is `None`).
    pub fn encrypt(
        &self,
        algorithm: &str,
        key: &[u8],
        iv: Option<&[u8]>,
        data: &[u8],
    ) -> Result<Vec<u8>, String> {
        let default_iv = [0u8; 16];
        let mut session = CipherSession::new(true, algorithm, key, iv.unwrap_or(&default_iv))?;
        let mut out = session.update(data)?;
        out.extend_from_slice(&session.finish()?);
        Ok(out)
    }

    /// One-shot decryption (`[0u8; 16]` default IV when `iv` is `None`).
    pub fn decrypt(
        &self,
        algorithm: &str,
        key: &[u8],
        iv: Option<&[u8]>,
        data: &[u8],
    ) -> Result<Vec<u8>, String> {
        let default_iv = [0u8; 16];
        let mut session = CipherSession::new(false, algorithm, key, iv.unwrap_or(&default_iv))?;
        let mut out = session.update(data)?;
        out.extend_from_slice(&session.finish()?);
        Ok(out)
    }

    /// One-shot cryptographic digest (`md5`, `sha1`, `sha256`, `sha384`, `sha512`; hyphens stripped).
    pub fn hash(&self, algorithm: &str, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut state = HashState::new(algorithm)?;
        state.update(data);
        Ok(state.finalize())
    }

    /// Cryptographically secure random bytes.
    pub fn random_bytes(&self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        rand::thread_rng().fill_bytes(&mut buf);
        buf
    }

    // ---- Stateful cipher handles ----

    pub fn cipher_create(
        &self,
        is_encrypt: bool,
        alg: &str,
        key: &[u8],
        iv: &[u8],
    ) -> Result<u64, String> {
        let session = CipherSession::new(is_encrypt, alg, key, iv)?;
        let handle = self.alloc_handle();
        self.ciphers.lock().insert(handle, session);
        Ok(handle)
    }

    pub fn cipher_update(&self, handle: u64, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut guard = self.ciphers.lock();
        let session = guard
            .get_mut(&handle)
            .ok_or_else(|| "cipher released".to_string())?;
        session.update(data)
    }

    /// Finalizes a cipher handle.
    ///
    /// For GCM encryption (`is_encrypt && is_gcm`), the handle stays alive in the map so a
    /// subsequent `cipher_get_auth_tag(handle)` call can retrieve the 16-byte authentication tag.
    pub fn cipher_final(&self, handle: u64) -> Result<Vec<u8>, String> {
        let mut guard = self.ciphers.lock();
        let keep_for_tag = {
            let session = guard
                .get(&handle)
                .ok_or_else(|| "cipher released".to_string())?;
            session.is_gcm() && session.encrypting && !session.finalized
        };

        if keep_for_tag {
            let res = guard.get_mut(&handle).unwrap().finish();
            if res.is_err() {
                guard.remove(&handle);
            }
            res
        } else {
            let mut session = guard
                .remove(&handle)
                .ok_or_else(|| "cipher released".to_string())?;
            session.finish()
        }
    }

    pub fn cipher_set_auto_padding(&self, handle: u64, auto_padding: bool) -> Result<(), String> {
        let mut guard = self.ciphers.lock();
        if let Some(session) = guard.get_mut(&handle) {
            session.set_auto_padding(auto_padding)?;
        }
        Ok(())
    }

    pub fn cipher_set_aad(&self, handle: u64, aad: &[u8]) -> Result<(), String> {
        let mut guard = self.ciphers.lock();
        if let Some(session) = guard.get_mut(&handle) {
            session.set_aad(aad)?;
        }
        Ok(())
    }

    pub fn cipher_set_auth_tag(&self, handle: u64, tag: &[u8]) -> Result<(), String> {
        let mut guard = self.ciphers.lock();
        if let Some(session) = guard.get_mut(&handle) {
            session.set_auth_tag(tag)?;
        }
        Ok(())
    }

    pub fn cipher_get_auth_tag(&self, handle: u64) -> Result<Option<Vec<u8>>, String> {
        let mut guard = self.ciphers.lock();
        let Some(session) = guard.remove(&handle) else {
            return Ok(None);
        };
        session.get_auth_tag()
    }

    // ---- Stateful hash handles ----

    pub fn hash_create(&self, alg: &str) -> Result<u64, String> {
        let state = HashState::new(alg)?;
        let handle = self.alloc_handle();
        self.hashes.lock().insert(handle, state);
        Ok(handle)
    }

    pub fn hash_update(&self, handle: u64, data: &[u8]) -> Result<(), String> {
        let mut guard = self.hashes.lock();
        let state = guard
            .get_mut(&handle)
            .ok_or_else(|| "Digest already called".to_string())?;
        state.update(data);
        Ok(())
    }

    pub fn hash_digest(&self, handle: u64) -> Result<Vec<u8>, String> {
        let state = self
            .hashes
            .lock()
            .remove(&handle)
            .ok_or_else(|| "Digest already called".to_string())?;
        Ok(state.finalize())
    }

    // ---- Synchronous bridge dispatcher ----

    /// Dispatches a synchronous `$znode.crypto.*` call from `bridge.js` / `local-modules.js`.
    pub fn dispatch_sync(&self, path: &str, args: &[Value]) -> Result<Value, String> {
        let member = path.strip_prefix("$znode.crypto.").unwrap_or(path);
        match member {
            "encrypt" => self.one_shot_wire(true, args),
            "decrypt" => self.one_shot_wire(false, args),
            "hash" => {
                // hash(data, algorithm, inputEncoding?, outputEncoding?, options?)
                let alg = arg_str(args, 1).unwrap_or("sha256");
                let data = Self::decode_input(args.first(), arg_str(args, 2))?;
                let digest = self.hash(alg, &data)?;
                Ok(Self::encode_output(&digest, arg_str(args, 3)))
            }
            "randomBytes" => {
                let n = arg_i64(args, 0).unwrap_or(0).max(0) as usize;
                let bytes = self.random_bytes(n);
                Ok(wire::encode_array_buffer(&bytes))
            }
            "__cipherCreate" => {
                let is_encrypt = match args.first() {
                    Some(Value::String(s)) => s == "cipher",
                    Some(Value::Bool(b)) => *b,
                    _ => false,
                };
                let alg = arg_str(args, 1).unwrap_or("");
                let key = Self::decode_input(args.get(2), Some("utf8"))?;
                let iv = Self::decode_input(args.get(3), Some("utf8"))?;
                let handle = self.cipher_create(is_encrypt, alg, &key, &iv)?;
                Ok(json!(handle))
            }
            "__cipherUpdate" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                let data = Self::decode_input(args.get(1), arg_str(args, 2))?;
                let out = self.cipher_update(handle, &data)?;
                Ok(Self::encode_output(&out, arg_str(args, 3)))
            }
            "__cipherFinal" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                let out = self.cipher_final(handle)?;
                Ok(Self::encode_output(&out, arg_str(args, 1)))
            }
            "__cipherOption" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                match arg_str(args, 1) {
                    Some("autoPadding") => {
                        let on = args.get(2).and_then(Value::as_bool).unwrap_or(true);
                        self.cipher_set_auto_padding(handle, on)?;
                        Ok(wire::undefined())
                    }
                    Some("authTag") => {
                        let tag = Self::decode_input(args.get(2), None)?;
                        self.cipher_set_auth_tag(handle, &tag)?;
                        Ok(wire::undefined())
                    }
                    Some("aad") => {
                        let aad = Self::decode_input(args.get(2), None)?;
                        self.cipher_set_aad(handle, &aad)?;
                        Ok(wire::undefined())
                    }
                    Some("getAuthTag") => match self.cipher_get_auth_tag(handle)? {
                        Some(tag) => Ok(wire::encode_array_buffer(&tag)),
                        None => Ok(wire::undefined()),
                    },
                    _ => Ok(wire::undefined()),
                }
            }
            "__cipherSetAutoPadding" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                let on = args.get(1).and_then(Value::as_bool).unwrap_or(true);
                self.cipher_set_auto_padding(handle, on)?;
                Ok(wire::undefined())
            }
            "__cipherSetAAD" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                let aad = Self::decode_input(args.get(1), None)?;
                self.cipher_set_aad(handle, &aad)?;
                Ok(wire::undefined())
            }
            "__cipherSetAuthTag" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                let tag = Self::decode_input(args.get(1), None)?;
                self.cipher_set_auth_tag(handle, &tag)?;
                Ok(wire::undefined())
            }
            "__cipherGetAuthTag" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                match self.cipher_get_auth_tag(handle)? {
                    Some(tag) => Ok(wire::encode_array_buffer(&tag)),
                    None => Ok(wire::undefined()),
                }
            }
            "__hashCreate" => {
                let alg = arg_str(args, 0).unwrap_or("sha256");
                let handle = self.hash_create(alg)?;
                Ok(json!(handle))
            }
            "__hashUpdate" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                let data = Self::decode_input(args.get(1), arg_str(args, 2))?;
                self.hash_update(handle, &data)?;
                Ok(wire::undefined())
            }
            "__hashDigest" => {
                let handle = arg_u64(args, 0).unwrap_or(0);
                let digest = self.hash_digest(handle)?;
                Ok(Self::encode_output(&digest, arg_str(args, 1)))
            }
            other => Err(format!("Unsupported crypto method: {other}")),
        }
    }

    fn one_shot_wire(&self, encrypting: bool, args: &[Value]) -> Result<Value, String> {
        // (data, algorithm, key, iv = zeros(16), encodings?, options?)
        let encodings = args.get(4).and_then(Value::as_object);
        let options = args.get(5).and_then(Value::as_object);

        let algorithm = arg_str(args, 1).unwrap_or("aes-256-cbc");
        let key_enc = encodings
            .and_then(|e| e.get("key"))
            .and_then(Value::as_str);
        let iv_enc = encodings
            .and_then(|e| e.get("iv"))
            .and_then(Value::as_str)
            .or(key_enc);
        let in_enc = encodings
            .and_then(|e| e.get("input"))
            .and_then(Value::as_str);
        let out_enc = encodings
            .and_then(|e| e.get("output"))
            .and_then(Value::as_str);

        let key = Self::decode_input(args.get(2), key_enc.or(Some("utf8")))?;
        let iv = match args.get(3) {
            None => vec![0u8; 16],
            Some(v) if wire::is_null_or_undefined(v) => vec![0u8; 16],
            Some(v) => Self::decode_input(Some(v), iv_enc.or(Some("utf8")))?,
        };
        let input = Self::decode_input(args.first(), in_enc)?;

        let mut session = CipherSession::new(encrypting, algorithm, &key, &iv)?;
        if let Some(opts) = options {
            if let Some(ap) = opts.get("autoPadding").and_then(Value::as_bool) {
                session.set_auto_padding(ap)?;
            }
            if let Some(aad_val) = opts.get("aad") {
                if !wire::is_null_or_undefined(aad_val) {
                    session.set_aad(&Self::decode_input(Some(aad_val), None)?)?;
                }
            }
            if !encrypting {
                if let Some(tag_val) = opts.get("authTag") {
                    if !wire::is_null_or_undefined(tag_val) {
                        session.set_auth_tag(&Self::decode_input(Some(tag_val), None)?)?;
                    }
                }
            }
        }

        let mut out = session.update(&input)?;
        out.extend_from_slice(&session.finish()?);
        Ok(Self::encode_output(&out, out_enc))
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn arg_str(args: &[Value], idx: usize) -> Option<&str> {
    args.get(idx).and_then(Value::as_str)
}

fn arg_u64(args: &[Value], idx: usize) -> Option<u64> {
    args.get(idx).and_then(Value::as_u64)
}

fn arg_i64(args: &[Value], idx: usize) -> Option<i64> {
    args.get(idx).and_then(Value::as_i64)
}
