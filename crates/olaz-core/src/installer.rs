//! Userspace Zalo PC macOS DMG fetcher, streaming SHA-256 verifier, DoH fallback,
//! and `7zz`/`7z` runtime extractor.
//!
//! Port of `Sources/Olaz/ZaloRuntime/ZaloRuntimeInstaller.swift` adapted for Linux
//! (no `hdiutil`, no Apple `SecStaticCode`, no Mach-O `ZaloCall` helper requirement).

use std::borrow::Cow;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use futures_util::StreamExt;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use url::Url;
use uuid::Uuid;

use crate::asar::AsarArchive;
use crate::platform::ZaloPlatform;

/// macOS Safari User-Agent so `zalo.me/download/zalo-pc` redirects to the macOS `.dmg`.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

/// Official redirect endpoint returning the latest Zalo PC for Mac `.dmg`.
pub const LATEST_DOWNLOAD_URL: &str = "https://zalo.me/download/zalo-pc";

const SHA256_CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB
const PROGRESS_REPORT_STRIDE: u64 = 1024 * 1024; // 1 MiB

/// Descriptor for an official Zalo PC macOS DMG release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZaloRelease {
    pub version: Cow<'static, str>,
    pub url: Cow<'static, str>,
    pub sha256: Option<Cow<'static, str>>,
}

impl ZaloRelease {
    /// Pinned Zalo PC release tested with this Olaz build.
    pub const PINNED: Self = Self {
        version: Cow::Borrowed("26.10.20"),
        url: Cow::Borrowed(
            "https://res-download-pc.zadn.vn/mac/ZaloSetup-universal-26.10.20.dmg",
        ),
        sha256: Some(Cow::Borrowed(
            "a45d30b7f7b060f76b87722ef419b8f41d23a2913cdef084dcb0955dde8d85a0",
        )),
    };

    pub fn new(
        version: impl Into<Cow<'static, str>>,
        url: impl Into<Cow<'static, str>>,
        sha256: Option<impl Into<Cow<'static, str>>>,
    ) -> Self {
        Self {
            version: version.into(),
            url: url.into(),
            sha256: sha256.map(Into::into),
        }
    }

    pub const fn pinned() -> Self {
        Self::PINNED
    }
}

impl Default for ZaloRelease {
    fn default() -> Self {
        Self::PINNED
    }
}

/// Progress steps emitted during [`ZaloRuntimeManager::install`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallProgress {
    Looking,
    Downloading { received: u64, total: Option<u64> },
    Verifying,
    Extracting,
    Installing,
    Complete,
}

/// Manages `<runtime_root>/{current,pending,work}` and installs `Contents/Info.plist`
/// + `Contents/Resources/app.asar` from the official VNG DMG.
#[derive(Debug, Clone)]
pub struct ZaloRuntimeManager {
    root: PathBuf,
    release: ZaloRelease,
    allows_latest: bool,
}

impl ZaloRuntimeManager {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            release: ZaloRelease::PINNED,
            allows_latest: true,
        }
    }

    pub fn from_platform(platform: &ZaloPlatform) -> Self {
        Self::new(platform.runtime_root())
    }

    pub fn with_release(mut self, release: ZaloRelease) -> Self {
        self.release = release;
        self
    }

    pub fn with_allows_latest(mut self, allows_latest: bool) -> Self {
        self.allows_latest = allows_latest;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn current_dir(&self) -> PathBuf {
        self.root.join("current")
    }

    pub fn pending_dir(&self) -> PathBuf {
        self.root.join("pending")
    }

    pub fn work_dir(&self) -> PathBuf {
        self.root.join("work")
    }

    pub fn release(&self) -> &ZaloRelease {
        &self.release
    }

    /// Returns `true` if `dir` contains both `Contents/Info.plist` and `Contents/Resources/app.asar`.
    ///
    /// Unlike macOS, Linux does not check for a Mach-O `ZaloCall` helper binary.
    pub fn is_complete(dir: &Path) -> bool {
        dir.join("Contents/Info.plist").is_file()
            && dir.join("Contents/Resources/app.asar").is_file()
    }

    /// Reads `CFBundleShortVersionString` from `<dir>/Contents/Info.plist`.
    pub fn installed_version(dir: &Path) -> Option<String> {
        let plist_path = dir.join("Contents/Info.plist");
        let val = plist::Value::from_file(&plist_path).ok()?;
        val.as_dictionary()?
            .get("CFBundleShortVersionString")?
            .as_string()
            .map(str::to_owned)
    }

    /// Compares dotted numeric versions (`"26.10.20"` > `"26.9.10"`).
    pub fn is_newer(version: &str, than: &str) -> bool {
        let a: Vec<u64> = version
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect();
        let b: Vec<u64> = than
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect();
        let len = a.len().max(b.len());
        for i in 0..len {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0);
            if x != y {
                return x > y;
            }
        }
        false
    }

    /// Extracts the version from an installer filename like `"ZaloSetup-universal-26.10.20.dmg"`.
    pub fn version_from_installer_name(name: &str) -> Option<String> {
        static RE: Lazy<Regex> = Lazy::new(|| {
            Regex::new(r"^ZaloSetup-[A-Za-z0-9]+-(\d+(?:\.\d+)+)\.dmg$").unwrap()
        });
        RE.captures(name).map(|c| c[1].to_string())
    }

    /// Cleans up interrupted `work/` artifacts and atomically promotes a complete `pending/`
    /// upgrade into `current/` (preserving the existing `current/` on error).
    pub fn prepare_for_launch(&self) {
        let current = self.current_dir();
        let pending = self.pending_dir();
        let work = self.work_dir();

        let _ = std::fs::remove_dir_all(&work);
        if !pending.exists() {
            return;
        }
        if !Self::is_complete(&pending) {
            let _ = std::fs::remove_dir_all(&pending);
            return;
        }

        let retired = work.join(format!("retired-{}", Uuid::new_v4()));
        let swap = (|| -> Result<()> {
            std::fs::create_dir_all(&work)?;
            if current.exists() {
                std::fs::rename(&current, &retired)?;
            }
            std::fs::rename(&pending, &current)?;
            Ok(())
        })();

        match swap {
            Ok(()) => {
                tracing::info!(
                    version = Self::installed_version(&current).as_deref().unwrap_or("?"),
                    "runtime: promoted pending upgrade to current"
                );
            }
            Err(err) => {
                if !current.exists() && retired.exists() {
                    let _ = std::fs::rename(&retired, &current);
                }
                tracing::error!(%err, "runtime: failed to promote pending upgrade");
            }
        }

        let _ = std::fs::remove_dir_all(&retired);
    }

    /// Opens the installed `AsarArchive` inside `current/Contents/Resources/app.asar`.
    pub fn open_asar(&self) -> Result<AsarArchive> {
        AsarArchive::open(&self.current_dir().join("Contents/Resources/app.asar"))
    }

    /// Computes the lowercase hex SHA-256 of a file by streaming in 4 MiB chunks.
    pub fn sha256_file(path: &Path) -> Result<String> {
        let mut file = std::fs::File::open(path)
            .with_context(|| format!("failed to open {} for SHA-256", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; SHA256_CHUNK_SIZE];
        loop {
            let n = file
                .read(&mut buf)
                .with_context(|| format!("failed while hashing {}", path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    /// Queries `https://zalo.me/download/zalo-pc` (HEAD, no redirect follow) for the latest Mac DMG.
    pub async fn latest_release() -> Result<ZaloRelease> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(BROWSER_USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()
            .context("failed to build HTTP client")?;

        let base = Url::parse(LATEST_DOWNLOAD_URL).unwrap();
        let resp = client
            .head(base.clone())
            .send()
            .await
            .context("HEAD request to zalo.me failed")?;

        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .context("zalo.me did not return a redirect Location header")?;

        let resolved = base
            .join(location)
            .context("invalid redirect URL from zalo.me")?;
        ensure!(
            resolved.scheme() == "https",
            "unexpected scheme on zalo.me redirect: {}",
            resolved
        );

        let filename = resolved
            .path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or("");
        let version = Self::version_from_installer_name(filename).with_context(|| {
            format!("could not parse Zalo version from installer filename '{filename}'")
        })?;

        Ok(ZaloRelease::new(version, resolved.to_string(), None::<String>))
    }

    /// Downloads, verifies, extracts, and atomically installs the Zalo runtime into `current/`.
    pub async fn install(
        &self,
        progress: impl Fn(InstallProgress) + Send + Sync,
    ) -> Result<String> {
        self.install_into(&self.current_dir(), progress).await
    }

    /// Downloads, verifies, and stages an upgrade into `pending/` for promotion on next launch.
    pub async fn stage_upgrade(
        &self,
        progress: impl Fn(InstallProgress) + Send + Sync,
    ) -> Result<String> {
        self.install_into(&self.pending_dir(), progress).await
    }

    /// Downloads, verifies SHA-256, extracts `Contents/Info.plist` + `Contents/Resources/app.asar`
    /// via `7zz`/`7z` in userspace, and atomically swaps the result into `destination`.
    pub async fn install_into(
        &self,
        destination: &Path,
        progress: impl Fn(InstallProgress) + Send + Sync,
    ) -> Result<String> {
        progress(InstallProgress::Looking);
        let work = self.work_dir();
        tokio::fs::create_dir_all(&work)
            .await
            .with_context(|| format!("failed to create work dir {}", work.display()))?;

        let archive_path = work.join(format!("ZaloSetup-{}.dmg", Uuid::new_v4()));
        let fetch_res = match self.fetch_release(&self.release, &archive_path, &progress).await {
            Ok(()) => Ok(()),
            Err(first_err) if self.allows_latest => {
                tracing::warn!(
                    version = %self.release.version,
                    %first_err,
                    "pinned Zalo release unavailable; falling back to latest zalo.me redirect"
                );
                let latest = Self::latest_release().await?;
                self.fetch_release(&latest, &archive_path, &progress).await
            }
            Err(e) => Err(e),
        };

        if let Err(err) = fetch_res {
            let _ = tokio::fs::remove_file(&archive_path).await;
            return Err(err);
        }

        let extract_res = self
            .extract_and_stage(&archive_path, destination, &progress)
            .await;
        let _ = tokio::fs::remove_file(&archive_path).await;
        let version = extract_res?;
        progress(InstallProgress::Complete);
        Ok(version)
    }

    async fn fetch_release(
        &self,
        release: &ZaloRelease,
        dest_file: &Path,
        progress: &(impl Fn(InstallProgress) + Send + Sync),
    ) -> Result<()> {
        let parsed_url = Url::parse(&release.url)
            .with_context(|| format!("invalid release URL: {}", release.url))?;

        if parsed_url.scheme() == "file" {
            let src = parsed_url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("invalid file:// URL {}", release.url))?;
            let meta = tokio::fs::metadata(&src)
                .await
                .with_context(|| format!("missing local DMG {}", src.display()))?;
            progress(InstallProgress::Downloading {
                received: 0,
                total: Some(meta.len()),
            });
            tokio::fs::copy(&src, dest_file)
                .await
                .with_context(|| format!("failed to copy {}", src.display()))?;
            progress(InstallProgress::Downloading {
                received: meta.len(),
                total: Some(meta.len()),
            });
        } else {
            progress(InstallProgress::Downloading {
                received: 0,
                total: None,
            });
            if let Err(primary_err) =
                download_with_client(&default_http_client()?, &parsed_url, dest_file, progress)
                    .await
            {
                tracing::warn!(
                    %primary_err,
                    url = %parsed_url,
                    "standard download failed; retrying with DoH (Google/Cloudflare)"
                );
                download_over_doh(&parsed_url, dest_file, progress).await?;
            }
        }

        progress(InstallProgress::Verifying);
        if let Some(expected_hex) = release.sha256.as_deref() {
            let dest_owned = dest_file.to_path_buf();
            let actual_hex =
                tokio::task::spawn_blocking(move || Self::sha256_file(&dest_owned)).await??;
            if !actual_hex.eq_ignore_ascii_case(expected_hex) {
                bail!(
                    "SHA-256 mismatch for Zalo {}: expected {}, got {}",
                    release.version,
                    expected_hex,
                    actual_hex
                );
            }
        }

        Ok(())
    }

    async fn extract_and_stage(
        &self,
        dmg_path: &Path,
        destination: &Path,
        progress: &(impl Fn(InstallProgress) + Send + Sync),
    ) -> Result<String> {
        progress(InstallProgress::Extracting);
        let seven_zip = find_seven_zip_binary().await?;

        let work = self.work_dir();
        let extract_dir = work.join(format!("extract-{}", Uuid::new_v4()));
        let staging_dir = work.join(format!("staging-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&extract_dir).await?;

        let res = async {
            // Stage 1: Fast targeted extraction directly from the DMG container.
            run_7z_extract(
                &seven_zip,
                dmg_path,
                &extract_dir,
                &["-ir!*Contents/Info.plist", "-ir!*Contents/Resources/app.asar"],
            )
            .await?;

            let mut found_contents = find_zalo_contents_dir(&extract_dir);

            // Stage 2: Some multi-partition UDIF DMGs unpack into intermediate `*.hfs` /
            // `*.apfs` / `*.img` partition blobs first; unpack those if Stage 1 did not
            // directly yield `Contents/Info.plist` + `Contents/Resources/app.asar`.
            if found_contents.is_none() {
                let part_dir = work.join(format!("parts-{}", Uuid::new_v4()));
                tokio::fs::create_dir_all(&part_dir).await?;
                let part_res = async {
                    run_7z_extract(&seven_zip, dmg_path, &part_dir, &[]).await?;
                    if let Some(dir) = find_zalo_contents_dir(&part_dir) {
                        return Ok::<Option<PathBuf>, anyhow::Error>(Some(dir));
                    }
                    for candidate in collect_partition_candidates(&part_dir) {
                        let _ = run_7z_extract(
                            &seven_zip,
                            &candidate,
                            &extract_dir,
                            &["-ir!*Contents/Info.plist", "-ir!*Contents/Resources/app.asar"],
                        )
                        .await;
                        if let Some(dir) = find_zalo_contents_dir(&extract_dir) {
                            return Ok(Some(dir));
                        }
                    }
                    Ok(None)
                }
                .await;

                match part_res {
                    Ok(Some(dir)) if dir.starts_with(&part_dir) => {
                        // Stage directly before cleaning up `part_dir`.
                        let staged =
                            stage_contents_into(&dir, &staging_dir, destination, &work, progress)
                                .await;
                        let _ = tokio::fs::remove_dir_all(&part_dir).await;
                        return staged;
                    }
                    Ok(opt) => {
                        let _ = tokio::fs::remove_dir_all(&part_dir).await;
                        found_contents = opt;
                    }
                    Err(e) => {
                        let _ = tokio::fs::remove_dir_all(&part_dir).await;
                        return Err(e);
                    }
                }
            }

            let contents_dir = found_contents.context(
                "extracted DMG did not contain Zalo.app/Contents/{Info.plist,Resources/app.asar}",
            )?;

            stage_contents_into(&contents_dir, &staging_dir, destination, &work, progress).await
        }
        .await;

        let _ = tokio::fs::remove_dir_all(&extract_dir).await;
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        res
    }
}

async fn stage_contents_into(
    contents_dir: &Path,
    staging_dir: &Path,
    destination: &Path,
    work_dir: &Path,
    progress: &(impl Fn(InstallProgress) + Send + Sync),
) -> Result<String> {
    progress(InstallProgress::Installing);

    let staged_contents = staging_dir.join("Contents");
    let staged_resources = staged_contents.join("Resources");
    tokio::fs::create_dir_all(&staged_resources).await?;

    let src_plist = contents_dir.join("Info.plist");
    let src_asar = contents_dir.join("Resources/app.asar");
    let dst_plist = staged_contents.join("Info.plist");
    let dst_asar = staged_resources.join("app.asar");

    move_or_copy_file(&src_plist, &dst_plist).await?;
    move_or_copy_file(&src_asar, &dst_asar).await?;

    ensure!(
        ZaloRuntimeManager::is_complete(staging_dir),
        "staged Zalo runtime at {} is incomplete",
        staging_dir.display()
    );

    let version = ZaloRuntimeManager::installed_version(staging_dir)
        .context("missing CFBundleShortVersionString in extracted Info.plist")?;

    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let retired = work_dir.join(format!("retired-{}", Uuid::new_v4()));
    if destination.exists() {
        tokio::fs::rename(destination, &retired)
            .await
            .with_context(|| format!("failed to retire existing {}", destination.display()))?;
    }

    if let Err(err) = tokio::fs::rename(staging_dir, destination).await {
        if !destination.exists() && retired.exists() {
            let _ = tokio::fs::rename(&retired, destination).await;
        }
        return Err(err).with_context(|| {
            format!(
                "failed to atomically move staged runtime into {}",
                destination.display()
            )
        });
    }

    let _ = tokio::fs::remove_dir_all(&retired).await;
    tracing::info!(%version, dest = %destination.display(), "installed Zalo runtime");
    Ok(version)
}

async fn move_or_copy_file(src: &Path, dst: &Path) -> Result<()> {
    if tokio::fs::rename(src, dst).await.is_ok() {
        return Ok(());
    }
    tokio::fs::copy(src, dst).await.with_context(|| {
        format!(
            "failed to copy extracted file {} -> {}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(())
}

async fn find_seven_zip_binary() -> Result<String> {
    for candidate in ["7zz", "7z"] {
        let ok = Command::new(candidate)
            .arg("i")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(candidate.to_string());
        }
    }
    bail!("neither '7zz' nor '7z' was found in PATH (install 7zip / p7zip to extract Zalo Setup DMG)")
}

async fn run_7z_extract(
    bin: &str,
    archive: &Path,
    out_dir: &Path,
    extra_args: &[&str],
) -> Result<()> {
    let mut cmd = Command::new(bin);
    cmd.arg("x")
        .arg("-y")
        .arg("-bd")
        .arg(format!("-o{}", out_dir.display()))
        .arg(archive);
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.stdin(std::process::Stdio::null());

    let output = cmd
        .output()
        .await
        .with_context(|| format!("failed to execute {bin} on {}", archive.display()))?;

    // 7-Zip exit codes: 0 = OK, 1 = Warning (non-fatal, e.g. macOS `/Applications` symlink),
    // 2 = Error (sometimes emitted on unsupported HFS+ resource forks even after extracting files).
    // Only treat non-zero as fatal if `out_dir` received nothing at all AND exit code >= 2.
    let code = output.status.code().unwrap_or(-1);
    if code != 0 && code != 1 {
        let has_any = std::fs::read_dir(out_dir)
            .map(|mut rd| rd.next().is_some())
            .unwrap_or(false);
        if !has_any {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("{bin} failed (exit {code}): {}", stderr.trim());
        }
    }
    Ok(())
}

/// Walks `root` looking for a `Contents` directory holding both `Info.plist` and `Resources/app.asar`,
/// preferring `CFBundleIdentifier == "com.vng.zalo"`.
fn find_zalo_contents_dir(root: &Path) -> Option<PathBuf> {
    let mut fallback = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            if p.file_name().and_then(|s| s.to_str()) == Some("Contents") {
                let plist_path = p.join("Info.plist");
                let asar_path = p.join("Resources/app.asar");
                if plist_path.is_file() && asar_path.is_file() {
                    let bundle_id = plist::Value::from_file(&plist_path)
                        .ok()
                        .and_then(|v| {
                            v.as_dictionary()?
                                .get("CFBundleIdentifier")?
                                .as_string()
                                .map(str::to_owned)
                        });
                    if bundle_id.as_deref() == Some("com.vng.zalo") {
                        return Some(p);
                    }
                    if fallback.is_none() {
                        fallback = Some(p.clone());
                    }
                }
            }
            stack.push(p);
        }
    }
    fallback
}

fn collect_partition_candidates(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(p);
            } else if meta.is_file() {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(ext.as_str(), "hfs" | "hfsx" | "apfs" | "img" | "dmg")
                    || meta.len() >= 1024 * 1024
                {
                    out.push(p);
                }
            }
        }
    }
    // Try largest partition image first (the main macOS payload partition).
    out.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    out
}

fn default_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .context("failed to build reqwest client")
}

async fn download_with_client(
    client: &reqwest::Client,
    url: &Url,
    dest: &Path,
    progress: &(impl Fn(InstallProgress) + Send + Sync),
) -> Result<()> {
    let resp = client
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("HTTP GET failed for {url}"))?;

    let status = resp.status();
    ensure!(
        status.is_success(),
        "Zalo download server returned HTTP {} for {}",
        status,
        url
    );

    let total = resp.content_length();
    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("failed to create download file {}", dest.display()))?;

    let mut received: u64 = 0;
    let mut last_reported: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("network error while streaming DMG body")?;
        file.write_all(&chunk)
            .await
            .context("failed writing downloaded DMG chunk")?;
        received += chunk.len() as u64;
        if received - last_reported >= PROGRESS_REPORT_STRIDE || Some(received) == total {
            last_reported = received;
            progress(InstallProgress::Downloading { received, total });
        }
    }

    file.flush().await?;
    Ok(())
}

/// Resolves `host` via Google DoH first, falling back to Cloudflare DoH (`hickory-resolver`).
pub async fn resolve_host_doh(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let configs = [
        ("Google DoH", ResolverConfig::google_https()),
        ("Cloudflare DoH", ResolverConfig::cloudflare_https()),
    ];

    let mut last_err = None;
    for (label, cfg) in configs {
        let resolver = TokioAsyncResolver::tokio(cfg, ResolverOpts::default());
        match resolver.lookup_ip(host).await {
            Ok(lookup) => {
                let addrs: Vec<SocketAddr> =
                    lookup.iter().map(|ip| SocketAddr::new(ip, port)).collect();
                if !addrs.is_empty() {
                    tracing::debug!(host, provider = label, ?addrs, "resolved host over DoH");
                    return Ok(addrs);
                }
            }
            Err(err) => {
                tracing::debug!(host, provider = label, %err, "DoH lookup failed");
                last_err = Some(err);
            }
        }
    }

    Err(anyhow::anyhow!(
        "DoH resolution failed for {host}: {:?}",
        last_err
    ))
}

async fn download_over_doh(
    initial_url: &Url,
    dest: &Path,
    progress: &(impl Fn(InstallProgress) + Send + Sync),
) -> Result<()> {
    let mut current_url = initial_url.clone();
    for _ in 0..5 {
        let host = current_url
            .host_str()
            .context("download URL missing host")?
            .to_string();
        let port = current_url.port_or_known_default().unwrap_or(443);
        let addrs = resolve_host_doh(&host, port).await?;

        let client = reqwest::Client::builder()
            .user_agent(BROWSER_USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addrs)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()
            .context("failed to build DoH-pinned reqwest client")?;

        let resp = client
            .get(current_url.clone())
            .send()
            .await
            .with_context(|| format!("DoH download GET failed for {current_url}"))?;

        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .context("redirect response missing Location header")?;
            let next = current_url
                .join(loc)
                .context("invalid redirect URL during DoH download")?;
            ensure!(next.scheme() == "https", "refusing non-HTTPS redirect {next}");
            current_url = next;
            continue;
        }

        ensure!(
            status.is_success(),
            "DoH download server returned HTTP {} for {}",
            status,
            current_url
        );

        let total = resp.content_length();
        let mut file = tokio::fs::File::create(dest).await?;
        let mut received: u64 = 0;
        let mut last_reported: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            received += chunk.len() as u64;
            if received - last_reported >= PROGRESS_REPORT_STRIDE || Some(received) == total {
                last_reported = received;
                progress(InstallProgress::Downloading { received, total });
            }
        }
        file.flush().await?;
        return Ok(());
    }

    bail!("too many redirects while downloading Zalo DMG over DoH")
}
