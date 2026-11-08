//! Host-side platform paths, device identity (`imei`), `electron-store` persistence,
//! database adapter normalization, and symlink-aware write confinement.
//!
//! Port of `Sources/Olaz/ZaloRuntime/ZaloPlatform.swift` and
//! `ZaloMainProcess.normalizeDatabaseAdapterState` (`ZaloMainProcess.swift:177-200`).

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use directories::{BaseDirs, UserDirs};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tempfile::NamedTempFile;
use uuid::Uuid;

/// Maximum symlink hops resolved before rejecting a path as a symlink loop (`ELOOP`).
const MAX_SYMLINK_HOPS: usize = 40;

/// Client identity constants passed by the official macOS Zalo main process to renderer pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentity {
    /// `type` query parameter of the official macOS client (`"23"`).
    pub client_type: String,
    /// `client_version` query parameter of the official macOS client (`"694"`).
    pub client_version: String,
}

impl Default for ClientIdentity {
    fn default() -> Self {
        Self {
            client_type: "23".to_string(),
            client_version: "694".to_string(),
        }
    }
}

impl ClientIdentity {
    /// Builds the official desktop User-Agent string (`"ZaloPC-darwin-23v694"`).
    ///
    /// The renderer derives the device ID from `md5(navigator.userAgent)`, so this exact
    /// format must be preserved on Linux as well.
    pub fn user_agent(&self) -> String {
        format!("ZaloPC-darwin-{}v{}", self.client_type, self.client_version)
    }
}

/// Host directories, device identity, JSON stores, and filesystem write sandbox.
#[derive(Debug)]
pub struct ZaloPlatform {
    app_version: RwLock<String>,
    pub identity: ClientIdentity,
    base_dir: PathBuf,
    user_data: PathBuf,
    runtime_root: PathBuf,
    webkit_dir: PathBuf,
    cache_dir: PathBuf,
    temp_dir: PathBuf,
    home_dir: PathBuf,
    downloads_dir: PathBuf,
    documents_dir: PathBuf,
    desktop_dir: PathBuf,
    pictures_dir: PathBuf,
    imei_cache: Mutex<Option<String>>,
    approved_write_paths: RwLock<Vec<PathBuf>>,
    approved_read_paths: RwLock<Vec<PathBuf>>,
}

impl ZaloPlatform {
    /// Creates a `ZaloPlatform` using standard Linux XDG directories:
    /// - `base_dir`: `~/.local/share/olaz`
    /// - `user_data`: `~/.local/share/olaz/ZaloData`
    /// - `runtime_root`: `~/.local/share/olaz/ZaloRuntime` (`current`, `pending`, `work`)
    /// - `webkit_dir`: `~/.local/share/olaz/webkit`
    /// - `cache_dir`: `~/.cache/olaz`
    /// - `temp_dir`: `/tmp/olaz`
    pub fn new(app_version: impl Into<String>) -> Self {
        let base_dirs = BaseDirs::new();
        let home_dir = base_dirs
            .as_ref()
            .map(|b| b.home_dir().to_path_buf())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/tmp"));

        let data_root = base_dirs
            .as_ref()
            .map(|b| b.data_local_dir().to_path_buf())
            .unwrap_or_else(|| home_dir.join(".local/share"));
        let cache_root = base_dirs
            .as_ref()
            .map(|b| b.cache_dir().to_path_buf())
            .unwrap_or_else(|| home_dir.join(".cache"));

        let user_dirs = UserDirs::new();
        let downloads_dir = user_dirs
            .as_ref()
            .and_then(|u| u.download_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| home_dir.join("Downloads"));
        let documents_dir = user_dirs
            .as_ref()
            .and_then(|u| u.document_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| home_dir.join("Documents"));
        let desktop_dir = user_dirs
            .as_ref()
            .and_then(|u| u.desktop_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| home_dir.join("Desktop"));
        let pictures_dir = user_dirs
            .as_ref()
            .and_then(|u| u.picture_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| home_dir.join("Pictures"));

        let base_dir = data_root.join("olaz");
        let user_data = base_dir.join("ZaloData");
        let runtime_root = base_dir.join("ZaloRuntime");
        let webkit_dir = base_dir.join("webkit");
        let cache_dir = cache_root.join("olaz");
        let temp_dir = PathBuf::from("/tmp/olaz");

        let _ = std::fs::create_dir_all(&user_data);
        let _ = std::fs::create_dir_all(&temp_dir);

        Self {
            app_version: RwLock::new(app_version.into()),
            identity: ClientIdentity::default(),
            base_dir,
            user_data,
            runtime_root,
            webkit_dir,
            cache_dir,
            temp_dir,
            home_dir,
            downloads_dir,
            documents_dir,
            desktop_dir,
            pictures_dir,
            imei_cache: Mutex::new(None),
            approved_write_paths: RwLock::new(Vec::new()),
            approved_read_paths: RwLock::new(Vec::new()),
        }
    }

    /// Creates a self-contained `ZaloPlatform` rooted inside `root` (ideal for isolated unit tests).
    pub fn with_root(root: impl Into<PathBuf>, app_version: impl Into<String>) -> Self {
        let base_dir = root.into();
        let user_data = base_dir.join("ZaloData");
        let runtime_root = base_dir.join("ZaloRuntime");
        let webkit_dir = base_dir.join("webkit");
        let cache_dir = base_dir.join("cache");
        let temp_dir = base_dir.join("tmp");
        let home_dir = base_dir.join("home");
        let downloads_dir = home_dir.join("Downloads");
        let documents_dir = home_dir.join("Documents");
        let desktop_dir = home_dir.join("Desktop");
        let pictures_dir = home_dir.join("Pictures");

        let _ = std::fs::create_dir_all(&user_data);
        let _ = std::fs::create_dir_all(&temp_dir);
        let _ = std::fs::create_dir_all(&downloads_dir);

        Self {
            app_version: RwLock::new(app_version.into()),
            identity: ClientIdentity::default(),
            base_dir,
            user_data,
            runtime_root,
            webkit_dir,
            cache_dir,
            temp_dir,
            home_dir,
            downloads_dir,
            documents_dir,
            desktop_dir,
            pictures_dir,
            imei_cache: Mutex::new(None),
            approved_write_paths: RwLock::new(Vec::new()),
            approved_read_paths: RwLock::new(Vec::new()),
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Returns the active Zalo PC app version (e.g. `"26.10.20"`).
    pub fn app_version(&self) -> String {
        self.app_version.read().clone()
    }

    /// Dynamically updates the Zalo PC app version (e.g. after first-run DMG installation).
    pub fn set_app_version(&self, ver: impl Into<String>) {
        *self.app_version.write() = ver.into();
    }

    pub fn user_data(&self) -> &Path {
        &self.user_data
    }

    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    pub fn runtime_current(&self) -> PathBuf {
        self.runtime_root.join("current")
    }

    pub fn runtime_pending(&self) -> PathBuf {
        self.runtime_root.join("pending")
    }

    pub fn runtime_work(&self) -> PathBuf {
        self.runtime_root.join("work")
    }

    pub fn webkit_dir(&self) -> &Path {
        &self.webkit_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    pub fn downloads_dir(&self) -> &Path {
        &self.downloads_dir
    }

    /// Grants write (and read) permission to a user-selected Save-As file or Open-Directory path.
    pub fn grant_writable_path(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        if let Some(resolved) = resolve_path_preserving_tail(path) {
            {
                let mut w = self.approved_write_paths.write();
                if !w.contains(&resolved) {
                    w.push(resolved.clone());
                }
            }
            let mut r = self.approved_read_paths.write();
            if !r.contains(&resolved) {
                r.push(resolved);
            }
        }
    }

    /// Grants read permission to a user-selected file or dropped path for `zfile://` media serving.
    pub fn grant_readable_path(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        if let Some(resolved) = resolve_path_preserving_tail(path) {
            let mut r = self.approved_read_paths.write();
            if !r.contains(&resolved) {
                r.push(resolved);
            }
        }
    }

    /// Returns a snapshot of user-approved readable paths (`showOpenDialog`, `showSaveDialog`, drag-drop).
    pub fn approved_read_paths(&self) -> Vec<PathBuf> {
        self.approved_read_paths.read().clone()
    }

    /// Stable per-install lowercase UUID v4 device identifier persisted in `<base_dir>/olaz-meta.json`.
    pub fn imei(&self) -> String {
        let mut guard = self.imei_cache.lock();
        if let Some(cached) = guard.as_ref() {
            return cached.clone();
        }

        let meta_path = self.base_dir.join("olaz-meta.json");
        let mut meta_obj: Map<String, Value> = std::fs::read(&meta_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();

        if let Some(existing) = meta_obj
            .get("imei")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let imei = existing.to_string();
            *guard = Some(imei.clone());
            return imei;
        }

        let fresh = Uuid::new_v4().to_string().to_lowercase();
        meta_obj.insert("imei".to_string(), Value::String(fresh.clone()));
        let _ = write_json_atomic(&meta_path, &Value::Object(meta_obj));
        *guard = Some(fresh.clone());
        fresh
    }

    /// Resolves Electron `app.getPath(name)` directory names.
    pub fn electron_path(&self, name: &str) -> PathBuf {
        match name {
            "userData" | "appData" => self.user_data.clone(),
            "temp" => self.temp_dir.clone(),
            "home" => self.home_dir.clone(),
            "downloads" => self.downloads_dir.clone(),
            "documents" => self.documents_dir.clone(),
            "desktop" => self.desktop_dir.clone(),
            "pictures" => self.pictures_dir.clone(),
            "logs" => self.user_data.join("logs"),
            other => {
                tracing::debug!(path_name = other, "electron_path fallback -> userData");
                self.user_data.clone()
            }
        }
    }

    /// Query parameters appended to renderer page URLs (`ZaloMainProcess.pageQuery`).
    pub fn page_query(&self, already_login: bool) -> BTreeMap<String, String> {
        let mut q = BTreeMap::new();
        q.insert("type".into(), self.identity.client_type.clone());
        q.insert("client_version".into(), self.identity.client_version.clone());
        q.insert("app_version".into(), self.app_version());
        q.insert("about_version".into(), String::new());
        q.insert("new_install".into(), "0".into());
        q.insert("fail_sqlite3".into(), "true".into());
        q.insert("mode_lang".into(), String::new());
        if already_login {
            q.insert("already_login".into(), "y".into());
        }
        q
    }

    /// Path of an `electron-store` JSON file (`<userData>/<safe_name>.json`), sanitizing `/` and `..` to `_`.
    pub fn store_path(&self, name: &str) -> PathBuf {
        let mut safe = name.replace(['/', '\\', '\0'], "_");
        while safe.contains("..") {
            safe = safe.replace("..", "_");
        }
        if safe.is_empty() {
            safe.push_str("config");
        }
        self.user_data.join(format!("{safe}.json"))
    }

    /// Reads an `electron-store` JSON file, returning an empty object if absent or unreadable.
    pub fn read_store(&self, name: &str) -> Map<String, Value> {
        let path = self.store_path(name);
        std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|val| match val {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Atomically writes a pretty-printed, deterministically key-sorted `electron-store` JSON file.
    pub fn write_store(&self, name: &str, obj: &Map<String, Value>) -> Result<()> {
        let path = self.store_path(name);
        write_json_atomic(&path, &Value::Object(obj.clone()))
    }

    /// Forces every account in `database-config.json` onto IndexedDB (`*adp-st` -> `0`),
    /// removes any `*upgradeRequired*` keys that would trigger a SQLite migration, and
    /// repairs any stale `"0.1.0"` `sh_app_ver` persisted during an earlier first-run boot.
    pub fn normalize_database_adapter_state(&self) {
        let mut store = self.read_store("database-config");
        if normalize_adapter_map(&mut store) {
            tracing::info!("database-config: reset adapter state to IndexedDB");
            if let Err(err) = self.write_store("database-config", &store) {
                tracing::warn!(%err, "failed to persist normalized database-config.json");
            }
        }

        let ver = self.app_version();
        if !ver.is_empty() && ver != "0.1.0" {
            for store_name in ["config", "default"] {
                let path = self.store_path(store_name);
                if !path.exists() {
                    continue;
                }
                let mut map = self.read_store(store_name);
                let mut fixed = false;
                for key in ["sh_app_ver", "appVersion"] {
                    if map.get(key).and_then(Value::as_str) == Some("0.1.0") {
                        map.insert(key.to_string(), Value::String(ver.clone()));
                        fixed = true;
                    }
                }
                if fixed {
                    let _ = self.write_store(store_name, &map);
                }
            }
        }
    }

    /// Returns `true` if `path` resolves inside one of the renderer-writable directories:
    /// `userData`, `temp`, or `downloads`.
    ///
    /// Resolves symlinks component-by-component on the longest existing prefix (including
    /// dangling symlinks and `symlink/..` traversals) before appending non-existent tail
    /// components, ensuring a symlink inside an allowed folder can never escape it.
    pub fn is_writable(&self, path: &Path) -> bool {
        if path.as_os_str().is_empty() {
            return false;
        }
        let Some(resolved) = resolve_path_preserving_tail(path) else {
            return false;
        };
        let zalo_received = self.documents_dir.join("Zalo Received Files");
        let allowed = [
            &self.user_data,
            &self.temp_dir,
            &self.downloads_dir,
            &self.cache_dir,
            &zalo_received,
        ];
        if allowed.iter().any(|root| {
            resolve_path_preserving_tail(root)
                .map(|canon_root| resolved.starts_with(&canon_root))
                .unwrap_or(false)
        }) {
            return true;
        }
        self.approved_write_paths
            .read()
            .iter()
            .any(|approved| resolved.starts_with(approved))
    }
}

impl Default for ZaloPlatform {
    fn default() -> Self {
        Self::new("26.10.20")
    }
}

fn normalize_adapter_map(map: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if key.to_ascii_lowercase().contains("upgraderequired") {
            map.remove(&key);
            changed = true;
            continue;
        }
        if key.ends_with("adp-st") {
            if let Some(val) = map.get_mut(&key) {
                match val {
                    Value::String(s) => {
                        if let Ok(Value::Object(mut states)) = serde_json::from_str::<Value>(s) {
                            if states.values().any(|v| v.as_i64() != Some(0)) {
                                for v in states.values_mut() {
                                    *v = json!(0);
                                }
                                if let Ok(encoded) = serde_json::to_string(&sort_value(&Value::Object(states))) {
                                    *s = encoded;
                                    changed = true;
                                }
                            }
                        } else if s.trim() != "0" {
                            *val = json!(0);
                            changed = true;
                        }
                    }
                    Value::Object(states) => {
                        if states.values().any(|v| v.as_i64() != Some(0)) {
                            for v in states.values_mut() {
                                *v = json!(0);
                            }
                            changed = true;
                        }
                    }
                    Value::Number(n) if n.as_i64() != Some(0) => {
                        *val = json!(0);
                        changed = true;
                    }
                    _ => {}
                }
            }
            continue;
        }
        if let Some(Value::Object(nested)) = map.get_mut(&key) {
            if normalize_adapter_map(nested) {
                changed = true;
            }
        }
    }
    changed
}

/// Resolves symlinks along all existing components of `path` (using `symlink_metadata` +
/// `read_link` so dangling symlinks pointing outside an allowed root are also caught!)
/// and appends any non-existent tail components lexically.
fn resolve_path_preserving_tail(path: &Path) -> Option<PathBuf> {
    let initial = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    resolve_os_components(&initial)
}

fn resolve_os_components(initial: &Path) -> Option<PathBuf> {
    use std::ffi::OsString;

    #[derive(Debug)]
    enum Step {
        Root,
        Cur,
        Parent,
        Normal(OsString),
    }

    fn push_front_path(queue: &mut VecDeque<Step>, p: &Path) {
        let steps: Vec<Step> = p
            .components()
            .map(|c| match c {
                Component::Prefix(_) | Component::RootDir => Step::Root,
                Component::CurDir => Step::Cur,
                Component::ParentDir => Step::Parent,
                Component::Normal(s) => Step::Normal(s.to_os_string()),
            })
            .collect();
        for step in steps.into_iter().rev() {
            queue.push_front(step);
        }
    }

    let mut queue = VecDeque::new();
    push_front_path(&mut queue, initial);

    let mut acc = PathBuf::from("/");
    let mut symlink_hops = 0usize;
    let mut in_missing_tail = false;
    let mut missing_depth = 0usize;

    while let Some(step) = queue.pop_front() {
        match step {
            Step::Root => {
                acc = PathBuf::from("/");
                in_missing_tail = false;
                missing_depth = 0;
            }
            Step::Cur => {}
            Step::Parent => {
                if in_missing_tail {
                    if missing_depth > 0 {
                        acc.pop();
                        missing_depth -= 1;
                        if missing_depth == 0 {
                            in_missing_tail = false;
                        }
                    } else {
                        acc.pop();
                    }
                } else {
                    acc.pop();
                }
            }
            Step::Normal(seg) => {
                if in_missing_tail {
                    acc.push(seg);
                    missing_depth += 1;
                    continue;
                }

                let candidate = acc.join(&seg);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        symlink_hops += 1;
                        if symlink_hops > MAX_SYMLINK_HOPS {
                            return None;
                        }
                        let target = std::fs::read_link(&candidate).ok()?;
                        if target.is_absolute() {
                            acc = PathBuf::from("/");
                        }
                        push_front_path(&mut queue, &target);
                    }
                    Ok(_) => {
                        acc = candidate;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        acc = candidate;
                        in_missing_tail = true;
                        missing_depth = 1;
                    }
                    Err(_) => return None,
                }
            }
        }
    }

    Some(acc)
}

/// Recursively rebuilds a `serde_json::Value` with `BTreeMap`-ordered keys so output is
/// guaranteed sorted even if `serde_json/preserve_order` is active in the dependency graph.
fn sort_value(value: &Value) -> BTreeValue<'_> {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&str, BTreeValue<'_>> =
                map.iter().map(|(k, v)| (k.as_str(), sort_value(v))).collect();
            BTreeValue::Object(sorted)
        }
        Value::Array(arr) => BTreeValue::Array(arr.iter().map(sort_value).collect()),
        other => BTreeValue::Leaf(other),
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum BTreeValue<'a> {
    Object(BTreeMap<&'a str, BTreeValue<'a>>),
    Array(Vec<BTreeValue<'a>>),
    Leaf(&'a Value),
}

pub(crate) fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .context("target store path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    let mut bytes = serde_json::to_vec_pretty(&sort_value(value))
        .context("failed to serialize JSON store")?;
    bytes.push(b'\n');

    let mut tmp = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temp file in {}", parent.display()))?;
    tmp.write_all(&bytes)
        .context("failed to write temp JSON store")?;
    tmp.flush().context("failed to flush temp JSON store")?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("failed to atomically persist {}", path.display()))?;
    Ok(())
}
