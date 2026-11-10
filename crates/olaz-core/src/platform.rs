//! Host-side platform paths, device identity (`imei`), `electron-store` persistence,
//! database adapter normalization, and symlink-aware write confinement.
//!
//! Port of `Sources/Olaz/ZaloRuntime/ZaloPlatform.swift` and
//! `ZaloMainProcess.normalizeDatabaseAdapterState` (`ZaloMainProcess.swift:177-200`).

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use directories::{BaseDirs, UserDirs};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tempfile::NamedTempFile;
use url::Url;
use uuid::Uuid;

/// Maximum symlink hops resolved before rejecting a path as a symlink loop (`ELOOP`).
const MAX_SYMLINK_HOPS: usize = 40;

/// Client identity constants passed by the official macOS Zalo main process to renderer pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentity {
    /// `type` query parameter of the official macOS client (`"23"`).
    pub client_type: String,
    /// `client_version` query parameter of the official macOS client (`"695"`).
    pub client_version: String,
}

impl Default for ClientIdentity {
    fn default() -> Self {
        Self {
            client_type: "23".to_string(),
            client_version: "695".to_string(),
        }
    }
}

impl ClientIdentity {
    /// Builds the official desktop User-Agent string (`"ZaloPC-darwin-23v695"`).
    ///
    /// The renderer derives the device ID from `md5(navigator.userAgent)`, so this exact
    /// format must be preserved on Linux as well.
    pub fn user_agent(&self) -> String {
        format!("ZaloPC-darwin-{}v{}", self.client_type, self.client_version)
    }
}

/// Resolves `$XDG_CONFIG_HOME/autostart` per the XDG Base Directory Specification,
/// falling back to `<home_dir>/.config/autostart` when `XDG_CONFIG_HOME` is unset, empty, or relative.
pub fn resolve_xdg_autostart_dir(xdg_config_home: Option<&OsStr>, home_dir: &Path) -> PathBuf {
    let config_root = xdg_config_home
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home_dir.join(".config"));
    config_root.join("autostart")
}

/// Sanitizes an account profile identifier to `[A-Za-z0-9_-]+`.
///
/// Empty strings and `"default"` (case-insensitive) normalize to `"default"`.
/// Non-empty strings containing zero valid `[A-Za-z0-9_-]` characters return an error.
pub fn sanitize_profile_slug(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        return Ok("default".to_string());
    }
    let slug: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if slug.is_empty() {
        bail!(
            "invalid profile name `{raw}`: must contain at least one ASCII letter, digit, '-' or '_'"
        );
    }
    if slug.eq_ignore_ascii_case("default") {
        return Ok("default".to_string());
    }
    Ok(slug)
}

/// Validates an optional upstream proxy URL (`socks5://`, `socks5h://`, `http://`, or `https://`).
pub fn validate_proxy_url(raw: &str) -> Result<Option<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed =
        Url::parse(trimmed).with_context(|| format!("invalid proxy URL `{trimmed}`"))?;
    match parsed.scheme() {
        "socks5" | "socks5h" | "http" | "https" => {}
        other => bail!(
            "unsupported proxy scheme `{other}` in `{trimmed}` (expected socks5, socks5h, http, or https)"
        ),
    }
    if parsed.host_str().map_or(true, str::is_empty) {
        bail!("proxy URL `{trimmed}` is missing a host");
    }
    reqwest::Proxy::all(trimmed)
        .with_context(|| format!("reqwest rejected proxy URL `{trimmed}`"))?;
    Ok(Some(trimmed.to_string()))
}

/// Lists available profile slugs under `base_data_dir` (`"default"` followed by sorted `<base_data_dir>/profiles/<slug>` directories).
pub fn list_profiles(base_data_dir: &Path) -> Vec<String> {
    let mut out = vec!["default".to_string()];
    let profiles_dir = base_data_dir.join("profiles");
    let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
        return out;
    };
    let mut extra: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            !name.is_empty()
                && !name.eq_ignore_ascii_case("default")
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .collect();
    extra.sort();
    extra.dedup();
    out.extend(extra);
    out
}

/// Host directories, device identity, JSON stores, and filesystem write sandbox.
#[derive(Debug)]
pub struct ZaloPlatform {
    app_version: RwLock<String>,
    pub identity: ClientIdentity,
    profile_slug: String,
    proxy_url: Option<String>,
    base_dir: PathBuf,
    config_dir: PathBuf,
    user_data: PathBuf,
    runtime_root: PathBuf,
    webkit_dir: PathBuf,
    cache_dir: PathBuf,
    temp_dir: PathBuf,
    home_dir: PathBuf,
    autostart_dir: PathBuf,
    isolated_root: bool,
    downloads_dir: PathBuf,
    documents_dir: PathBuf,
    desktop_dir: PathBuf,
    pictures_dir: PathBuf,
    videos_dir: PathBuf,
    music_dir: PathBuf,
    imei_cache: Mutex<Option<String>>,
    approved_write_paths: RwLock<Vec<PathBuf>>,
    approved_read_paths: RwLock<Vec<PathBuf>>,
}

impl ZaloPlatform {
    /// Creates a `ZaloPlatform` using standard Linux XDG directories:
    /// - `base_dir`: `~/.local/share/olaz`
    /// - `config_dir`: `~/.config/olaz`
    /// - `user_data`: `~/.local/share/olaz/ZaloData`
    /// - `runtime_root`: `~/.local/share/olaz/ZaloRuntime` (`current`, `pending`, `work`)
    /// - `webkit_dir`: `~/.local/share/olaz/webkit`
    /// - `cache_dir`: `~/.cache/olaz`
    /// - `temp_dir`: `/tmp/olaz`
    /// - `autostart_dir`: `$XDG_CONFIG_HOME/autostart` (or `~/.config/autostart`)
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
        let config_root = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| base_dirs.as_ref().map(|b| b.config_dir().to_path_buf()))
            .unwrap_or_else(|| home_dir.join(".config"));
        let cache_root = base_dirs
            .as_ref()
            .map(|b| b.cache_dir().to_path_buf())
            .unwrap_or_else(|| home_dir.join(".cache"));
        let autostart_dir = resolve_xdg_autostart_dir(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            &home_dir,
        );

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
        let videos_dir = user_dirs
            .as_ref()
            .and_then(|u| u.video_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| home_dir.join("Videos"));
        let music_dir = user_dirs
            .as_ref()
            .and_then(|u| u.audio_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| home_dir.join("Music"));

        let base_dir = data_root.join("olaz");
        let config_dir = config_root.join("olaz");
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
            profile_slug: "default".to_string(),
            proxy_url: None,
            base_dir,
            config_dir,
            user_data,
            runtime_root,
            webkit_dir,
            cache_dir,
            temp_dir,
            home_dir,
            autostart_dir,
            isolated_root: false,
            downloads_dir,
            documents_dir,
            desktop_dir,
            pictures_dir,
            videos_dir,
            music_dir,
            imei_cache: Mutex::new(None),
            approved_write_paths: RwLock::new(Vec::new()),
            approved_read_paths: RwLock::new(Vec::new()),
        }
    }

    /// Creates a `ZaloPlatform` configured from `OLAZ_PROFILE`, `OLAZ_DATA_DIR`, and `OLAZ_PROXY`.
    pub fn from_env() -> Result<Self> {
        let profile = std::env::var("OLAZ_PROFILE").ok();
        let data_dir_override = std::env::var_os("OLAZ_DATA_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let proxy_url = std::env::var("OLAZ_PROXY")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::from_options(profile.as_deref(), data_dir_override, proxy_url)
    }

    /// Creates a `ZaloPlatform` with optional multi-account `profile` (`[A-Za-z0-9_-]+`),
    /// explicit `data_dir_override` (`OLAZ_DATA_DIR`), and upstream `proxy_url` (`OLAZ_PROXY`).
    ///
    /// - `"default"` or empty `profile` uses the root directories (`~/.local/share/olaz`,
    ///   `~/.config/olaz`, `~/.cache/olaz` — or `<data_dir_override>` when overridden) so
    ///   existing single-account installations remain 100% untouched.
    /// - Any non-default `<slug>` isolates data, config, and cache under
    ///   `<base_data_dir>/profiles/<slug>`, `<base_config_dir>/profiles/<slug>`, and
    ///   `<base_cache_dir>/profiles/<slug>`.
    pub fn from_options(
        profile: Option<&str>,
        data_dir_override: Option<PathBuf>,
        proxy_url: Option<String>,
    ) -> Result<Self> {
        let profile_slug = match profile {
            Some(p) => sanitize_profile_slug(p)?,
            None => "default".to_string(),
        };
        let validated_proxy = match proxy_url {
            Some(ref u) => validate_proxy_url(u)?,
            None => None,
        };

        let base_dirs = BaseDirs::new();
        let default_home_dir = base_dirs
            .as_ref()
            .map(|b| b.home_dir().to_path_buf())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/tmp"));

        if let Some(raw_override) = data_dir_override {
            if raw_override.as_os_str().is_empty() {
                bail!("data_dir_override must not be empty");
            }
            let base_data_dir = if raw_override.is_absolute() {
                raw_override
            } else {
                std::env::current_dir()
                    .context("failed to resolve relative OLAZ_DATA_DIR against current_dir")?
                    .join(raw_override)
            };
            let base_config_dir = base_data_dir.join("config");
            let base_cache_dir = base_data_dir.join("cache");

            let (base_dir, config_dir, cache_dir, temp_dir) = if profile_slug == "default" {
                (
                    base_data_dir.clone(),
                    base_config_dir,
                    base_cache_dir,
                    base_data_dir.join("tmp"),
                )
            } else {
                (
                    base_data_dir.join("profiles").join(&profile_slug),
                    base_config_dir.join("profiles").join(&profile_slug),
                    base_cache_dir.join("profiles").join(&profile_slug),
                    base_data_dir.join("tmp").join("profiles").join(&profile_slug),
                )
            };

            let user_data = base_dir.join("ZaloData");
            let runtime_root = base_dir.join("ZaloRuntime");
            let webkit_dir = base_dir.join("webkit");
            let home_dir = base_dir.join("home");
            let autostart_dir = config_dir.join("autostart");
            let downloads_dir = home_dir.join("Downloads");
            let documents_dir = home_dir.join("Documents");
            let desktop_dir = home_dir.join("Desktop");
            let pictures_dir = home_dir.join("Pictures");
            let videos_dir = home_dir.join("Videos");
            let music_dir = home_dir.join("Music");

            let _ = std::fs::create_dir_all(&user_data);
            let _ = std::fs::create_dir_all(&config_dir);
            let _ = std::fs::create_dir_all(&cache_dir);
            let _ = std::fs::create_dir_all(&temp_dir);
            let _ = std::fs::create_dir_all(&downloads_dir);

            return Ok(Self {
                app_version: RwLock::new(env!("CARGO_PKG_VERSION").to_string()),
                identity: ClientIdentity::default(),
                profile_slug,
                proxy_url: validated_proxy,
                base_dir,
                config_dir,
                user_data,
                runtime_root,
                webkit_dir,
                cache_dir,
                temp_dir,
                home_dir,
                autostart_dir,
                isolated_root: true,
                downloads_dir,
                documents_dir,
                desktop_dir,
                pictures_dir,
                videos_dir,
                music_dir,
                imei_cache: Mutex::new(None),
                approved_write_paths: RwLock::new(Vec::new()),
                approved_read_paths: RwLock::new(Vec::new()),
            });
        }

        let data_root = base_dirs
            .as_ref()
            .map(|b| b.data_local_dir().to_path_buf())
            .unwrap_or_else(|| default_home_dir.join(".local/share"));
        let config_root = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| base_dirs.as_ref().map(|b| b.config_dir().to_path_buf()))
            .unwrap_or_else(|| default_home_dir.join(".config"));
        let cache_root = base_dirs
            .as_ref()
            .map(|b| b.cache_dir().to_path_buf())
            .unwrap_or_else(|| default_home_dir.join(".cache"));
        let autostart_dir = resolve_xdg_autostart_dir(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            &default_home_dir,
        );

        let user_dirs = UserDirs::new();
        let downloads_dir = user_dirs
            .as_ref()
            .and_then(|u| u.download_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| default_home_dir.join("Downloads"));
        let documents_dir = user_dirs
            .as_ref()
            .and_then(|u| u.document_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| default_home_dir.join("Documents"));
        let desktop_dir = user_dirs
            .as_ref()
            .and_then(|u| u.desktop_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| default_home_dir.join("Desktop"));
        let pictures_dir = user_dirs
            .as_ref()
            .and_then(|u| u.picture_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| default_home_dir.join("Pictures"));
        let videos_dir = user_dirs
            .as_ref()
            .and_then(|u| u.video_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| default_home_dir.join("Videos"));
        let music_dir = user_dirs
            .as_ref()
            .and_then(|u| u.audio_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| default_home_dir.join("Music"));

        let base_data_dir = data_root.join("olaz");
        let base_config_dir = config_root.join("olaz");
        let base_cache_dir = cache_root.join("olaz");

        let (base_dir, config_dir, cache_dir, temp_dir) = if profile_slug == "default" {
            (
                base_data_dir,
                base_config_dir,
                base_cache_dir,
                PathBuf::from("/tmp/olaz"),
            )
        } else {
            (
                base_data_dir.join("profiles").join(&profile_slug),
                base_config_dir.join("profiles").join(&profile_slug),
                base_cache_dir.join("profiles").join(&profile_slug),
                PathBuf::from(format!("/tmp/olaz-{profile_slug}")),
            )
        };

        let user_data = base_dir.join("ZaloData");
        let runtime_root = base_dir.join("ZaloRuntime");
        let webkit_dir = base_dir.join("webkit");

        let _ = std::fs::create_dir_all(&user_data);
        let _ = std::fs::create_dir_all(&temp_dir);

        Ok(Self {
            app_version: RwLock::new(env!("CARGO_PKG_VERSION").to_string()),
            identity: ClientIdentity::default(),
            profile_slug,
            proxy_url: validated_proxy,
            base_dir,
            config_dir,
            user_data,
            runtime_root,
            webkit_dir,
            cache_dir,
            temp_dir,
            home_dir: default_home_dir,
            autostart_dir,
            isolated_root: false,
            downloads_dir,
            documents_dir,
            desktop_dir,
            pictures_dir,
            videos_dir,
            music_dir,
            imei_cache: Mutex::new(None),
            approved_write_paths: RwLock::new(Vec::new()),
            approved_read_paths: RwLock::new(Vec::new()),
        })
    }

    /// Creates a self-contained `ZaloPlatform` rooted inside `root` (ideal for isolated unit tests).
    pub fn with_root(root: impl Into<PathBuf>, app_version: impl Into<String>) -> Self {
        let base_dir = root.into();
        let config_dir = base_dir.join("config");
        let user_data = base_dir.join("ZaloData");
        let runtime_root = base_dir.join("ZaloRuntime");
        let webkit_dir = base_dir.join("webkit");
        let cache_dir = base_dir.join("cache");
        let temp_dir = base_dir.join("tmp");
        let home_dir = base_dir.join("home");
        let autostart_dir = base_dir.join("config/autostart");
        let downloads_dir = home_dir.join("Downloads");
        let documents_dir = home_dir.join("Documents");
        let desktop_dir = home_dir.join("Desktop");
        let pictures_dir = home_dir.join("Pictures");
        let videos_dir = home_dir.join("Videos");
        let music_dir = home_dir.join("Music");

        let _ = std::fs::create_dir_all(&user_data);
        let _ = std::fs::create_dir_all(&temp_dir);
        let _ = std::fs::create_dir_all(&downloads_dir);

        Self {
            app_version: RwLock::new(app_version.into()),
            identity: ClientIdentity::default(),
            profile_slug: "default".to_string(),
            proxy_url: None,
            base_dir,
            config_dir,
            user_data,
            runtime_root,
            webkit_dir,
            cache_dir,
            temp_dir,
            home_dir,
            autostart_dir,
            isolated_root: true,
            downloads_dir,
            documents_dir,
            desktop_dir,
            pictures_dir,
            videos_dir,
            music_dir,
            imei_cache: Mutex::new(None),
            approved_write_paths: RwLock::new(Vec::new()),
            approved_read_paths: RwLock::new(Vec::new()),
        }
    }

    /// Active sanitized profile slug (`"default"` for primary account, or `[A-Za-z0-9_-]+`).
    pub fn profile_slug(&self) -> &str {
        &self.profile_slug
    }

    /// Optional validated upstream SOCKS5/HTTP(S) proxy URL (`OLAZ_PROXY`).
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    /// Profile-scoped configuration directory (`~/.config/olaz` or `<base_config_dir>/profiles/<slug>`).
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Lists available profile slugs under `base_data_dir` (`"default"` plus sorted `<base_data_dir>/profiles/<slug>`).
    pub fn list_profiles(base_data_dir: &Path) -> Vec<String> {
        list_profiles(base_data_dir)
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Directory where XDG autostart `.desktop` entries live (`$XDG_CONFIG_HOME/autostart` in
    /// production, or `<root>/config/autostart` inside [`Self::with_root`] test sandboxes).
    pub fn autostart_dir(&self) -> &Path {
        &self.autostart_dir
    }

    /// Returns `true` when constructed via [`Self::with_root`] (isolated sandbox / test root).
    pub fn is_isolated_root(&self) -> bool {
        self.isolated_root
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

    /// Returns a snapshot of user-approved writable paths (`showSaveDialog`, `showOpenDialog` directory).
    pub fn approved_write_paths(&self) -> Vec<PathBuf> {
        self.approved_write_paths.read().clone()
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
            "videos" => self.videos_dir.clone(),
            "music" => self.music_dir.clone(),
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
    /// removes any `*upgradeRequired*` or Zalo `26.11.10` SQLite migration latch keys
    /// (`db4-ftu-required`, `sf-mg-gate`, `ismigtin`, `sqlite_m_d`), and
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

    /// Returns `true` if `path` resolves inside one of the confined app/media directories or
    /// a user-approved read/write path.
    pub fn is_readable(&self, path: &Path) -> bool {
        if path.as_os_str().is_empty() {
            return false;
        }
        let Some(resolved) = resolve_path_preserving_tail(path) else {
            return false;
        };
        let allowed = [
            &self.user_data,
            &self.temp_dir,
            &self.downloads_dir,
            &self.cache_dir,
            &self.documents_dir,
            &self.desktop_dir,
            &self.pictures_dir,
            &self.videos_dir,
            &self.music_dir,
        ];
        if allowed.iter().any(|root| {
            resolve_path_preserving_tail(root)
                .map(|canon_root| resolved.starts_with(&canon_root))
                .unwrap_or(false)
        }) {
            return true;
        }
        if self
            .approved_read_paths
            .read()
            .iter()
            .any(|approved| resolved.starts_with(approved))
        {
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
        Self::new("26.11.10")
    }
}

fn normalize_adapter_map(map: &mut Map<String, Value>) -> bool {
    const MIGRATION_FLAG_SUFFIXES: &[&str] = &[
        "db4-ftu-required",
        "sf-mg-gate",
        "ismigtin",
        "sqlite_m_d",
    ];
    let mut changed = false;
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        let lower = key.to_ascii_lowercase();
        if lower.contains("upgraderequired")
            || MIGRATION_FLAG_SUFFIXES
                .iter()
                .any(|s| lower == *s || lower.ends_with(s))
        {
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

#[cfg(test)]
mod ctdozlite_platform_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sanitize_profile_slug_handles_default_and_unsafe_chars() {
        assert_eq!(sanitize_profile_slug("").unwrap(), "default");
        assert_eq!(sanitize_profile_slug("   ").unwrap(), "default");
        assert_eq!(sanitize_profile_slug("default").unwrap(), "default");
        assert_eq!(sanitize_profile_slug("DEFAULT").unwrap(), "default");
        assert_eq!(
            sanitize_profile_slug("work-account_02").unwrap(),
            "work-account_02"
        );
        assert_eq!(
            sanitize_profile_slug("../evil/path!@#").unwrap(),
            "evilpath"
        );
        assert!(sanitize_profile_slug("!!!").is_err());
    }

    #[test]
    fn validate_proxy_url_accepts_socks5_and_http_and_rejects_bad_schemes() {
        assert_eq!(validate_proxy_url("").unwrap(), None);
        assert_eq!(validate_proxy_url("   ").unwrap(), None);
        assert_eq!(
            validate_proxy_url("socks5://127.0.0.1:1080")
                .unwrap()
                .as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
        assert_eq!(
            validate_proxy_url("socks5h://user:pass@127.0.0.1:1080")
                .unwrap()
                .as_deref(),
            Some("socks5h://user:pass@127.0.0.1:1080")
        );
        assert_eq!(
            validate_proxy_url("http://127.0.0.1:8080")
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            validate_proxy_url("https://proxy.example.com:8443")
                .unwrap()
                .as_deref(),
            Some("https://proxy.example.com:8443")
        );
        assert!(validate_proxy_url("ftp://127.0.0.1:21").is_err());
        assert!(validate_proxy_url("not a url").is_err());
    }

    #[test]
    fn from_options_isolates_profiles_and_lists_sorted_slugs() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();

        let default_plat = ZaloPlatform::from_options(
            Some("default"),
            Some(root.clone()),
            Some("socks5://127.0.0.1:1080".to_string()),
        )
        .expect("default platform");
        assert_eq!(default_plat.profile_slug(), "default");
        assert_eq!(
            default_plat.proxy_url(),
            Some("socks5://127.0.0.1:1080")
        );
        assert_eq!(default_plat.base_dir(), root.as_path());
        assert_eq!(default_plat.user_data(), root.join("ZaloData"));
        assert_eq!(default_plat.config_dir(), root.join("config"));
        assert_eq!(default_plat.cache_dir(), root.join("cache"));

        let work_plat = ZaloPlatform::from_options(Some("work"), Some(root.clone()), None)
            .expect("work platform");
        assert_eq!(work_plat.profile_slug(), "work");
        assert_eq!(work_plat.proxy_url(), None);
        assert_eq!(work_plat.base_dir(), root.join("profiles/work"));
        assert_eq!(work_plat.user_data(), root.join("profiles/work/ZaloData"));
        assert_eq!(work_plat.config_dir(), root.join("config/profiles/work"));
        assert_eq!(work_plat.cache_dir(), root.join("cache/profiles/work"));
        assert_ne!(default_plat.imei(), work_plat.imei());

        let _personal_plat =
            ZaloPlatform::from_options(Some("personal"), Some(root.clone()), None)
                .expect("personal platform");

        assert_eq!(
            ZaloPlatform::list_profiles(&root),
            vec![
                "default".to_string(),
                "personal".to_string(),
                "work".to_string()
            ]
        );
    }
}
