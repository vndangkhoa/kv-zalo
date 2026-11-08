//! JSON wire value encoding/decoding matching `Sources/Olaz/HostScripts/bridge.js`
//! and `Sources/Olaz/ZaloRuntime/ZaloBridge.swift`.
//!
//! Across WebKit script message handlers and synchronous XHR bridge endpoints,
//! binary buffers, `ArrayBuffer`s, `undefined`, and Node `fs.Stats` objects are
//! tagged as structured JSON envelopes.

use std::fs::Metadata;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use base64::{alphabet, Engine as _};
use serde_json::{json, Value};

const PERMISSIVE_B64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Encodes a byte slice as a JS `Uint8Array` wire value: `{"__bytes": "<base64>"}`.
pub fn encode_bytes(bytes: &[u8]) -> Value {
    json!({
        "__bytes": STANDARD.encode(bytes),
    })
}

/// Encodes a byte slice as a JS `ArrayBuffer` wire value:
/// `{"__bytes": "<base64>", "__arraybuffer": true}`.
pub fn encode_array_buffer(bytes: &[u8]) -> Value {
    json!({
        "__bytes": STANDARD.encode(bytes),
        "__arraybuffer": true,
    })
}

/// Decodes a binary wire value back into raw bytes.
///
/// Accepts:
/// - `{"__bytes": "<base64>"}` (both `Uint8Array` and `ArrayBuffer` envelopes)
/// - `{"type": "Buffer", "data": [u8, ...]}` (Node `Buffer.toJSON()` envelope)
/// - `[u8, ...]` (plain JSON byte arrays where every element is an integer in `0..=255`)
///
/// Plain JSON strings intentionally return `None` so callers (such as `CryptoBridge`)
/// can apply explicit string encodings (`"utf8"`, `"hex"`, `"base64"`, etc.).
pub fn decode_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Object(map) => {
            if let Some(b64) = map.get("__bytes").and_then(Value::as_str) {
                return Some(decode_base64_forgiving(b64));
            }
            if map.get("type").and_then(Value::as_str) == Some("Buffer") {
                if let Some(arr) = map.get("data").and_then(Value::as_array) {
                    return decode_u8_array(arr);
                }
            }
            None
        }
        Value::Array(arr) => decode_u8_array(arr),
        _ => None,
    }
}

/// Decodes either a binary wire value (`decode_bytes`) or a UTF-8 JSON string.
///
/// Mirrors `Array.data(_:)` in `ZaloBridge.swift`.
pub fn decode_bytes_or_utf8(value: &Value) -> Option<Vec<u8>> {
    if let Some(bytes) = decode_bytes(value) {
        return Some(bytes);
    }
    value.as_str().map(|s| s.as_bytes().to_vec())
}

/// Encodes JS `undefined` as `{"__undefined": true}`.
pub fn undefined() -> Value {
    json!({ "__undefined": true })
}

/// Returns `true` if `value` is the tagged `{"__undefined": true}` wire sentinel.
pub fn is_undefined(value: &Value) -> bool {
    value
        .as_object()
        .and_then(|o| o.get("__undefined"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// Returns `true` if `value` is JSON `null` or the tagged `{"__undefined": true}` sentinel.
pub fn is_null_or_undefined(value: &Value) -> bool {
    value.is_null() || is_undefined(value)
}

/// Normalized file metadata fields serialized into a `{"__stats": true, ...}` wire object
/// consumed by `bridge.js`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireStats {
    pub size: u64,
    pub mode: u32,
    pub mtime_ms: f64,
    pub atime_ms: f64,
    pub ctime_ms: f64,
    pub birthtime_ms: f64,
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
}

impl From<&Metadata> for WireStats {
    fn from(meta: &Metadata) -> Self {
        let mtime_ms = system_time_ms(meta.modified().ok()).unwrap_or(0.0);
        let atime_ms = system_time_ms(meta.accessed().ok()).unwrap_or(mtime_ms);
        let birthtime_ms = system_time_ms(meta.created().ok()).unwrap_or(mtime_ms);

        #[cfg(unix)]
        let (mode, ctime_ms) = {
            use std::os::unix::fs::MetadataExt;
            let ctime = (meta.ctime() as f64) * 1000.0 + (meta.ctime_nsec() as f64) / 1_000_000.0;
            (meta.mode(), ctime)
        };
        #[cfg(not(unix))]
        let (mode, ctime_ms) = {
            let perm = if meta.permissions().readonly() {
                0o444
            } else {
                0o644
            };
            (perm, mtime_ms)
        };

        let ft = meta.file_type();
        Self {
            size: meta.len(),
            mode,
            mtime_ms,
            atime_ms,
            ctime_ms,
            birthtime_ms,
            is_file: ft.is_file(),
            is_directory: ft.is_dir(),
            is_symbolic_link: ft.is_symlink(),
        }
    }
}

impl From<Metadata> for WireStats {
    fn from(meta: Metadata) -> Self {
        Self::from(&meta)
    }
}

/// Encodes file metadata into the `{"__stats": true, ...}` envelope reconstructed into
/// a Node `fs.Stats`-like object by `bridge.js`.
pub fn encode_stats(stats: impl Into<WireStats>) -> Value {
    let s = stats.into();
    json!({
        "__stats": true,
        "size": s.size,
        "mode": s.mode,
        "mtimeMs": s.mtime_ms,
        "atimeMs": s.atime_ms,
        "ctimeMs": s.ctime_ms,
        "birthtimeMs": s.birthtime_ms,
        "isFile": s.is_file,
        "isDirectory": s.is_directory,
        "isSymbolicLink": s.is_symbolic_link,
    })
}

/// Forgiving base64 decoder matching Node `Buffer.from(str, 'base64')` and Foundation's
/// `.ignoreUnknownCharacters`: skips whitespace and non-alphabet bytes, accepts `-`/`_`
/// URL-safe characters, stops at `=`, and drops any trailing lone 6-bit quantum.
pub(crate) fn decode_base64_forgiving(input: &str) -> Vec<u8> {
    if let Ok(exact) = STANDARD.decode(input) {
        return exact;
    }
    let mut cleaned = Vec::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'=' => break,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => cleaned.push(b),
            b'-' => cleaned.push(b'+'),
            b'_' => cleaned.push(b'/'),
            _ => {}
        }
    }
    if cleaned.len() % 4 == 1 {
        cleaned.pop();
    }
    PERMISSIVE_B64.decode(&cleaned).unwrap_or_default()
}

fn decode_u8_array(arr: &[Value]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let n = item.as_u64()?;
        let byte = u8::try_from(n).ok()?;
        out.push(byte);
    }
    Some(out)
}

fn system_time_ms(t: Option<SystemTime>) -> Option<f64> {
    let t = t?;
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs_f64() * 1000.0),
        Err(neg) => Some(-neg.duration().as_secs_f64() * 1000.0),
    }
}
