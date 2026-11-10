//! Read-only bundle provider (`AsarArchive`, read-only directory, or in-memory map)
//! and on-the-fly HTML / Worker transformer for `olaz-web`.
//!
//! Guarantees zero disk writes:
//! - Never creates or modifies files on disk.
//! - Generates `globalThis.__zaloBridgeSpec` purely in RAM from `olaz_core::bundle_patcher::bridge_ipc_table()`.
//! - Applies `olaz_core::bundle_patcher::patch_asar_bundle` on read, rewrites `zfile://local/` -> `/__zfile/`,
//!   relaxes `<meta http-equiv="Content-Security-Policy">` for browser `blob: data: ws: wss: https:`,
//!   defers Zalo `<script src="...">` tags behind `data-olaz-defer-src` until `window.__olazBootReady`
//!   hydrates `IndexedDB`, and injects `/__olaz/{crypto_sync,bridge_table,preload}.js` in `<head>`
//!   and Zalo Web Workers (`*-worker.*.js`, excluding `pdf-worker.*.js`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use once_cell::sync::Lazy;
use olaz_core::bundle_patcher::{bridge_ipc_table, patch_asar_bundle, BRIDGE_GLOBALS};
use olaz_core::{mime_for_path, AsarArchive};
use parking_lot::{Mutex, RwLock};
use regex::{Captures, Regex};
use serde_json::{json, Value};

/// Embedded pure-TypedArray browser crypto/vault/cookie/stream runtime (`js/crypto_sync.js`).
pub const CRYPTO_SYNC_JS: &str = include_str!("../js/crypto_sync.js");

/// Embedded browser preload, multi-account namespacer, PIN vault, media bridge,
/// anti-recall & call recorder runtime (`js/{preload,media_bridge,anti_recall,call_recorder}.js`).
pub const PRELOAD_JS: &str = concat!(
    include_str!("../js/preload.js"),
    "\n",
    include_str!("../js/media_bridge.js"),
    "\n",
    include_str!("../js/anti_recall.js"),
    "\n",
    include_str!("../js/call_recorder.js"),
);

/// Embedded Service Worker serving `/__zfile/*` with HTTP `Range` (`206`) & denoising (`js/sw.js`).
pub const SW_JS: &str = include_str!("../js/sw.js");

/// Embedded client-side session boot router (`js/boot.html`).
pub const BOOT_HTML: &str = include_str!("../js/boot.html");

/// Crisp KV Zalo SVG brand mark (`assets/logo.svg`).
pub const LOGO_SVG: &str = include_str!("../assets/logo.svg");

/// Responsive mobile & standalone PWA stylesheet (`assets/mobile.css`).
pub const MOBILE_CSS: &str = include_str!("../assets/mobile.css");

/// 192x192 RGBA PNG app icon (`assets/icon-192.png`).
pub const ICON_192_PNG: &[u8] = include_bytes!("../assets/icon-192.png");

/// 512x512 RGBA PNG app icon (`assets/icon-512.png`).
pub const ICON_512_PNG: &[u8] = include_bytes!("../assets/icon-512.png");

/// 512x512 full-bleed opaque maskable PNG app icon (`assets/icon-maskable-512.png`).
pub const ICON_MASKABLE_512_PNG: &[u8] = include_bytes!("../assets/icon-maskable-512.png");

/// 180x180 full-bleed opaque iOS/iPadOS Home Screen icon (`assets/apple-touch-icon.png`).
pub const APPLE_TOUCH_ICON_PNG: &[u8] = include_bytes!("../assets/apple-touch-icon.png");

/// Multi-resolution (`16x16`, `32x32`, `48x48`) favicon (`assets/favicon.ico`).
pub const FAVICON_ICO: &[u8] = include_bytes!("../assets/favicon.ico");

const BROWSER_CSP_META: &str = concat!(
    r#"<meta http-equiv="Content-Security-Policy" content=""#,
    "default-src 'self' 'unsafe-inline' 'unsafe-eval' blob: data: ws: wss: https:; ",
    "script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; ",
    "style-src 'self' 'unsafe-inline' blob: data: https:; ",
    "img-src 'self' blob: data: https: http:; ",
    "media-src 'self' blob: data: https: http:; ",
    "font-src 'self' blob: data: https:; ",
    "connect-src 'self' blob: data: ws: wss: https:; ",
    "manifest-src 'self'; ",
    "worker-src 'self' blob:; ",
    "frame-src 'self' blob:;",
    r#"">"#,
);

const VIEWPORT_META_TAG: &str =
    r#"<meta name="viewport" content="width=device-width, initial-scale=1, minimum-scale=1, viewport-fit=cover">"#;

const HEAD_PWA_TAGS: &str = concat!(
    r##"<meta name="theme-color" content="#0068FF">"##,
    r#"<meta name="application-name" content="KV Zalo">"#,
    r#"<meta name="mobile-web-app-capable" content="yes">"#,
    r#"<meta name="apple-mobile-web-app-capable" content="yes">"#,
    r#"<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">"#,
    r#"<meta name="apple-mobile-web-app-title" content="KV Zalo">"#,
    r#"<link rel="manifest" href="/manifest.webmanifest">"#,
    r#"<link rel="icon" type="image/svg+xml" href="/favicon.svg">"#,
    r#"<link rel="icon" type="image/png" sizes="192x192" href="/__olaz/icons/icon-192.png">"#,
    r#"<link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">"#,
    r#"<link rel="stylesheet" href="/__olaz/mobile.css">"#,
);

const HEAD_BOOT_SCRIPTS: &str = concat!(
    r#"<script src="/__olaz/crypto_sync.js"></script>"#,
    r#"<script src="/__olaz/bridge_table.js"></script>"#,
    r#"<script src="/__olaz/preload.js"></script>"#,
);

const WORKER_IMPORT_PREAMBLE: &str =
    "try { importScripts('/__olaz/crypto_sync.js', '/__olaz/bridge_table.js', '/__olaz/preload.js'); } catch (__olazImpErr) {}\n";

static CSP_META_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)<meta\b[^>]*http-equiv\s*=\s*["']?Content-Security-Policy["']?[^>]*>"#)
        .expect("valid CSP meta regex")
});

static VIEWPORT_META_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)<meta\b[^>]*\bname\s*=\s*["']?viewport["']?[^>]*>"#)
        .expect("valid viewport meta regex")
});

static SCRIPT_SRC_TAG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)<script\b([^>]*?)\bsrc\s*=\s*(["'])([^"']+)(["'])([^>]*)>"#)
        .expect("valid script src regex")
});

static HEAD_OPEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<head(?:\s[^>]*)?>").expect("valid head regex"));

static WEB_BRIDGE_SPEC_SCRIPT: Lazy<String> = Lazy::new(build_in_memory_bridge_spec_script);

/// Returns the precomputed `globalThis.__zaloBridgeSpec = ...;` script built strictly in RAM
/// without touching the host filesystem.
pub fn web_bridge_spec_script() -> &'static str {
    &WEB_BRIDGE_SPEC_SCRIPT
}

fn build_in_memory_bridge_spec_script() -> String {
    let mut globals: BTreeSet<String> = BRIDGE_GLOBALS.iter().map(|&s| s.to_string()).collect();
    let mut members: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut values: BTreeMap<String, Value> = BTreeMap::new();

    let process_arch = "x64";
    let os_info = json!({
        "release": "22.6.0",
        "linuxRelease": "",
        "version": "Darwin Kernel Version 22.6.0",
        "arch": process_arch,
        "hostname": "OlazWeb",
        "homedir": "/olaz-vfs/home",
        "tmpdir": "/olaz-vfs/tmp",
        "totalmem": 8_589_934_592u64,
        "freemem": 4_294_967_296u64,
        "cpus": 4,
        "uptime": 3600,
        "username": "user",
    });

    for (path, val) in [
        ("$zenv.isPreloaded", json!(true)),
        ("$zenv.isRenderer", json!(true)),
        ("$zenv.platform", json!("darwin")),
        ("$zenv.process_arch", json!(process_arch)),
        ("$zenv.arch", json!(process_arch)),
        ("$znode.os.__info", os_info),
        (
            "$znode.fs.constants",
            json!({ "F_OK": 0, "R_OK": 4, "W_OK": 2, "X_OK": 1 }),
        ),
        (
            "$znode.fs.constanst",
            json!({ "F_OK": 0, "R_OK": 4, "W_OK": 2, "X_OK": 1 }),
        ),
    ] {
        members.insert(path.to_string(), "value");
        values.insert(path.to_string(), val);
    }

    for path in [
        "$zelectron.storePath",
        "$zelectron.storeRead",
        "$zelectron.storeWrite",
        "$zelectronNative.clipboardText",
        "$zelectronNative.clipboardWriteText",
        "$zelectronNative.clipboardFormats",
        "$zelectronNative.clipboardImagePng",
        "$zelectronNative.clipboardWriteImagePng",
        "$znode.nativelibs.zfileAddon.canRead",
        "$znode.nativelibs.zfileAddon.canWrite",
        "$znode.nativelibs.zfileAddon.canReadAndWrite",
        "$zFileManager.getSentryConfig",
        "$znode.fs.accessSync",
        "$znode.fs.existsSync",
        "$znode.fsExtra.existsSync",
        "$znode.fs.statSync",
        "$znode.fs.lstatSync",
        "$znode.fsExtra.statSync",
        "$znode.fs.mkdirSync",
        "$znode.fsExtra.ensureDirSync",
        "$znode.fs.readdirSync",
        "$znode.fs.readFileSync",
        "$znode.fs.writeFileSync",
        "$znode.fs.unlinkSync",
        "$znode.fs.rmdirSync",
        "$znode.fs.rmSync",
        "$znode.fsExtra.removeSync",
        "$znode.fs.renameSync",
        "$znode.fsExtra.renameSync",
        "$znode.fs.copyFileSync",
        "$znode.fsExtra.copySync",
        "$znode.nativelibs.fileUtilsAddon.getDiskUsage",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeSync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeTreeSync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeByGlobSync",
        "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksSync",
        "$znode.nativelibs.fileUtilitiesAddon.detectFilesystemSync",
        "$znode.nativelibs.fileUtilitiesAddon.cancelJob",
        "$znode.nativelibs.dbUtilsAddon.decompressAndDecryptDb_V2",
        "$znode.nativelibs.dbUtilsAddon.decompressAndDecryptDb",
        "$znode.nativelibs.dbUtilsAddon.parseBinNet",
        "$znode.crypto.encrypt",
        "$znode.crypto.decrypt",
        "$znode.crypto.hash",
        "$znode.crypto.randomBytes",
        "$znode.crypto.__cipherCreate",
        "$znode.crypto.__cipherUpdate",
        "$znode.crypto.__cipherFinal",
        "$znode.crypto.__cipherOption",
        "$znode.crypto.__hashCreate",
        "$znode.crypto.__hashUpdate",
        "$znode.crypto.__hashDigest",
    ] {
        members.insert(path.to_string(), "sync");
    }

    for path in [
        "$znode.fs.access",
        "$znode.fsPromise.access",
        "$znode.fs.exists",
        "$znode.fsExtra.pathExists",
        "$znode.fs.stat",
        "$znode.fs.lstat",
        "$znode.fsPromise.stat",
        "$znode.fsPromise.lstat",
        "$znode.fs.mkdir",
        "$znode.fsPromise.mkdir",
        "$znode.fsExtra.ensureDir",
        "$znode.fs.readdir",
        "$znode.fsPromise.readdir",
        "$znode.fs.readFile",
        "$znode.fsPromise.readFile",
        "$znode.fs.writeFile",
        "$znode.fsPromise.writeFile",
        "$znode.fs.unlink",
        "$znode.fsPromise.unlink",
        "$znode.fs.rmdir",
        "$znode.fsPromise.rmdir",
        "$znode.fs.rm",
        "$znode.fsPromise.rm",
        "$znode.fsExtra.remove",
        "$znode.rimraf",
        "$znode.fs.rename",
        "$znode.fsPromise.rename",
        "$znode.fsExtra.move",
        "$znode.fsExtra.rename",
        "$znode.fs.copyFile",
        "$znode.fsPromise.copyFile",
        "$znode.fsExtra.copy",
        "$znode.fs.link",
        "$znode.fsPromise.link",
        "$znode.shell.openPath",
        "$znode.shell.openExternal",
        "$znode.shell.showItemInFolder",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeAsync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeTreeAsync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeByGlobAsync",
        "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync",
        "$znode.nativelibs.fileUtilitiesAddon.detectFilesystemAsync",
        "$znode.nativelibs.zfileAddon.getDiskInfo",
        "$znode.nativelibs.zfileAddon.getInfo",
        "$znode.nativelibs.zfileAddon.copyFolder",
        "$znode.nativelibs.zfileAddon.cancelCopy",
        "$zFileManager.saveSentryConfig",
        "$zFileManager.getViewerkey",
        "$zFileManager.writeRecentId",
        "$zFileManager.writeZLog",
        "$zFileManager.writeZLogBetween",
        "$zFileManager.readZLog",
        "$zFileManager.scandirLog",
        "$zFileManager.getExtraLogFiles",
        "$zFileManager.getQosLog",
        "$zFileManager.getActionLog",
        "$zFileManager.readUptimeLog",
        "$zFileManager.removeQosLog",
        "$zFileManager.removeActionLog",
        "$zFileManager.writeActionLog",
        "$zFileManager.writeUptimeLog",
        "$zFileManager.truncateFileUpto",
        "$zFileManager.cleanupLogFileDescriptors",
        "$zFileManager.initBinaryLogFile",
        "$zFileManager.sealCalfLog",
        "$zFileManager.mergeCalfLogGroup",
        "$zFileManager.writeLogRange",
        "$zFileManager.readLogRange",
        "$zFileManager.trustBlob",
        "$zFileManager.isTrustPath",
        "$zFileManager.trustPathFromBlob",
        "$zFileManager.getInstallSource",
        "$zFileManager.getFileArrayBuffer",
        "$zFileManager.readFileAsync",
        "$zFileManager.svcStatAsync",
        "$zFileManager.svcReadPath",
        "$zFileManager.readFileSlice",
        "$zFileManager.zfileFindNoiseSignature",
        "$zFileManager.getMd5LargeFileObject",
        "$zFileManager.zfileGetChecksum",
        "$zFileManager.testChecksum",
        "$zFileManager.writeBufferToPath",
        "$zFileManager.appendBufferToPath",
        "$zFileManager.zcloudCloneAndMatchChecksum",
        "$zFileManager.zcloudDenoiseFile",
        "$zFileManager.zcloudNoiseFile",
        "$zconfig.getAutostartState",
        "$zconfig.setStartInTray",
        "$zconfig.migrateLegacyAutoLaunch",
        "$zFileManager.generateVideoThumbnail",
    ] {
        members.insert(path.to_string(), "async");
    }

    let ipc_table = bridge_ipc_table();
    for path in members.keys().chain(ipc_table.keys()) {
        if let Some(g) = path.split('.').next() {
            globals.insert(g.to_string());
        }
    }

    let globals_vec: Vec<String> = globals.into_iter().collect();
    let spec = json!({
        "globals": globals_vec,
        "members": members,
        "values": values,
        "ipc": ipc_table,
    });
    let serialized = serde_json::to_string(&spec).unwrap_or_else(|_| "{}".to_string());
    format!("globalThis.__zaloBridgeSpec = {serialized};\n")
}

#[derive(Clone)]
enum BundleBacking {
    Asar(Arc<AsarArchive>),
    Dir(PathBuf),
    Memory(Arc<BTreeMap<String, Vec<u8>>>),
}

/// Zero-copy or reference-counted immutable asset payload.
///
/// Cloning an [`AssetBytes`] only increments an atomic reference count (`Arc` or `Bytes`)
/// and allocates **zero** heap bytes per HTTP request.
#[derive(Clone)]
pub enum AssetBytes {
    /// Zero-copy slice directly into the memory-mapped read-only `app.asar` archive.
    MmapSlice {
        archive: Arc<AsarArchive>,
        start: usize,
        end: usize,
    },
    /// Reference-counted immutable buffer for transformed/patched assets or Dir/Memory backings.
    Shared(Bytes),
}

impl std::fmt::Debug for AssetBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MmapSlice { start, end, .. } => f
                .debug_struct("AssetBytes::MmapSlice")
                .field("len", &end.saturating_sub(*start))
                .finish(),
            Self::Shared(b) => f
                .debug_struct("AssetBytes::Shared")
                .field("len", &b.len())
                .finish(),
        }
    }
}

impl AsRef<[u8]> for AssetBytes {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::MmapSlice {
                archive,
                start,
                end,
            } => archive.slice(*start, *end).unwrap_or(&[]),
            Self::Shared(b) => b.as_ref(),
        }
    }
}

impl Deref for AssetBytes {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}

impl PartialEq<[u8]> for AssetBytes {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_ref() == other
    }
}

impl PartialEq<Vec<u8>> for AssetBytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.as_ref() == other.as_slice()
    }
}

/// Read-only static Zalo bundle asset provider with zero-copy ASAR slicing and cached patching.
#[derive(Clone)]
pub struct BundleProvider {
    backing: BundleBacking,
    cache: Arc<RwLock<HashMap<String, ServedAsset>>>,
    miss_gate: Arc<Mutex<()>>,
}

impl std::fmt::Debug for BundleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.backing {
            BundleBacking::Asar(_) => f.debug_struct("BundleProvider::Asar").finish(),
            BundleBacking::Dir(p) => f
                .debug_struct("BundleProvider::Dir")
                .field("path", p)
                .finish(),
            BundleBacking::Memory(m) => f
                .debug_struct("BundleProvider::Memory")
                .field("file_count", &m.len())
                .finish(),
        }
    }
}

/// Resolved bundle asset ready to serve over HTTP with zero per-request heap cloning.
#[derive(Debug, Clone)]
pub struct ServedAsset {
    pub bytes: AssetBytes,
    pub content_type: Arc<str>,
}

impl BundleProvider {
    fn with_backing(backing: BundleBacking) -> Self {
        Self {
            backing,
            cache: Arc::new(RwLock::new(HashMap::new())),
            miss_gate: Arc::new(Mutex::new(())),
        }
    }

    /// Opens a read-only `app.asar` archive (`olaz_core::asar::AsarArchive::open`).
    pub fn from_asar(path: impl AsRef<Path>) -> Result<Self> {
        let archive = AsarArchive::open(path.as_ref())
            .with_context(|| format!("failed to open ASAR archive at {}", path.as_ref().display()))?;
        Ok(Self::with_backing(BundleBacking::Asar(Arc::new(archive))))
    }

    /// Opens a read-only asset directory on disk (useful for tests without a 100 MB `.asar`).
    pub fn from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        if !p.is_dir() {
            bail!("bundle directory does not exist: {}", p.display());
        }
        Ok(Self::with_backing(BundleBacking::Dir(p.to_path_buf())))
    }

    /// Constructs a pure-RAM bundle provider from `(relative_path -> bytes)`.
    pub fn from_memory(files: BTreeMap<String, Vec<u8>>) -> Self {
        Self::with_backing(BundleBacking::Memory(Arc::new(files)))
    }

    /// Automatically opens `path` as a directory if `path.is_dir()` or as an `AsarArchive` if it is a file.
    pub fn open_auto(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        if p.is_dir() {
            Self::from_dir(p)
        } else {
            Self::from_asar(p)
        }
    }

    /// Looks up `request_path`, applies bundle/HTML/worker patches once in memory, and returns
    /// an [`AssetBytes`]-backed [`ServedAsset`] (`O(1)` atomic refcount clone on cache hits).
    pub fn get_asset(&self, request_path: &str) -> Option<ServedAsset> {
        let clean_rel = sanitize_relative_asset_path(request_path)?;
        if let Some(hit) = self.cache.read().get(&clean_rel) {
            return Some(hit.clone());
        }

        // Singleflight cold-miss gate prevents concurrent burst requests for 17+ MB bundles
        // from running duplicate regex/string transforms simultaneously on cold boot.
        let _guard = self.miss_gate.lock();
        if let Some(hit) = self.cache.read().get(&clean_rel) {
            return Some(hit.clone());
        }

        let asset = self.load_and_transform_uncached(&clean_rel)?;
        self.cache.write().insert(clean_rel, asset.clone());
        Some(asset)
    }

    fn load_and_transform_uncached(&self, clean_rel: &str) -> Option<ServedAsset> {
        let candidates = if let Some(stripped) = clean_rel.strip_prefix("pc-dist/") {
            [clean_rel.to_string(), stripped.to_string()]
        } else {
            [format!("pc-dist/{clean_rel}"), clean_rel.to_string()]
        };

        for cand in &candidates {
            match &self.backing {
                BundleBacking::Asar(archive) => {
                    if let Some((start, end)) = archive.byte_range(cand) {
                        let raw_slice = archive.slice(start, end)?;
                        let bytes = match transform_bundle_asset_opt(cand, raw_slice) {
                            Some(patched) => AssetBytes::Shared(Bytes::from(patched)),
                            None => AssetBytes::MmapSlice {
                                archive: Arc::clone(archive),
                                start,
                                end,
                            },
                        };
                        return Some(ServedAsset {
                            bytes,
                            content_type: Arc::from(detect_mime(cand)),
                        });
                    }
                    if let Some(cow) = archive.read_or_unpacked(cand) {
                        let bytes = match transform_bundle_asset_opt(cand, &cow) {
                            Some(patched) => AssetBytes::Shared(Bytes::from(patched)),
                            None => AssetBytes::Shared(Bytes::from(cow.into_owned())),
                        };
                        return Some(ServedAsset {
                            bytes,
                            content_type: Arc::from(detect_mime(cand)),
                        });
                    }
                }
                BundleBacking::Dir(root) => {
                    let full = root.join(cand);
                    if full.is_file() {
                        if let Ok(raw_vec) = std::fs::read(&full) {
                            let final_vec =
                                transform_bundle_asset_opt(cand, &raw_vec).unwrap_or(raw_vec);
                            return Some(ServedAsset {
                                bytes: AssetBytes::Shared(Bytes::from(final_vec)),
                                content_type: Arc::from(detect_mime(cand)),
                            });
                        }
                    }
                }
                BundleBacking::Memory(map) => {
                    if let Some(raw_vec) = map.get(cand) {
                        let final_vec = transform_bundle_asset_opt(cand, raw_vec)
                            .unwrap_or_else(|| raw_vec.clone());
                        return Some(ServedAsset {
                            bytes: AssetBytes::Shared(Bytes::from(final_vec)),
                            content_type: Arc::from(detect_mime(cand)),
                        });
                    }
                }
            }
        }
        None
    }
}

fn sanitize_relative_asset_path(raw: &str) -> Option<String> {
    let without_query = raw.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(without_query)
        .decode_utf8()
        .ok()?;
    let trimmed = decoded.trim_start_matches('/');
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.contains('\\') {
        return None;
    }
    let mut parts = Vec::new();
    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return None;
        }
        parts.push(seg);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn detect_mime(rel_path: &str) -> String {
    let base = mime_for_path(rel_path);
    if base != "application/octet-stream" {
        return base.to_string();
    }
    mime_guess::from_path(rel_path)
        .first_or_octet_stream()
        .to_string()
}

#[inline]
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Applies `olaz_core::bundle_patcher::patch_asar_bundle` plus web-specific HTML/Worker transforms,
/// returning `Some(patched_bytes)` only when the asset actually requires modification, or `None`
/// when the raw slice can be served zero-copy without allocating.
pub fn transform_bundle_asset_opt(rel_path: &str, raw_bytes: &[u8]) -> Option<Vec<u8>> {
    let maybe_patched = patch_asar_bundle(rel_path, raw_bytes);
    let bytes: &[u8] = maybe_patched.as_deref().unwrap_or(raw_bytes);
    let clean_path = rel_path.split(['?', '#']).next().unwrap_or(rel_path);
    let filename = clean_path.rsplit('/').next().unwrap_or(clean_path);
    let lower_name = filename.to_ascii_lowercase();

    if lower_name.ends_with(".html") || lower_name.ends_with(".htm") {
        if let Ok(html) = std::str::from_utf8(bytes) {
            return Some(transform_zalo_html(html).into_bytes());
        }
    }

    if lower_name.ends_with(".js")
        && lower_name.contains("-worker.")
        && !lower_name.starts_with("pdf-worker.")
    {
        if let Ok(js) = std::str::from_utf8(bytes) {
            let rewritten = js.replace("zfile://local/", "/__zfile/");
            let mut out = String::with_capacity(WORKER_IMPORT_PREAMBLE.len() + rewritten.len());
            out.push_str(WORKER_IMPORT_PREAMBLE);
            out.push_str(&rewritten);
            return Some(out.into_bytes());
        }
    }

    if (lower_name.ends_with(".js") || lower_name.ends_with(".css"))
        && contains_subslice(bytes, b"zfile://local/")
    {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return Some(text.replace("zfile://local/", "/__zfile/").into_bytes());
        }
    }

    maybe_patched
}

/// Applies `olaz_core::bundle_patcher::patch_asar_bundle` plus web-specific HTML/Worker transforms.
pub fn transform_bundle_asset(rel_path: &str, raw_bytes: &[u8]) -> Vec<u8> {
    transform_bundle_asset_opt(rel_path, raw_bytes).unwrap_or_else(|| raw_bytes.to_vec())
}

fn sanitize_profile_slot(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() || trimmed == "default" || trimmed.len() > 64 {
        return None;
    }
    if trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Builds a W3C Web App Manifest JSON document (`application/manifest+json`), optionally scoped
/// to an isolated multi-account profile slot (`?profile=<slot>`).
pub fn build_web_manifest_json(profile_slot: Option<&str>) -> String {
    let (name, short_name, id_and_start_url) = match sanitize_profile_slot(profile_slot) {
        Some(slot) => (
            format!("KV Zalo ({slot})"),
            format!("Zalo·{slot}"),
            format!("/?profile={slot}&source=pwa"),
        ),
        None => (
            "KV Zalo".to_string(),
            "KV Zalo".to_string(),
            "/?source=pwa".to_string(),
        ),
    };

    let manifest = json!({
        "name": name,
        "short_name": short_name,
        "id": id_and_start_url,
        "start_url": id_and_start_url,
        "scope": "/",
        "display": "standalone",
        "display_override": ["window-controls-overlay", "standalone", "minimal-ui"],
        "orientation": "any",
        "background_color": "#0f172a",
        "theme_color": "#0068FF",
        "description": "Zero-persistence self-hosted Zalo Web & PWA with browser-local E2EE vault, multi-account isolation, and media controls.",
        "categories": ["social", "communication", "productivity"],
        "icons": [
            {
                "src": "/__olaz/logo.svg",
                "sizes": "any",
                "type": "image/svg+xml",
                "purpose": "any"
            },
            {
                "src": "/__olaz/icons/icon-192.png",
                "sizes": "192x192",
                "type": "image/png",
                "purpose": "any"
            },
            {
                "src": "/__olaz/icons/icon-512.png",
                "sizes": "512x512",
                "type": "image/png",
                "purpose": "any"
            },
            {
                "src": "/__olaz/icons/icon-maskable-512.png",
                "sizes": "512x512",
                "type": "image/png",
                "purpose": "maskable"
            }
        ],
        "shortcuts": [
            {
                "name": "Open Default Vault",
                "short_name": "Default Vault",
                "url": "/?source=pwa",
                "icons": [
                    {
                        "src": "/__olaz/icons/icon-192.png",
                        "sizes": "192x192",
                        "type": "image/png"
                    }
                ]
            },
            {
                "name": "Boot Session Router",
                "short_name": "Session Router",
                "url": "/boot.html",
                "icons": [
                    {
                        "src": "/__olaz/icons/icon-192.png",
                        "sizes": "192x192",
                        "type": "image/png"
                    }
                ]
            }
        ]
    });

    serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".to_string())
}

/// Transforms a Zalo HTML entrypoint (`login.html`, `index.html`, `shared-worker.html`, etc.):
/// 1. Rewrites `zfile://local/` -> `/__zfile/`.
/// 2. Rewrites `<meta http-equiv="Content-Security-Policy">` to permit `'self' blob: data: ws: wss: https:` + `manifest-src 'self'`.
/// 3. Normalizes `<meta name="viewport">` to include `viewport-fit=cover`.
/// 4. Defers Zalo `<script src="...">` tags (`data-olaz-defer-src="..."`) until `window.__olazBootReady`
///    hydrates `IndexedDB` (and unlocks any optional SubtleCrypto PIN Vault).
/// 5. Injects PWA metadata, icons, `/__olaz/mobile.css`, and `/__olaz/{crypto_sync,bridge_table,preload}.js` into `<head>`.
pub fn transform_zalo_html(html: &str) -> String {
    let step1 = html.replace("zfile://local/", "/__zfile/");

    let step2 = if CSP_META_RE.is_match(&step1) {
        CSP_META_RE.replace_all(&step1, BROWSER_CSP_META).into_owned()
    } else {
        step1
    };

    let had_viewport = VIEWPORT_META_RE.is_match(&step2);
    let step2_vp = if had_viewport {
        VIEWPORT_META_RE
            .replace_all(&step2, VIEWPORT_META_TAG)
            .into_owned()
    } else {
        step2
    };

    let step3 = SCRIPT_SRC_TAG_RE
        .replace_all(&step2_vp, |caps: &Captures<'_>| {
            let before = &caps[1];
            let quote_open = &caps[2];
            let src_val = &caps[3];
            let quote_close = &caps[4];
            let after = &caps[5];
            if src_val.starts_with("/__olaz/") || before.contains("data-olaz-defer-src") {
                caps[0].to_string()
            } else {
                format!(
                    "<script{before}data-olaz-defer-src={quote_open}{src_val}{quote_close}{after}>"
                )
            }
        })
        .into_owned();

    if step3.contains("/__olaz/preload.js") {
        return step3;
    }

    let head_injection = if had_viewport {
        format!("{HEAD_PWA_TAGS}{HEAD_BOOT_SCRIPTS}")
    } else {
        format!("{VIEWPORT_META_TAG}{HEAD_PWA_TAGS}{HEAD_BOOT_SCRIPTS}")
    };

    if let Some(m) = HEAD_OPEN_RE.find(&step3) {
        let mut out = String::with_capacity(step3.len() + head_injection.len());
        out.push_str(&step3[..m.end()]);
        out.push_str(&head_injection);
        out.push_str(&step3[m.end()..]);
        out
    } else {
        format!("{head_injection}{step3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_import_preamble_injected_into_all_zalo_workers_except_pdf_worker() {
        let sample_zd = br#"const u = "zfile://local/olaz-vfs/photo.jpg"; self.onmessage = () => {};"#;
        for worker_path in [
            "pc-dist/zd-worker.cfeba43a9a8e4e40dda3.js",
            "pc-dist/dal-worker.cfeba43a9a8e4e40dda3.js",
            "pc-dist/search-worker.cfeba43a9a8e4e40dda3.js",
            "pc-dist/opfs-worker.cfeba43a9a8e4e40dda3.js",
            "pc-dist/preview-thumb-worker.cfeba43a9a8e4e40dda3.js",
            "pc-dist/mainless-worker.cfeba43a9a8e4e40dda3.js",
            "pc-dist/cpu-heavy-worker.cfeba43a9a8e4e40dda3.js",
            "pc-dist/trust-worker.cfeba43a9a8e4e40dda3.js",
        ] {
            let out = transform_bundle_asset_opt(worker_path, sample_zd)
                .unwrap_or_else(|| panic!("expected {worker_path} to be transformed"));
            let s = std::str::from_utf8(&out).unwrap();
            assert!(
                s.starts_with(WORKER_IMPORT_PREAMBLE),
                "expected WORKER_IMPORT_PREAMBLE on {worker_path}, got: {s}"
            );
            assert!(
                s.contains("/__zfile/olaz-vfs/photo.jpg") && !s.contains("zfile://local/"),
                "expected zfile://local/ rewrite on {worker_path}"
            );
        }

        // `sync-v2-sub-worker.*.js` must get BOTH `expose_webpack_require` (`self.__swReq=`) AND `WORKER_IMPORT_PREAMBLE`
        let sw_raw = b"!function(modules){var a={};function r(n){return modules[n];}r.c={};r.e=function(e){var t=[],n=a[e];return t}}([]);";
        let sw_out = transform_bundle_asset_opt(
            "pc-dist/sync-v2-sub-worker.cfeba43a9a8e4e40dda3.js",
            sw_raw,
        )
        .expect("sync-v2-sub-worker must be transformed");
        let sw_str = std::str::from_utf8(&sw_out).unwrap();
        assert!(sw_str.starts_with(WORKER_IMPORT_PREAMBLE));
        assert!(sw_str.contains("self.__swReq=r;"));

        // `pdf-worker.*.js` must NOT receive `WORKER_IMPORT_PREAMBLE`
        let pdf_raw = b"self.pdfjsWorker = true;";
        assert!(
            transform_bundle_asset_opt("pc-dist/pdf-worker.cfeba43a9a8e4e40dda3.js", pdf_raw)
                .is_none(),
            "untouched pdf-worker.*.js must remain None (zero-copy) without WORKER_IMPORT_PREAMBLE"
        );
        let pdf_transformed =
            transform_bundle_asset("pc-dist/pdf-worker.cfeba43a9a8e4e40dda3.js", pdf_raw);
        assert_eq!(pdf_transformed, pdf_raw);
    }

    #[test]
    fn sw_js_retries_delayed_idb_writes_before_returning_404() {
        let harness = r#"
          const fs = require('node:fs');
          const vm = require('node:vm');
          const assert = require('node:assert/strict');
          const input = JSON.parse(fs.readFileSync(0, 'utf8'));

          (async () => {
            const stores = new Map();
            let getAttempts = 0;

            const fakeIndexedDB = {
              open(dbName) {
                const req = {};
                queueMicrotask(() => {
                  const dbMap = stores.get(dbName) || new Map();
                  stores.set(dbName, dbMap);
                  req.result = {
                    objectStoreNames: { contains: () => true },
                    createObjectStore() {},
                    close() {},
                    transaction() {
                      return {
                        objectStore() {
                          return {
                            get(key) {
                              getAttempts += 1;
                              const g = {};
                              queueMicrotask(() => {
                                g.result = dbMap.get(key);
                                if (g.onsuccess) g.onsuccess();
                              });
                              return g;
                            },
                          };
                        },
                      };
                    },
                  };
                  if (req.onsuccess) req.onsuccess();
                });
                return req;
              },
            };

            let fetchHandler = null;
            const swScope = {
              URL,
              Uint8Array,
              ArrayBuffer,
              DataView,
              TextEncoder,
              TextDecoder,
              crypto: globalThis.crypto,
              Blob,
              Response,
              Headers,
              Request,
              queueMicrotask,
              setTimeout,
              clearTimeout,
              console,
              location: { origin: 'http://127.0.0.1:8080' },
              indexedDB: fakeIndexedDB,
              clients: { claim: () => Promise.resolve() },
              skipWaiting: () => Promise.resolve(),
              importScripts: () => {
                vm.runInContext(input.cryptoJs, swScope, { filename: 'crypto_sync.js' });
              },
              addEventListener(type, fn) {
                if (type === 'fetch') fetchHandler = fn;
              },
            };
            swScope.self = swScope;
            swScope.globalThis = swScope;
            vm.createContext(swScope);
            vm.runInContext(input.swJs, swScope, { filename: 'sw.js' });

            function dispatchZfile(path) {
              return new Promise((resolve) => {
                fetchHandler({
                  request: new Request('http://127.0.0.1:8080' + path),
                  respondWith: (p) => resolve(p),
                });
              });
            }

            // 1) Delayed commit at 30ms -> resolved by 80ms first retry (2 attempts total)
            getAttempts = 0;
            setTimeout(() => {
              const m = stores.get('olaz_vfs_v1__acc1') || new Map();
              m.set('/photos/late1.jpg', { path: '/photos/late1.jpg', mime: 'image/jpeg', data: new Uint8Array([1, 2, 3, 4]) });
              stores.set('olaz_vfs_v1__acc1', m);
            }, 30);
            const r1 = await dispatchZfile('/__zfile/@slot/acc1/photos/late1.jpg');
            assert.equal(r1.status, 200);
            assert.equal(getAttempts, 2);

            // 2) Delayed commit at 120ms -> resolved by 150ms second retry (3 attempts total)
            getAttempts = 0;
            setTimeout(() => {
              const m = stores.get('olaz_vfs_v1__acc1') || new Map();
              m.set('/photos/late2.jpg', { path: '/photos/late2.jpg', mime: 'image/jpeg', data: new Uint8Array([9, 8, 7]) });
              stores.set('olaz_vfs_v1__acc1', m);
            }, 120);
            const r2 = await dispatchZfile('/__zfile/@slot/acc1/photos/late2.jpg');
            assert.equal(r2.status, 200);
            assert.equal(getAttempts, 3);

            // 3) Truly missing file -> 3 attempts (0ms, +80ms, +150ms) then 404
            getAttempts = 0;
            const r3 = await dispatchZfile('/__zfile/@slot/acc1/photos/missing.jpg');
            assert.equal(r3.status, 404);
            assert.equal(getAttempts, 3);
          })().catch((e) => {
            console.error(e);
            process.exit(1);
          });
        "#;

        let mut child = std::process::Command::new("node")
            .arg("-e")
            .arg(harness)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn node");
        {
            use std::io::Write;
            let payload = serde_json::to_vec(&json!({
                "cryptoJs": CRYPTO_SYNC_JS,
                "swJs": SW_JS,
            }))
            .unwrap();
            child.stdin.take().unwrap().write_all(&payload).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "sw.js retry test failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
