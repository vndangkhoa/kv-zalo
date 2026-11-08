//! `zcall-v2` wire transport (`CallTransport.swift` byte-exact port: `AES-128-CBC` PKCS#7,
//! `CallChunker`, `CallFrameDecoder`) and Phase 1 `LinuxCallCoordinator`.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use aes::Aes128;
use base64::Engine as _;
use cipher::block_padding::Pkcs7;
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::notifications::NotificationRequest;

type Aes128CbcEnc = cbc::Encryptor<Aes128>;
type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// Official `ZPC_SECRET_VALUES.callV2TransportAesKey` (base64 of a 16-byte AES-128 key).
pub const CALL_V2_KEY_BASE64: &str = "yjAF9oqMWl6XfXYJn9mA7w==";

/// 16 zero bytes (`Buffer.from("0".repeat(32), "hex")`).
pub const ZERO_IV: [u8; 16] = [0u8; 16];

/// Phase 1 user-facing message explaining why direct outgoing calls are disabled on Linux.
pub const LINUX_CALL_UNSUPPORTED_MESSAGE: &str =
    "Chưa hỗ trợ gọi thoại/video trực tiếp trên Linux — vui lòng nhận cuộc gọi trên điện thoại";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CallTransportError {
    #[error("AES-128 key must be exactly 16 bytes (got {0})")]
    InvalidKeyLength(usize),
}

/// Wire-level `AES-128-CBC` + PKCS#7 hex framing shared between Zalo main process and `ZaloCall`.
pub struct CallTransportCrypto;

impl CallTransportCrypto {
    /// Decodes the 16-byte static `callV2TransportAesKey`.
    pub fn default_key() -> [u8; 16] {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(CALL_V2_KEY_BASE64)
            .expect("valid static base64 key");
        decoded
            .try_into()
            .expect("callV2TransportAesKey decodes to 16 bytes")
    }

    /// Encrypts `text` with `AES-128-CBC` (PKCS#7, 16-byte zero IV) using the default key, returning lowercase hex.
    pub fn encrypt(text: &str) -> Result<String, CallTransportError> {
        let key = Self::default_key();
        Self::encrypt_with_key(text, &key)
    }

    /// Encrypts `text` with an explicit 16-byte AES-128 key, returning lowercase hex.
    pub fn encrypt_with_key(text: &str, key: &[u8]) -> Result<String, CallTransportError> {
        let enc = Aes128CbcEnc::new_from_slices(key, &ZERO_IV)
            .map_err(|_| CallTransportError::InvalidKeyLength(key.len()))?;
        let sealed = enc.encrypt_padded_vec_mut::<Pkcs7>(text.as_bytes());
        Ok(hex::encode(sealed))
    }

    /// Decrypts lowercase/uppercase hex frame using the default key; returns `None` on invalid hex,
    /// non-block-multiple length, bad PKCS#7 padding, or invalid UTF-8.
    pub fn decrypt(hex_frame: &str) -> Option<String> {
        let key = Self::default_key();
        Self::decrypt_with_key(hex_frame, &key)
    }

    /// Decrypts hex frame with an explicit 16-byte AES-128 key.
    pub fn decrypt_with_key(hex_frame: &str, key: &[u8]) -> Option<String> {
        let bytes = hex::decode(hex_frame).ok()?;
        if bytes.is_empty() || bytes.len() % 16 != 0 {
            return None;
        }
        let dec = Aes128CbcDec::new_from_slices(key, &ZERO_IV).ok()?;
        let plain = dec.decrypt_padded_vec_mut::<Pkcs7>(&bytes).ok()?;
        String::from_utf8(plain).ok()
    }
}

/// `buildListMsgs` port (`main-dist/main.js` module `4KIH`):
/// - Payloads `<= 4000` bytes become `["<payload>$"]`.
/// - Payloads `> 4000` bytes are sliced into `<slice>#<seq>#<count>#<index>#$`.
#[derive(Debug, Clone)]
pub struct CallChunker {
    sequence: i64,
}

impl Default for CallChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl CallChunker {
    pub const CHUNK_SIZE: usize = 4000;

    pub fn new() -> Self {
        Self { sequence: 1 }
    }

    pub fn sequence(&self) -> i64 {
        self.sequence
    }

    /// Splits `payload` using the current wall-clock millisecond timestamp.
    pub fn chunks(&mut self, payload: &str) -> Vec<String> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        self.chunks_at(payload, now_ms)
    }

    /// Splits `payload` using an explicit `now_ms` timestamp (`sequence = max(sequence + 1, now_ms)`).
    pub fn chunks_at(&mut self, payload: &str, now_ms: i64) -> Vec<String> {
        let bytes = payload.as_bytes();
        let count = bytes.len().div_ceil(Self::CHUNK_SIZE);
        self.sequence = (self.sequence + 1).max(now_ms);

        if count <= 1 {
            return vec![format!("{payload}$")];
        }

        let seq = self.sequence;
        (0..count)
            .map(|index| {
                let start = index * Self::CHUNK_SIZE;
                let end = ((index + 1) * Self::CHUNK_SIZE).min(bytes.len());
                let slice = std::str::from_utf8(&bytes[start..end])
                    .expect("hex/ASCII payload slices align on UTF-8 boundaries");
                format!("{slice}#{seq}#{count}#{index}#$")
            })
            .collect()
    }
}

/// Reassembles multi-chunk `<slice>#<seq>#<count>#<index>#$` frames emitted by [`CallChunker`].
#[derive(Debug, Default, Clone)]
pub struct CallChunkReassembler {
    parts: HashMap<i64, HashMap<usize, String>>,
}

impl CallChunkReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one `$`-stripped frame (`<hex>` or `<slice>#<seq>#<count>#<index>#`) and returns
    /// the complete hex payload once all chunks of `seq` have arrived.
    pub fn feed_frame(&mut self, frame_without_dollar: &str) -> Option<String> {
        if !frame_without_dollar.contains('#') {
            return Some(frame_without_dollar.to_string());
        }
        let fields: Vec<&str> = frame_without_dollar.split('#').collect();
        if fields.len() != 4 && fields.len() != 5 {
            return None;
        }
        let seq: i64 = fields[1].parse().ok()?;
        let count: usize = fields[2].parse().ok()?;
        let index: usize = fields[3].parse().ok()?;
        if count == 0 || index >= count {
            return None;
        }
        let entry = self.parts.entry(seq).or_default();
        entry.insert(index, fields[0].to_string());
        if entry.len() < count {
            return None;
        }
        let mut joined = String::new();
        for i in 0..count {
            joined.push_str(entry.get(&i)?);
        }
        self.parts.remove(&seq);
        Some(joined)
    }
}

/// `$`-delimited stream decoder from `ZaloCall` -> main process with a 4 MiB buffer cap.
#[derive(Debug, Clone)]
pub struct CallFrameDecoder {
    buffer: Vec<u8>,
    max_buffer_bytes: usize,
}

impl Default for CallFrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CallFrameDecoder {
    /// 4 MiB maximum buffered tail before dropping runaway unterminated data.
    pub const MAX_BUFFER_BYTES: usize = 4 * 1024 * 1024;
    pub const TERMINATOR: u8 = b'$';

    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer_bytes: Self::MAX_BUFFER_BYTES,
        }
    }

    pub fn with_max_buffer_bytes(max_buffer_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer_bytes,
        }
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
    }

    /// Feeds incoming socket bytes using the default `callV2TransportAesKey`, returning decoded JSON objects.
    /// Corrupt/non-JSON frames and 4 MiB buffer overflows increment `dropped`.
    pub fn feed(
        &mut self,
        data: &[u8],
        dropped: &mut usize,
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        let key = CallTransportCrypto::default_key();
        self.feed_with_key(data, &key, dropped)
    }

    /// Feeds incoming socket bytes using an explicit 16-byte AES-128 key.
    pub fn feed_with_key(
        &mut self,
        data: &[u8],
        key: &[u8],
        dropped: &mut usize,
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.buffer.extend_from_slice(data);
        let mut messages = Vec::new();

        while let Some(end) = self.buffer.iter().position(|&b| b == Self::TERMINATOR) {
            let frame_bytes: Vec<u8> = self.buffer.drain(..=end).take(end).collect();
            if frame_bytes.is_empty() {
                continue;
            }
            match Self::parse_frame(&frame_bytes, key) {
                Some(obj) => messages.push(obj),
                None => *dropped += 1,
            }
        }

        if self.buffer.len() > self.max_buffer_bytes {
            self.buffer.clear();
            *dropped += 1;
        }

        messages
    }

    /// Decrypts a single `$`-stripped frame, strips NUL bytes (`\0`), and parses a JSON object.
    pub fn parse_frame(
        frame: &[u8],
        key: &[u8],
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let raw_text = String::from_utf8_lossy(frame);
        let trimmed = raw_text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let plain = CallTransportCrypto::decrypt_with_key(trimmed, key)?;
        let cleaned = plain.replace('\0', "");
        match serde_json::from_str::<serde_json::Value>(&cleaned).ok()? {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        }
    }
}

/// Parsed incoming VoIP call signaling event for desktop ring notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingVoipCall {
    pub call_id: Option<String>,
    pub conv_id: String,
    pub name: String,
    pub caller: Option<String>,
    pub avatar: Option<String>,
    pub is_group: bool,
    pub is_video: bool,
}

impl IncomingVoipCall {
    pub fn notification_title(&self) -> &'static str {
        if self.is_group {
            "Cuộc gọi nhóm đến"
        } else if self.is_video {
            "Cuộc gọi video đến"
        } else {
            "Cuộc gọi đến"
        }
    }

    pub fn notification_body(&self) -> String {
        match (&self.caller, self.is_group) {
            (Some(caller), true) => format!(
                "{} đang gọi trong {} — vui lòng nhận cuộc gọi trên điện thoại",
                caller, self.name
            ),
            _ => format!(
                "{} đang gọi cho bạn — vui lòng nhận cuộc gọi trên điện thoại",
                self.name
            ),
        }
    }

    /// Converts this incoming call into a [`NotificationRequest`] for [`crate::notifications::LinuxNotifier`].
    pub fn to_notification_request(&self) -> NotificationRequest {
        NotificationRequest {
            title: self.notification_title().to_string(),
            body: self.notification_body(),
            tag: if self.conv_id.is_empty() {
                "zalo-call".to_string()
            } else {
                self.conv_id.clone()
            },
            silent: false,
            icon: self
                .avatar
                .as_deref()
                .and_then(NotificationRequest::parse_icon),
        }
    }
}

/// Result returned when the UI queries or attempts an outgoing call in Phase 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingCallStatus {
    pub can_call: bool,
    pub reason: String,
}

/// Phase 1 Linux VoIP coordinator:
/// - Cleanly reports `can_call: false` with a clear Vietnamese message for outgoing calls.
/// - Parses incoming `voip` / `call-adapter` signaling payloads to trigger desktop ring banners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinuxCallCoordinator {
    pub can_call: bool,
    pub unsupported_reason: String,
    pub active_incoming: Option<IncomingVoipCall>,
    pub last_notice: Option<String>,
}

impl Default for LinuxCallCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxCallCoordinator {
    pub fn new() -> Self {
        Self {
            can_call: false,
            unsupported_reason: LINUX_CALL_UNSUPPORTED_MESSAGE.to_string(),
            active_incoming: None,
            last_notice: None,
        }
    }

    pub fn can_call(&self) -> bool {
        self.can_call
    }

    pub fn status(&self) -> OutgoingCallStatus {
        OutgoingCallStatus {
            can_call: self.can_call,
            reason: self.unsupported_reason.clone(),
        }
    }

    /// Rejects outgoing calls in Phase 1 with `LINUX_CALL_UNSUPPORTED_MESSAGE` and records `last_notice`.
    pub fn start_outgoing_call(&mut self, _target_id: &str, _video: bool) -> OutgoingCallStatus {
        self.last_notice = Some(self.unsupported_reason.clone());
        self.status()
    }

    /// Clears any active incoming ring state (e.g. call ended, answered on phone, or timed out).
    pub fn clear_incoming(&mut self) {
        self.active_incoming = None;
    }

    /// Parses incoming `voip` / `call` signaling events from either `call-adapter.js`
    /// (`{call: {kind: "incoming", ...}}`) or raw VoIP signaling (`{event: "voip", ...}`).
    ///
    /// Returns `Some(IncomingVoipCall)` when a desktop ring notification should be shown.
    pub fn handle_voip_event(&mut self, payload: &serde_json::Value) -> Option<IncomingVoipCall> {
        let obj = payload
            .get("call")
            .and_then(|v| v.as_object())
            .or_else(|| payload.get("voip").and_then(|v| v.as_object()))
            .or_else(|| payload.as_object())?;

        let kind = obj
            .get("kind")
            .or_else(|| obj.get("event"))
            .or_else(|| obj.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Clear active ring when call ends / transitions to free.
        if matches!(kind, "ended" | "cancel" | "hangup" | "free")
            || obj.get("state").and_then(|v| v.as_str()) == Some("free")
        {
            self.active_incoming = None;
            return None;
        }

        let is_incoming = matches!(kind, "incoming" | "voip" | "ring" | "invite")
            || (kind.is_empty() && obj.contains_key("callerId"));
        if !is_incoming {
            return None;
        }

        // Official rule: if already busy in another call, nothing rings.
        if obj.get("busy").and_then(|v| v.as_bool()).unwrap_or(false) {
            return None;
        }

        let is_group = obj
            .get("group")
            .or_else(|| obj.get("isGroup"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let is_video = obj
            .get("video")
            .or_else(|| obj.get("isVideo"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let non_empty_str = |key: &str| -> Option<String> {
            obj.get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        };

        let raw_name = non_empty_str("name").or_else(|| non_empty_str("groupName"));
        let raw_caller = non_empty_str("caller").or_else(|| non_empty_str("callerName"));

        // Exact parity with `ZaloEngine+CallHook.swift:199-203`:
        // Group call uses group `name`; if missing, `caller` stands in (with separate `caller` cleared),
        // else `"Cuộc gọi nhóm"`. 1-1 uses `name`, else `caller`, else `"Một người bạn"`.
        let name = raw_name
            .clone()
            .or_else(|| raw_caller.clone())
            .unwrap_or_else(|| {
                if is_group {
                    "Cuộc gọi nhóm".to_string()
                } else {
                    "Một người bạn".to_string()
                }
            });

        let caller = if is_group && raw_name.is_some() {
            raw_caller
        } else {
            None
        };

        let avatar = non_empty_str("avatar").or_else(|| non_empty_str("callerAvatar"));
        let conv_id = non_empty_str("convId")
            .or_else(|| non_empty_str("groupId"))
            .or_else(|| non_empty_str("callerId"))
            .or_else(|| non_empty_str("fromUid"))
            .unwrap_or_default();
        let call_id = non_empty_str("callId").or_else(|| non_empty_str("id"));

        let incoming = IncomingVoipCall {
            call_id,
            conv_id,
            name,
            caller,
            avatar,
            is_group,
            is_video,
        };
        self.active_incoming = Some(incoming.clone());
        Some(incoming)
    }
}
