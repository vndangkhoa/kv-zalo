//! Structured VoIP diagnostics logger writing to `<data_dir>/logs/voip/voip.log`
//! with automatic 2 MiB rotation and redaction of sensitive call tokens/keys.

use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum size of a single VoIP log file before rotating to `voip.log.1`.
pub const MAX_VOIP_LOG_BYTES: u64 = 2 * 1024 * 1024;

static GLOBAL_LOGGER: OnceLock<VoipLogger> = OnceLock::new();

/// Sensitive JSON field names redacted before writing to disk.
const SENSITIVE_KEYS: &[&str] = &[
    "sessid",
    "session",
    "zpw_sek",
    "cookie",
    "cookies",
    "secret",
    "secretkey",
    "token",
    "authtoken",
    "accesstoken",
    "authkey",
    "cipherkey",
    "masterkey",
    "mastersalt",
];

struct InnerLogFile {
    dir: PathBuf,
    file_path: PathBuf,
    file: Option<File>,
    written_bytes: u64,
}

/// Thread-safe rotating VoIP logger instance.
pub struct VoipLogger {
    inner: Mutex<InnerLogFile>,
}

impl VoipLogger {
    /// Creates a logger writing into `<data_dir>/logs/voip/voip.log`.
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("logs").join("voip");
        let _ = fs::create_dir_all(&dir);
        let file_path = dir.join("voip.log");
        let existing_len = fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .ok();
        Self {
            inner: Mutex::new(InnerLogFile {
                dir,
                file_path,
                file,
                written_bytes: existing_len,
            }),
        }
    }

    /// Path of the active `voip.log` file.
    pub fn log_file_path(&self) -> PathBuf {
        self.inner.lock().file_path.clone()
    }

    /// Appends a single timestamped line to `voip.log`, rotating at 2 MiB.
    pub fn log(&self, category: &str, message: &str) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let clean_msg = redact_inline_tokens(message);
        let line = format!("[{now_ms}] [{category}] {clean_msg}\n");

        let mut guard = self.inner.lock();
        if guard.written_bytes + (line.len() as u64) > MAX_VOIP_LOG_BYTES {
            guard.file.take();
            let rotated = guard.dir.join("voip.log.1");
            let _ = fs::rename(&guard.file_path, rotated);
            guard.file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&guard.file_path)
                .ok();
            guard.written_bytes = 0;
        }
        if guard.file.is_none() {
            let _ = fs::create_dir_all(&guard.dir);
            guard.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&guard.file_path)
                .ok();
        }
        if let Some(ref mut f) = guard.file {
            if f.write_all(line.as_bytes()).is_ok() {
                let _ = f.flush();
                guard.written_bytes += line.len() as u64;
            }
        }
    }

    /// Logs a JSON value after redacting sensitive fields (`sessId`, `session`, etc.).
    pub fn log_json(&self, category: &str, label: &str, value: &Value) {
        let redacted = redact_voip_value(value);
        let serialized = serde_json::to_string(&redacted).unwrap_or_else(|_| "{}".to_string());
        self.log(category, &format!("{label}: {serialized}"));
    }
}

/// Initializes the process-wide VoIP logger rooted at `<data_dir>/logs/voip/voip.log`.
pub fn init_global_voip_log(data_dir: &Path) -> &'static VoipLogger {
    GLOBAL_LOGGER.get_or_init(|| VoipLogger::new(data_dir))
}

/// Writes to the global VoIP logger if initialized (falls back to `tracing::debug!`).
pub fn voip_log(category: &str, message: &str) {
    if let Some(logger) = GLOBAL_LOGGER.get() {
        logger.log(category, message);
    } else {
        tracing::debug!(target: "olaz_voip", category = category, "{}", redact_inline_tokens(message));
    }
}

/// Writes a redacted JSON payload to the global VoIP logger if initialized.
pub fn voip_log_json(category: &str, label: &str, value: &Value) {
    if let Some(logger) = GLOBAL_LOGGER.get() {
        logger.log_json(category, label, value);
    } else {
        let redacted = redact_voip_value(value);
        tracing::debug!(target: "olaz_voip", category = category, label = label, payload = %redacted);
    }
}

/// Redacts a secret string while preserving length for protocol diagnostics.
pub fn redact_secret_str(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let char_len = secret.len();
    format!("[REDACTED len={char_len}]")
}

/// Recursively clones `value` while redacting sensitive token/session fields
/// (including nested JSON-encoded string fields like `params` and `extendData`).
pub fn redact_voip_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                let lower = k.to_ascii_lowercase();
                if SENSITIVE_KEYS.iter().any(|sk| lower == *sk || lower.ends_with(sk)) {
                    let s = match v {
                        Value::String(raw) => redact_secret_str(raw),
                        other => redact_secret_str(&other.to_string()),
                    };
                    out.insert(k.clone(), Value::String(s));
                } else if (lower == "params" || lower == "extenddata") && v.is_string() {
                    let raw = v.as_str().unwrap_or_default();
                    if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                        let redacted_nested = redact_voip_value(&parsed);
                        let encoded = serde_json::to_string(&redacted_nested)
                            .unwrap_or_else(|_| "[REDACTED_JSON]".to_string());
                        out.insert(k.clone(), Value::String(encoded));
                    } else {
                        out.insert(k.clone(), Value::String(redact_inline_tokens(raw)));
                    }
                } else {
                    out.insert(k.clone(), redact_voip_value(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_voip_value).collect()),
        Value::String(s) => Value::String(redact_inline_tokens(s)),
        other => other.clone(),
    }
}

/// Formats up to `max_bytes` of a packet as lowercase hex with total byte length.
pub fn hex_preview(bytes: &[u8], max_bytes: usize) -> String {
    let take = bytes.len().min(max_bytes);
    let prefix = hex::encode(&bytes[..take]);
    if bytes.len() > take {
        format!("{prefix}...({}B)", bytes.len())
    } else {
        format!("{prefix}({}B)", bytes.len())
    }
}

fn redact_inline_tokens(input: &str) -> String {
    // Redact inline `sessId=...` or `"sessId":"..."` or `"session":"..."` patterns if present.
    let mut out = input.to_string();
    for key in ["\"sessId\":\"", "\"session\":\"", "sessId=", "session="] {
        let mut search_from = 0usize;
        while let Some(rel_idx) = out[search_from..].find(key) {
            let val_start = search_from + rel_idx + key.len();
            let val_end = if key.ends_with('"') {
                out[val_start..]
                    .find('"')
                    .map(|e| val_start + e)
                    .unwrap_or(out.len())
            } else {
                out[val_start..]
                    .find(|c: char| c.is_whitespace() || c == ',' || c == '&' || c == ';')
                    .map(|e| val_start + e)
                    .unwrap_or(out.len())
            };
            let secret_len = val_end.saturating_sub(val_start);
            if secret_len > 0 && !out[val_start..val_end].starts_with("[REDACTED") {
                let replacement = format!("[REDACTED len={secret_len}]");
                out.replace_range(val_start..val_end, &replacement);
                search_from = val_start + replacement.len();
            } else {
                search_from = val_end.saturating_add(1).min(out.len());
            }
            if search_from >= out.len() {
                break;
            }
        }
    }
    out
}
