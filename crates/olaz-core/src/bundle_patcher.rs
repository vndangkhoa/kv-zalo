//! Host script embedding, `__zaloBridgeSpec` generation, 21-script ordered injection,
//! and on-the-fly ASAR JavaScript bundle rewriters.
//!
//! Port of `Sources/Olaz/ZaloRuntime/OfficialClientHost.swift`, `ZaloBridge.swift`,
//! and `BridgeImplementation.swift`.

use std::collections::{BTreeMap, BTreeSet};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::platform::ZaloPlatform;

/// Raw `Sources/Olaz/Resources/bridge-table.json` embedded at compile time.
pub const BRIDGE_TABLE_JSON: &str = include_str!("../../../Sources/Olaz/Resources/bridge-table.json");

// All 19 `Sources/Olaz/HostScripts/*.js` embedded at compile time.
pub const SCRIPT_NETWORK_SHIM: &str = include_str!("../../../Sources/Olaz/HostScripts/network-shim.js");
pub const SCRIPT_LOCAL_MODULES: &str = include_str!("../../../Sources/Olaz/HostScripts/local-modules.js");
pub const SCRIPT_BRIDGE: &str = include_str!("../../../Sources/Olaz/HostScripts/bridge.js");
pub const SCRIPT_STATE_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/state-adapter.js");
pub const SCRIPT_CHAT_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/chat-adapter.js");
pub const SCRIPT_MEDIA_STORE_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/media-store-adapter.js");
pub const SCRIPT_CARD_ACTIONS_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/card-actions-adapter.js");
pub const SCRIPT_STICKER_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/sticker-adapter.js");
pub const SCRIPT_COMPOSER_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/composer-adapter.js");
pub const SCRIPT_MESSAGE_ACTIONS_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/message-actions-adapter.js");
pub const SCRIPT_TRANSFER_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/transfer-adapter.js");
pub const SCRIPT_CONVERSATIONS_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/conversations-adapter.js");
pub const SCRIPT_CONVERSATION_ACTIONS_ADAPTER: &str =
    include_str!("../../../Sources/Olaz/HostScripts/conversation-actions-adapter.js");
pub const SCRIPT_CONTACTS_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/contacts-adapter.js");
pub const SCRIPT_GROUP_CREATE_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/group-create-adapter.js");
pub const SCRIPT_NOTIFICATION_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/notification-adapter.js");
pub const SCRIPT_ZCLOUD_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/zcloud-adapter.js");
pub const SCRIPT_POLL_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/poll-adapter.js");
pub const SCRIPT_CALL_ADAPTER: &str = include_str!("../../../Sources/Olaz/HostScripts/call-adapter.js");

/// `ConsoleHandler.script` from `OfficialClientHost.swift:470-477`.
pub const CONSOLE_HANDLER_SCRIPT: &str = r#"(() => {
  const post = (lvl, a) => { try { webkit.messageHandlers.zlog.postMessage(lvl + ' ' + a.map(x => { try { return typeof x === 'string' ? x : (x instanceof Error ? x.message + ' | ' + (x.stack || '').split('\n').slice(0, 3).join(' < ') : JSON.stringify(x)); } catch { return String(x); } }).join(' ').slice(0, 1200)); } catch {} };
  for (const lvl of ['log','warn','error','info']) { const o = console[lvl]; console[lvl] = (...a) => { post('[' + lvl + ']', a); return o.apply(console, a); }; }
  addEventListener('error', e => post('[uncaught]', [e.message + ' @ ' + e.filename + ':' + e.lineno + ':' + e.colno]));
  addEventListener('unhandledrejection', e => post('[unhandled]', [e.reason instanceof Error ? e.reason : (() => { try { const j = JSON.stringify(e.reason, (k, v) => typeof v === 'string' && v.length > 60 ? v.slice(0, 60) + '…' : v); return j && j !== '{}' ? j.slice(0, 400) : String(e.reason); } catch { return String(e.reason); } })()]));
})();"#;

/// Base worker prelude prepended to `dal-worker.*.js` (`OfficialClientHost.swift:382-437`).
pub const WORKER_PRELUDE_BASE: &str = r#"(() => {
  const fwd = (k, v) => { try { self.postMessage({ __olazLog: k + ' ' + String(v).slice(0, 300) }); } catch {} };
  self.__olazFwd = fwd;
  /*DEBUG_EVAL*/
  const errText = (e) => e ? (e.name || '') + ': ' + (e.message || e) : String(e);
  self.addEventListener('error', (e) => fwd('error', e.message));
  self.addEventListener('unhandledrejection', (e) => fwd('unhandledrejection', errText(e.reason)));
  for (const lvl of ['warn', 'error']) { const o = console[lvl]; console[lvl] = (...a) => { fwd('console.' + lvl, a.map((x) => x instanceof Error ? errText(x) : (typeof x === 'string' ? x : '')).join(' ')); return o.apply(console, a); }; }
  // The sync-v2 sub-worker writes imported messages through its "lite IDB" path, whose
  // encryption-state store needs the DB client (`database-manager.createQueryBuilder`).
  // Zalo's own SQLite-migrated clients never take this path and nothing in a web worker
  // creates the client (Application lifecycle is unsupported there), so create it once after
  // the sub-worker script has loaded — through its official singleton accessor.
  const ensureDatabaseClient = () => {
    try {
      const req = self.__swReq;
      if (!req || !req.c) { fwd('webpack require not exposed', ''); return; }
      for (const id of Object.keys(req.c)) {
        try {
          const d = req.c[id] && req.c[id].exports && req.c[id].exports.default;
          if (d && typeof d.getInstance === 'function' && String(d.getInstance).includes('createQueryBuilder')) { d.getInstance(); fwd('database client ready', id); return; }
        } catch (e) { /* a throwing export getter must not abort the search */ }
      }
      fwd('database client accessor not found', '');
    } catch (e) { fwd('database client setup failed', errText(e)); }
  };
  if (typeof self.importScripts === 'function') {
    const imp = self.importScripts;
    self.importScripts = function (...urls) { const r = imp.apply(this, urls); if (urls.some((u) => /sync-v2-sub-worker/.test(String(u)))) ensureDatabaseClient(); return r; };
  }
  if (typeof IDBDatabase !== 'undefined') {
    const tx = IDBDatabase.prototype.transaction;
    IDBDatabase.prototype.transaction = function (...a) {
      const t = tx.apply(this, a);
      t.addEventListener('abort', () => fwd('idb-abort', errText(t.error)));
      return t;
    };
    for (const m of ['add', 'put']) {
      const o = IDBObjectStore.prototype[m];
      IDBObjectStore.prototype[m] = function (...a) {
        try { const r = o.apply(this, a); r.addEventListener('error', () => fwd('idb-' + m + '-error', this.name + ' ' + errText(r.error))); return r; }
        catch (e) { fwd('idb-' + m + '-throw', this.name + ' ' + errText(e)); throw e; }
      };
    }
    const open = IDBFactory.prototype.open;
    IDBFactory.prototype.open = function (...a) {
      const r = open.apply(this, a);
      r.addEventListener('error', () => fwd('idb-open-error', errText(r.error)));
      r.addEventListener('blocked', () => fwd('idb-open-blocked', ''));
      return r;
    };
  }
})();
"#;

pub const WORKER_DEBUG_EVAL: &str = r#"// Debug builds: a probe worker created from the dev eval channel can run code in this scope.
  self.addEventListener('message', async (e) => {
    const d = e.data;
    if (!d || typeof d !== 'object' || typeof d.__olazEval !== 'string') return;
    e.stopImmediatePropagation();
    try { const r = await (0, eval)(d.__olazEval); self.postMessage({ __olazEvalResult: JSON.stringify(r === undefined ? null : r) }); }
    catch (err) { self.postMessage({ __olazEvalResult: JSON.stringify({ error: errText(err) }) }); }
  });"#;

/// Returns the active `WORKER_PRELUDE` string prepended to `dal-worker.*.js`.
pub fn worker_prelude() -> String {
    if cfg!(debug_assertions) {
        WORKER_PRELUDE_BASE.replace("/*DEBUG_EVAL*/", WORKER_DEBUG_EVAL)
    } else {
        WORKER_PRELUDE_BASE.to_string()
    }
}

/// Entry from `bridge-table.json` mapping a preload member path to an Electron IPC channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcSpecEntry {
    pub channel: String,
    #[serde(rename = "type")]
    pub kind: String,
}

static PARSED_IPC_TABLE: Lazy<BTreeMap<String, IpcSpecEntry>> = Lazy::new(|| {
    let root: Value = serde_json::from_str(BRIDGE_TABLE_JSON)
        .expect("embedded bridge-table.json must be valid JSON");
    let mut out = BTreeMap::new();
    if let Some(table) = root.get("table").and_then(Value::as_object) {
        for (path, entry) in table {
            let Some(channel) = entry.get("channel").and_then(Value::as_str) else {
                continue;
            };
            let Some(kind) = entry.get("type").and_then(Value::as_str) else {
                continue;
            };
            out.insert(
                path.clone(),
                IpcSpecEntry {
                    channel: channel.to_string(),
                    kind: kind.to_string(),
                },
            );
        }
    }
    out
});

/// Returns the parsed preload-member -> IPC channel table from `bridge-table.json`.
pub fn bridge_ipc_table() -> &'static BTreeMap<String, IpcSpecEntry> {
    &PARSED_IPC_TABLE
}

/// Top-level window globals declared by `BridgeImplementation.globals`.
pub const BRIDGE_GLOBALS: &[&str] = &[
    "$zcommon",
    "$znode",
    "$zwindow",
    "$zresource",
    "$zlogger",
    "$zupdater",
    "$zapp",
    "$zdownload",
    "$zdb",
    "$zsessionManager",
    "$zmulti",
    "$zcall",
    "$zsharedWorker",
    "$zelectron",
    "$zelectronNative",
    "$zuri",
    "$zscreencap",
    "$zlogin",
    "$zperf",
    "$zconfig",
    "$zMetric",
    "$zFileManager",
    "$zpc",
    "$zFeatures",
    "$zcloud",
    "$zMsgChannel",
    "$zscript",
    "$zInAppPayment",
    "$capp",
    "$zbackup",
    "$zpreload",
    "$zsqlitebw",
    "$zsafeStorage",
    "$zMigrateDrive",
    "$zTFModels",
    "$znetwork",
    "$zprotocol",
    "$znoti",
    "$zsqlite",
    "__SENTRY_IPC__",
    "MetriczGlobal",
];

/// Builds `window.__zaloBridgeSpec = ...;` matching `ZaloBridge.specScript()` and
/// `BridgeImplementation.install(...)`.
pub fn bridge_spec_script(platform: &ZaloPlatform) -> String {
    let mut globals: BTreeSet<String> = BRIDGE_GLOBALS.iter().map(|&s| s.to_string()).collect();
    let mut members: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut values: BTreeMap<String, Value> = BTreeMap::new();

    let process_arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    };

    // $zenv constants
    for (path, val) in [
        ("$zenv.isPreloaded", json!(true)),
        ("$zenv.isRenderer", json!(true)),
        ("$zenv.platform", json!("darwin")),
        ("$zenv.process_arch", json!(process_arch)),
        ("$zenv.arch", json!(process_arch)),
        ("$znode.os.__info", collect_os_info(platform, process_arch)),
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

    // Sync members
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

    // Async members
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
        "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync",
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
    format!("window.__zaloBridgeSpec = {serialized};")
}

/// Formats an embedded host script with its `//# sourceURL=olaz-<name>.js` footer.
pub fn host_script(name: &str) -> Option<String> {
    let raw = match name {
        "network-shim" => SCRIPT_NETWORK_SHIM,
        "local-modules" => SCRIPT_LOCAL_MODULES,
        "bridge" => SCRIPT_BRIDGE,
        "state-adapter" => SCRIPT_STATE_ADAPTER,
        "chat-adapter" => SCRIPT_CHAT_ADAPTER,
        "media-store-adapter" => SCRIPT_MEDIA_STORE_ADAPTER,
        "card-actions-adapter" => SCRIPT_CARD_ACTIONS_ADAPTER,
        "sticker-adapter" => SCRIPT_STICKER_ADAPTER,
        "composer-adapter" => SCRIPT_COMPOSER_ADAPTER,
        "message-actions-adapter" => SCRIPT_MESSAGE_ACTIONS_ADAPTER,
        "transfer-adapter" => SCRIPT_TRANSFER_ADAPTER,
        "conversations-adapter" => SCRIPT_CONVERSATIONS_ADAPTER,
        "conversation-actions-adapter" => SCRIPT_CONVERSATION_ACTIONS_ADAPTER,
        "contacts-adapter" => SCRIPT_CONTACTS_ADAPTER,
        "group-create-adapter" => SCRIPT_GROUP_CREATE_ADAPTER,
        "notification-adapter" => SCRIPT_NOTIFICATION_ADAPTER,
        "zcloud-adapter" => SCRIPT_ZCLOUD_ADAPTER,
        "poll-adapter" => SCRIPT_POLL_ADAPTER,
        "call-adapter" => SCRIPT_CALL_ADAPTER,
        _ => return None,
    };
    Some(format!("{raw}\n//# sourceURL=olaz-{name}.js"))
}

/// Ordered list of the 21 document-start user scripts (`(name, source)`) matching
/// `OfficialClientHost.swift:86-89`.
pub fn ordered_user_scripts(platform: &ZaloPlatform, trace_workers: bool) -> Vec<(String, String)> {
    let console_src = if trace_workers {
        format!("window.__zaloTraceWorkers = true;\n{CONSOLE_HANDLER_SCRIPT}")
    } else {
        CONSOLE_HANDLER_SCRIPT.to_string()
    };

    let mut scripts = Vec::with_capacity(21);
    scripts.push(("console".to_string(), console_src));
    scripts.push((
        "network-shim".to_string(),
        host_script("network-shim").unwrap(),
    ));
    scripts.push(("bridge-spec".to_string(), bridge_spec_script(platform)));

    for name in [
        "local-modules",
        "bridge",
        "state-adapter",
        "chat-adapter",
        "media-store-adapter",
        "card-actions-adapter",
        "sticker-adapter",
        "composer-adapter",
        "message-actions-adapter",
        "transfer-adapter",
        "conversations-adapter",
        "conversation-actions-adapter",
        "contacts-adapter",
        "group-create-adapter",
        "notification-adapter",
        "zcloud-adapter",
        "poll-adapter",
        "call-adapter",
    ] {
        scripts.push((name.to_string(), host_script(name).unwrap()));
    }

    debug_assert_eq!(scripts.len(), 21);
    scripts
}

/// Linux direct-UI host extension injected between `local-modules` and `bridge` when
/// mounting `OfficialClientHost` (`login.html` / `index.html` / `popup-viewer.html`) directly.
pub const LINUX_DIRECT_UI_SCRIPT: &str = r#"(() => {
  'use strict';
  if (window.__olazLinuxDirectUi) return;
  window.__olazLinuxDirectUi = true;

  const local = window.__zaloLocal || (window.__zaloLocal = {});
  const sync = (p, args) => window.__zaloBridge.callSync(p, args || []);
  const callAsync = (p, args) => window.__zaloBridge.callAsync(p, args || []);
  const sendIPC = (channel, args) => {
    try {
      const p = window.webkit.messageHandlers.zbridge.postMessage({ kind: 'send', channel, args });
      if (p && typeof p.catch === 'function') p.catch(() => {});
    } catch {}
  };
  const postState = (state) => {
    try { window.webkit.messageHandlers.zstate.postMessage(state); } catch {}
  };
  const postDrag = (msg) => {
    try { window.webkit.messageHandlers.__olaz_win_drag.postMessage(msg); } catch {}
  };
  const diag = (s) => {
    try { window.webkit.messageHandlers.zlog.postMessage('[direct-ui] ' + s); } catch {}
  };
  const safe = (fn, d) => {
    try { const v = fn(); return v === undefined ? d : v; } catch { return d; }
  };
  const path = local['$znode.path'];

  // 1. Native clipboard image read/write (`$zelectronNative.getClipboardImage`)
  local['$zelectronNative.getClipboardImage'] = () => {
    const png = safe(() => sync('$zelectronNative.clipboardImagePng', []), null);
    const bytes = png instanceof Uint8Array ? png : (png && png.byteLength ? new Uint8Array(png) : new Uint8Array(0));
    return {
      isEmpty: () => !bytes || bytes.length === 0,
      toPNG: () => bytes,
      toJPEG: () => bytes,
    };
  };
  local['$zelectronNative.writeImageToClipboard'] = (buf) => {
    if (!buf) return;
    const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
    if (bytes.length) safe(() => sync('$zelectronNative.clipboardWriteImagePng', [bytes]), null);
  };

  // 2. `$zFileManager` photo picker, blob helpers, and path trust
  const DEFAULT_PHOTO_EXTS = ['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'jxl', 'heic'];
  local['$zFileManager.getPhotoWithPaths'] = async (title, defaultPath, opts = {}) => {
    const exts = (opts && Array.isArray(opts.extensions) && opts.extensions.length) ? opts.extensions : DEFAULT_PHOTO_EXTS;
    const res = await window.$zwindow.showOpenDialog({
      title: title || '',
      defaultPath: defaultPath || '',
      filters: [{ name: 'Images', extensions: exts }],
      properties: ['multiSelections', 'openFile'],
    });
    const r = (res && Array.isArray(res.filePaths)) ? res.filePaths : [];
    if (!r.length) throw new Error("can't get photoPaths!");
    const photoResults = await Promise.all(r.map(async (p) => {
      try {
        const st = await window.$znode.fs.stat(p);
        const name = path ? path.basename(p) : String(p).split('/').pop();
        const dot = name.lastIndexOf('.');
        const ext = dot > 0 ? name.slice(dot + 1).toLowerCase() : '';
        if (!exts.map((x) => String(x).toLowerCase()).includes(ext)) {
          return { isSuccess: false, data: { errCode: -2, message: 'invalid extension: ' + ext } };
        }
        if (opts && typeof opts.checkPhotoFileErr === 'function') {
          const err = opts.checkPhotoFileErr({ name, size: st.size });
          if (err) return { isSuccess: false, data: err };
        }
        if (!opts || !opts.maxSizePhoto || st.size <= opts.maxSizePhoto) {
          const ab = await window.$zFileManager.getFileArrayBuffer(p);
          // Leave `data.path` empty when `content` bytes are already loaded so `onUploadPhotoClicked`
          // keeps the `Blob`/`File` in Zalo's native browser upload fast-path (`photoPaths` still
          // carries `r` for `setLastFileUploadPath`).
          return { isSuccess: true, data: { name, path: '', size: st.size, content: new Uint8Array(ab), needReadBlob: false } };
        }
        return { isSuccess: true, data: { name, path: p, size: st.size, needReadBlob: true } };
      } catch (e) {
        return { isSuccess: false, data: { errCode: -1, message: e } };
      }
    }));
    return { photoPaths: r, photoResults };
  };
  local['$zFileManager.getFileBlob'] = async (p) => {
    const ab = await window.$zFileManager.getFileArrayBuffer(p);
    return new Blob([ab]);
  };
  local['$zFileManager.getFileBlobURL'] = async (p) => {
    const blob = await local['$zFileManager.getFileBlob'](p);
    return URL.createObjectURL(blob);
  };
  local['$zFileManager.trustBlob'] = (b) => {
    if (b && typeof b.path === 'string' && b.path) {
      return safe(() => window.$zFileManager.trustPathFromBlob(b.path), true);
    }
    return true;
  };
  local['$zFileManager.writeBlobToPath'] = async (destPath, blob) => {
    if (!destPath || !blob) return;
    const ab = typeof blob.arrayBuffer === 'function'
      ? await blob.arrayBuffer()
      : await new Response(blob).arrayBuffer();
    await callAsync('$zFileManager.writeBufferToPath', [String(destPath), new Uint8Array(ab)]);
  };

  // 2b. `$zFileManager` / `$zpc.zfile` / `$zpc.zprocess` / `$zcloud` streaming chunk upload,
  //     checksum, E2EE/pcloud cipher transforms, and file-noise helpers.
  const toU8 = (v) => {
    if (!v) return new Uint8Array(0);
    if (v instanceof Uint8Array) return v;
    if (v instanceof ArrayBuffer) return new Uint8Array(v);
    if (ArrayBuffer.isView(v)) return new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
    return new Uint8Array(0);
  };
  const toExactArrayBuffer = (u8) => (
    u8.byteOffset === 0 && u8.byteLength === u8.buffer.byteLength
      ? u8.buffer
      : u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength)
  );
  const concatManyU8 = (parts) => {
    if (!parts.length) return new Uint8Array(0);
    if (parts.length === 1) return parts[0];
    let total = 0;
    for (let i = 0; i < parts.length; i++) total += parts[i].byteLength;
    const out = new Uint8Array(total);
    let off = 0;
    for (let i = 0; i < parts.length; i++) {
      out.set(parts[i], off);
      off += parts[i].byteLength;
    }
    return out;
  };
  const u8ToB64 = (u8) => {
    let s = '';
    for (let i = 0; i < u8.byteLength; i++) s += String.fromCharCode(u8[i]);
    return window.btoa(s);
  };
  const uploadCipherCache = new Map();
  const uploadEncHashCache = new Map();
  const transformUploadSlice = (sessionId, rawU8, secure, isLastPart) => {
    if (!secure || !secure.cryptography || (secure.type !== 'e2ee' && secure.type !== 'pcloud')) {
      return { bytes: rawU8, hasSecure: false, tag: null, checksumEncr: undefined };
    }
    const cfg = secure.cryptography;
    const keyU8 = toU8(cfg.key);
    const ivU8 = toU8(cfg.iv);
    const cacheKey = `${sessionId || ''}:${cfg.alg || ''}:${u8ToB64(keyU8)}:${u8ToB64(ivU8)}`;
    let state = uploadCipherCache.get(cacheKey);
    if (!state) {
      state = { cipher: null, pending: new Uint8Array(0) };
      uploadCipherCache.set(cacheKey, state);
    }
    let encBytes;
    let tag = null;
    if (cfg.type === 'block') {
      const blockSize = Number(cfg.blockSize) || 4096;
      const work = state.pending.byteLength ? concatManyU8([state.pending, rawU8]) : rawU8;
      const fullBlocks = Math.floor(work.byteLength / blockSize);
      const outParts = [];
      for (let b = 0; b < fullBlocks; b++) {
        const blk = work.subarray(b * blockSize, (b + 1) * blockSize);
        const c = window.$znode.crypto.createCipheriv(cfg.alg, keyU8, ivU8);
        const u = toU8(c.update(blk));
        const f = toU8(c.final());
        outParts.push(f.byteLength ? concatManyU8([u, f]) : u);
      }
      const rem = work.subarray(fullBlocks * blockSize);
      if (isLastPart) {
        if (rem.byteLength) outParts.push(rem.slice());
        state.pending = new Uint8Array(0);
        uploadCipherCache.delete(cacheKey);
        tag = new Uint8Array(0);
      } else {
        state.pending = rem.slice();
      }
      encBytes = concatManyU8(outParts);
    } else {
      if (!state.cipher) {
        state.cipher = window.$znode.crypto.createCipheriv(cfg.alg, keyU8, ivU8, { authTagLength: 16 });
      }
      const u = toU8(state.cipher.update(rawU8));
      if (isLastPart) {
        const f = toU8(state.cipher.final());
        encBytes = f.byteLength ? concatManyU8([u, f]) : u;
        tag = safe(() => toU8(state.cipher.getAuthTag()), null) || new Uint8Array(0);
        cfg.tag = tag;
        uploadCipherCache.delete(cacheKey);
      } else {
        encBytes = u;
      }
    }
    let checksumEncr;
    if (secure.type === 'pcloud') {
      const hk = String(sessionId || cacheKey);
      let h = uploadEncHashCache.get(hk);
      if (!h) {
        h = window.$znode.crypto.createHash('md5');
        uploadEncHashCache.set(hk, h);
      }
      if (encBytes.byteLength) h.update(encBytes);
      if (isLastPart) {
        checksumEncr = h.digest('hex');
        uploadEncHashCache.delete(hk);
      }
    }
    return { bytes: encBytes, hasSecure: true, tag, checksumEncr };
  };
  const readChunksFromPath = async (sessionId, fileObj, chunkSize, partCfg, hashSink, secure, isLastPart, checkNoise) => {
    const srcPath = (fileObj && (fileObj.path || fileObj.filePath || fileObj.folderPath)) ? String(fileObj.path || fileObj.filePath || fileObj.folderPath) : '';
    if (!srcPath) throw new Error('Invalid upload file path');
    const fileSize = Number(fileObj.size) || 0;
    const cs = Math.max(1, Number(chunkSize) || (2 * 1024 * 1024));
    const totalChunks = Math.max(1, Math.ceil(fileSize / cs));
    const startChunkId = (partCfg && Number(partCfg.startChunkId)) || 1;
    const endChunkId = (partCfg && Number(partCfg.endChunkId)) || totalChunks;
    const byteStart = (startChunkId - 1) * cs;
    const byteEnd = Math.min(fileSize, endChunkId * cs);
    const expectedLen = Math.max(0, byteEnd - byteStart);
    const sliceAb = expectedLen > 0
      ? await callAsync('$zFileManager.readFileSlice', [srcPath, byteStart, expectedLen, !!checkNoise])
      : new ArrayBuffer(0);
    let rawU8 = toU8(sliceAb);
    if (rawU8.byteLength < expectedLen) {
      const padded = new Uint8Array(expectedLen);
      padded.set(rawU8, 0);
      rawU8 = padded;
    }
    if (hashSink && rawU8.byteLength) {
      const plainAb = toExactArrayBuffer(rawU8);
      if (hashSink.ref && typeof hashSink.ref.append === 'function') {
        hashSink.ref.append(plainAb);
        hashSink.size = (Number(hashSink.size) || 0) + rawU8.byteLength;
      } else if (hashSink.hash && typeof hashSink.hash.append === 'function') {
        hashSink.hash.append(plainAb);
        hashSink.size = (Number(hashSink.size) || 0) + rawU8.byteLength;
      }
    }
    const { bytes: outU8, hasSecure, tag, checksumEncr } = transformUploadSlice(
      sessionId,
      rawU8,
      secure,
      !!isLastPart
    );
    const outAb = toExactArrayBuffer(outU8);
    const chunks = [];
    for (let cid = startChunkId; cid <= endChunkId; cid++) {
      const off = (cid - startChunkId) * cs;
      const rem = Math.max(0, outAb.byteLength - off);
      const len = cid === endChunkId ? rem : Math.min(cs, rem);
      chunks.push(new DataView(outAb, Math.min(off, outAb.byteLength), len));
    }
    if (isLastPart && hasSecure) {
      const res = { chunks, noChunk: totalChunks, tag: new Uint8Array(tag || 0) };
      if (checksumEncr !== undefined) res.checksumEncr = checksumEncr;
      return res;
    }
    return { chunks, noChunk: totalChunks };
  };

  local['$zFileManager.readChunksArrayObject'] = (sessionId, fileObj, chunkSize, range, secureOptions, isLastPart) =>
    readChunksFromPath(
      sessionId,
      fileObj,
      chunkSize,
      range || {},
      range && range.refHash ? range.refHash : null,
      secureOptions,
      !!isLastPart,
      false
    );
  local['$zpc.zprocess.getActiveResourcesInfo'] = () => [];
  local['$zpc.zfile.isTrustPath'] = async () => true;
  local['$zpc.zfile.findNoiseSignature'] = (p) => callAsync('$zFileManager.zfileFindNoiseSignature', [String(p || '')]);
  local['$zpc.zfile.getChecksum'] = async (p) => {
    try { return await callAsync('$zFileManager.zfileGetChecksum', [String(p || '')]); }
    catch { return undefined; }
  };
  local['$zpc.zfile.compareChecksum'] = async (p, expected) => {
    if (!p || !expected) return false;
    return !!(await callAsync('$zFileManager.testChecksum', [String(p), String(expected), true]));
  };
  local['$zpc.zfile.readFileByChunksForUpload'] = (sessionId, fileObj, opts = {}) => {
    const part = (opts && opts.part) || {};
    const cs = Number(part.chunkSize) || (2 * 1024 * 1024);
    const totalChunks = Math.max(1, Math.ceil((Number(fileObj && fileObj.size) || 0) / cs));
    const endChunkId = Number(part.endChunkId) || totalChunks;
    const isLastPart = part.isLastPart !== undefined ? !!part.isLastPart : (endChunkId >= totalChunks);
    const p = String((fileObj && fileObj.path) || '');
    const checkNoise = p.includes('ZaloDownloads') && p.includes('fileNoise');
    return readChunksFromPath(
      sessionId,
      fileObj,
      cs,
      part,
      opts ? opts.hash : null,
      opts ? opts.secure : null,
      isLastPart,
      checkNoise
    );
  };

  // 2c. `$zcloud` main-process file/cache helpers
  local['$zcloud.getHeap'] = async () => (typeof performance !== 'undefined' && performance.memory && performance.memory.usedJSHeapSize) || 0;
  local['$zcloud.writeHeap'] = () => {};
  local['$zcloud.AppendLog'] = async () => {};
  local['$zcloud.writeBufferToFile'] = async (arg) => {
    if (!arg || !arg.outputPath) return;
    await callAsync('$zFileManager.writeBufferToPath', [String(arg.outputPath), toU8(arg.buffer)]);
  };
  local['$zcloud.removeFile'] = async (p) => {
    if (!p) return;
    try { await callAsync('$znode.fs.rm', [String(p), { force: true, recursive: true }]); } catch {}
  };
  local['$zcloud.matchChecksum'] = async (arg) => {
    if (!arg || !arg.filePath || !arg.checksum) return false;
    return !!(await callAsync('$zFileManager.testChecksum', [String(arg.filePath), String(arg.checksum), false]));
  };
  local['$zcloud.cloneAndMatchChecksum'] = async (arg) => {
    if (!arg || !arg.filePath || !arg.checksum) return '';
    return await callAsync('$zFileManager.zcloudCloneAndMatchChecksum', [String(arg.checksum), String(arg.filePath)]);
  };
  local['$zcloud.denoiseFile'] = async (arg) => {
    if (!arg || !arg.src || !arg.dest) return;
    await callAsync('$zFileManager.zcloudDenoiseFile', [String(arg.src), String(arg.dest), String(arg.checksum || '')]);
  };
  local['$zcloud.noiseFile'] = async (arg) => {
    if (!arg || !arg.src || !arg.userId || !arg.checksum) return { savePath: '' };
    const base = safe(() => sync('$zelectron.appGetPath', ['userData']), '') || '';
    const sub = arg.isGroup ? 'group/' : '';
    const dest = `${base}/ZaloDownloads/${arg.userId}/fileNoise/${sub}${arg.checksum}`;
    return await callAsync('$zFileManager.zcloudNoiseFile', [String(arg.src), dest, String(arg.checksum)]);
  };

  // 2d. Emit `XMLHttpRequest.upload` progress/load events on `ProxiedXHR`
  if (typeof window.XMLHttpRequest === 'function'
      && window.XMLHttpRequest.prototype
      && !window.XMLHttpRequest.prototype.__olazUploadEvents) {
    const origSend = window.XMLHttpRequest.prototype.send;
    const bodyByteLen = (b) => {
      if (b == null) return 0;
      if (typeof b === 'string') return new TextEncoder().encode(b).byteLength;
      if (b instanceof Blob) return b.size;
      if (b instanceof ArrayBuffer) return b.byteLength;
      if (ArrayBuffer.isView(b)) return b.byteLength;
      return 0;
    };
    const emitUpload = (up, type, loaded, total) => {
      if (!up) return;
      let ev;
      try {
        ev = new ProgressEvent(type, { lengthComputable: total > 0, loaded, total });
      } catch {
        ev = new Event(type);
      }
      safe(() => up.dispatchEvent(ev), null);
      const handler = up['on' + type];
      if (typeof handler === 'function') safe(() => handler.call(up, ev), null);
    };
    window.XMLHttpRequest.prototype.send = function (body) {
      const up = this && this.upload;
      const total = bodyByteLen(body);
      if (up && total > 0 && typeof this.addEventListener === 'function') {
        emitUpload(up, 'loadstart', 0, total);
        let settled = false;
        const finish = (terminalType, loadedBytes) => {
          if (settled) return;
          settled = true;
          if (terminalType === 'load') emitUpload(up, 'progress', loadedBytes, total);
          emitUpload(up, terminalType, loadedBytes, total);
          emitUpload(up, 'loadend', loadedBytes, total);
        };
        this.addEventListener('load', () => finish('load', total), { once: true });
        this.addEventListener('error', () => finish('error', 0), { once: true });
        this.addEventListener('abort', () => finish('abort', 0), { once: true });
      }
      return origSend.apply(this, arguments);
    };
    window.XMLHttpRequest.prototype.__olazUploadEvents = true;
  }

  // 3. `$znode.nativelibs.zfile` (Settings -> Data Management disk info + folder copy)
  local['$znode.nativelibs.zfile'] = () => ({
    canRead: (p) => safe(() => sync('$znode.nativelibs.zfileAddon.canRead', [String(p)]), true),
    canWrite: (p) => safe(() => sync('$znode.nativelibs.zfileAddon.canWrite', [String(p)]), true),
    canReadAndWrite: (p) => safe(() => sync('$znode.nativelibs.zfileAddon.canReadAndWrite', [String(p)]), true),
    getInfo: (p) => callAsync('$znode.nativelibs.zfileAddon.getInfo', [String(p || '/')]),
    getDiskInfo: (p) => callAsync('$znode.nativelibs.zfileAddon.getDiskInfo', [String(p || '/')]),
    diskInfo: async () => {
      const u = safe(() => sync('$znode.nativelibs.fileUtilsAddon.getDiskUsage', ['/']), null) || {};
      const totalSpace = Number(u.total || u.totalSpace || 100 * 1024 * 1024 * 1024);
      const freeSpace = Number(u.free || u.available || u.freeSpace || 50 * 1024 * 1024 * 1024);
      const root = { label: '/', mountpoint: '/', totalSpace, freeSpace, total: totalSpace, free: freeSpace };
      return new Proxy({ '/': root }, {
        get: (t, k) => (typeof k === 'string' && k.startsWith('/')) ? Object.assign({}, root, { mountpoint: '/', label: '/' }) : t[k],
      });
    },
    copyFolder: (src, dst, cb) => callAsync('$znode.nativelibs.zfileAddon.copyFolder', [String(src || ''), String(dst || '')]).then((r) => {
      if (typeof cb === 'function') cb(null, r);
      return r;
    }),
    cancelCopy: () => callAsync('$znode.nativelibs.zfileAddon.cancelCopy', []),
  });

  // 4. `$zcall` preload serialization + incoming call alert WITHOUT suppressing native modals
  const packCallMsg = (message) => {
    const optional = !!(message && message._optional);
    let body = message;
    if (optional) { body = Object.assign({}, message); delete body._optional; }
    const json = JSON.stringify(body === undefined ? null : body);
    return [json === undefined ? 'null' : json, optional];
  };
  let lastIncomingCallAt = 0;
  local['$zcall.sendDataToNative'] = (message) => {
    try {
      const control = message && message.type === 'control' ? message.data : null;
      if (control && control.act_type === 'voip' && (control.act === 'request' || control.act === 'group_request')) {
        if (Date.now() - lastIncomingCallAt >= 2000) {
          lastIncomingCallAt = Date.now();
          const data = (control.data && typeof control.data === 'object') ? control.data : {};
          const caller = control._caller || {};
          const uid = String(data.uidN || data.fromUid || data.fromId || data.uid || '');
          postState({
            page: 'main',
            call: {
              kind: 'incoming',
              uid,
              name: String(caller.name || data.groupName || data.dName || uid),
              caller: String(caller.name || ''),
              avatar: String(caller.avatar || data.groupAvatar || ''),
              group: control.act === 'group_request',
              busy: !!control.inCallStatus,
            },
          });
        }
      }
    } catch {}
    sendIPC('call-send-to-native', packCallMsg(message));
  };
  local['$zcall.initCall'] = (message) => {
    sendIPC('call-init', packCallMsg(message));
  };
  local['$znode.nativelibs.zcallAddon'] = { getListDevices: () => '[]' };

  // 5. Rewrite raw `file:///...` media/image subresources to `zfile://local/...`
  const rewriteFileUrl = (u) => (typeof u === 'string' && u.startsWith('file:'))
    ? ('zfile://local/' + u.replace(/^file:\/+/, ''))
    : u;
  const patchSrcProp = (Ctor) => {
    if (!Ctor || !Ctor.prototype) return;
    const desc = Object.getOwnPropertyDescriptor(Ctor.prototype, 'src');
    if (!desc || !desc.set || desc.set.__olazZfile) return;
    const origSet = desc.set;
    const wrappedSet = function (val) { return origSet.call(this, rewriteFileUrl(val)); };
    wrappedSet.__olazZfile = true;
    Object.defineProperty(Ctor.prototype, 'src', Object.assign({}, desc, { set: wrappedSet }));
  };
  patchSrcProp(window.HTMLImageElement);
  patchSrcProp(window.HTMLMediaElement);
  patchSrcProp(window.HTMLSourceElement);
  const origSetAttr = Element.prototype.setAttribute;
  Element.prototype.setAttribute = function (name, val) {
    if (typeof name === 'string' && name.toLowerCase() === 'src') {
      const tag = this.tagName;
      if (tag === 'IMG' || tag === 'AUDIO' || tag === 'VIDEO' || tag === 'SOURCE') {
        val = rewriteFileUrl(val);
      }
    }
    return origSetAttr.call(this, name, val);
  };
  if (typeof window.Audio === 'function' && !window.Audio.__olazZfile) {
    const NativeAudio = window.Audio;
    const WrappedAudio = function (src) {
      return arguments.length ? new NativeAudio(rewriteFileUrl(src)) : new NativeAudio();
    };
    WrappedAudio.prototype = NativeAudio.prototype;
    Object.setPrototypeOf(WrappedAudio, NativeAudio);
    WrappedAudio.__olazZfile = true;
    window.Audio = WrappedAudio;
  }

  // 6. Capture-phase `drop` listener: leave regular dropped files/images as pure disk-backed
  //    browser `File` objects (without `.path`) so Zalo uses its native `FileReader` / WebKit
  //    upload pipeline; only attach POSIX `.path` when a dropped entry is a directory (`isDirectory`).
  window.addEventListener('drop', (ev) => {
    const dt = ev.dataTransfer;
    if (!dt || !dt.items) return;
    const uriText = safe(() => dt.getData('text/uri-list'), '') || '';
    const uriPaths = uriText.split(/\r?\n/)
      .filter((l) => l.startsWith('file://'))
      .map((u) => { try { return decodeURIComponent(new URL(u).pathname); } catch { return ''; } })
      .filter(Boolean);
    const pool = uriPaths.length ? uriPaths : (Array.isArray(window.__olazLastDragPaths) ? window.__olazLastDragPaths : []);
    if (!pool.length) return;
    const attachDirPath = (f, fallback) => {
      if (f && !f.path) {
        const matched = pool.find((p) => p.endsWith('/' + f.name) || p === fallback) || fallback;
        if (matched) {
          try { Object.defineProperty(f, 'path', { value: matched, configurable: true }); } catch {}
        }
      }
      return f;
    };
    let idx = 0;
    for (let i = 0; i < dt.items.length; i++) {
      const it = dt.items[i];
      if (!it || it.kind !== 'file') continue;
      const fallback = pool[idx++] || '';
      const entry = safe(() => (typeof it.webkitGetAsEntry === 'function' ? it.webkitGetAsEntry() : null), null);
      if (entry && entry.isDirectory && typeof it.getAsFile === 'function' && !it.getAsFile.__olazPath) {
        const origGet = it.getAsFile.bind(it);
        const wrapped = function () { return attachDirPath(origGet(), fallback); };
        wrapped.__olazPath = true;
        try { it.getAsFile = wrapped; } catch {}
        if (dt.files && dt.files[i]) attachDirPath(dt.files[i], fallback);
      }
    }
  }, true);

  // 7. Frameless window drag, resize edges, and double-click maximize (`__olaz_win_drag`)
  const INTERACTIVE_SEL = 'button, input, textarea, select, a, label, [role="button"], .no-drag, .clickable, .titlebar__btns, .titlebar__menu__btn, .titlebar__menu__btnPreviewPhoto, i.btn, .z--btn, .z--btn--v2, #contact-search-input';
  const DRAG_SEL = '.title-drag, #titleBar, .login-title-bar, .zlogin-titlebar, .titlebar, .draggable';
  const detectResizeEdge = (e) => {
    const b = 5;
    const w = window.innerWidth, h = window.innerHeight;
    if (w < 120 || h < 120) return null;
    const x = e.clientX, y = e.clientY;
    const left = x >= 0 && x < b, right = x <= w && x > w - b;
    const top = y >= 0 && y < b, bottom = y <= h && y > h - b;
    if (top && left) return 'north-west';
    if (top && right) return 'north-east';
    if (bottom && left) return 'south-west';
    if (bottom && right) return 'south-east';
    if (top) return 'north';
    if (bottom) return 'south';
    if (left) return 'west';
    if (right) return 'east';
    if (e.target && e.target.closest && e.target.closest('.titlebar__resize')) return 'north';
    return null;
  };
  window.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    const t = e.target;
    if (!t || typeof t.closest !== 'function') return;
    const edge = detectResizeEdge(e);
    if (edge && !t.closest(INTERACTIVE_SEL)) {
      e.preventDefault();
      postDrag({
        action: 'resize',
        edge,
        button: 1,
        screenX: Math.round(e.screenX),
        screenY: Math.round(e.screenY),
        time: Math.round(e.timeStamp || 0),
      });
      return;
    }
    if (t.closest(INTERACTIVE_SEL)) return;
    if (!t.closest(DRAG_SEL)) return;
    if (e.detail === 2) {
      postDrag({ action: 'toggle-maximize' });
      return;
    }
    if (e.detail === 1) {
      postDrag({
        action: 'move',
        button: 1,
        screenX: Math.round(e.screenX),
        screenY: Math.round(e.screenY),
        time: Math.round(e.timeStamp || 0),
      });
    }
  }, false);

  // 8. Page lifecycle & webpack module hooks (`login.html` vs `index.html`)
  const page = location.pathname.split('/').pop();
  if (page === 'login.html') {
    const notifyLogin = () => postState({ page: 'login' });
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', notifyLogin, { once: true });
    else notifyLogin();
    return;
  }
  if (page !== 'index.html') return;

  const eachExport = (req, visit) => {
    for (const [id, mod] of Object.entries((req && req.c) || {})) {
      const ex = mod && mod.exports;
      if (!ex) continue;
      visit(ex, id);
      if (typeof ex === 'object') {
        for (const k of Object.keys(ex)) {
          try { if (ex[k]) visit(ex[k], id); } catch {}
        }
      }
    }
  };
  const diRegistrations = (req) => {
    const out = [];
    const seen = new Set();
    eachExport(req, (ex) => {
      if (!ex || typeof ex !== 'object' || seen.has(ex)) return;
      const registry = ex._registry && ex._registry._registryMap;
      if (typeof ex.resolve !== 'function' || !(registry instanceof Map)) return;
      seen.add(ex);
      for (const regs of registry.values()) for (const r of regs || []) out.push(r);
    });
    return out;
  };
  window.__zaloEachExport = eachExport;
  window.__zaloDI = diRegistrations;

  // Fallback global-ID resolver for `patch_sync_global_id_resolver` in `main-startup.*.js`
  let gidSvc = null;
  window.__swzGlobalIdFallback = async (uids) => {
    const req = window.__zaloRequire;
    if (!gidSvc && req) {
      eachExport(req, (ex) => {
        if (gidSvc || !ex || typeof ex !== 'object') return;
        try {
          if (ex._userIdMap instanceof Map && ex._globalIdMap instanceof Map && typeof ex.getGlobalId === 'function' && typeof ex.getGlobalIdFromServer === 'function') {
            gidSvc = ex;
          }
        } catch {}
      });
    }
    const ids = Array.isArray(uids) ? [...new Set(uids.filter((u) => typeof u === 'string' && u && !u.startsWith('g')))] : [];
    const out = {};
    if (!gidSvc || !ids.length) return out;
    for (let i = 0; i < ids.length; i += 100) {
      const part = ids.slice(i, i + 100);
      const m = await gidSvc.getGlobalId(part);
      for (const id of part) if (m && typeof m[id] === 'string' && m[id]) out[id] = m[id];
    }
    return out;
  };

  const withRequire = (cb) => {
    const tryCapture = () => {
      if (window.__zaloRequire) return cb(window.__zaloRequire);
      const jsonp = window.webpackJsonp;
      if (!jsonp || jsonp.push === Array.prototype.push) return false;
      const id = '__olaz_probe_' + Date.now();
      jsonp.push([[id], { [id]: (_m, _e, r) => { window.__zaloRequire = r; } }, [[id]]]);
      return window.__zaloRequire ? cb(window.__zaloRequire) : false;
    };
    if (tryCapture() === false) {
      const t = setInterval(() => { if (tryCapture() !== false) clearInterval(t); }, 200);
      setTimeout(() => clearInterval(t), 60000);
    }
  };
  window.__zaloWithRequire = withRequire;

  withRequire((req) => {
    // Keep session on IndexedDB (`waitForDBMigrate` -> `{ enable: false }`)
    const patchDb = () => {
      let done = false;
      for (const reg of diRegistrations(req)) {
        for (const c of [reg.instance, reg.provider && reg.provider.useClass, reg.provider]) {
          const proto = typeof c === 'function' ? c.prototype : (c && typeof c === 'object' ? Object.getPrototypeOf(c) : null);
          if (!proto || typeof proto.waitForDBMigrate !== 'function' || proto.waitForDBMigrate.__olaz) continue;
          const original = proto.waitForDBMigrate;
          proto.waitForDBMigrate = function (session, config, diskConfig) {
            diag('db: SQLite migration disabled (IndexedDB active)');
            return original.call(this, session, Object.assign({}, config || {}, { enable: false }), diskConfig);
          };
          proto.waitForDBMigrate.__olaz = true;
          done = true;
        }
      }
      return done;
    };
    if (!patchDb()) {
      const t = setInterval(() => { if (patchDb()) clearInterval(t); }, 150);
      setTimeout(() => clearInterval(t), 60000);
    }

    // Pin `image_resizer.enable_libvips_macos = 0` so photo uploads use Web Worker OffscreenCanvas
    const pinResizer = () => {
      let pinned = false;
      eachExport(req, (ex) => {
        if (pinned || !ex || typeof ex !== 'object' || ex === window.$capp) return;
        try {
          if (Object.prototype.hasOwnProperty.call(ex, 'stickerUrl') && Object.prototype.hasOwnProperty.call(ex, 'sendToMeId')) {
            if (!ex.__olazResizer) {
              let value = ex.image_resizer;
              const patched = (v) => (v && typeof v === 'object' && !Array.isArray(v))
                ? Object.assign({}, v, { enable_libvips_macos: 0 })
                : v;
              Object.defineProperty(ex, 'image_resizer', {
                configurable: true,
                enumerable: true,
                get: () => patched(value),
                set: (v) => { value = v; },
              });
              Object.defineProperty(ex, '__olazResizer', { value: true });
            }
            pinned = true;
          }
        } catch {}
      });
      return pinned;
    };
    if (!pinResizer()) {
      const t = setInterval(() => { if (pinResizer()) clearInterval(t); }, 400);
      setTimeout(() => clearInterval(t), 60000);
    }

    // Attach Redux store once for user identity & window transition notification (event-driven, no polling)
    const attachStore = () => {
      let store = null;
      eachExport(req, (ex) => {
        if (!store && ex && typeof ex === 'object' && typeof ex.getState === 'function' && typeof ex.subscribe === 'function') {
          try {
            const st = ex.getState();
            if (st && st.user && st.conversations) store = ex;
          } catch {}
        }
      });
      if (!store) return false;
      window.__zaloStore = store;
      let lastKey = '';
      const publishUser = () => {
        const u = (store.getState() || {}).user || {};
        const user = {
          userId: u.userId || '',
          displayName: u.displayName || '',
          zaloName: u.zaloName || '',
          avatar: u.avatar || '',
          phoneNumber: u.phoneNumber || '',
        };
        const key = JSON.stringify(user);
        if (key !== lastKey) {
          lastKey = key;
          postState({ page: 'main', user });
        }
      };
      store.subscribe(publishUser);
      publishUser();
      return true;
    };
    if (!attachStore()) {
      const t = setInterval(() => { if (attachStore()) clearInterval(t); }, 400);
      setTimeout(() => clearInterval(t), 60000);
    }
    return true;
  });
})();
//# sourceURL=olaz-linux-direct-ui.js"#;

/// Lean, zero-polling Web `Notification` -> `$znoti.createNotification` shim for direct-UI mode.
pub const LINUX_DIRECT_NOTIFICATION_SCRIPT: &str = r#"(() => {
  'use strict';
  if (window.__olazDirectNotification) return;
  if (location.pathname.split('/').pop() !== 'index.html') return;
  window.__olazDirectNotification = true;

  let permission = 'granted';
  const instances = new Map();
  const earlyEvents = new Map();
  let wired = false;

  const wire = () => {
    if (wired || typeof window.$znoti === 'undefined') return;
    wired = true;
    const on = (subscribe, type) => subscribe((_event, payload) => {
      const p = typeof payload === 'string' ? { id: payload } : (payload || {});
      const inst = instances.get(p.id);
      if (!inst) {
        if (!earlyEvents.has(p.id)) earlyEvents.set(p.id, []);
        earlyEvents.get(p.id).push(type);
        setTimeout(() => earlyEvents.delete(p.id), 10000);
        return;
      }
      inst._emit(type);
      if (type === 'close') instances.delete(p.id);
    });
    on((cb) => window.$znoti.onNotificationClick(cb), 'click');
    on((cb) => window.$znoti.onNotificationShow(cb), 'show');
    on((cb) => window.$znoti.onNotificationClose(cb), 'close');
  };

  class ZaloNotification extends EventTarget {
    constructor(title, options = {}) {
      super();
      this.title = String(title == null ? '' : title);
      this.body = options.body == null ? '' : String(options.body);
      this.tag = options.tag == null ? '' : String(options.tag);
      this.icon = options.icon == null ? '' : String(options.icon);
      this.silent = options.silent == null ? null : !!options.silent;
      this.requireInteraction = !!options.requireInteraction;
      this.data = options.data == null ? null : options.data;
      this.onclick = null; this.onshow = null; this.onclose = null; this.onerror = null;
      this._id = null; this._closeRequested = false;
      wire();
      Promise.resolve(window.$znoti.createNotification(this.title, {
        body: this.body, icon: this.icon, tag: this.tag, silent: this.silent === true,
      })).then((id) => {
        if (!id) { this._emit('error'); return; }
        this._id = id;
        instances.set(id, this);
        for (const type of earlyEvents.get(id) || []) {
          this._emit(type);
          if (type === 'close') instances.delete(id);
        }
        earlyEvents.delete(id);
        if (this._closeRequested) window.$znoti.closeNotification(id);
      }).catch(() => { this._emit('error'); });
    }
    close() {
      this._closeRequested = true;
      if (this._id) window.$znoti.closeNotification(this._id);
    }
    _emit(type) {
      const event = new Event(type);
      try { this.dispatchEvent(event); } catch {}
      const handler = this['on' + type];
      if (typeof handler === 'function') {
        try { handler.call(this, event); } catch (e) { setTimeout(() => { throw e; }); }
      }
    }
    static get permission() { return permission; }
    static get maxActions() { return 0; }
    static requestPermission(cb) {
      if (typeof cb === 'function') { try { cb(permission); } catch {} }
      return Promise.resolve(permission);
    }
  }
  Object.defineProperty(window, 'Notification', { value: ZaloNotification, writable: true, configurable: true });
})();
//# sourceURL=olaz-direct-notification.js"#;

/// Lean 7-script user-script list for mounting `OfficialClientHost` directly as the visible UI
/// (matching `vndangkhoa/zalo-linux` with zero headless Redux polling or modal suppression).
pub fn direct_ui_user_scripts(platform: &ZaloPlatform, trace_workers: bool) -> Vec<(String, String)> {
    let console_src = if trace_workers {
        format!("window.__zaloTraceWorkers = true;\n{CONSOLE_HANDLER_SCRIPT}")
    } else {
        CONSOLE_HANDLER_SCRIPT.to_string()
    };

    vec![
        ("console".to_string(), console_src),
        (
            "network-shim".to_string(),
            host_script("network-shim").unwrap(),
        ),
        ("bridge-spec".to_string(), bridge_spec_script(platform)),
        (
            "local-modules".to_string(),
            host_script("local-modules").unwrap(),
        ),
        (
            "linux-direct-ui".to_string(),
            LINUX_DIRECT_UI_SCRIPT.to_string(),
        ),
        ("bridge".to_string(), host_script("bridge").unwrap()),
        (
            "direct-notification".to_string(),
            LINUX_DIRECT_NOTIFICATION_SCRIPT.to_string(),
        ),
    ]
}

/// Minimal 6-script user-script list for secondary popup windows (`popup-viewer.html` / `child.html`).
pub fn popup_user_scripts(platform: &ZaloPlatform) -> Vec<(String, String)> {
    vec![
        ("console".to_string(), CONSOLE_HANDLER_SCRIPT.to_string()),
        (
            "network-shim".to_string(),
            host_script("network-shim").unwrap(),
        ),
        ("bridge-spec".to_string(), bridge_spec_script(platform)),
        (
            "local-modules".to_string(),
            host_script("local-modules").unwrap(),
        ),
        (
            "linux-direct-ui".to_string(),
            LINUX_DIRECT_UI_SCRIPT.to_string(),
        ),
        ("bridge".to_string(), host_script("bridge").unwrap()),
    ]
}

// ============================================================================
// On-the-fly ASAR bundle patchers (`OfficialClientHost.swift:270-437`)
// ============================================================================

/// Applies official and Linux-parity ASAR JavaScript bundle rewriters when `path` matches:
/// 1. `/dal-worker.*.js` -> prepends `WORKER_PRELUDE`
/// 2. `/sync-v2-sub-worker.*.js` -> exposes webpack require as `self.__swReq=<ident>;`
/// 3. `/lazy/main-startup.*.js` -> rewrites `resolveWithRetry` to consult `window.__swzGlobalIdFallback`
/// 4. `pc-dist/*.js` & `pc-dist/lazy/*.js` -> applies `vndangkhoa/zalo-linux` UI/window/clipboard/call patches
///
/// Returns `Some(patched_bytes)` if `path` matched and was rewritten, or `None` when
/// no rewrite applies (or when a shape check deliberately left the bundle untouched).
pub fn patch_asar_bundle(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    let clean = path.split(['?', '#']).next().unwrap_or(path);
    let stripped = clean.trim_start_matches('/');
    let rel = stripped.strip_prefix("pc-dist/").unwrap_or(stripped);

    if rel.starts_with("dal-worker.") && rel.ends_with(".js") {
        let prelude = worker_prelude();
        let mut out = Vec::with_capacity(prelude.len() + bytes.len());
        out.extend_from_slice(prelude.as_bytes());
        out.extend_from_slice(bytes);
        return Some(out);
    }

    if rel.starts_with("sync-v2-sub-worker.") && rel.ends_with(".js") {
        return expose_webpack_require(bytes);
    }

    if rel.starts_with("lazy/main-startup.") && rel.ends_with(".js") {
        let step1 = patch_sync_global_id_resolver(bytes);
        let input = step1.as_deref().unwrap_or(bytes);
        if let Some(step2) = patch_zalo_linux_renderer_bundle(input) {
            return Some(step2);
        }
        return step1;
    }

    if rel.ends_with(".js") && (!rel.contains('/') || rel.starts_with("lazy/")) {
        return patch_zalo_linux_renderer_bundle(bytes);
    }

    None
}

const WIN32_TITLEBAR_CONTROLS_TEMPLATE: &str = r#"r.a.createElement("div",{className:"titlebar__btns"+(this.state.isMaximized?" is-maximized":""),style:t},this.props.isPopupWindow||!this.props.status||this.props.status.isAppLock?null:r.a.createElement("i",{className:`fa fa-Lock_24_Line btn titlebar__menu__btn ${s}`,onClick:this._showAppLock.bind(this)}),r.a.createElement("i",{className:`fa fa-Minus_24_Line btn titlebar__menu__btn ${s}`,onClick:this.minimize.bind(this)}),this.props.hideMaximize?null:this.state.isMaximized?r.a.createElement("i",{className:`fa fa-Maximize_24_Line btn titlebar__menu__btn ${s}`,onClick:this.maximize.bind(this)}):r.a.createElement("i",{className:`fa fa-Minimize_24_Line btn titlebar__menu__btn ${s}`,onClick:this.maximize.bind(this)}),!this.props.hideClose&&r.a.createElement("i",{className:`fa fa-Close_24_Line btn titlebar__menu__btn ${s}`,onClick:this.quit.bind(this)}))))"#;

const WIN32_MEDIA_VIEWER_CONTROLS_TEMPLATE: &str = r#"r.a.createElement("div",{className:"titlebar__btns"+(this.state.isMaximized?" is-maximized":""),style:t},r.a.createElement("i",{className:"fa fa-Minus_24_Line btn titlebar__menu__btnPreviewPhoto",onClick:this.minimize.bind(this),style:{height:"24px"}}),this.props.hideMaximize?null:this.state.isMaximized?r.a.createElement("i",{className:"fa fa-Maximize_24_Line btn titlebar__menu__btnPreviewPhoto",style:{height:"24px"},onClick:this.maximize.bind(this)}):r.a.createElement("i",{className:"fa fa-Minimize_24_Line btn titlebar__menu__btnPreviewPhoto",onClick:this.maximize.bind(this),style:{height:"24px"}}),r.a.createElement("i",{className:"fa fa-Close_24_Line btn titlebar__menu__btnPreviewPhoto --close",onClick:this.quit.bind(this),style:{height:"24px"}}))"#;

static LOGIN_PLATFORM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(handlerMinimiseAppWindowOS:\(\)=>\{\$zwindow\.minimize\([^)]+\)\},platform:)"DARWIN""#,
    )
    .expect("valid login platform regex")
});

static TITLEBAR_REACT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"([A-Za-z_$][\w$]*)\.a\.createElement\("div",\{id:"titleBar""#)
        .expect("valid titleBar react var regex")
});

static TITLEBAR_STYLE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"titlebar__btns clickable",style:([A-Za-z_$][\w$]*)\}"#)
        .expect("valid titleBar style var regex")
});

static TITLEBAR_SUFFIX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"\.c,\{className:([A-Za-z_$][\w$]*)\}\)"#)
        .expect("valid titleBar suffix var regex")
});

static TITLE_NAME_MACOS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"`title-name\s*\+\s*macos\s+\$\{"#).expect("valid title-name macos regex")
});

static MEDIA_VIEWER_HEAD_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"className\|\|" titlebar rel flx"\},null,e,([A-Za-z_$][\w$]*)\.a\.createElement\("div",\{className:"draggable w100 title-drag""#,
    )
    .expect("valid media-viewer head regex")
});

static MEDIA_VIEWER_TITLE_MAC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#""titlebar__title\s+mac""#).expect("valid titlebar__title mac regex")
});

static CLIPBOARD_PASTE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"([A-Za-z_$][\w$]*)=\(t,n=""\)=>\{([A-Za-z_$][\w$]*)\|\|\(([A-Za-z_$][\w$]*)=!0,\$zscreencap\.getClipboard\(e\.currentUserId\)"#,
    )
    .expect("valid clipboard paste regex")
});

static CALL_IS_SUPPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"isSupport\(\)\{return!!([A-Za-z_$][\w$]*)\.default\.enable_mac_call&&\(([A-Za-z_$][\w$]*)\.default\.enableCall&&([A-Za-z_$][\w$]*)\)\}"#,
    )
    .expect("valid call isSupport regex")
});

static CONTACT_MINI_INFO_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"([A-Za-z_$][\w$]*)\.default\.getMiniInfo\("#)
        .expect("valid contact getMiniInfo regex")
});

static CONTACT_DNAME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"([A-Za-z_$][\w$]*)\.default\.getDName\("#)
        .expect("valid contact getDName regex")
});

static CALL_LOG_ACTION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"[A-Za-z_$][\w$]*\?"recommened\.calling":"recommened\.receivecall""#)
        .expect("valid call-log action regex")
});

static TRANSFORM_LOCAL_URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"static transformLocalUrl\(e\)\{return ([A-Za-z_$][\w$]*)\.checkNeedTransformLocalUrl\(e\)\?"zfile://"\+e:e\}"#,
    )
    .expect("valid transformLocalUrl regex")
});

static JXL_RESIZE_CODEC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"Object\([A-Za-z_$][\w$]*\.jxlNativeSupported\)\(\)\?\(([A-Za-z_$][\w$]*=this\.resizerFactory\.createResizer\([A-Za-z_$][\w$]*\.e\.LIBJXL\))"#,
    )
    .expect("valid JXL resize codec regex")
});

static JXL_DECODE_CODEC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"Object\([A-Za-z_$][\w$]*\.jxlNativeSupported\)\(\)\?([A-Za-z_$][\w$]*\._decoder\|\|\([A-Za-z_$][\w$]*\.ImageLoaderConfig\.nestedKey\("offload_config\.enable_offload_decode_jxl"\))"#,
    )
    .expect("valid JXL decode codec regex")
});

static JXL_ENCODE_CODEC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"Object\([A-Za-z_$][\w$]*\.jxlNativeSupported\)\(\)\?\(([A-Za-z_$][\w$]*\._encoder\|\|\([A-Za-z_$][\w$]*\.ImageLoaderConfig\.nestedKey\("offload_config\.enable_offload_encode_jxl"\))"#,
    )
    .expect("valid JXL encode codec regex")
});

static IMAGE_RESIZER_MAIN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"![A-Za-z_$][\w$]*\.default\.image_resizer\.enable_libvips_macos\|\|"image/jpeg"!==[A-Za-z_$][\w$]*&&"image/png"!==[A-Za-z_$][\w$]*\?this\.compatibilityValidation\.isOffscreenCanvasSupported\(\)\?this\.resizerFactory\.createResizer\(([A-Za-z_$][\w$]*\.e)\.WEB_WORKER\):this\.compatibilityValidation\.isAutoRotationSupported\(\)\?this\.resizerFactory\.createResizer\([A-Za-z_$][\w$]*\.e\.MAIN\):this\.resizerFactory\.createResizer\([A-Za-z_$][\w$]*\.e\.PRIMITIVE\):this\.resizerFactory\.createResizer\([A-Za-z_$][\w$]*\.e\.LIBVIPS\)"#,
    )
    .expect("valid image resizer MAIN regex")
});

/// Applies `vndangkhoa/zalo-linux` renderer patches on-the-fly without altering `getClientType() == 23`:
/// - Login titlebar `platform:"WIN32"` (`fa-Minus` + `fa-Close`)
/// - Main `#titleBar` Windows controls (`Lock`, `Minus`, `Maximize`/`Minimize`, `Close`) + `"WIN32"` layout class
/// - `MediaViewer` (`popup-viewer.html`) Windows controls + `titlebar__resize`
/// - `Ctrl+V` clipboard image paste (`$zelectronNative.getClipboardImage()` + `File` items fast-path)
/// - Linux call support gate (`Number(e.split(".")[0])<17` + `isSupport()`)
/// - Incoming call caller name/avatar enrichment (`e._caller`) & chat call-log outcome bubbles
/// - WASM JXL encode/decode/resize routing + HTML `<canvas>` (`MAIN`) JPEG/PNG resizer routing
pub fn patch_zalo_linux_renderer_bundle(bytes: &[u8]) -> Option<Vec<u8>> {
    const FAST_NEEDLES: &[&[u8]] = &[
        b"handlerMinimiseAppWindowOS",
        b"id:\"titleBar\"",
        b"draggable w100 title-drag",
        b"$zscreencap.getClipboard(e.currentUserId)",
        b"Number(e.split(\".\")[0])<17",
        b"enable_mac_call",
        b"const t={type:\"control\",data:e}",
        b"sync_call_message.ui)return;",
        b"{\"isEnableCallback\":1}",
        b"checkNeedTransformLocalUrl",
        b"Boolean(!1)?\"zfile://\"+e:e",
        b"enable_offload_encode_jxl",
        b"enable_offload_decode_jxl",
        b"enable_libvips_macos",
        b"createResizer(",
    ];
    if !FAST_NEEDLES
        .iter()
        .any(|needle| find_subslice(bytes, needle).is_some())
    {
        return None;
    }

    let Ok(src_in) = std::str::from_utf8(bytes) else {
        return None;
    };

    let mut s = src_in.to_string();
    let mut changed = false;

    // 1. Login window titlebar controls (`login-startup.*.js`): render Windows min/close buttons
    //    while keeping `getClientType(){return 23}` (`DARWIN`) untouched for session/E2EE parity.
    if s.contains("handlerMinimiseAppWindowOS") && LOGIN_PLATFORM_RE.is_match(&s) {
        let next = LOGIN_PLATFORM_RE
            .replace_all(&s, r#"${1}"WIN32""#)
            .into_owned();
        if next != s {
            s = next;
            changed = true;
        }
    }

    // 2. Main `#titleBar` Windows controls + `"WIN32"` class + strip macOS traffic-light padding.
    const TITLEBAR_NULL_ANCHOR: &str = r#"STR_NEW_VER"})))):null))"#;
    const TITLEBAR_DONE_MARKER: &str = "fa-Lock_24_Line btn titlebar__menu__btn";
    const TITLEBAR_DARWIN_CLASS: &str = r#"?" locked ":"")+"DARWIN""#;
    const TITLEBAR_WIN32_CLASS: &str = r#"?" locked ":"")+"WIN32""#;

    if s.contains(r#"id:"titleBar""#)
        && !s.contains(TITLEBAR_DONE_MARKER)
        && s.contains(TITLEBAR_NULL_ANCHOR)
    {
        let react_var = TITLEBAR_REACT_RE
            .captures(&s)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        let style_var = TITLEBAR_STYLE_RE
            .captures(&s)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        let suffix_var = TITLEBAR_SUFFIX_RE
            .captures(&s)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        if let (Some(rv), Some(st), Some(sf)) = (react_var, style_var, suffix_var) {
            let controls = WIN32_TITLEBAR_CONTROLS_TEMPLATE
                .replace("r.a.createElement", &format!("{rv}.a.createElement"))
                .replace(",style:t}", &format!(",style:{st}}}"))
                .replace("${s}", &format!("${{{sf}}}"));
            s = s.replacen(
                TITLEBAR_NULL_ANCHOR,
                &format!(r#"STR_NEW_VER"}})))):{controls}"#),
                1,
            );
            if s.contains(TITLEBAR_DARWIN_CLASS) {
                s = s.replace(TITLEBAR_DARWIN_CLASS, TITLEBAR_WIN32_CLASS);
            }
            s = TITLE_NAME_MACOS_RE
                .replace_all(&s, "`title-name + ${")
                .into_owned();
            changed = true;
        }
    }

    // 3. `MediaViewer` (`popup-viewer.html`) Windows controls + resize edge + strip `mac` title class.
    const MV_DONE_MARKER: &str = "titlebar__menu__btnPreviewPhoto";
    const MV_DRAG_TAIL: &str = r#".a.createElement("div",{className:"draggable w100 title-drag",style:{position:"absolute",left:0},onDoubleClick:this.maximize.bind(this)})"#;
    if !s.contains(MV_DONE_MARKER) {
        if let Some(rv) = MEDIA_VIEWER_HEAD_RE
            .captures(&s)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
        {
            let drag = format!("{rv}{MV_DRAG_TAIL}");
            let mac_tail = format!(r#"className||" titlebar rel flx"}},null,e,{drag},null))"#);
            if s.contains(&mac_tail) {
                let btns = WIN32_MEDIA_VIEWER_CONTROLS_TEMPLATE
                    .replace("r.a.createElement", &format!("{rv}.a.createElement"));
                let win_tail = format!(
                    r#"className||" titlebar rel flx"}},{rv}.a.createElement("div",{{className:"titlebar__resize"}}),e,{drag},{btns}))"#
                );
                s = s.replacen(&mac_tail, &win_tail, 1);
                s = MEDIA_VIEWER_TITLE_MAC_RE
                    .replace_all(&s, r#""titlebar__title ""#)
                    .into_owned();
                changed = true;
            }
        }
    }

    // 4. `Ctrl+V` clipboard image paste fast-path (`$zelectronNative.getClipboardImage()` + `File` items).
    const CLIP_MARKER: &str = "/*__znative_clip_paste__*/";
    if !s.contains(CLIP_MARKER) && s.contains("$zscreencap.getClipboard(e.currentUserId)") {
        let matched = CLIPBOARD_PASTE_RE
            .captures_iter(&s)
            .find(|cap| &cap[2] == &cap[3])
            .map(|cap| {
                let full = cap.get(0).unwrap();
                (
                    full.start(),
                    full.end(),
                    cap[1].to_string(),
                    cap[2].to_string(),
                )
            });
        if let Some((start, end, fn_var, guard_var)) = matched {
            let inject = format!(
                r#"{fn_var}=(t,n="")=>{{{CLIP_MARKER}try{{var _zn=("undefined"!=typeof $zelectronNative&&$zelectronNative)||("undefined"!=typeof window&&window.$zelectronNative);var _zimg=_zn&&_zn.getClipboardImage&&_zn.getClipboardImage();if(_zimg&&!_zimg.isEmpty()){{var _zb=_zimg.toPNG();if(_zb&&_zb.length&&typeof e.uploadPhoto==="function"){{var _zf=new File([new Uint8Array(_zb)],"clipboard.png",{{type:"image/png"}});e.uploadPhoto([_zf],e.currentUserId);return}}}}}}catch(_zerr){{}}try{{if(typeof e.uploadPhoto==="function"&&Array.isArray(t)&&t.length>0){{e.uploadPhoto(t,e.currentUserId);return}}}}catch(_zerr2){{}}{guard_var}||({guard_var}=!0,$zscreencap.getClipboard(e.currentUserId)"#
            );
            s.replace_range(start..end, &inject);
            changed = true;
        }
    }

    // 5. Linux call support gate (`ae() < 17` bypass + drop `enable_mac_call` gate in `isSupport()`).
    const AE_ANCHOR: &str = r#"Number(e.split(".")[0])<17"#;
    const AE_PATCHED: &str = "!1/*call-linux*/";
    const FORCED_MARKER: &str = "/*mac-forced*/";
    if s.contains(AE_ANCHOR) {
        s = s.replace(AE_ANCHOR, AE_PATCHED);
        changed = true;
    }
    if !s.contains(FORCED_MARKER) && s.contains("enable_mac_call") {
        let matched = CALL_IS_SUPPORT_RE
            .captures_iter(&s)
            .find(|cap| &cap[1] == &cap[2])
            .map(|cap| {
                let full = cap.get(0).unwrap();
                (
                    full.start(),
                    full.end(),
                    cap[1].to_string(),
                    cap[3].to_string(),
                )
            });
        if let Some((start, end, obj, os_ok)) = matched {
            let repl = format!("isSupport(){{{FORCED_MARKER}return!!({obj}.default.enableCall&&{os_ok})}}");
            s.replace_range(start..end, &repl);
            changed = true;
        }
    }

    // 6. Incoming call caller name/avatar enrichment (`e._caller`) inside `CallController.handleControl`.
    const CALL_ENRICH_MARKER: &str = "__zcallEnrich";
    const CALL_CONTROL_ANCHOR: &str = r#"const t={type:"control",data:e}"#;
    if !s.contains(CALL_ENRICH_MARKER) {
        if let Some(idx) = s.find(CALL_CONTROL_ANCHOR) {
            let win_start = floor_char_boundary(&s, idx.saturating_sub(3000));
            let win_end = ceil_char_boundary(&s, (idx + 3000).min(s.len()));
            let window = &s[win_start..win_end];
            let alias = CONTACT_MINI_INFO_RE
                .captures(window)
                .or_else(|| CONTACT_DNAME_RE.captures(window))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());
            if let Some(a) = alias {
                let inject = format!(
                    r#"try{{if(e&&e.act==="request"&&e.data){{var __zcallEnrich=1;var _u=e.data.fromId||e.data.uidN;var _p=_u&&{a}.default.getMiniInfo&&{a}.default.getMiniInfo(_u);if(_p)e._caller={{name:_p.dName||_p.displayName||String(_u),avatar:_p.avatar||_p.avt||null}};}}}}catch(_e){{}}{CALL_CONTROL_ANCHOR}"#
                );
                s.replace_range(idx..idx + CALL_CONTROL_ANCHOR.len(), &inject);
                changed = true;
            }
        }
    }

    // 7. Chat call-log bubble (`sync_call_message.ui&&0` + per-outcome action/params).
    const GUARD_DONE: &str = "sync_call_message.ui&&0)return;";
    const GUARD_FIND: &str = "sync_call_message.ui)return;";
    const BUBBLE_DONE: &str = ",reason:n.reason,calltype:n.calltype,missed:n.missed}";
    const BUBBLE_FIND: &str = "const e={caller:n.role,duration:1e3*n.duration,userId:n.partnerId}";
    const BUBBLE_REPL: &str = "const e={caller:n.role,duration:1e3*n.duration,userId:n.partnerId,reason:n.reason,calltype:n.calltype,missed:n.missed}";
    const ACTION_DONE: &str = r#""recommened.misscall":t.caller?"#;
    const ACTION_REPL: &str =
        r#"t.missed?"recommened.misscall":t.caller?"recommened.calling":"recommened.receivecall""#;
    const PARAMS_DONE: &str = "isCaller:t.caller?1:0";
    const PARAMS_FIND: &str = r#"params:'{"isEnableCallback":1}'"#;
    const PARAMS_REPL: &str = r#"params:JSON.stringify({isCaller:t.caller?1:0,calltype:t.calltype||0,reason:t.reason||0,isEnableCallback:1})"#;

    if !s.contains(GUARD_DONE) && s.contains(GUARD_FIND) {
        s = s.replace(GUARD_FIND, GUARD_DONE);
        changed = true;
    }
    if !s.contains(BUBBLE_DONE) && s.contains(BUBBLE_FIND) {
        s = s.replace(BUBBLE_FIND, BUBBLE_REPL);
        changed = true;
    }
    if !s.contains(ACTION_DONE) && CALL_LOG_ACTION_RE.is_match(&s) {
        s = CALL_LOG_ACTION_RE.replace_all(&s, ACTION_REPL).into_owned();
        changed = true;
    }
    if !s.contains(PARAMS_DONE) && s.contains(PARAMS_FIND) {
        s = s.replace(PARAMS_FIND, PARAMS_REPL);
        changed = true;
    }

    // 8. Local file subresource URL transform (`file:///...` -> `zfile://local/...`) for WebKit2GTK.
    const ZFILE_TRANSFORM_REPL: &str = r#"static transformLocalUrl(e){return typeof e==="string"&&e.startsWith("file:")?"zfile://local/"+e.replace(/^file:\/+/,""):e}"#;
    const ZFILE_INLINE_FIND: &str = r#"Boolean(!1)?"zfile://"+e:e"#;
    const ZFILE_INLINE_REPL: &str =
        r#"(typeof e==="string"&&e.startsWith("file:")?"zfile://local/"+e.replace(/^file:\/+/,""):e)"#;

    if s.contains("checkNeedTransformLocalUrl") && TRANSFORM_LOCAL_URL_RE.is_match(&s) {
        let next = TRANSFORM_LOCAL_URL_RE
            .replace_all(&s, ZFILE_TRANSFORM_REPL)
            .into_owned();
        if next != s {
            s = next;
            changed = true;
        }
    }
    if s.contains(ZFILE_INLINE_FIND) {
        s = s.replace(ZFILE_INLINE_FIND, ZFILE_INLINE_REPL);
        changed = true;
    }

    // 9. Route JXL encode/decode/resize codecs to Zalo's built-in WASM JXL worker (`yt`, `Me`,
    //    `LIBJXL_WASM`) instead of Electron native `$zFeatures.libjxl` or 60s offload BrowserWindow
    //    tasks (`St`, `Tt`, `ve`, `ue`) while preserving `jxlNativeSupported() === true` for native
    //    desktop file download UI actions.
    if s.contains("jxlNativeSupported") {
        for (re, repl) in [
            (&*JXL_RESIZE_CODEC_RE, "(!1/*olaz-wasm-jxl*/)?(${1}"),
            (&*JXL_DECODE_CODEC_RE, "(!1/*olaz-wasm-jxl*/)?${1}"),
            (&*JXL_ENCODE_CODEC_RE, "(!1/*olaz-wasm-jxl*/)?(${1}"),
        ] {
            if re.is_match(&s) {
                let next = re.replace_all(&s, repl).into_owned();
                if next != s {
                    s = next;
                    changed = true;
                }
            }
        }
    }

    // 10. Route JPEG/PNG resizing in `ee.resizeImage` to main-thread HTML `<canvas>` (`MAIN`)
    //     so photo uploads never hit Electron `LIBVIPS` or software-rendered Worker `OffscreenCanvas`.
    if s.contains("enable_libvips_macos") && IMAGE_RESIZER_MAIN_RE.is_match(&s) {
        let next = IMAGE_RESIZER_MAIN_RE
            .replace_all(
                &s,
                "(!0/*olaz-main-resizer*/?this.resizerFactory.createResizer(${1}.MAIN):null)",
            )
            .into_owned();
        if next != s {
            s = next;
            changed = true;
        }
    }

    if changed {
        Some(s.into_bytes())
    } else {
        None
    }
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Exposes the private webpack `require` identifier as `self.__swReq=<ident>;` inside
/// `sync-v2-sub-worker.*.js` (`AsarSchemeHandler.exposeWebpackRequire`).
pub fn expose_webpack_require(bytes: &[u8]) -> Option<Vec<u8>> {
    const TAIL: &[u8] = b"=function(e){var t=[],n=a[e]";

    let first_pos = find_subslice(bytes, TAIL);
    let Some(pos) = first_pos else {
        tracing::warn!("sync-v2-sub-worker served unpatched: runtime shape not found");
        return None;
    };

    if find_subslice(&bytes[pos + TAIL.len()..], TAIL).is_some() {
        tracing::warn!("sync-v2-sub-worker served unpatched: runtime shape ambiguous");
        return None;
    }

    if pos < 2 || bytes[pos - 1] != b'e' || bytes[pos - 2] != b'.' {
        tracing::warn!("sync-v2-sub-worker served unpatched: no .e accessor");
        return None;
    }

    let ident_end = pos - 2;
    let mut ident_start = ident_end;
    while ident_start > 0 && is_js_ident_byte(bytes[ident_start - 1]) {
        ident_start -= 1;
    }

    if ident_start >= ident_end
        || bytes[ident_start].is_ascii_digit()
        || ident_start == 0
        || (bytes[ident_start - 1] != b'}' && bytes[ident_start - 1] != b';')
    {
        tracing::warn!("sync-v2-sub-worker served unpatched: not at a statement boundary");
        return None;
    }

    let ident = std::str::from_utf8(&bytes[ident_start..ident_end]).ok()?;
    let inject = format!("self.__swReq={ident};");
    let mut out = Vec::with_capacity(bytes.len() + inject.len());
    out.extend_from_slice(&bytes[..ident_start]);
    out.extend_from_slice(inject.as_bytes());
    out.extend_from_slice(&bytes[ident_start..]);
    Some(out)
}

static RESOLVER_RE: Lazy<Regex> = Lazy::new(|| {
    let ident = r"[A-Za-z_$][A-Za-z0-9_$]*";
    let pat = format!(
        r"({ident})\.resolveWithRetry\(({ident}),\(({ident})=>({ident})\.default\.getGlobalIdsFromUserIds\(({ident})\)\)\)"
    );
    Regex::new(&pat).expect("valid global-id resolver regex")
});

/// Rewrites `<svc>.resolveWithRetry(<ids>, (<arg> => <mgr>.default.getGlobalIdsFromUserIds(<arg>)))`
/// inside `lazy/main-startup.*.js` to consult `window.__swzGlobalIdFallback` for unmapped IDs.
pub fn patch_sync_global_id_resolver(bytes: &[u8]) -> Option<Vec<u8>> {
    let Ok(source) = std::str::from_utf8(bytes) else {
        tracing::warn!("main-startup served without global-id fallback: not utf8");
        return None;
    };

    let matches: Vec<_> = RESOLVER_RE
        .captures_iter(source)
        .filter(|cap| &cap[3] == &cap[5])
        .collect();

    if matches.len() != 1 {
        let why = if matches.is_empty() {
            "resolver shape not found"
        } else {
            "resolver shape ambiguous"
        };
        tracing::warn!("main-startup served without global-id fallback: {why}");
        return None;
    }

    let cap = &matches[0];
    let full = cap.get(0).unwrap();
    let svc = &cap[1];
    let ids = &cap[2];
    let arg = &cap[3];
    let mgr = &cap[4];

    let replacement = format!(
        "{svc}.resolveWithRetry({ids},(async {arg}=>{{let __r=await {mgr}.default.getGlobalIdsFromUserIds({arg})||{{}};const __m={arg}.filter(__i=>!__r[__i]),__f=window.__swzGlobalIdFallback;if(__m.length&&typeof __f===\"function\")try{{__r=Object.assign({{}},__r,await __f(__m))}}catch(__e){{}}return __r}}))"
    );

    let mut out = String::with_capacity(source.len() + replacement.len());
    out.push_str(&source[..full.start()]);
    out.push_str(&replacement);
    out.push_str(&source[full.end()..]);
    Some(out.into_bytes())
}

fn is_js_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn collect_os_info(platform: &ZaloPlatform, arch: &str) -> Value {
    // Report Darwin 22.6.0 (`17 <= 22 < 23` = macOS 13.6 Ventura):
    // - `>= 17` satisfies macOS 10.13+ feature gates (`Number(os.release().split(".")[0]) < 17`)
    // - `< 23` keeps `jxlNativeSupported` (`+os.release().split(".")[0] >= 23`) false so Zalo
    //   decodes `.jxl` images via its built-in `LIBJXL_WASM` worker instead of Apple ImageIO.
    let release = "22.6.0".to_string();
    let linux_release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let version = std::fs::read_to_string("/proc/sys/kernel/version")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Olaz".to_string());
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let (totalmem, freemem) = read_proc_meminfo();
    let uptime = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map(|secs| secs as u64)
        .unwrap_or(0);
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_string());

    json!({
        "release": release,
        "linuxRelease": linux_release,
        "version": version,
        "arch": arch,
        "hostname": hostname,
        "homedir": platform.electron_path("home").to_string_lossy(),
        "tmpdir": platform.electron_path("temp").to_string_lossy(),
        "totalmem": totalmem,
        "freemem": freemem,
        "cpus": cpus,
        "uptime": uptime,
        "username": username,
    })
}

fn read_proc_meminfo() -> (u64, u64) {
    let default_total = 8u64 * 1024 * 1024 * 1024;
    let default_free = 4u64 * 1024 * 1024 * 1024;
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return (default_total, default_free);
    };
    let mut total_kb = None;
    let mut avail_kb = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok());
        }
    }
    (
        total_kb.map(|k| k * 1024).unwrap_or(default_total),
        avail_kb.map(|k| k * 1024).unwrap_or(default_free),
    )
}
