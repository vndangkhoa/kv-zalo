//! Persistent privacy controls (`block_typing`, `block_seen`, `block_delivered`) stored
//! atomically at `<config_dir>/privacy.json` and projected into WebView user scripts.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Filename of the persisted privacy settings inside `<config_dir>`.
pub const PRIVACY_CONFIG_FILENAME: &str = "privacy.json";

/// Filename of the persisted call recording directory settings inside `<config_dir>`.
pub const CALL_RECORDING_CONFIG_FILENAME: &str = "call_recording.json";

static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(0);

const fn default_anti_recall_true() -> bool {
    true
}

/// User-configurable outbound receipt, typing indicator, and anti-recall flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyConfig {
    #[serde(default, alias = "blockTyping")]
    pub block_typing: bool,
    #[serde(default, alias = "blockSeen")]
    pub block_seen: bool,
    #[serde(default, alias = "blockDelivered")]
    pub block_delivered: bool,
    #[serde(default = "default_anti_recall_true", alias = "antiRecall")]
    pub anti_recall: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            block_typing: false,
            block_seen: false,
            block_delivered: false,
            anti_recall: true,
        }
    }
}

impl PrivacyConfig {
    /// Returns `<config_dir>/privacy.json`.
    pub fn file_path(config_dir: &Path) -> PathBuf {
        config_dir.join(PRIVACY_CONFIG_FILENAME)
    }

    /// Loads [`PrivacyConfig`] from `<config_dir>/privacy.json`, falling back to [`Self::default`]
    /// (`anti_recall: true`, outbound blockers `false`) when the file does not exist or cannot be parsed.
    pub fn load_from(config_dir: &Path) -> Self {
        let path = Self::file_path(config_dir);
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default()
    }

    /// Atomically persists this [`PrivacyConfig`] to `<config_dir>/privacy.json` (`0o600`).
    pub fn save_to(&self, config_dir: &Path) -> Result<()> {
        let path = Self::file_path(config_dir);
        let mut bytes =
            serde_json::to_vec_pretty(self).context("failed to serialize PrivacyConfig")?;
        bytes.push(b'\n');
        write_atomic_private(&path, &bytes)
    }

    /// Applies partial updates from a JSON object (supporting both `snake_case` and `camelCase` keys).
    pub fn apply_patch(&mut self, patch: &Value) {
        let Some(obj) = patch.as_object() else {
            return;
        };
        if let Some(v) = obj
            .get("block_typing")
            .or_else(|| obj.get("blockTyping"))
            .and_then(Value::as_bool)
        {
            self.block_typing = v;
        }
        if let Some(v) = obj
            .get("block_seen")
            .or_else(|| obj.get("blockSeen"))
            .and_then(Value::as_bool)
        {
            self.block_seen = v;
        }
        if let Some(v) = obj
            .get("block_delivered")
            .or_else(|| obj.get("blockDelivered"))
            .and_then(Value::as_bool)
        {
            self.block_delivered = v;
        }
        if let Some(v) = obj
            .get("anti_recall")
            .or_else(|| obj.get("antiRecall"))
            .and_then(Value::as_bool)
        {
            self.anti_recall = v;
        }
    }

    /// Generates the JavaScript init/update snippet invoking `window.__olazSetPrivacy({...})`
    /// (and seeding `window.__olazPrivacy` for early `UserScriptInjectionTime::Start` scripts).
    pub fn to_js_init_script(&self) -> String {
        let payload = json!({
            "block_typing": self.block_typing,
            "block_seen": self.block_seen,
            "block_delivered": self.block_delivered,
            "anti_recall": self.anti_recall,
            "blockTyping": self.block_typing,
            "blockSeen": self.block_seen,
            "blockDelivered": self.block_delivered,
            "antiRecall": self.anti_recall,
        });
        format!(
            "window.__olazPrivacy = {payload}; window.__olazSetPrivacy && window.__olazSetPrivacy({payload});"
        )
    }
}

pub(crate) fn write_atomic_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    let (tmp_path, mut file) = create_unique_tmp(parent)
        .with_context(|| format!("failed to create temp file in {}", parent.display()))?;

    let write_res = (|| -> Result<()> {
        file.write_all(contents)
            .context("failed to write temporary config bytes")?;
        file.sync_all()
            .context("failed to fsync temporary config file")?;
        drop(file);
        fs::rename(&tmp_path, path)
            .with_context(|| format!("failed to atomically replace {}", path.display()))?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}

fn create_unique_tmp(parent: &Path) -> std::io::Result<(PathBuf, File)> {
    let pid = std::process::id();
    for _ in 0..128 {
        let seq = NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".olaz-tmp-{pid}-{seq}"));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        match opts.open(&candidate) {
            Ok(f) => return Ok((candidate, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "exhausted temporary file name attempts",
    ))
}

/// Persisted call recording directory configuration stored at `<config_dir>/call_recording.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecordingSettings {
    #[serde(default, alias = "recordingsDir")]
    pub recordings_dir: Option<PathBuf>,
}

impl CallRecordingSettings {
    /// Returns `<config_dir>/call_recording.json`.
    pub fn file_path(config_dir: &Path) -> PathBuf {
        config_dir.join(CALL_RECORDING_CONFIG_FILENAME)
    }

    /// Loads [`CallRecordingSettings`] from `<config_dir>/call_recording.json`, falling back to
    /// [`Self::default`] (`recordings_dir: None`) when the file is absent or malformed.
    pub fn load_from(config_dir: &Path) -> Self {
        let path = Self::file_path(config_dir);
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default()
    }

    /// Atomically persists this [`CallRecordingSettings`] to `<config_dir>/call_recording.json` (`0o600`).
    pub fn save_to(&self, config_dir: &Path) -> Result<()> {
        let path = Self::file_path(config_dir);
        let mut bytes = serde_json::to_vec_pretty(self)
            .context("failed to serialize CallRecordingSettings")?;
        bytes.push(b'\n');
        write_atomic_private(&path, &bytes)
    }

    /// Resolves the effective call recordings directory, falling back to `default_dir` when no
    /// custom path has been configured.
    pub fn resolved_dir(&self, default_dir: &Path) -> PathBuf {
        self.recordings_dir
            .as_ref()
            .filter(|p| !p.as_os_str().is_empty())
            .cloned()
            .unwrap_or_else(|| default_dir.to_path_buf())
    }

    /// Generates the JavaScript init/update snippet invoking `window.__olazSetCallRecording({...})`.
    pub fn to_js_init_script(active_dir: &Path, default_dir: &Path) -> String {
        let payload = json!({
            "dir": active_dir.to_string_lossy(),
            "defaultDir": default_dir.to_string_lossy(),
            "isCustom": active_dir != default_dir,
        });
        format!(
            "window.__olazCallRecording = {payload}; window.__olazSetCallRecording && window.__olazSetCallRecording({payload});"
        )
    }
}
