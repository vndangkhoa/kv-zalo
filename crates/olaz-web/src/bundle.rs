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
//!   and `dal-worker.*.js`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use once_cell::sync::Lazy;
use olaz_core::bundle_patcher::{bridge_ipc_table, patch_asar_bundle, BRIDGE_GLOBALS};
use olaz_core::{mime_for_path, AsarArchive};
use regex::{Captures, Regex};
use serde_json::{json, Value};

/// Embedded pure-TypedArray browser crypto/vault/cookie/stream runtime (`js/crypto_sync.js`).
pub const CRYPTO_SYNC_JS: &str = include_str!("../js/crypto_sync.js");

/// Embedded browser preload, multi-account namespacer, PIN vault & relay client (`js/preload.js`).
pub const PRELOAD_JS: &str = include_str!("../js/preload.js");

/// Embedded Service Worker serving `/__zfile/*` with HTTP `Range` (`206`) & denoising (`js/sw.js`).
pub const SW_JS: &str = include_str!("../js/sw.js");

/// Embedded client-side session boot router (`js/boot.html`).
pub const BOOT_HTML: &str = include_str!("../js/boot.html");

const BROWSER_CSP_META: &str = concat!(
    r#"<meta http-equiv="Content-Security-Policy" content=""#,
    "default-src 'self' 'unsafe-inline' 'unsafe-eval' blob: data: ws: wss: https:; ",
    "script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; ",
    "style-src 'self' 'unsafe-inline' blob: data: https:; ",
    "img-src 'self' blob: data: https: http:; ",
    "media-src 'self' blob: data: https: http:; ",
    "font-src 'self' blob: data: https:; ",
    "connect-src 'self' blob: data: ws: wss: https:; ",
    "worker-src 'self' blob:; ",
    "frame-src 'self' blob:;",
    r#"">"#,
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

/// Read-only static Zalo bundle asset provider with on-the-fly HTML & JS patching.
#[derive(Clone)]
pub struct BundleProvider {
    backing: BundleBacking,
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

/// Resolved bundle asset ready to serve over HTTP.
#[derive(Debug, Clone)]
pub struct ServedAsset {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

impl BundleProvider {
    /// Opens a read-only `app.asar` archive (`olaz_core::asar::AsarArchive::open`).
    pub fn from_asar(path: impl AsRef<Path>) -> Result<Self> {
        let archive = AsarArchive::open(path.as_ref())
            .with_context(|| format!("failed to open ASAR archive at {}", path.as_ref().display()))?;
        Ok(Self {
            backing: BundleBacking::Asar(Arc::new(archive)),
        })
    }

    /// Opens a read-only asset directory on disk (useful for tests without a 100 MB `.asar`).
    pub fn from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        if !p.is_dir() {
            bail!("bundle directory does not exist: {}", p.display());
        }
        Ok(Self {
            backing: BundleBacking::Dir(p.to_path_buf()),
        })
    }

    /// Constructs a pure-RAM bundle provider from `(relative_path -> bytes)`.
    pub fn from_memory(files: BTreeMap<String, Vec<u8>>) -> Self {
        Self {
            backing: BundleBacking::Memory(Arc::new(files)),
        }
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

    /// Looks up `request_path`, applies bundle/HTML/worker patches in memory, and returns the asset.
    pub fn get_asset(&self, request_path: &str) -> Option<ServedAsset> {
        let clean_rel = sanitize_relative_asset_path(request_path)?;
        let candidates = if let Some(stripped) = clean_rel.strip_prefix("pc-dist/") {
            vec![clean_rel.clone(), stripped.to_string()]
        } else {
            vec![format!("pc-dist/{clean_rel}"), clean_rel.clone()]
        };

        let (matched_rel, raw_bytes) = candidates
            .into_iter()
            .find_map(|cand| self.read_raw_candidate(&cand).map(|b| (cand, b)))?;

        let patched = transform_bundle_asset(&matched_rel, &raw_bytes);
        let content_type = detect_mime(&matched_rel);
        Some(ServedAsset {
            bytes: patched,
            content_type,
        })
    }

    fn read_raw_candidate(&self, rel: &str) -> Option<Vec<u8>> {
        match &self.backing {
            BundleBacking::Asar(archive) => archive.read(rel).map(|slice| slice.to_vec()),
            BundleBacking::Dir(root) => {
                let full = root.join(rel);
                if !full.is_file() {
                    return None;
                }
                std::fs::read(&full).ok()
            }
            BundleBacking::Memory(map) => map.get(rel).cloned(),
        }
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

/// Applies `olaz_core::bundle_patcher::patch_asar_bundle` plus web-specific HTML/Worker transforms.
pub fn transform_bundle_asset(rel_path: &str, raw_bytes: &[u8]) -> Vec<u8> {
    let maybe_patched = patch_asar_bundle(rel_path, raw_bytes);
    let bytes: &[u8] = maybe_patched.as_deref().unwrap_or(raw_bytes);
    let filename = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let lower_name = filename.to_ascii_lowercase();

    if lower_name.ends_with(".html") || lower_name.ends_with(".htm") {
        if let Ok(html) = std::str::from_utf8(bytes) {
            return transform_zalo_html(html).into_bytes();
        }
    }

    if lower_name.starts_with("dal-worker.") && lower_name.ends_with(".js") {
        if let Ok(js) = std::str::from_utf8(bytes) {
            let rewritten = js.replace("zfile://local/", "/__zfile/");
            let mut out = String::with_capacity(WORKER_IMPORT_PREAMBLE.len() + rewritten.len());
            out.push_str(WORKER_IMPORT_PREAMBLE);
            out.push_str(&rewritten);
            return out.into_bytes();
        }
    }

    if lower_name.ends_with(".js") || lower_name.ends_with(".css") {
        if let Ok(text) = std::str::from_utf8(bytes) {
            if text.contains("zfile://local/") {
                return text.replace("zfile://local/", "/__zfile/").into_bytes();
            }
        }
    }

    maybe_patched.unwrap_or_else(|| raw_bytes.to_vec())
}

/// Transforms a Zalo HTML entrypoint (`login.html`, `index.html`, `shared-worker.html`, etc.):
/// 1. Rewrites `zfile://local/` -> `/__zfile/`.
/// 2. Rewrites `<meta http-equiv="Content-Security-Policy">` to permit `'self' blob: data: ws: wss: https:`.
/// 3. Defers Zalo `<script src="...">` tags (`data-olaz-defer-src="..."`) until `window.__olazBootReady`
///    hydrates `IndexedDB` (and unlocks any optional SubtleCrypto PIN Vault).
/// 4. Injects `<script src="/__olaz/crypto_sync.js"></script><script src="/__olaz/bridge_table.js"></script><script src="/__olaz/preload.js"></script>` into `<head>`.
pub fn transform_zalo_html(html: &str) -> String {
    let step1 = html.replace("zfile://local/", "/__zfile/");

    let step2 = if CSP_META_RE.is_match(&step1) {
        CSP_META_RE.replace_all(&step1, BROWSER_CSP_META).into_owned()
    } else {
        step1
    };

    let step3 = SCRIPT_SRC_TAG_RE
        .replace_all(&step2, |caps: &Captures<'_>| {
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

    if let Some(m) = HEAD_OPEN_RE.find(&step3) {
        let mut out = String::with_capacity(step3.len() + HEAD_BOOT_SCRIPTS.len());
        out.push_str(&step3[..m.end()]);
        out.push_str(HEAD_BOOT_SCRIPTS);
        out.push_str(&step3[m.end()..]);
        out
    } else {
        format!("{HEAD_BOOT_SCRIPTS}{step3}")
    }
}
