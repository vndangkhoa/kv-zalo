//! Release update checker (GitHub Releases primary + Forgejo fallback), semantic version
//! comparison against `env!("CARGO_PKG_VERSION")`, `SHA256SUMS.txt` parser, `sha2::Sha256`
//! artifact verification, and transactional portable binary installation.
//!
//! Adapted from `ctdozlite/src/updater.rs`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::privacy::write_atomic_private;

/// Primary GitHub Releases `/releases/latest` endpoint.
pub const GITHUB_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/vndangkhoa/kv-zalo/releases/latest";

/// Fallback self-hosted Forgejo Releases `/releases/latest` endpoint.
pub const FORGEJO_LATEST_RELEASE_URL: &str =
    "https://git.khoavo.myds.me/api/v1/repos/vndangkhoa/kv-zalo/releases/latest";

/// Canonical upstream repository slug.
pub const DEFAULT_REPO_SLUG: &str = "vndangkhoa/kv-zalo";

/// Current compiled Olaz crate version (`env!("CARGO_PKG_VERSION")`).
pub const CURRENT_OLAZ_VERSION: &str = env!("CARGO_PKG_VERSION");

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

/// Upstream release forge that answered the update check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseSource {
    GitHub,
    Forgejo,
    Custom,
}

/// Resource limits for downloading a single release artifact.
#[derive(Debug, Clone, Copy)]
pub struct DownloadLimits {
    /// Smallest accepted artifact size in bytes.
    pub min_bytes: u64,
    /// Maximum buffered artifact size in bytes.
    pub max_bytes: u64,
    /// End-to-end deadline covering response headers and body transfer.
    pub timeout: Duration,
}

impl Default for DownloadLimits {
    fn default() -> Self {
        Self {
            min_bytes: 512 * 1024,
            max_bytes: 512 * 1024 * 1024,
            timeout: Duration::from_secs(240),
        }
    }
}

/// Release download parameters supplied to [`download_release`].
pub struct DownloadRequest<'a> {
    /// HTTPS asset download URL.
    pub url: &'a str,
    /// Single ordinary filename (never a path).
    pub asset_name: &'a str,
    /// Optional expected 64-char hex SHA-256 digest.
    pub expected_sha256: Option<&'a str>,
    /// Optional exact byte size advertised by the release metadata.
    pub expected_size: Option<u64>,
    /// Resource bounds for this transfer.
    pub limits: DownloadLimits,
}

/// Explicit local installation destination selected by the host.
pub enum InstallTarget {
    /// Atomically replace a portable executable and preserve its permissions.
    Portable(PathBuf),
    /// Save an installer/package archive for the user or package manager.
    Installer(PathBuf),
}

/// Parsed release metadata from GitHub Releases or Forgejo Releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseUpdateInfo {
    pub source: ReleaseSource,
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_title: String,
    pub release_notes: String,
    pub html_url: String,
    pub download_url: Option<String>,
    pub asset_name: Option<String>,
    pub asset_size: u64,
    pub expected_sha256: Option<String>,
    pub published_at: String,
    pub sha256sums_url: Option<String>,
}

/// Rejects empty strings, `.`, `..`, control characters, and path separators.
pub fn validate_asset_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':', '\0'])
        || name.chars().any(char::is_control)
    {
        return Err("invalid release asset filename".into());
    }
    Ok(())
}

/// Computes the lowercase 64-character SHA-256 hex digest of `bytes`.
pub fn compute_sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// Verifies `bytes` against a 64-character hex SHA-256 digest in constant time.
pub fn verify_sha256_bytes(bytes: &[u8], expected_hex: &str) -> Result<()> {
    let clean = expected_hex.trim().to_ascii_lowercase();
    if clean.len() != 64 || !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid expected SHA-256 hex digest `{expected_hex}`");
    }
    let actual = compute_sha256_hex(bytes);
    if actual.as_bytes().ct_eq(clean.as_bytes()).unwrap_u8() != 1 {
        bail!("SHA-256 mismatch: expected {clean}, got {actual}");
    }
    Ok(())
}

/// Compares semantic versions (`"v0.2.0"` > `"0.1.0"`, `"0.1.0"` > `"0.1.0-rc.1"`).
pub fn is_newer_semver(remote: &str, current: &str) -> bool {
    fn parse_ver(s: &str) -> (u64, u64, u64, bool, &str) {
        let trimmed = s.trim().trim_start_matches(['v', 'V']);
        let without_build = trimmed.split('+').next().unwrap_or(trimmed);
        let (core, prerelease) = match without_build.split_once('-') {
            Some((c, pre)) => (c, pre),
            None => (without_build, ""),
        };
        let is_stable = prerelease.is_empty();
        let mut parts = core.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            is_stable,
            prerelease,
        )
    }
    parse_ver(remote) > parse_ver(current)
}

/// Extracts a 64-character lowercase SHA-256 hex digest for `asset_name` from `SHA256SUMS.txt`.
pub fn parse_sha256sums_for_asset(sha256sums_text: &str, asset_name: &str) -> Option<String> {
    let target = asset_name.trim();
    if target.is_empty() {
        return None;
    }
    for line in sha256sums_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // BSD format: `SHA256 (filename) = <64hex>`
        if let Some(rest) = trimmed.strip_prefix("SHA256 (") {
            if let Some((fname, eq_hash)) = rest.split_once(") =") {
                let hash = eq_hash.trim();
                let clean_fname = fname.trim().trim_start_matches("./");
                if clean_fname == target
                    && hash.len() == 64
                    && hash.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Some(hash.to_ascii_lowercase());
                }
            }
        }
        // GNU coreutils format: `<64hex>  filename` or `<64hex> *./filename`
        let mut parts = trimmed.split_whitespace();
        let Some(hash) = parts.next() else { continue };
        let Some(fname) = parts.next() else { continue };
        let clean_fname = fname.trim_start_matches('*').trim_start_matches("./");
        if clean_fname == target
            && hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Scores a release asset filename for `(target_os, target_arch)`.
/// Returns `<= 0` when incompatible or metadata-only.
pub fn select_platform_asset_score(name: &str, target_os: &str, target_arch: &str) -> i32 {
    let n_low = name.to_ascii_lowercase();
    if n_low.is_empty()
        || n_low.ends_with(".txt")
        || n_low.ends_with(".json")
        || n_low.ends_with(".sig")
        || n_low.ends_with(".asc")
        || n_low.ends_with(".sha256")
    {
        return -1;
    }

    let mentions_arm = n_low.contains("aarch64") || n_low.contains("arm64");
    let mentions_x86 = n_low.contains("x86_64") || n_low.contains("amd64") || n_low.contains("x64");
    let arch_bonus = match target_arch {
        "aarch64" if mentions_arm => 15,
        "aarch64" if mentions_x86 => -200,
        "x86_64" if mentions_x86 => 15,
        "x86_64" if mentions_arm => -200,
        _ => 0,
    };

    let base = match target_os {
        "windows" => {
            if n_low.ends_with(".exe") && n_low.contains("portable") {
                110
            } else if n_low.ends_with(".exe") && n_low.contains("setup") {
                90
            } else if n_low.ends_with(".exe") {
                80
            } else {
                0
            }
        }
        "macos" => {
            if n_low.ends_with(".dmg") {
                100
            } else {
                0
            }
        }
        _ => {
            if n_low.ends_with(".appimage") {
                100
            } else if n_low.ends_with(".pkg.tar.zst") {
                95
            } else if n_low.ends_with(".deb") && !n_low.contains("legacy") {
                90
            } else if (n_low.ends_with(".tar.gz") || n_low.ends_with(".tar.xz"))
                && !n_low.contains("legacy")
            {
                80
            } else if n_low.ends_with(".rpm") {
                70
            } else {
                0
            }
        }
    };

    if base <= 0 {
        0
    } else {
        base + arch_bonus
    }
}

/// Parses a GitHub Releases or Forgejo `/releases/latest` JSON payload into [`ReleaseUpdateInfo`].
pub fn parse_release_manifest(
    rel: &Value,
    current_ver: &str,
    repo_slug: &str,
    target_os: &str,
    target_arch: &str,
    source: ReleaseSource,
) -> ReleaseUpdateInfo {
    let raw_tag = rel
        .get("tag_name")
        .or_else(|| rel.get("version"))
        .and_then(Value::as_str)
        .unwrap_or(current_ver);
    let latest_ver = raw_tag.trim().trim_start_matches(['v', 'V']).to_string();
    let has_update = is_newer_semver(&latest_ver, current_ver);

    let release_title = rel
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Olaz v{latest_ver}"));

    let release_notes = rel
        .get("body")
        .or_else(|| rel.get("notes"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    let html_url = rel
        .get("html_url")
        .or_else(|| rel.get("release_url"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://github.com/{repo_slug}/releases/tag/v{latest_ver}"));

    let published_at = rel
        .get("published_at")
        .or_else(|| rel.get("created_at"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .split('T')
        .next()
        .unwrap_or("")
        .to_string();

    let mut best_url: Option<String> = None;
    let mut best_name: Option<String> = None;
    let mut best_size: u64 = 0;
    let mut best_sha256: Option<String> = None;
    let mut best_score: i32 = 0;
    let mut sha256sums_url: Option<String> = None;

    if let Some(assets) = rel.get("assets").and_then(Value::as_array) {
        for a in assets {
            let name = a.get("name").and_then(Value::as_str).unwrap_or("");
            let dl = a
                .get("browser_download_url")
                .and_then(Value::as_str)
                .unwrap_or("");
            let sz = a.get("size").and_then(Value::as_u64).unwrap_or(0);
            if (name.eq_ignore_ascii_case("SHA256SUMS.txt")
                || name.eq_ignore_ascii_case("SHA256SUMS"))
                && !dl.is_empty()
            {
                sha256sums_url = Some(dl.to_string());
                continue;
            }
            if name.is_empty() || dl.is_empty() {
                continue;
            }
            let score = select_platform_asset_score(name, target_os, target_arch);
            if score > best_score {
                best_score = score;
                best_url = Some(dl.to_string());
                best_name = Some(name.to_string());
                best_size = sz;
                best_sha256 = a
                    .get("sha256")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        a.get("digest")
                            .and_then(Value::as_str)
                            .and_then(|d| d.strip_prefix("sha256:"))
                    })
                    .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(|h| h.to_ascii_lowercase());
            }
        }
    }

    ReleaseUpdateInfo {
        source,
        current_version: current_ver.to_string(),
        latest_version: latest_ver,
        has_update,
        release_title,
        release_notes,
        html_url,
        download_url: best_url,
        asset_name: best_name,
        asset_size: best_size,
        expected_sha256: best_sha256,
        published_at,
        sha256sums_url,
    }
}

async fn fetch_release_json(client: &reqwest::Client, url: &str) -> Result<Value> {
    let resp = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            format!("olaz/{CURRENT_OLAZ_VERSION}"),
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .with_context(|| format!("HTTP GET failed for {url}"))?
        .error_for_status()
        .with_context(|| format!("non-2xx status from {url}"))?;

    resp.json::<Value>()
        .await
        .with_context(|| format!("invalid release JSON from {url}"))
}

/// Queries `primary_url` (GitHub Releases) and falls back to `fallback_url` (Forgejo Releases)
/// when the primary endpoint fails, resolving `SHA256SUMS.txt` automatically if needed.
pub async fn check_for_updates_from_urls(
    client: &reqwest::Client,
    primary_url: &str,
    fallback_url: &str,
    current_version: &str,
) -> Result<ReleaseUpdateInfo> {
    let (json_val, source) = match fetch_release_json(client, primary_url).await {
        Ok(val) => (val, ReleaseSource::GitHub),
        Err(primary_err) => {
            warn!(%primary_err, "primary GitHub release check failed; falling back to Forgejo");
            let val = fetch_release_json(client, fallback_url)
                .await
                .with_context(|| {
                    format!("both GitHub ({primary_err}) and Forgejo ({fallback_url}) release checks failed")
                })?;
            (val, ReleaseSource::Forgejo)
        }
    };

    let mut info = parse_release_manifest(
        &json_val,
        current_version,
        DEFAULT_REPO_SLUG,
        std::env::consts::OS,
        std::env::consts::ARCH,
        source,
    );

    if info.expected_sha256.is_none() {
        if let (Some(ref sums_url), Some(ref asset_name)) = (&info.sha256sums_url, &info.asset_name)
        {
            if let Ok(resp) = client
                .get(sums_url)
                .header(
                    reqwest::header::USER_AGENT,
                    format!("olaz/{CURRENT_OLAZ_VERSION}"),
                )
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(sums_text) = resp.text().await {
                        info.expected_sha256 = parse_sha256sums_for_asset(&sums_text, asset_name);
                    }
                }
            }
        }
    }

    Ok(info)
}

/// Checks for a newer Olaz release against [`GITHUB_LATEST_RELEASE_URL`] with automatic
/// fallback to [`FORGEJO_LATEST_RELEASE_URL`], comparing against `env!("CARGO_PKG_VERSION")`.
pub async fn check_for_updates(client: &reqwest::Client) -> Result<ReleaseUpdateInfo> {
    check_for_updates_from_urls(
        client,
        GITHUB_LATEST_RELEASE_URL,
        FORGEJO_LATEST_RELEASE_URL,
        CURRENT_OLAZ_VERSION,
    )
    .await
}

/// Downloads and validates a release artifact into memory, enforcing size limits, executable
/// header sanity checks, and SHA-256 integrity (`sha2::Sha256`) before touching disk.
pub async fn download_release(
    client: &reqwest::Client,
    request: &DownloadRequest<'_>,
    mut progress: impl FnMut(u64, u64),
) -> std::result::Result<Vec<u8>, String> {
    validate_asset_name(request.asset_name)?;
    let limits = request.limits;
    if limits.min_bytes > limits.max_bytes || limits.max_bytes > usize::MAX as u64 {
        return Err("invalid update download size limits".into());
    }
    if let Some(hash) = request.expected_sha256 {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid expected SHA-256 hex digest".into());
        }
    }
    if request
        .expected_size
        .is_some_and(|size| size > limits.max_bytes)
    {
        return Err("release artifact exceeds maximum allowed byte size".into());
    }

    let transfer = async {
        let mut response = client
            .get(request.url)
            .header(
                reqwest::header::USER_AGENT,
                format!("olaz/{CURRENT_OLAZ_VERSION}"),
            )
            .timeout(limits.timeout)
            .send()
            .await
            .map_err(|e| format!("release download failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("release server returned error: {e}"))?;

        let length = response.content_length();
        if length.is_some_and(|size| size > limits.max_bytes) {
            return Err("release artifact exceeds maximum allowed byte size".into());
        }
        if let (Some(expected), Some(actual)) = (request.expected_size, length) {
            if expected != actual {
                return Err("release Content-Length does not match published size".into());
            }
        }

        let total = request.expected_size.or(length).unwrap_or(0);
        let mut bytes = Vec::new();
        let mut hasher = Sha256::new();
        let mut last_progress = std::time::Instant::now();

        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("connection interrupted during release download: {e}"))?
        {
            if (chunk.len() as u64) > limits.max_bytes.saturating_sub(bytes.len() as u64) {
                return Err("release artifact exceeded maximum allowed byte size".into());
            }
            hasher.update(&chunk);
            bytes.extend_from_slice(&chunk);
            if last_progress.elapsed() >= Duration::from_millis(80) {
                progress(bytes.len() as u64, total);
                last_progress = std::time::Instant::now();
            }
        }

        let actual = bytes.len() as u64;
        if actual < limits.min_bytes {
            return Err("downloaded release artifact is smaller than minimum size".into());
        }
        if length.is_some_and(|size| size != actual)
            || request.expected_size.is_some_and(|size| size != actual)
        {
            return Err("downloaded release artifact is truncated".into());
        }
        if let Some(expected) = request.expected_sha256 {
            let actual_hex = hex::encode(hasher.finalize());
            let clean_expected = expected.to_ascii_lowercase();
            if actual_hex.as_bytes().ct_eq(clean_expected.as_bytes()).unwrap_u8() != 1 {
                return Err(format!(
                    "SHA-256 verification failed: expected {clean_expected}, got {actual_hex}"
                ));
            }
        }

        let name = request.asset_name.to_ascii_lowercase();
        if name.ends_with(".appimage") && !bytes.starts_with(b"\x7fELF") {
            return Err("AppImage artifact does not carry a valid ELF header".into());
        }
        progress(actual, total.max(actual));
        Ok(bytes)
    };

    tokio::time::timeout(limits.timeout, transfer)
        .await
        .map_err(|_| "release download timed out".to_string())?
}

/// Downloads an artifact from `url`, strictly verifies its `expected_sha256` via `sha2::Sha256`,
/// and optionally writes it atomically to `destination`.
pub async fn download_and_verify_artifact(
    client: &reqwest::Client,
    url: &str,
    asset_name: &str,
    expected_sha256: &str,
    limits: DownloadLimits,
    destination: Option<&Path>,
) -> Result<Vec<u8>> {
    let req = DownloadRequest {
        url,
        asset_name,
        expected_sha256: Some(expected_sha256),
        expected_size: None,
        limits,
    };
    let bytes = download_release(client, &req, |_, _| {})
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    verify_sha256_bytes(&bytes, expected_sha256)?;
    if let Some(dest) = destination {
        write_atomic_private(dest, &bytes)?;
    }
    Ok(bytes)
}

fn staging_file(destination: &Path) -> std::result::Result<(PathBuf, File), String> {
    let parent = destination
        .parent()
        .ok_or("update destination path has no parent directory")?;
    for _ in 0..100 {
        let id = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".olaz-update-{}-{id}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("failed to create update staging file: {error}")),
        }
    }
    Err("failed to allocate unique update staging file".into())
}

fn remove_staging(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Staged transactional portable installation that preserves the existing binary on rollback/drop.
pub struct DeferredPortableInstall {
    prepared: PreparedInstall,
}

impl DeferredPortableInstall {
    pub fn launch_and_finish(
        self,
        launch: impl FnOnce(&Path) -> std::result::Result<(), String>,
    ) -> std::result::Result<(), String> {
        self.prepared.finish(launch)
    }

    pub fn abort(mut self) -> std::result::Result<(), String> {
        self.prepared.rollback()
    }
}

/// Stages validated portable binary bytes next to `destination` without replacing it yet.
pub fn stage_portable_release(
    bytes: &[u8],
    destination: &Path,
) -> std::result::Result<DeferredPortableInstall, String> {
    Ok(DeferredPortableInstall {
        prepared: prepare_install(bytes, destination, true)?,
    })
}

/// Atomically installs validated artifact bytes to `target` and runs `launch`, rolling back on failure.
pub fn install_release(
    bytes: &[u8],
    target: &InstallTarget,
    launch: impl FnOnce(&Path) -> std::result::Result<(), String>,
) -> std::result::Result<(), String> {
    let (destination, portable) = match target {
        InstallTarget::Portable(path) => (path, true),
        InstallTarget::Installer(path) => (path, false),
    };
    prepare_install(bytes, destination, portable)?.finish(launch)
}

struct PreparedInstall {
    destination: PathBuf,
    stage: PathBuf,
    backup: Option<PathBuf>,
    installed: bool,
    committed: bool,
}

impl PreparedInstall {
    fn finish(
        mut self,
        launch: impl FnOnce(&Path) -> std::result::Result<(), String>,
    ) -> std::result::Result<(), String> {
        fs::rename(&self.stage, &self.destination)
            .map_err(|error| format!("failed to replace update target: {error}"))?;
        self.installed = true;
        if let Err(error) = launch(&self.destination) {
            self.rollback().map_err(|rollback| {
                format!("launch failed: {error}; rollback also failed: {rollback}")
            })?;
            return Err(format!("launch failed, rolled back update: {error}"));
        }
        self.committed = true;
        Ok(())
    }

    fn rollback(&mut self) -> std::result::Result<(), String> {
        if self.installed && !self.committed {
            match &self.backup {
                Some(backup) => fs::rename(backup, &self.destination),
                None => fs::remove_file(&self.destination),
            }
            .map_err(|error| format!("failed to restore backup executable: {error}"))?;
            self.installed = false;
        }
        Ok(())
    }
}

impl Drop for PreparedInstall {
    fn drop(&mut self) {
        if self.rollback().is_err() {
            return;
        }
        remove_staging(&self.stage);
        if let Some(backup) = &self.backup {
            remove_staging(backup);
        }
    }
}

fn prepare_install(
    bytes: &[u8],
    destination: &Path,
    portable: bool,
) -> std::result::Result<PreparedInstall, String> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_file() => Some(metadata),
        Ok(_) => return Err("update destination is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !portable => None,
        Err(error) => return Err(format!("failed to stat update destination: {error}")),
    };
    let (stage, mut file) = staging_file(destination)?;
    let prepared = (|| {
        file.write_all(bytes)?;
        if portable {
            if let Some(metadata) = &metadata {
                file.set_permissions(metadata.permissions())?;
            }
        }
        file.sync_all()
    })();
    drop(file);
    if let Err(error) = prepared {
        remove_staging(&stage);
        return Err(format!("failed to write staged update: {error}"));
    }
    let backup = if metadata.is_some() {
        let (path, file) = match staging_file(destination) {
            Ok(value) => value,
            Err(error) => {
                remove_staging(&stage);
                return Err(error);
            }
        };
        drop(file);
        if let Err(error) = fs::copy(destination, &path) {
            remove_staging(&stage);
            remove_staging(&path);
            return Err(format!("failed to back up existing executable: {error}"));
        }
        Some(path)
    } else {
        None
    };
    Ok(PreparedInstall {
        destination: destination.to_path_buf(),
        stage,
        backup,
        installed: false,
        committed: false,
    })
}
