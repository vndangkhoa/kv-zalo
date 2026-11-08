//! WebKit2GTK Zalo client host (`OfficialClientHost`), Electron preload bridge (`ZaloBridge`),
//! `zalo-app://` zero-extraction ASAR URI scheme handler, `zfile://` confined local media scheme,
//! loopback HTTP 206 GStreamer media proxy, and frameless popup (`MediaViewer` / `child.html`) host.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use javascriptcore::ValueExt as JscValueExt;
use olaz_core::{
    direct_ui_user_scripts, mime_for_path, ordered_user_scripts, patch_asar_bundle,
    popup_user_scripts, wire, AsarArchive, ZaloPlatform,
};
use olaz_net::{serve_local_media_range, MediaRangeResponse, NetworkProxy, WsEvent, WsEventSink};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use soup::{MessageHeaders, MessageHeadersType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;
use webkit2gtk::{
    DownloadExt, FileChooserRequest, FileChooserRequestExt, HardwareAccelerationPolicy, LoadEvent,
    NavigationAction, NavigationPolicyDecision, NavigationPolicyDecisionExt, PolicyDecisionExt,
    PolicyDecisionType, SecurityManagerExt, Settings, SettingsExt, URIRequestExt,
    URISchemeRequest, URISchemeRequestExt, URISchemeResponse, URISchemeResponseExt,
    UserContentInjectedFrames, UserContentManager, UserContentManagerExt, UserScript,
    UserScriptInjectionTime, WebContext, WebContextExt, WebView, WebViewExt, WebsiteDataManager,
};

/// Custom URI scheme serving `pc-dist/` from `app.asar` and `/__olaz/bridge-sync`.
pub const ZALO_APP_SCHEME: &str = "zalo-app";

/// Custom URI scheme serving sandbox-confined local media & attachments (`zfile://local/<abs_path>`)
/// with HTTP `206 Partial Content` byte-range support (equivalent to `zalo-linux` Electron `zfile:`).
pub const ZFILE_SCHEME: &str = "zfile";

/// Path handled synchronously via `XMLHttpRequest(..., false)` by `HostScripts/bridge.js`.
pub const BRIDGE_SYNC_PATH: &str = "/__olaz/bridge-sync";

/// Preamble injected BEFORE all `HostScripts/*.js` so `window.webkit.messageHandlers`
/// exposes Promise-returning `.znet.postMessage(...)` and `.zbridge.postMessage(...)` plus
/// fire-and-forget `.zlog.postMessage(...)` and `.zstate.postMessage(...)` on top of WebKit2GTK's
/// `register_script_message_handler` without modifying a single byte of `Sources/Olaz/HostScripts/*.js`.
const OLAZ_WEBKIT_BRIDGE_PREAMBLE: &str = r#"(() => {
  'use strict';
  if (window.__olazRpcInstalled) return;
  window.__olazRpcInstalled = true;

  const w = window.webkit;
  const mh = w && w.messageHandlers;
  if (!w || !mh) return;
  Object.defineProperty(window, '__olazKeepAlive', {
    value: [w, mh],
    configurable: false,
    enumerable: false,
    writable: false,
  });

  if (window !== window.top) {
    try {
      const topMh = window.top && window.top.webkit && window.top.webkit.messageHandlers;
      if (topMh && topMh.zbridge && topMh.znet) {
        mh.zlog = {
          postMessage(msg) {
            try {
              if (topMh.zlog && typeof topMh.zlog.postMessage === 'function') {
                topMh.zlog.postMessage(msg);
              }
            } catch {}
          },
        };
        mh.zstate = {
          postMessage(msg) {
            try {
              if (topMh.zstate && typeof topMh.zstate.postMessage === 'function') {
                topMh.zstate.postMessage(msg);
              }
            } catch {}
          },
        };
        mh.znet = {
          postMessage(msg) {
            try {
              return topMh.znet.postMessage(msg);
            } catch (err) {
              return Promise.reject(err);
            }
          },
        };
        mh.zbridge = {
          postMessage(msg) {
            try {
              return topMh.zbridge.postMessage(msg);
            } catch (err) {
              return Promise.reject(err);
            }
          },
        };
        if (!mh.__olaz_win_drag && topMh.__olaz_win_drag) {
          mh.__olaz_win_drag = topMh.__olaz_win_drag;
        }
        return;
      }
    } catch {}
  }

  const rawLog = mh.__olaz_zlog;
  const rawState = mh.__olaz_zstate;
  const rawNet = mh.__olaz_znet;
  const rawBridge = mh.__olaz_zbridge;

  let nextId = 1;
  const pending = new Map();

  window.__olazRpcReply = (id, ok, payloadJson) => {
    const entry = pending.get(id);
    if (!entry) return;
    pending.delete(id);
    let parsed = undefined;
    if (typeof payloadJson === 'string') {
      try { parsed = JSON.parse(payloadJson); } catch { parsed = payloadJson; }
    } else {
      parsed = payloadJson;
    }
    if (ok) {
      entry.resolve(parsed);
    } else {
      const msg = typeof parsed === 'string'
        ? parsed
        : (parsed && typeof parsed.error === 'string' ? parsed.error : String(parsed));
      entry.reject(msg);
    }
  };

  const makeRpcHandler = (rawHandler, label) => ({
    postMessage(msg) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        try {
          if (!rawHandler || typeof rawHandler.postMessage !== 'function') {
            throw new Error('Missing native message handler for ' + label);
          }
          const maybePromise = rawHandler.postMessage(
            JSON.stringify({ id, body: msg === undefined ? null : msg })
          );
          if (maybePromise && typeof maybePromise.catch === 'function') {
            maybePromise.catch((err) => {
              if (pending.delete(id)) reject(err);
            });
          }
        } catch (err) {
          pending.delete(id);
          reject(err);
        }
      });
    },
  });

  mh.zlog = {
    postMessage(msg) {
      try {
        if (rawLog && typeof rawLog.postMessage === 'function') {
          const p = rawLog.postMessage(typeof msg === 'string' ? msg : JSON.stringify(msg));
          if (p && typeof p.catch === 'function') p.catch(() => {});
        }
      } catch {}
    },
  };

  mh.zstate = {
    postMessage(msg) {
      try {
        if (rawState && typeof rawState.postMessage === 'function') {
          const p = rawState.postMessage(typeof msg === 'string' ? msg : JSON.stringify(msg));
          if (p && typeof p.catch === 'function') p.catch(() => {});
        }
      } catch {}
    },
  };

  mh.znet = makeRpcHandler(rawNet, 'znet');
  mh.zbridge = makeRpcHandler(rawBridge, 'zbridge');
})();
//# sourceURL=olaz-webkit-preamble.js"#;

/// Injected ONLY when neither `libgstautodetect.so` nor our bundled `libgstolazauto.so` is present,
/// preventing `WebCore::MediaPlayerPrivateGStreamer::createAudioSink()` `RELEASE_ASSERT(audioSink)` `SIGABRT`.
const OLAZ_MISSING_AUDIO_SINK_GUARD: &str = r#"(() => {
  'use strict';
  if (window.__olazAudioGuardInstalled) return;
  window.__olazAudioGuardInstalled = true;

  const noop = () => {};
  const resolveVoid = () => Promise.resolve();

  if (typeof HTMLMediaElement !== 'undefined' && HTMLMediaElement.prototype) {
    const proto = HTMLMediaElement.prototype;
    try { proto.play = resolveVoid; } catch {}
    try { proto.load = noop; } catch {}
    try { proto.pause = noop; } catch {}
    try { proto.canPlayType = () => ''; } catch {}
    const srcStore = new WeakMap();
    try {
      Object.defineProperty(proto, 'src', {
        configurable: true,
        enumerable: true,
        get() { return srcStore.get(this) || ''; },
        set(v) { srcStore.set(this, v == null ? '' : String(v)); },
      });
    } catch {}
    try {
      Object.defineProperty(proto, 'srcObject', {
        configurable: true,
        enumerable: true,
        get() { return null; },
        set() {},
      });
    } catch {}
    if (typeof Element !== 'undefined' && Element.prototype && Element.prototype.setAttribute) {
      const origSetAttr = Element.prototype.setAttribute;
      Element.prototype.setAttribute = function(name, value) {
        if (this instanceof HTMLMediaElement && String(name).toLowerCase() === 'src') {
          srcStore.set(this, value == null ? '' : String(value));
          return;
        }
        return origSetAttr.call(this, name, value);
      };
    }
  }

  class SafeAudioStub extends EventTarget {
    constructor(src = '') {
      super();
      this.src = src == null ? '' : String(src);
      this.srcObject = null;
      this.volume = 1;
      this.muted = false;
      this.loop = false;
      this.autoplay = false;
      this.preload = 'none';
      this.paused = true;
      this.ended = true;
      this.currentTime = 0;
      this.duration = 0;
      this.readyState = 4;
      this.networkState = 0;
      this.error = null;
    }
    play() { return Promise.resolve(); }
    load() {}
    pause() {}
    canPlayType() { return ''; }
    cloneNode() { return new SafeAudioStub(this.src); }
    setAttribute(k, v) { if (String(k).toLowerCase() === 'src') this.src = v == null ? '' : String(v); }
    getAttribute(k) { return String(k).toLowerCase() === 'src' ? this.src : null; }
    removeAttribute(k) { if (String(k).toLowerCase() === 'src') this.src = ''; }
  }
  try { window.Audio = SafeAudioStub; } catch {}
})();
//# sourceURL=olaz-audio-sink-guard.js"#;

/// Official renderer entry pages inside `pc-dist/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPage {
    Login,
    Main,
    SharedWorker,
}

impl HostPage {
    pub const fn filename(self) -> &'static str {
        match self {
            Self::Login => "login.html",
            Self::Main => "index.html",
            Self::SharedWorker => "shared-worker.html",
        }
    }
}

/// Lifecycle navigation events emitted by [`OfficialClientHost`].
#[derive(Debug, Clone)]
pub enum PageEvent {
    StartedLoading,
    FinishedLoading(HostPage),
    Failed(String),
    Terminated,
}

pub type BridgeFuture = Pin<Box<dyn Future<Output = Result<Value, String>>>>;
pub type AsyncBridgeHandler = Rc<dyn Fn(Vec<Value>) -> BridgeFuture>;
pub type SyncBridgeHandler = Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync>;
pub type PopupBridgeFactory = Rc<dyn Fn(&Rc<ZaloBridge>, &gtk::Window, &Rc<ZaloBridge>)>;

static NEXT_BRIDGE_ID: AtomicU64 = AtomicU64::new(1);

/// Rust port of `ZaloBridge.swift`: serves `$z*` async/sync member calls and Electron IPC channels.
pub struct ZaloBridge {
    id: u64,
    webview: RefCell<Option<glib::WeakRef<WebView>>>,
    async_handlers: RefCell<HashMap<String, AsyncBridgeHandler>>,
    sync_handlers: Mutex<HashMap<String, SyncBridgeHandler>>,
    channel_handlers: RefCell<HashMap<String, AsyncBridgeHandler>>,
    channel_sync_handlers: Mutex<HashMap<String, SyncBridgeHandler>>,
    subscribed_channels: Mutex<HashSet<String>>,
    provided_channels: Mutex<HashSet<String>>,
    subscriptions: Mutex<HashSet<String>>,
    unhandled_channels: Mutex<HashMap<String, usize>>,
    trace: RefCell<Option<Rc<dyn Fn(&str, &[Value])>>>,
    on_handler_error: RefCell<Option<Rc<dyn Fn(&str, &str)>>>,
    on_channel_activity: RefCell<Option<Rc<dyn Fn(&str)>>>,
}

impl ZaloBridge {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            id: NEXT_BRIDGE_ID.fetch_add(1, Ordering::Relaxed),
            webview: RefCell::new(None),
            async_handlers: RefCell::new(HashMap::new()),
            sync_handlers: Mutex::new(HashMap::new()),
            channel_handlers: RefCell::new(HashMap::new()),
            channel_sync_handlers: Mutex::new(HashMap::new()),
            subscribed_channels: Mutex::new(HashSet::new()),
            provided_channels: Mutex::new(HashSet::new()),
            subscriptions: Mutex::new(HashSet::new()),
            unhandled_channels: Mutex::new(HashMap::new()),
            trace: RefCell::new(None),
            on_handler_error: RefCell::new(None),
            on_channel_activity: RefCell::new(None),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn attach_webview(&self, webview: &WebView) {
        let weak = glib::WeakRef::new();
        weak.set(Some(webview));
        *self.webview.borrow_mut() = Some(weak);
    }

    pub fn webview(&self) -> Option<WebView> {
        self.webview.borrow().as_ref().and_then(|w| w.upgrade())
    }

    pub fn set_trace(&self, cb: impl Fn(&str, &[Value]) + 'static) {
        *self.trace.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn set_on_handler_error(&self, cb: impl Fn(&str, &str) + 'static) {
        *self.on_handler_error.borrow_mut() = Some(Rc::new(cb));
    }

    #[allow(dead_code)]
    pub fn set_on_channel_activity(&self, cb: impl Fn(&str) + 'static) {
        *self.on_channel_activity.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn is_subscribed_channel(&self, channel: &str) -> bool {
        self.subscribed_channels.lock().contains(channel)
    }

    pub fn unhandled_channels_snapshot(&self) -> HashMap<String, usize> {
        self.unhandled_channels.lock().clone()
    }

    /// Formats a human-readable summary of registered handlers and unhandled IPC channels.
    pub fn stats_summary(&self) -> String {
        format!(
            "async={}, sync={}, ipc_async={}, ipc_sync={}, unhandled={}",
            self.async_handlers.borrow().len(),
            self.sync_handlers.lock().len(),
            self.channel_handlers.borrow().len(),
            self.channel_sync_handlers.lock().len(),
            self.unhandled_channels.lock().len(),
        )
    }

    /// Registers an async handler for a preload path (`$zFileManager.*`, `$znode.fs.*`, etc.).
    pub fn register_async<F, Fut>(&self, path: impl Into<String>, handler: F)
    where
        F: Fn(Vec<Value>) -> Fut + 'static,
        Fut: Future<Output = Result<Value, String>> + 'static,
    {
        self.async_handlers.borrow_mut().insert(
            path.into(),
            Rc::new(move |args| Box::pin(handler(args))),
        );
    }

    /// Registers a synchronous handler for a preload path (`$znode.crypto.*`, `$znode.fs.*Sync`, etc.).
    pub fn register_sync<F>(&self, path: impl Into<String>, handler: F)
    where
        F: Fn(&[Value]) -> Result<Value, String> + Send + Sync + 'static,
    {
        self.sync_handlers
            .lock()
            .insert(path.into(), Arc::new(handler));
    }

    /// Registers an async `ipcMain.handle(channel)` handler.
    pub fn handle_channel<F, Fut>(&self, channel: impl Into<String>, handler: F)
    where
        F: Fn(Vec<Value>) -> Fut + 'static,
        Fut: Future<Output = Result<Value, String>> + 'static,
    {
        self.channel_handlers.borrow_mut().insert(
            channel.into(),
            Rc::new(move |args| Box::pin(handler(args))),
        );
    }

    /// Registers a synchronous `ipcMain` handler (answers both `REQUEST_SYNC` and async `invoke`).
    pub fn handle_channel_sync<F>(&self, channel: impl Into<String>, handler: F)
    where
        F: Fn(&[Value]) -> Result<Value, String> + Send + Sync + 'static,
    {
        self.channel_sync_handlers
            .lock()
            .insert(channel.into(), Arc::new(handler));
    }

    /// Removes any async handler for `channel` and installs a synchronous handler in its place
    /// (used to scope popup window controls to their own `gtk::Window`).
    pub fn override_channel_sync<F>(&self, channel: impl Into<String>, handler: F)
    where
        F: Fn(&[Value]) -> Result<Value, String> + Send + Sync + 'static,
    {
        let ch = channel.into();
        self.channel_handlers.borrow_mut().remove(&ch);
        self.channel_sync_handlers
            .lock()
            .insert(ch, Arc::new(handler));
    }

    /// Acknowledges benign Electron channels with `undefined`.
    pub fn acknowledge_channels(&self, channels: &[&str]) {
        let mut sync_map = self.channel_sync_handlers.lock();
        let async_map = self.channel_handlers.borrow();
        for &ch in channels {
            if !async_map.contains_key(ch) && !sync_map.contains_key(ch) {
                sync_map.insert(ch.to_string(), Arc::new(|_| Ok(wire::undefined())));
            }
        }
    }

    /// Pushes `webContents.send(channel, ...args)` to the renderer (`window.__zaloBridge.ipcEmit`).
    pub fn send(&self, channel: &str, args: Vec<Value>) -> bool {
        let Some(wv) = self.webview() else {
            return false;
        };
        let ch_json = serde_json::to_string(channel).unwrap_or_else(|_| "\"\"".to_string());
        let args_json = serde_json::to_string(&args).unwrap_or_else(|_| "[]".to_string());
        let script = format!(
            "window.__zaloBridge && window.__zaloBridge.ipcEmit({ch_json}, {args_json});"
        );
        wv.evaluate_javascript(&script, None, None, gio::Cancellable::NONE, |_| {});
        self.is_subscribed_channel(channel)
    }

    /// Pushes an event for a path-keyed listener (`window.__zaloBridge.emit` / `emitRaw`).
    #[allow(dead_code)]
    pub fn emit(&self, path: &str, args: Vec<Value>, with_event: bool) -> bool {
        if !self.subscriptions.lock().contains(path) {
            return false;
        }
        let Some(wv) = self.webview() else {
            return false;
        };
        let p_json = serde_json::to_string(path).unwrap_or_else(|_| "\"\"".to_string());
        let args_json = serde_json::to_string(&args).unwrap_or_else(|_| "[]".to_string());
        let method = if with_event { "emit" } else { "emitRaw" };
        let script =
            format!("window.__zaloBridge && window.__zaloBridge.{method}({p_json}, {args_json});");
        wv.evaluate_javascript(&script, None, None, gio::Cancellable::NONE, |_| {});
        true
    }

    /// Invokes a renderer `PROVIDE` handler (`window.__zaloBridge.ipcInvokeRenderer`).
    #[allow(dead_code)]
    pub async fn invoke_renderer(&self, channel: &str, args: Vec<Value>) -> Result<Value, String> {
        let Some(wv) = self.webview() else {
            return Err("no renderer".to_string());
        };
        let ch_json = serde_json::to_string(channel).map_err(|e| e.to_string())?;
        let args_json = serde_json::to_string(&args).map_err(|e| e.to_string())?;
        let body = format!(
            "return await window.__zaloBridge.ipcInvokeRenderer({ch_json}, {args_json});"
        );
        eval_async_js_on_webview(&wv, &body).await
    }

    fn emit_trace(&self, key: &str, args: &[Value]) {
        let cb = self.trace.borrow().clone();
        if let Some(cb) = cb {
            cb(key, args);
        }
    }

    fn emit_error(&self, key: &str, err: &str) {
        let cb = self.on_handler_error.borrow().clone();
        if let Some(cb) = cb {
            cb(key, err);
        }
    }

    /// Handles an asynchronous `zbridge` message posted from `HostScripts/bridge.js`.
    pub fn handle_async_message(
        self: &Rc<Self>,
        body: Value,
        reply: Box<dyn FnOnce(Result<Value, String>)>,
    ) {
        let Some(obj) = body.as_object() else {
            reply(Err("bad bridge message".to_string()));
            return;
        };
        let Some(kind) = obj.get("kind").and_then(Value::as_str) else {
            reply(Err("bad bridge message".to_string()));
            return;
        };
        let args: Vec<Value> = obj
            .get("args")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if let Some(channel) = obj.get("channel").and_then(Value::as_str) {
            match kind {
                "subscribe" => {
                    self.subscribed_channels.lock().insert(channel.to_string());
                    let cb = self.on_channel_activity.borrow().clone();
                    if let Some(cb) = cb {
                        cb(channel);
                    }
                    reply(Ok(wire::undefined()));
                }
                "provide" => {
                    self.provided_channels.lock().insert(channel.to_string());
                    let cb = self.on_channel_activity.borrow().clone();
                    if let Some(cb) = cb {
                        cb(channel);
                    }
                    reply(Ok(wire::undefined()));
                }
                _ => {
                    let trace_key = format!("ipc:{channel}");
                    self.emit_trace(&trace_key, &args);
                    let async_handler = self.channel_handlers.borrow().get(channel).cloned();
                    if let Some(handler) = async_handler {
                        let bridge_this = Rc::clone(self);
                        glib::MainContext::default().spawn_local(async move {
                            let res = handler(args).await;
                            if let Err(ref err) = res {
                                bridge_this.emit_error(&trace_key, err);
                            }
                            reply(res);
                        });
                        return;
                    }

                    let sync_handler = self.channel_sync_handlers.lock().get(channel).cloned();
                    if let Some(handler) = sync_handler {
                        let res = handler(&args);
                        if let Err(ref err) = res {
                            self.emit_error(&trace_key, err);
                        }
                        reply(res);
                        return;
                    }

                    {
                        let mut unhandled = self.unhandled_channels.lock();
                        *unhandled.entry(channel.to_string()).or_insert(0) += 1;
                    }
                    tracing::debug!(kind, channel, "unhandled ipc channel; settling undefined");
                    reply(Ok(wire::undefined()));
                }
            }
            return;
        }

        let Some(path) = obj.get("path").and_then(Value::as_str) else {
            reply(Err("bad bridge message".to_string()));
            return;
        };

        if kind == "subscribe" {
            self.subscriptions.lock().insert(path.to_string());
            reply(Ok(wire::undefined()));
            return;
        }

        self.emit_trace(path, &args);
        let async_handler = self.async_handlers.borrow().get(path).cloned();
        if let Some(handler) = async_handler {
            let bridge_this = Rc::clone(self);
            let path_owned = path.to_string();
            glib::MainContext::default().spawn_local(async move {
                let res = handler(args).await;
                if let Err(ref err) = res {
                    bridge_this.emit_error(&path_owned, err);
                }
                reply(res);
            });
            return;
        }

        let sync_handler = self.sync_handlers.lock().get(path).cloned();
        if let Some(handler) = sync_handler {
            let res = handler(&args);
            if let Err(ref err) = res {
                self.emit_error(path, err);
            }
            reply(res);
            return;
        }

        tracing::debug!(path, "unhandled async bridge call; settling undefined");
        reply(Ok(wire::undefined()));
    }

    /// Handles blocking synchronous XHR requests to `zalo-app://local/__olaz/bridge-sync`.
    pub fn handle_sync_request(&self, raw_body: &[u8]) -> Vec<u8> {
        let Ok(parsed) = serde_json::from_slice::<Value>(raw_body) else {
            return serde_json::to_vec(&json!({ "error": "bad sync request" }))
                .unwrap_or_else(|_| b"{}".to_vec());
        };
        let Some(obj) = parsed.as_object() else {
            return serde_json::to_vec(&json!({ "error": "bad sync request" }))
                .unwrap_or_else(|_| b"{}".to_vec());
        };

        let args: Vec<Value> = obj
            .get("args")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let (key, handler) = if let Some(channel) = obj.get("channel").and_then(Value::as_str) {
            let h = self.channel_sync_handlers.lock().get(channel).cloned();
            if h.is_none() {
                *self
                    .unhandled_channels
                    .lock()
                    .entry(channel.to_string())
                    .or_insert(0) += 1;
            }
            (format!("ipc:{channel}"), h)
        } else {
            let path = obj.get("path").and_then(Value::as_str).unwrap_or("?");
            let h = self.sync_handlers.lock().get(path).cloned();
            (path.to_string(), h)
        };

        self.emit_trace(&key, &args);

        let payload = match handler {
            Some(h) => match h(&args) {
                Ok(val) => json!({ "value": val }),
                Err(err) => {
                    self.emit_error(&key, &err);
                    json!({ "error": err })
                }
            },
            None => {
                tracing::debug!(key, "unhandled sync bridge call; settling undefined");
                json!({ "value": wire::undefined() })
            }
        };

        serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())
    }
}

/// Shared WebKit2GTK `WebContext` + `WebsiteDataManager` used by the main Zalo host
/// (`index.html` / `login.html`), secondary `MediaViewer` popups (`popup-viewer.html`), and
/// the hidden `shared-worker.html` host so they share `IndexedDB` and `localStorage` under
/// origin `zalo-app://local`.
#[derive(Clone)]
pub struct ZaloWebRuntime {
    context: WebContext,
    asar: Arc<RwLock<Option<Arc<AsarArchive>>>>,
    bridges: Rc<RefCell<BridgeRegistry>>,
}

#[derive(Default)]
struct BridgeRegistry {
    by_webview_ptr: HashMap<usize, Weak<ZaloBridge>>,
    primary: Option<Weak<ZaloBridge>>,
}

impl ZaloWebRuntime {
    pub fn new(
        platform: &Arc<ZaloPlatform>,
        initial_asar: Option<Arc<AsarArchive>>,
        tokio_handle: &tokio::runtime::Handle,
    ) -> Self {
        let data_dir = platform.webkit_dir().join("zalo-data");
        let cache_dir = platform.webkit_dir().join("zalo-cache");
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(platform.downloads_dir());
        let _ = std::fs::create_dir_all(platform.temp_dir());
        let _ = std::fs::create_dir_all(olaz_linux::SCREENSHOT_STAGING_DIR);
        let _ = olaz_linux::ensure_olaz_gst_plugin_staged();

        let asar_slot = Arc::new(RwLock::new(initial_asar));

        // Start loopback HTTP 206 media proxy BEFORE creating `WebContext` so `OLAZ_GST_MEDIA_PROXY_BASE`
        // is inherited by WebKit's `WebKitWebProcess` when playing `<audio>`/`<video>` via `olazmediasrc`.
        start_gst_media_proxy(
            Arc::clone(platform),
            Arc::clone(&asar_slot),
            tokio_handle,
        );

        let data_manager = WebsiteDataManager::builder()
            .base_data_directory(data_dir.to_string_lossy().as_ref())
            .base_cache_directory(cache_dir.to_string_lossy().as_ref())
            .build();

        let context = WebContext::with_website_data_manager(&data_manager);

        // Expose staged GStreamer plugin, confined app directories, and read-only user/mount roots
        // (`$HOME`, `/mnt`, `/media`, `/run/media`) to WebKit's bubblewrap sandbox so disk-backed
        // `File` objects dropped or picked from file managers are readable inside `WebKitWebProcess`.
        let gst_dir = olaz_linux::olaz_gst_plugin_dir();
        let home_dir = platform.electron_path("home");
        for sandbox_dir in [
            gst_dir.as_path(),
            platform.user_data(),
            platform.downloads_dir(),
            platform.temp_dir(),
            platform.cache_dir(),
            Path::new(olaz_linux::SCREENSHOT_STAGING_DIR),
            home_dir.as_path(),
            Path::new("/mnt"),
            Path::new("/media"),
            Path::new("/run/media"),
        ] {
            if sandbox_dir.exists() {
                context.add_path_to_sandbox(sandbox_dir, true);
            }
        }

        if let Some(sec) = context.security_manager() {
            for scheme in [ZALO_APP_SCHEME, ZFILE_SCHEME] {
                sec.register_uri_scheme_as_secure(scheme);
                sec.register_uri_scheme_as_cors_enabled(scheme);
            }
        }

        let bridges = Rc::new(RefCell::new(BridgeRegistry::default()));

        let asar_for_scheme = Arc::clone(&asar_slot);
        let bridges_for_scheme = Rc::clone(&bridges);
        context.register_uri_scheme(ZALO_APP_SCHEME, move |request| {
            handle_zalo_app_scheme(request, &asar_for_scheme, &bridges_for_scheme);
        });

        let platform_for_zfile = Arc::clone(platform);
        context.register_uri_scheme(ZFILE_SCHEME, move |request| {
            handle_zfile_scheme(request, &platform_for_zfile);
        });

        // Save `<a download>` / `blob:` renderer downloads cleanly into `~/Downloads/`.
        let platform_for_dl = Arc::clone(platform);
        context.connect_download_started(move |_, download| {
            let platform_dl = Arc::clone(&platform_for_dl);
            download.connect_decide_destination(move |dl, suggested| {
                let dir = platform_dl.downloads_dir();
                let _ = std::fs::create_dir_all(&dir);
                let safe_name = sanitize_download_filename(suggested);
                let dest = unique_destination_path(&dir, &safe_name);
                platform_dl.grant_writable_path(&dest);
                if let Ok(uri) = Url::from_file_path(&dest) {
                    dl.set_destination(uri.as_str());
                    return true;
                }
                false
            });
        });

        Self {
            context,
            asar: asar_slot,
            bridges,
        }
    }

    pub fn context(&self) -> &WebContext {
        &self.context
    }

    pub fn set_asar(&self, archive: Arc<AsarArchive>) {
        *self.asar.write() = Some(archive);
    }

    pub fn asar(&self) -> Option<Arc<AsarArchive>> {
        self.asar.read().clone()
    }

    pub fn register_bridge(&self, webview: &WebView, bridge: &Rc<ZaloBridge>, is_primary: bool) {
        let ptr = webview.as_ptr() as usize;
        let mut reg = self.bridges.borrow_mut();
        reg.by_webview_ptr.insert(ptr, Rc::downgrade(bridge));
        if is_primary || reg.primary.is_none() {
            reg.primary = Some(Rc::downgrade(bridge));
        }
    }

    pub fn unregister_webview(&self, webview: &WebView) {
        let ptr = webview.as_ptr() as usize;
        self.bridges.borrow_mut().by_webview_ptr.remove(&ptr);
    }
}

fn sanitize_download_filename(suggested: &str) -> String {
    let base = Path::new(suggested.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim();
    if base.is_empty() || base == "." || base == ".." {
        format!("zalo-download-{}", uuid::Uuid::new_v4().simple())
    } else {
        base.to_string()
    }
}

fn unique_destination_path(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let ext = Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    for idx in 1..10_000 {
        let next_name = if ext.is_empty() {
            format!("{stem} ({idx})")
        } else {
            format!("{stem} ({idx}).{ext}")
        };
        let next = dir.join(next_name);
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!("{}-{}", uuid::Uuid::new_v4().simple(), filename))
}

fn zfile_allowed_roots(platform: &ZaloPlatform) -> Vec<PathBuf> {
    let mut roots = vec![
        platform.user_data().to_path_buf(),
        platform.cache_dir().to_path_buf(),
        platform.temp_dir().to_path_buf(),
        platform.downloads_dir().to_path_buf(),
        platform.electron_path("pictures"),
        platform.electron_path("documents"),
        platform.electron_path("desktop"),
        PathBuf::from(olaz_linux::SCREENSHOT_STAGING_DIR),
    ];
    roots.extend(platform.approved_read_paths());
    roots
}

fn decode_zfile_uri_to_path(raw_uri: &str) -> Option<PathBuf> {
    let parsed = Url::parse(raw_uri).ok()?;
    let host = parsed.host_str().unwrap_or("");
    let raw_path = parsed.path();
    let combined = if !host.is_empty() && host != "local" {
        format!("/{host}{raw_path}")
    } else {
        raw_path.to_string()
    };
    let decoded = percent_encoding::percent_decode_str(&combined)
        .decode_utf8()
        .ok()?;
    let normalized = if decoded.starts_with('/') {
        decoded.into_owned()
    } else {
        format!("/{decoded}")
    };
    Some(PathBuf::from(normalized))
}

fn handle_zfile_scheme(request: &URISchemeRequest, platform: &ZaloPlatform) {
    let raw_uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
    let Some(file_path) = decode_zfile_uri_to_path(&raw_uri) else {
        finish_scheme_response(
            request,
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"invalid zfile URI".to_vec(),
        );
        return;
    };
    let range_header = request
        .http_headers()
        .and_then(|h| h.one("Range"))
        .map(|g| g.to_string());
    let allowed = zfile_allowed_roots(platform);
    match serve_local_media_range(&file_path, range_header.as_deref(), &allowed) {
        Ok(media) => finish_scheme_media_range_response(request, media),
        Err(err) => {
            tracing::debug!(%err, path = %file_path.display(), "denied or missing zfile:// path");
            finish_scheme_response(
                request,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"file unavailable".to_vec(),
            );
        }
    }
}

fn handle_zalo_app_scheme(
    request: &URISchemeRequest,
    asar_slot: &Arc<RwLock<Option<Arc<AsarArchive>>>>,
    bridges: &Rc<RefCell<BridgeRegistry>>,
) {
    let raw_uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
    let parsed_path = Url::parse(&raw_uri)
        .ok()
        .map(|u| u.path().to_string())
        .or_else(|| request.path().map(|p| p.to_string()))
        .unwrap_or_default();

    // 1. Blocking synchronous bridge XHR (`POST zalo-app://local/__olaz/bridge-sync`)
    if parsed_path == BRIDGE_SYNC_PATH {
        let body_bytes = read_gio_stream_sync(request.http_body());
        let bridge_opt = {
            let reg = bridges.borrow();
            request
                .web_view()
                .and_then(|wv| reg.by_webview_ptr.get(&(wv.as_ptr() as usize)))
                .and_then(Weak::upgrade)
                .or_else(|| reg.primary.as_ref().and_then(Weak::upgrade))
        };

        let resp_bytes = match bridge_opt {
            Some(bridge) => bridge.handle_sync_request(&body_bytes),
            None => br#"{"error":"bridge not ready"}"#.to_vec(),
        };
        finish_scheme_response(request, 200, "OK", "application/json", resp_bytes);
        return;
    }

    // 2. Serve `pc-dist/<rel_path>` out of `app.asar` (with on-the-fly JS/CSS bundle patching)
    let rel_clean = parsed_path.trim_start_matches('/');
    let rel = rel_clean.strip_prefix("pc-dist/").unwrap_or(rel_clean);
    if rel.is_empty()
        || rel
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        finish_scheme_response(
            request,
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"bad request".to_vec(),
        );
        return;
    }

    let Some(asar) = asar_slot.read().clone() else {
        finish_scheme_response(
            request,
            503,
            "Service Unavailable",
            "text/plain; charset=utf-8",
            b"Zalo runtime not loaded yet".to_vec(),
        );
        return;
    };

    let asar_lookup_path = format!("pc-dist/{rel}");
    let Some(raw_bytes) = asar.read_or_unpacked(&asar_lookup_path) else {
        tracing::debug!(path = %asar_lookup_path, "missing file in app.asar");
        finish_scheme_response(
            request,
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        );
        return;
    };

    let body = patch_asar_bundle(rel, &raw_bytes).unwrap_or_else(|| raw_bytes.into_owned());
    let mime = mime_for_path(rel);
    let range_header = request
        .http_headers()
        .and_then(|h| h.one("Range"))
        .map(|g| g.to_string());
    if let Some(ref rng) = range_header {
        let (status, reason, content_range, sliced) = slice_in_memory_range(&body, rng);
        finish_scheme_range_bytes(request, status, reason, mime, content_range.as_deref(), sliced);
        return;
    }
    finish_scheme_response(request, 200, "OK", mime, body);
}

fn slice_in_memory_range(
    full: &[u8],
    range_header: &str,
) -> (u32, &'static str, Option<String>, Vec<u8>) {
    let total = full.len();
    let Some(spec) = range_header.trim().strip_prefix("bytes=") else {
        return (200, "OK", None, full.to_vec());
    };
    if total == 0 {
        return (
            416,
            "Range Not Satisfiable",
            Some("bytes */0".to_string()),
            Vec::new(),
        );
    }
    let first_part = spec.split(',').next().unwrap_or("").trim();
    let Some((start_s, end_s)) = first_part.split_once('-') else {
        return (200, "OK", None, full.to_vec());
    };
    let (start, end) = if start_s.is_empty() {
        let Ok(suffix) = end_s.parse::<usize>() else {
            return (200, "OK", None, full.to_vec());
        };
        if suffix == 0 {
            return (
                416,
                "Range Not Satisfiable",
                Some(format!("bytes */{total}")),
                Vec::new(),
            );
        }
        (total.saturating_sub(suffix), total - 1)
    } else {
        let Ok(st) = start_s.parse::<usize>() else {
            return (200, "OK", None, full.to_vec());
        };
        if st >= total {
            return (
                416,
                "Range Not Satisfiable",
                Some(format!("bytes */{total}")),
                Vec::new(),
            );
        }
        let en = if end_s.is_empty() {
            total - 1
        } else {
            end_s.parse::<usize>().unwrap_or(total - 1).min(total - 1)
        };
        if en < st {
            return (
                416,
                "Range Not Satisfiable",
                Some(format!("bytes */{total}")),
                Vec::new(),
            );
        }
        (st, en)
    };
    (
        206,
        "Partial Content",
        Some(format!("bytes {start}-{end}/{total}")),
        full[start..=end].to_vec(),
    )
}

fn start_gst_media_proxy(
    platform: Arc<ZaloPlatform>,
    asar_slot: Arc<RwLock<Option<Arc<AsarArchive>>>>,
    tokio_handle: &tokio::runtime::Handle,
) {
    if std::env::var_os("OLAZ_GST_MEDIA_PROXY_BASE").is_some() {
        return;
    }
    let Ok(std_listener) = std::net::TcpListener::bind("127.0.0.1:0") else {
        tracing::warn!("failed to bind 127.0.0.1:0 for GStreamer media proxy");
        return;
    };
    if std_listener.set_nonblocking(true).is_err() {
        return;
    }
    let Ok(local_addr) = std_listener.local_addr() else {
        return;
    };
    let port = local_addr.port();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let base_url = format!("http://127.0.0.1:{port}/{token}");
    std::env::set_var("OLAZ_GST_MEDIA_PROXY_BASE", &base_url);
    tracing::debug!(port, "started loopback HTTP 206 GStreamer media proxy");

    let Ok(listener) = tokio::net::TcpListener::from_std(std_listener) else {
        return;
    };

    tokio_handle.spawn(async move {
        let prefix = format!("/{token}/");
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let platform = Arc::clone(&platform);
            let asar_slot = Arc::clone(&asar_slot);
            let prefix = prefix.clone();
            tokio::spawn(async move {
                let _ = handle_gst_proxy_conn(&mut stream, &prefix, &platform, &asar_slot).await;
            });
        }
    });
}

async fn handle_gst_proxy_conn(
    stream: &mut tokio::net::TcpStream,
    prefix: &str,
    platform: &ZaloPlatform,
    asar_slot: &Arc<RwLock<Option<Arc<AsarArchive>>>>,
) -> std::io::Result<()> {
    let mut req_buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    while req_buf.len() < 16_384 {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        req_buf.extend_from_slice(&chunk[..n]);
        if req_buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let req_text = String::from_utf8_lossy(&req_buf);
    let mut lines = req_text.split("\r\n");
    let Some(req_line) = lines.next() else {
        return Ok(());
    };
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let raw_target = parts.next().unwrap_or("/");
    let is_head = method.eq_ignore_ascii_case("HEAD");
    if !is_head && !method.eq_ignore_ascii_case("GET") {
        write_http_response(
            stream,
            405,
            "Method Not Allowed",
            "text/plain",
            None,
            b"method not allowed",
            true,
        )
        .await?;
        return Ok(());
    }

    let mut range_header: Option<String> = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("range") {
                range_header = Some(v.trim().to_string());
            }
        }
    }

    let Some(after_token) = raw_target.strip_prefix(prefix) else {
        write_http_response(
            stream,
            403,
            "Forbidden",
            "text/plain",
            None,
            b"forbidden",
            is_head,
        )
        .await?;
        return Ok(());
    };

    let path_no_query = after_token.split('?').next().unwrap_or(after_token);
    let Some((scheme, rest)) = path_no_query.split_once('/') else {
        write_http_response(
            stream,
            400,
            "Bad Request",
            "text/plain",
            None,
            b"bad request",
            is_head,
        )
        .await?;
        return Ok(());
    };

    match scheme {
        ZFILE_SCHEME => {
            let encoded_path = rest.strip_prefix("local/").unwrap_or(rest);
            let Ok(decoded) = percent_encoding::percent_decode_str(encoded_path).decode_utf8()
            else {
                write_http_response(
                    stream,
                    400,
                    "Bad Request",
                    "text/plain",
                    None,
                    b"bad path",
                    is_head,
                )
                .await?;
                return Ok(());
            };
            let abs_path = if decoded.starts_with('/') {
                PathBuf::from(decoded.as_ref())
            } else {
                PathBuf::from(format!("/{decoded}"))
            };
            let allowed = zfile_allowed_roots(platform);
            match serve_local_media_range(&abs_path, range_header.as_deref(), &allowed) {
                Ok(media) => {
                    write_http_response(
                        stream,
                        u32::from(media.status),
                        media.status_text,
                        media.mime_type,
                        media.content_range.as_deref(),
                        &media.body,
                        is_head,
                    )
                    .await?;
                }
                Err(_) => {
                    write_http_response(
                        stream,
                        404,
                        "Not Found",
                        "text/plain",
                        None,
                        b"not found",
                        is_head,
                    )
                    .await?;
                }
            }
        }
        ZALO_APP_SCHEME => {
            let rel_raw = rest.strip_prefix("local/").unwrap_or(rest);
            let rel = rel_raw.strip_prefix("pc-dist/").unwrap_or(rel_raw);
            if rel.is_empty()
                || rel
                    .split('/')
                    .any(|seg| seg.is_empty() || seg == "." || seg == "..")
            {
                write_http_response(
                    stream,
                    400,
                    "Bad Request",
                    "text/plain",
                    None,
                    b"bad request",
                    is_head,
                )
                .await?;
                return Ok(());
            }
            let Some(asar) = asar_slot.read().clone() else {
                write_http_response(
                    stream,
                    503,
                    "Service Unavailable",
                    "text/plain",
                    None,
                    b"unavailable",
                    is_head,
                )
                .await?;
                return Ok(());
            };
            let lookup = format!("pc-dist/{rel}");
            let Some(bytes) = asar.read_or_unpacked(&lookup) else {
                write_http_response(
                    stream,
                    404,
                    "Not Found",
                    "text/plain",
                    None,
                    b"not found",
                    is_head,
                )
                .await?;
                return Ok(());
            };
            let mime = mime_for_path(rel);
            if let Some(ref rng) = range_header {
                let (status, reason, cr, sliced) = slice_in_memory_range(&bytes, rng);
                write_http_response(
                    stream,
                    status,
                    reason,
                    mime,
                    cr.as_deref(),
                    &sliced,
                    is_head,
                )
                .await?;
            } else {
                write_http_response(stream, 200, "OK", mime, None, &bytes, is_head).await?;
            }
        }
        _ => {
            write_http_response(
                stream,
                404,
                "Not Found",
                "text/plain",
                None,
                b"unknown scheme",
                is_head,
            )
            .await?;
        }
    }

    Ok(())
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u32,
    reason: &str,
    content_type: &str,
    content_range: Option<&str>,
    body: &[u8],
    is_head: bool,
) -> std::io::Result<()> {
    let mut hdr = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Accept-Ranges: bytes\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n",
        body.len()
    );
    if let Some(cr) = content_range {
        hdr.push_str(&format!("Content-Range: {cr}\r\n"));
    }
    hdr.push_str("\r\n");
    stream.write_all(hdr.as_bytes()).await?;
    if !is_head && !body.is_empty() {
        stream.write_all(body).await?;
    }
    let _ = stream.shutdown().await;
    Ok(())
}

fn read_gio_stream_sync(stream: Option<gio::InputStream>) -> Vec<u8> {
    let Some(stream) = stream else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf, gio::Cancellable::NONE) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(err) => {
                tracing::warn!(%err, "failed reading URISchemeRequest HTTP body");
                break;
            }
        }
    }
    out
}

fn finish_scheme_response(
    request: &URISchemeRequest,
    status: u32,
    reason: &str,
    content_type: &str,
    body: Vec<u8>,
) {
    finish_scheme_range_bytes(request, status, reason, content_type, None, body);
}

fn finish_scheme_range_bytes(
    request: &URISchemeRequest,
    status: u32,
    reason: &str,
    content_type: &str,
    content_range: Option<&str>,
    body: Vec<u8>,
) {
    let len = body.len() as i64;
    let gbytes = glib::Bytes::from_owned(body);
    let stream = gio::MemoryInputStream::from_bytes(&gbytes);
    let response = URISchemeResponse::new(&stream, len);
    response.set_status(status, Some(reason));
    response.set_content_type(content_type);

    let headers = MessageHeaders::new(MessageHeadersType::Response);
    headers.append("Access-Control-Allow-Origin", "*");
    headers.append("Accept-Ranges", "bytes");
    headers.append("Cache-Control", "no-store");
    if let Some(cr) = content_range {
        headers.append("Content-Range", cr);
    }
    response.set_http_headers(headers);

    request.finish_with_response(&response);
}

fn finish_scheme_media_range_response(request: &URISchemeRequest, media: MediaRangeResponse) {
    let len = media.body.len() as i64;
    let gbytes = glib::Bytes::from_owned(media.body);
    let stream = gio::MemoryInputStream::from_bytes(&gbytes);
    let response = URISchemeResponse::new(&stream, len);
    response.set_status(u32::from(media.status), Some(media.status_text));
    response.set_content_type(media.mime_type);

    let headers = MessageHeaders::new(MessageHeadersType::Response);
    headers.append("Access-Control-Allow-Origin", "*");
    headers.append("Accept-Ranges", media.accept_ranges);
    if let Some(ref cr) = media.content_range {
        headers.append("Content-Range", cr);
    }
    for (k, v) in &media.headers {
        if !k.eq_ignore_ascii_case("accept-ranges") && !k.eq_ignore_ascii_case("content-range") {
            headers.append(k, v);
        }
    }
    response.set_http_headers(headers);

    request.finish_with_response(&response);
}

fn parse_window_edge(edge_str: &str) -> Option<gdk::WindowEdge> {
    match edge_str {
        "north" => Some(gdk::WindowEdge::North),
        "south" => Some(gdk::WindowEdge::South),
        "east" => Some(gdk::WindowEdge::East),
        "west" => Some(gdk::WindowEdge::West),
        "north-west" => Some(gdk::WindowEdge::NorthWest),
        "north-east" => Some(gdk::WindowEdge::NorthEast),
        "south-west" => Some(gdk::WindowEdge::SouthWest),
        "south-east" => Some(gdk::WindowEdge::SouthEast),
        _ => None,
    }
}

fn install_win_drag_handler(ucm: &UserContentManager, webview: &WebView) {
    let wv_weak = {
        let w = glib::WeakRef::new();
        w.set(Some(webview));
        w
    };
    ucm.connect_script_message_received(Some("__olaz_win_drag"), move |_, js_res| {
        let Some(wv) = wv_weak.upgrade() else {
            return;
        };
        let Some(win) = wv
            .toplevel()
            .and_then(|top| top.downcast::<gtk::Window>().ok())
        else {
            return;
        };
        let Some(val) = js_res.js_value() else {
            return;
        };
        let raw = val.to_str();
        let Ok(payload) = serde_json::from_str::<Value>(raw.as_str()) else {
            return;
        };
        let action = payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("move");
        let button = payload
            .get("button")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .clamp(1, 5) as i32;
        let screen_x = payload
            .get("screenX")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .round() as i32;
        let screen_y = payload
            .get("screenY")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .round() as i32;
        let time = gtk::current_event_time();

        match action {
            "toggle-maximize" => {
                if win.is_maximized() {
                    win.unmaximize();
                } else {
                    win.maximize();
                }
            }
            "resize" => {
                if !win.is_maximized() {
                    if let Some(edge) = payload
                        .get("edge")
                        .and_then(Value::as_str)
                        .and_then(parse_window_edge)
                    {
                        win.begin_resize_drag(edge, button, screen_x, screen_y, time);
                    }
                }
            }
            _ => {
                win.begin_move_drag(button, screen_x, screen_y, time);
            }
        }
    });
}

fn wire_ucm_znet_and_zbridge(
    ucm: &UserContentManager,
    webview: &WebView,
    bridge: &Rc<ZaloBridge>,
    network: &NetworkProxy,
    tokio_handle: &tokio::runtime::Handle,
    on_console: &Rc<RefCell<Option<Rc<dyn Fn(String)>>>>,
) {
    let console_slot = Rc::clone(on_console);
    ucm.connect_script_message_received(Some("__olaz_zlog"), move |_, js_res| {
        let Some(val) = js_res.js_value() else {
            return;
        };
        let line = val.to_str().to_string();
        let cb = console_slot.borrow().clone();
        if let Some(cb) = cb {
            cb(line);
        } else {
            tracing::trace!(target: "olaz::renderer", "{line}");
        }
    });

    let net_proxy = network.clone();
    let rt_handle = tokio_handle.clone();
    let wv_weak_net = {
        let w = glib::WeakRef::new();
        w.set(Some(webview));
        w
    };
    ucm.connect_script_message_received(Some("__olaz_znet"), move |_, js_res| {
        let Some(val) = js_res.js_value() else {
            return;
        };
        let raw = val.to_str();
        let Ok(envelope) = serde_json::from_str::<Value>(raw.as_str()) else {
            return;
        };
        let Some(rpc_id) = envelope.get("id").and_then(Value::as_u64) else {
            return;
        };
        let body = envelope.get("body").cloned().unwrap_or(Value::Null);

        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel::<WsEvent>();
        let wv_for_ws = wv_weak_net.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Some(ev) = ws_rx.recv().await {
                let Some(wv) = wv_for_ws.upgrade() else {
                    break;
                };
                let kind_json =
                    serde_json::to_string(&ev.kind).unwrap_or_else(|_| "\"error\"".to_string());
                let payload_json =
                    serde_json::to_string(&ev.payload).unwrap_or_else(|_| "{}".to_string());
                let js = format!(
                    "window.__zaloNet && window.__zaloNet.socketEvent({}, {}, {});",
                    ev.id, kind_json, payload_json
                );
                wv.evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
            }
        });

        let net_clone = net_proxy.clone();
        let wv_for_reply = wv_weak_net.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        rt_handle.spawn(async move {
            let out = net_clone
                .handle_request(body, WsEventSink::from_sender(ws_tx))
                .await;
            let _ = done_tx.send(out);
        });

        glib::MainContext::default().spawn_local(async move {
            if let Ok(result) = done_rx.await {
                if let Some(wv) = wv_for_reply.upgrade() {
                    send_rpc_reply(&wv, rpc_id, result);
                }
            }
        });
    });

    let bridge_for_msg = Rc::clone(bridge);
    let wv_weak_bridge = {
        let w = glib::WeakRef::new();
        w.set(Some(webview));
        w
    };
    ucm.connect_script_message_received(Some("__olaz_zbridge"), move |_, js_res| {
        let Some(val) = js_res.js_value() else {
            return;
        };
        let raw = val.to_str();
        let Ok(envelope) = serde_json::from_str::<Value>(raw.as_str()) else {
            return;
        };
        let Some(rpc_id) = envelope.get("id").and_then(Value::as_u64) else {
            return;
        };
        let body = envelope.get("body").cloned().unwrap_or(Value::Null);
        let wv_reply = wv_weak_bridge.clone();
        bridge_for_msg.handle_async_message(
            body,
            Box::new(move |res| {
                if let Some(wv) = wv_reply.upgrade() {
                    send_rpc_reply(&wv, rpc_id, res);
                }
            }),
        );
    });
}

fn configure_zalo_policy(webview: &WebView, allow_popups: bool) {
    webview.connect_decide_policy(move |_, decision, decision_type| {
        if decision_type != PolicyDecisionType::NavigationAction
            && decision_type != PolicyDecisionType::NewWindowAction
        {
            return false;
        }
        let Ok(nav_decision) = decision.clone().downcast::<NavigationPolicyDecision>() else {
            return false;
        };
        let Some(nav_action) = nav_decision.navigation_action() else {
            decision.ignore();
            return true;
        };
        let Some(req) = nav_action.request() else {
            decision.ignore();
            return true;
        };
        let Some(uri_gstr) = req.uri() else {
            decision.ignore();
            return true;
        };
        let uri = uri_gstr.as_str();
        let scheme = Url::parse(uri)
            .ok()
            .map(|u| u.scheme().to_ascii_lowercase())
            .unwrap_or_default();

        if decision_type == PolicyDecisionType::NewWindowAction {
            if allow_popups
                && matches!(
                    scheme.as_str(),
                    ZALO_APP_SCHEME | ZFILE_SCHEME | "about" | "blob" | "data" | ""
                )
            {
                decision.use_();
                return true;
            }
            if matches!(scheme.as_str(), "http" | "https" | "mailto") {
                let _ = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE);
            }
            decision.ignore();
            return true;
        }

        if matches!(
            scheme.as_str(),
            ZALO_APP_SCHEME | ZFILE_SCHEME | "about" | "blob" | "data"
        ) {
            decision.use_();
            return true;
        }

        // Use `NavigationPolicyDecisionExt::frame_name` so Ubuntu 22.04 LTS (`webkit2gtk-4.1` 2.38)
        // does not require WebKit 2.40+ symbols.
        #[allow(deprecated)]
        let is_subframe = nav_decision.frame_name().is_some();
        if is_subframe {
            decision.use_();
            return true;
        }

        if matches!(scheme.as_str(), "http" | "https" | "mailto") {
            let _ = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE);
        }
        decision.ignore();
        true
    });
}

/// WebKit2GTK host running `login.html`, `index.html`, or `shared-worker.html`.
///
/// When `direct_ui` is `true`, `webview` is mounted directly inside `OlazAppWindow` with
/// `vndangkhoa/zalo-linux` renderer patches, frameless drag/resize, XDG portal file choosers,
/// `MediaViewer` (`popup-viewer.html`) popup window support, and inline GStreamer media playback.
pub struct OfficialClientHost {
    platform: Arc<ZaloPlatform>,
    runtime: ZaloWebRuntime,
    bridge: Rc<ZaloBridge>,
    network: NetworkProxy,
    ucm: UserContentManager,
    offscreen_window: gtk::OffscreenWindow,
    webview: WebView,
    direct_ui: bool,
    current_page: RefCell<Option<HostPage>>,
    pending_files: Rc<RefCell<Option<Vec<String>>>>,
    file_pick_lock: tokio::sync::Mutex<()>,
    debug_window: RefCell<Option<gtk::Window>>,
    popup_bridge_factory: Rc<RefCell<Option<PopupBridgeFactory>>>,
    child_windows: Rc<RefCell<Vec<(String, gtk::Window)>>>,
    crash_timestamps: RefCell<Vec<Instant>>,
    shut_down: Cell<bool>,
    on_state: Rc<RefCell<Option<Rc<dyn Fn(Value)>>>>,
    on_console: Rc<RefCell<Option<Rc<dyn Fn(String)>>>>,
    on_page_event: Rc<RefCell<Option<Rc<dyn Fn(PageEvent)>>>>,
}

impl OfficialClientHost {
    const CRASH_WINDOW: Duration = Duration::from_secs(15);
    const MAX_CRASH_RECOVERIES: usize = 3;

    pub fn new(
        platform: &Arc<ZaloPlatform>,
        runtime: ZaloWebRuntime,
        network: NetworkProxy,
        tokio_handle: tokio::runtime::Handle,
        is_primary: bool,
        configure_bridge: impl FnOnce(&Rc<ZaloBridge>),
    ) -> Rc<Self> {
        Self::new_with_mode(
            platform,
            runtime,
            network,
            tokio_handle,
            is_primary,
            false,
            configure_bridge,
        )
    }

    pub fn new_with_mode(
        platform: &Arc<ZaloPlatform>,
        runtime: ZaloWebRuntime,
        network: NetworkProxy,
        tokio_handle: tokio::runtime::Handle,
        is_primary: bool,
        direct_ui: bool,
        configure_bridge: impl FnOnce(&Rc<ZaloBridge>),
    ) -> Rc<Self> {
        let bridge = ZaloBridge::new();
        configure_bridge(&bridge);

        let ucm = UserContentManager::new();
        ucm.register_script_message_handler("__olaz_zlog");
        ucm.register_script_message_handler("__olaz_zstate");
        ucm.register_script_message_handler("__olaz_znet");
        ucm.register_script_message_handler("__olaz_zbridge");
        if direct_ui {
            ucm.register_script_message_handler("__olaz_win_drag");
        }

        populate_host_user_scripts(&ucm, platform, direct_ui);

        let ua = platform.identity.user_agent();
        let settings = build_zalo_webkit_settings(&ua, direct_ui);

        let webview = WebView::builder()
            .web_context(runtime.context())
            .user_content_manager(&ucm)
            .settings(&settings)
            .build();
        webview.set_hexpand(true);
        webview.set_vexpand(true);

        let offscreen_window = gtk::OffscreenWindow::new();
        offscreen_window.set_default_size(1100, 760);
        if !direct_ui {
            webview.set_size_request(1100, 760);
        }
        offscreen_window.add(&webview);
        offscreen_window.show_all();

        bridge.attach_webview(&webview);
        runtime.register_bridge(&webview, &bridge, is_primary);

        let on_state: Rc<RefCell<Option<Rc<dyn Fn(Value)>>>> = Rc::new(RefCell::new(None));
        let on_console: Rc<RefCell<Option<Rc<dyn Fn(String)>>>> = Rc::new(RefCell::new(None));
        let on_page_event: Rc<RefCell<Option<Rc<dyn Fn(PageEvent)>>>> =
            Rc::new(RefCell::new(None));
        let pending_files: Rc<RefCell<Option<Vec<String>>>> = Rc::new(RefCell::new(None));
        let popup_bridge_factory: Rc<RefCell<Option<PopupBridgeFactory>>> =
            Rc::new(RefCell::new(None));
        let child_windows: Rc<RefCell<Vec<(String, gtk::Window)>>> =
            Rc::new(RefCell::new(Vec::new()));

        let host = Rc::new(Self {
            platform: Arc::clone(platform),
            runtime: runtime.clone(),
            bridge: Rc::clone(&bridge),
            network: network.clone(),
            ucm: ucm.clone(),
            offscreen_window,
            webview: webview.clone(),
            direct_ui,
            current_page: RefCell::new(None),
            pending_files: Rc::clone(&pending_files),
            file_pick_lock: tokio::sync::Mutex::new(()),
            debug_window: RefCell::new(None),
            popup_bridge_factory: Rc::clone(&popup_bridge_factory),
            child_windows: Rc::clone(&child_windows),
            crash_timestamps: RefCell::new(Vec::new()),
            shut_down: Cell::new(false),
            on_state: Rc::clone(&on_state),
            on_console: Rc::clone(&on_console),
            on_page_event: Rc::clone(&on_page_event),
        });

        wire_ucm_znet_and_zbridge(
            &ucm,
            &webview,
            &bridge,
            &network,
            &tokio_handle,
            &on_console,
        );

        if direct_ui {
            install_win_drag_handler(&ucm, &webview);
        }

        // Wire `__olaz_zstate`
        let state_slot = Rc::clone(&on_state);
        ucm.connect_script_message_received(Some("__olaz_zstate"), move |_, js_res| {
            let Some(val) = js_res.js_value() else {
                return;
            };
            let raw = val.to_str();
            let Ok(parsed) = serde_json::from_str::<Value>(raw.as_str()) else {
                tracing::warn!("ignoring malformed zstate JSON payload");
                return;
            };
            let cb = state_slot.borrow().clone();
            if let Some(cb) = cb {
                cb(parsed);
            }
        });

        // File chooser hook: uses `pending_files` when scripted, or launches XDG FileChooser portal in `direct_ui` mode!
        let files_for_chooser = Rc::clone(&pending_files);
        let platform_for_chooser = Arc::clone(platform);
        let rt_for_chooser = tokio_handle.clone();
        webview.connect_run_file_chooser(move |_, req: &FileChooserRequest| {
            let queued = files_for_chooser.borrow_mut().take();
            if let Some(paths) = queued {
                if paths.is_empty() {
                    req.cancel();
                } else {
                    let max = if req.selects_multiple() {
                        paths.len()
                    } else {
                        1
                    };
                    let refs: Vec<&str> = paths.iter().take(max).map(String::as_str).collect();
                    req.select_files(&refs);
                }
                return true;
            }

            if !direct_ui {
                req.cancel();
                return true;
            }

            let multiple = req.selects_multiple();
            let req_owned = req.clone();
            let platform_pick = Arc::clone(&platform_for_chooser);
            let (tx, rx) = tokio::sync::oneshot::channel();
            rt_for_chooser.spawn(async move {
                let picked =
                    olaz_linux::pick_open_dialog("Select File", multiple, false, None, &[]).await;
                let _ = tx.send(picked);
            });
            glib::MainContext::default().spawn_local(async move {
                match rx.await {
                    Ok(Ok(paths)) if !paths.is_empty() => {
                        let mut strings = Vec::with_capacity(paths.len());
                        for p in paths {
                            platform_pick.grant_readable_path(&p);
                            strings.push(p.to_string_lossy().into_owned());
                        }
                        let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
                        req_owned.select_files(&refs);
                    }
                    _ => {
                        req_owned.cancel();
                    }
                }
            });
            true
        });

        // Drag-and-drop file path extraction (`File.path` parity with Electron in `direct_ui` mode)
        if direct_ui {
            let platform_for_dnd = Arc::clone(platform);
            webview.connect_drag_data_received(move |wv, _ctx, _x, _y, sel_data, _info, _time| {
                let uris = sel_data.uris();
                if uris.is_empty() {
                    return;
                }
                let mut local_paths = Vec::new();
                for uri in uris {
                    if let Ok(parsed) = Url::parse(uri.as_str()) {
                        if let Ok(path) = parsed.to_file_path() {
                            platform_for_dnd.grant_readable_path(&path);
                            local_paths.push(path.to_string_lossy().into_owned());
                        }
                    }
                }
                if !local_paths.is_empty() {
                    if let Ok(json_arr) = serde_json::to_string(&local_paths) {
                        let js = format!("window.__olazLastDragPaths = {json_arr};");
                        wv.evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
                    }
                }
            });
        }

        configure_zalo_policy(&webview, direct_ui);

        // Secondary window (`MediaViewer` `popup-viewer.html` & `child.html`) creation
        if direct_ui {
            let platform_popup = Arc::clone(platform);
            let runtime_popup = runtime;
            let network_popup = network;
            let tokio_popup = tokio_handle;
            let parent_bridge_popup = Rc::clone(&bridge);
            let factory_slot = Rc::clone(&popup_bridge_factory);
            let children_slot = Rc::clone(&child_windows);
            let console_popup = Rc::clone(&on_console);

            webview.connect_create(move |parent_wv, nav_action: &NavigationAction| {
                let target_uri = nav_action
                    .request()
                    .and_then(|r| r.uri())
                    .map(|g| g.to_string())
                    .unwrap_or_default();
                let scheme = Url::parse(&target_uri)
                    .ok()
                    .map(|u| u.scheme().to_ascii_lowercase())
                    .unwrap_or_default();

                if matches!(scheme.as_str(), "http" | "https" | "mailto") {
                    let _ =
                        gio::AppInfo::launch_default_for_uri(&target_uri, gio::AppLaunchContext::NONE);
                    return None;
                }

                let mut nav_clone = nav_action.clone();
                let frame_name = nav_clone
                    .frame_name()
                    .map(|g| g.to_string())
                    .unwrap_or_default();
                let parse_param = |raw: &str, key: &str| -> Option<String> {
                    let q = raw.split_once('?').map(|(_, q)| q).unwrap_or(raw);
                    url::form_urlencoded::parse(q.as_bytes())
                        .find(|(k, _)| k == key)
                        .map(|(_, v)| v.into_owned())
                };
                let viewer_window_id = parse_param(&frame_name, "zpopupWindowId")
                    .or_else(|| parse_param(&target_uri, "zpopupWindowId"))
                    .unwrap_or_else(|| {
                        if target_uri.contains("file-viewer") || frame_name.contains("file-viewer")
                        {
                            "file-viewer".to_owned()
                        } else {
                            "photo-viewer".to_owned()
                        }
                    });
                let default_w = parse_param(&frame_name, "width")
                    .and_then(|s| s.parse::<i32>().ok())
                    .map(|v| v.clamp(520, 3840))
                    .unwrap_or(1040);
                let default_h = parse_param(&frame_name, "height")
                    .and_then(|s| s.parse::<i32>().ok())
                    .map(|v| v.clamp(400, 2160))
                    .unwrap_or(720);

                let popup_win = gtk::Window::new(gtk::WindowType::Toplevel);
                popup_win.set_title("Zalo — Media Viewer");
                popup_win.set_decorated(false);
                popup_win.set_default_size(default_w, default_h);
                popup_win.set_size_request(520, 400);
                popup_win.set_position(gtk::WindowPosition::Center);

                let child_ucm = UserContentManager::new();
                child_ucm.register_script_message_handler("__olaz_zlog");
                child_ucm.register_script_message_handler("__olaz_zstate");
                child_ucm.register_script_message_handler("__olaz_znet");
                child_ucm.register_script_message_handler("__olaz_zbridge");
                child_ucm.register_script_message_handler("__olaz_win_drag");
                populate_popup_user_scripts(&child_ucm, &platform_popup);

                let child_settings =
                    build_zalo_webkit_settings(&platform_popup.identity.user_agent(), true);
                let child_wv = WebView::builder()
                    .related_view(parent_wv)
                    .user_content_manager(&child_ucm)
                    .settings(&child_settings)
                    .build();
                child_wv.set_hexpand(true);
                child_wv.set_vexpand(true);

                let child_bridge = ZaloBridge::new();
                child_bridge.attach_webview(&child_wv);
                runtime_popup.register_bridge(&child_wv, &child_bridge, false);

                if let Some(factory) = factory_slot.borrow().clone() {
                    factory(&child_bridge, &popup_win, &parent_bridge_popup);
                }

                wire_ucm_znet_and_zbridge(
                    &child_ucm,
                    &child_wv,
                    &child_bridge,
                    &network_popup,
                    &tokio_popup,
                    &console_popup,
                );
                install_win_drag_handler(&child_ucm, &child_wv);
                configure_zalo_policy(&child_wv, false);

                popup_win.add(&child_wv);
                popup_win.show_all();
                popup_win.present();
                children_slot
                    .borrow_mut()
                    .push((viewer_window_id.clone(), popup_win.clone()));

                let win_for_ready = popup_win.clone();
                child_wv.connect_ready_to_show(move |_| {
                    win_for_ready.show_all();
                    win_for_ready.present();
                });

                let win_for_load = popup_win.clone();
                let child_bridge_load = Rc::clone(&child_bridge);
                child_wv.connect_load_changed(move |_, ev| {
                    if ev == LoadEvent::Finished {
                        if !win_for_load.is_visible() {
                            win_for_load.show_all();
                            win_for_load.present();
                        }
                        child_bridge_load.send("visibility-change", vec![json!("show")]);
                        child_bridge_load
                            .send("visibility-change", vec![json!("focus"), json!("1")]);
                    }
                });

                let win_for_close = popup_win.clone();
                child_wv.connect_close(move |_| {
                    win_for_close.close();
                });

                popup_win.connect_key_press_event(|w, ev| {
                    if ev.keyval() == gdk::keys::constants::Escape {
                        w.close();
                        return glib::Propagation::Stop;
                    }
                    glib::Propagation::Proceed
                });

                let child_bridge_state = Rc::clone(&child_bridge);
                popup_win.connect_window_state_event(move |_, ev| {
                    if ev
                        .changed_mask()
                        .contains(gdk::WindowState::MAXIMIZED)
                    {
                        if ev.new_window_state().contains(gdk::WindowState::MAXIMIZED) {
                            child_bridge_state.send("visibility-change", vec![json!("maximize")]);
                        } else {
                            child_bridge_state
                                .send("visibility-change", vec![json!("unmaximize")]);
                        }
                    }
                    glib::Propagation::Proceed
                });

                let rt_cleanup = runtime_popup.clone();
                let child_wv_cleanup = child_wv.clone();
                let parent_bridge_cleanup = Rc::clone(&parent_bridge_popup);
                let children_cleanup = Rc::clone(&children_slot);
                popup_win.connect_destroy(move |destroyed_win| {
                    rt_cleanup.unregister_webview(&child_wv_cleanup);
                    children_cleanup
                        .borrow_mut()
                        .retain(|(_, w)| w.as_ptr() != destroyed_win.as_ptr());
                    parent_bridge_cleanup
                        .send("close-media-viewer", vec![json!(viewer_window_id)]);
                });

                Some(child_wv.upcast::<gtk::Widget>())
            });
        }

        // Page load lifecycle callbacks
        let host_weak_load = Rc::downgrade(&host);
        webview.connect_load_changed(move |_, load_event| {
            let Some(h) = host_weak_load.upgrade() else {
                return;
            };
            match load_event {
                LoadEvent::Started => {
                    let cb = h.on_page_event.borrow().clone();
                    if let Some(cb) = cb {
                        cb(PageEvent::StartedLoading);
                    }
                }
                LoadEvent::Finished => {
                    let page_opt = *h.current_page.borrow();
                    if let Some(page) = page_opt {
                        let cb = h.on_page_event.borrow().clone();
                        if let Some(cb) = cb {
                            cb(PageEvent::FinishedLoading(page));
                        }
                    }
                }
                _ => {}
            }
        });

        let host_weak_fail = Rc::downgrade(&host);
        webview.connect_load_failed(move |_, _event, failing_uri, err| {
            if let Some(h) = host_weak_fail.upgrade() {
                let cb = h.on_page_event.borrow().clone();
                if let Some(cb) = cb {
                    cb(PageEvent::Failed(format!("{failing_uri}: {err}")));
                }
            }
            false
        });

        // Emit `PageEvent::Terminated` (or rate-limited reload fallback) if WebKitWebProcess crashes.
        let host_weak_term = Rc::downgrade(&host);
        webview.connect_web_process_terminated(move |wv, reason| {
            tracing::error!(?reason, "Zalo renderer WebProcess terminated");
            let Some(h) = host_weak_term.upgrade() else {
                return;
            };
            if h.shut_down.get() {
                return;
            }
            let cb = h.on_page_event.borrow().clone();
            if let Some(cb) = cb {
                cb(PageEvent::Terminated);
            } else if h.allow_crash_recovery() {
                wv.reload();
            } else {
                tracing::error!(
                    "Zalo renderer WebProcess crashed repeatedly (>3 in 15s); suppressing reload loop"
                );
            }
        });

        host
    }

    pub fn bridge(&self) -> &Rc<ZaloBridge> {
        &self.bridge
    }

    pub fn webview(&self) -> &WebView {
        &self.webview
    }

    pub fn is_direct_ui(&self) -> bool {
        self.direct_ui
    }

    #[allow(dead_code)]
    pub fn current_page(&self) -> Option<HostPage> {
        *self.current_page.borrow()
    }

    pub fn set_popup_bridge_factory(
        &self,
        factory: impl Fn(&Rc<ZaloBridge>, &gtk::Window, &Rc<ZaloBridge>) + 'static,
    ) {
        *self.popup_bridge_factory.borrow_mut() = Some(Rc::new(factory));
    }

    /// Rebuilds all injected `UserScript`s on `self.ucm` using the latest `platform.app_version()`
    /// (called after first-run DMG installation updates `platform.app_version`).
    pub fn reload_user_scripts(&self, platform: &ZaloPlatform) {
        self.ucm.remove_all_scripts();
        populate_host_user_scripts(&self.ucm, platform, self.direct_ui);
    }

    /// Records a WebProcess crash timestamp and returns `true` if automatic recovery is allowed
    /// (at most 3 crashes within a rolling 15-second window).
    pub fn allow_crash_recovery(&self) -> bool {
        let now = Instant::now();
        let mut times = self.crash_timestamps.borrow_mut();
        times.retain(|t| now.saturating_duration_since(*t) < Self::CRASH_WINDOW);
        if times.len() >= Self::MAX_CRASH_RECOVERIES {
            return false;
        }
        times.push(now);
        true
    }

    pub fn set_on_state(&self, cb: impl Fn(Value) + 'static) {
        *self.on_state.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn set_on_console(&self, cb: impl Fn(String) + 'static) {
        *self.on_console.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn set_on_page_event(&self, cb: impl Fn(PageEvent) + 'static) {
        *self.on_page_event.borrow_mut() = Some(Rc::new(cb));
    }

    /// Navigates the WebView to `zalo-app://local/<page>?<sorted_query>`.
    pub fn load(&self, page: HostPage, query: &BTreeMap<String, String>) {
        *self.current_page.borrow_mut() = Some(page);
        let mut url =
            Url::parse(&format!("{ZALO_APP_SCHEME}://local/{}", page.filename())).unwrap();
        {
            let mut qp = url.query_pairs_mut();
            for (k, v) in query {
                qp.append_pair(k, v);
            }
        }
        tracing::info!(page = page.filename(), "loading official Zalo page");
        self.webview.load_uri(url.as_str());
    }

    /// Evaluates an async JS function body inside the renderer (`return await ...`) and returns JSON.
    pub async fn evaluate(&self, script_body: &str) -> Result<Value, String> {
        eval_async_js_on_webview(&self.webview, script_body).await
    }

    /// Evaluates `script_body` while answering `<input type="file">` with `files`.
    #[allow(dead_code)]
    pub async fn evaluate_providing_files(
        &self,
        script_body: &str,
        files: &[PathBuf],
    ) -> Result<Value, String> {
        let _guard = self.file_pick_lock.lock().await;
        for p in files {
            self.platform.grant_readable_path(p);
        }
        let path_strings: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        *self.pending_files.borrow_mut() = Some(path_strings);
        let res = self.evaluate(script_body).await;
        *self.pending_files.borrow_mut() = None;
        res
    }

    /// Reparents `self.webview` into `target` container (used by `OlazAppWindow` to display
    /// Zalo's renderer directly inside the frameless primary window).
    pub fn mount_into(&self, target: &impl IsA<gtk::Container>) {
        reparent_webview_to(&self.webview, target.upcast_ref::<gtk::Container>());
    }

    /// Opens or brings to front a developer window displaying the Zalo DOM (used by `--engine-probe`).
    pub fn show_debug_window(&self) {
        if self.direct_ui {
            if let Some(win) = self
                .webview
                .toplevel()
                .and_then(|w| w.downcast::<gtk::Window>().ok())
            {
                win.present();
            }
            return;
        }
        if self.debug_window.borrow().is_none() {
            let win = gtk::Window::new(gtk::WindowType::Toplevel);
            win.set_title("Olaz — Zalo Renderer (Debug)");
            win.set_default_size(1100, 760);

            let wv_weak = {
                let w = glib::WeakRef::new();
                w.set(Some(&self.webview));
                w
            };
            let off_weak = {
                let w = glib::WeakRef::new();
                w.set(Some(&self.offscreen_window));
                w
            };
            win.connect_delete_event(move |w, _| {
                w.hide();
                if let (Some(wv), Some(off)) = (wv_weak.upgrade(), off_weak.upgrade()) {
                    reparent_webview_to(&wv, off.upcast_ref::<gtk::Container>());
                    off.show_all();
                }
                glib::Propagation::Stop
            });
            *self.debug_window.borrow_mut() = Some(win);
        }
        if let Some(ref win) = *self.debug_window.borrow() {
            reparent_webview_to(&self.webview, win.upcast_ref::<gtk::Container>());
            win.show_all();
            win.present();
        }
    }

    /// Toggles visibility of the developer renderer window in headless mode.
    pub fn toggle_debug_window(&self) -> bool {
        if self.direct_ui {
            return true;
        }
        let currently_visible = self
            .debug_window
            .borrow()
            .as_ref()
            .is_some_and(|w| w.is_visible());
        if currently_visible {
            if let Some(ref win) = *self.debug_window.borrow() {
                win.hide();
            }
            reparent_webview_to(
                &self.webview,
                self.offscreen_window.upcast_ref::<gtk::Container>(),
            );
            self.offscreen_window.show_all();
            false
        } else {
            self.show_debug_window();
            true
        }
    }

    fn find_child_windows(&self, window_id: &str) -> Vec<gtk::Window> {
        self.child_windows
            .borrow()
            .iter()
            .filter(|(id, _)| id == window_id)
            .map(|(_, w)| w.clone())
            .collect()
    }

    pub fn close_child_window(&self, window_id: &str) -> bool {
        let wins = self.find_child_windows(window_id);
        if wins.is_empty() {
            return false;
        }
        for w in wins {
            w.close();
        }
        true
    }

    pub fn focus_child_window(&self, window_id: &str) -> bool {
        let wins = self.find_child_windows(window_id);
        if wins.is_empty() {
            return false;
        }
        for w in wins {
            w.deiconify();
            w.present();
        }
        true
    }

    pub fn minimize_child_window(&self, window_id: &str) -> bool {
        let wins = self.find_child_windows(window_id);
        if wins.is_empty() {
            return false;
        }
        for w in wins {
            w.iconify();
        }
        true
    }

    pub fn toggle_maximize_child_window(&self, window_id: &str, target: Option<bool>) -> bool {
        let wins = self.find_child_windows(window_id);
        if wins.is_empty() {
            return false;
        }
        for w in wins {
            let should_max = target.unwrap_or_else(|| !w.is_maximized());
            if should_max {
                w.maximize();
            } else {
                w.unmaximize();
            }
        }
        true
    }

    pub fn toggle_fullscreen_child_window(&self, window_id: &str, target: Option<bool>) -> bool {
        let wins = self.find_child_windows(window_id);
        if wins.is_empty() {
            return false;
        }
        for w in wins {
            let cur = w
                .window()
                .is_some_and(|gw| gw.state().contains(gdk::WindowState::FULLSCREEN));
            let should_fs = target.unwrap_or(!cur);
            if should_fs {
                w.fullscreen();
            } else {
                w.unfullscreen();
            }
        }
        true
    }

    pub fn show_child_window(&self, window_id: &str, focus: bool) -> bool {
        let wins = self.find_child_windows(window_id);
        if wins.is_empty() {
            return false;
        }
        for w in wins {
            w.show_all();
            w.deiconify();
            if focus {
                w.present();
            }
        }
        true
    }

    /// Tears down active sockets, closes popup windows, unregisters the WebView, and destroys containers.
    pub fn shutdown(&self) {
        if self.shut_down.replace(true) {
            return;
        }
        let popups: Vec<(String, gtk::Window)> = self.child_windows.borrow_mut().drain(..).collect();
        for (_, p) in popups {
            unsafe {
                p.destroy();
            }
        }
        self.network.close_all_sockets();
        self.runtime.unregister_webview(&self.webview);
        self.webview.stop_loading();
        self.webview.load_html("", None);
        if let Some(parent) = self.webview.parent() {
            if let Ok(container) = parent.downcast::<gtk::Container>() {
                container.remove(&self.webview);
            }
        }
        if let Some(win) = self.debug_window.borrow_mut().take() {
            unsafe {
                win.destroy();
            }
        }
        unsafe {
            self.offscreen_window.destroy();
        }
    }
}

fn build_zalo_webkit_settings(user_agent: &str, direct_ui: bool) -> Settings {
    let settings = Settings::new();
    settings.set_user_agent(Some(user_agent));
    settings.set_enable_page_cache(false);
    if std::env::var_os("OLAZ_ENABLE_WEBKIT_HWACCEL").is_none() {
        settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
    }
    settings.set_enable_developer_extras(true);
    settings.set_javascript_can_open_windows_automatically(direct_ui);
    settings.set_media_playback_requires_user_gesture(!direct_ui);
    settings.set_enable_back_forward_navigation_gestures(false);
    settings.set_enable_write_console_messages_to_stdout(false);
    settings
}

fn inject_common_preamble_and_meta(ucm: &UserContentManager, platform: &ZaloPlatform) {
    ucm.add_script(&UserScript::new(
        OLAZ_WEBKIT_BRIDGE_PREAMBLE,
        UserContentInjectedFrames::AllFrames,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    ));

    let ver_json =
        serde_json::to_string(&platform.app_version()).unwrap_or_else(|_| "\"26.10.20\"".into());
    let runtime_meta_script = format!(
        "(() => {{\n\
           'use strict';\n\
           const appVersion = {ver_json};\n\
           window.__zaloRuntime = Object.assign(window.__zaloRuntime || {{}}, {{ appVersion }});\n\
           if (window === window.top && appVersion && appVersion !== '0.1.0') {{\n\
             try {{\n\
               const cur = window.localStorage && window.localStorage.getItem('sh_app_ver');\n\
               if (cur === '0.1.0' || cur === '\"0.1.0\"') {{\n\
                 window.localStorage.removeItem('sh_app_ver');\n\
               }}\n\
             }} catch {{}}\n\
           }}\n\
         }})();\n//# sourceURL=olaz-runtime-meta.js"
    );
    ucm.add_script(&UserScript::new(
        &runtime_meta_script,
        UserContentInjectedFrames::AllFrames,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    ));

    if !olaz_linux::has_gstreamer_audio_sink() {
        tracing::warn!(
            "No GStreamer audio sink plugin (libgstolazauto.so / libgstautodetect.so) found; injecting HTMLMediaElement guard"
        );
        ucm.add_script(&UserScript::new(
            OLAZ_MISSING_AUDIO_SINK_GUARD,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
    }
}

fn populate_host_user_scripts(
    ucm: &UserContentManager,
    platform: &ZaloPlatform,
    direct_ui: bool,
) {
    inject_common_preamble_and_meta(ucm, platform);

    let trace_workers = std::env::var_os("OLAZ_TRACE_WORKERS").is_some();
    let scripts = if direct_ui {
        direct_ui_user_scripts(platform, trace_workers)
    } else {
        ordered_user_scripts(platform, trace_workers)
    };
    for (_name, script_src) in scripts {
        ucm.add_script(&UserScript::new(
            &script_src,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
    }
}

fn populate_popup_user_scripts(ucm: &UserContentManager, platform: &ZaloPlatform) {
    inject_common_preamble_and_meta(ucm, platform);

    for (_name, script_src) in popup_user_scripts(platform) {
        ucm.add_script(&UserScript::new(
            &script_src,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
    }
}

fn reparent_webview_to(webview: &WebView, target: &gtk::Container) {
    if let Some(parent) = webview.parent() {
        if parent.as_ptr() == target.upcast_ref::<gtk::Widget>().as_ptr() {
            return;
        }
        if let Ok(container) = parent.downcast::<gtk::Container>() {
            container.remove(webview);
        }
    }
    target.add(webview);
}

fn send_rpc_reply(webview: &WebView, rpc_id: u64, result: Result<Value, String>) {
    let (ok, payload_val) = match result {
        Ok(val) => (true, val),
        Err(err) => (false, Value::String(err)),
    };
    let json_text = serde_json::to_string(&payload_val).unwrap_or_else(|_| "null".to_string());
    let quoted_json = serde_json::to_string(&json_text).unwrap_or_else(|_| "\"null\"".to_string());
    let js =
        format!("window.__olazRpcReply && window.__olazRpcReply({rpc_id}, {ok}, {quoted_json});");
    webview.evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
}

/// Evaluates `script_body` inside an `async () => { ... }` function via
/// `WebViewExt::call_async_javascript_function_future` and deserializes its result.
pub async fn eval_async_js_on_webview(
    webview: &WebView,
    script_body: &str,
) -> Result<Value, String> {
    let wrapped = format!(
        "const __olazVal = await (async () => {{\n{script_body}\n}})();\n\
         return JSON.stringify(__olazVal === undefined ? {{\"__undefined\":true}} : __olazVal, \
         (_k, v) => typeof v === 'bigint' ? String(v) : v);"
    );

    let jsc_val = webview
        .call_async_javascript_function_future(&wrapped, None, None, None)
        .await
        .map_err(|e| e.to_string())?;

    let text = jsc_val.to_str();
    let val: Value = serde_json::from_str(text.as_str()).unwrap_or(Value::Null);
    if wire::is_undefined(&val) {
        Ok(Value::Null)
    } else {
        Ok(val)
    }
}
