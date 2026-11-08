//! Electron main-process stand-in (`ZaloMainProcess`) and hidden `shared-worker.html`
//! task router (`SharedWorkerRouter`).
//!
//! Port of `Sources/Olaz/ZaloRuntime/ZaloMainProcess.swift`,
//! `Sources/Olaz/ZaloRuntime/SharedWorkerRouter.swift`, and
//! `Sources/Olaz/ZaloRuntime/BridgeImplementation.swift` for Linux.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gdk::gdk_pixbuf::prelude::*;
use gtk::prelude::*;
use olaz_core::{
    wire, CryptoBridge, FsBridge, KeychainCipher, LinuxSecretBackend, SafeStorage, ZaloPlatform,
};
use olaz_linux::{
    pick_open_dialog, pick_save_file_with_dir, FileCard, IncomingVoipCall, LinuxCallCoordinator,
    LinuxNotifier, NotificationEvent, NotificationIcon, NotificationPolicy, NotificationRequest,
    DEFAULT_KICKOUT_NOTICE,
};
use olaz_net::{
    is_observed_zalo_endpoint, DownloadManager, MediaLoader, NetworkProxy, ZaloCookieJar,
    DEFAULT_IMAGE_DEADLINE, DOWNLOAD_IPC_CHANNEL,
};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Map, Value};
use tokio::sync::oneshot;

use crate::client_host::{HostPage, OfficialClientHost, PageEvent, ZaloBridge, ZaloWebRuntime, ZALO_APP_SCHEME};

// ============================================================================
// SharedWorkerRouter (`main-dist/main.js` `sharedWorkerManager`)
// ============================================================================

#[derive(Debug)]
struct PendingWorkerTask {
    requester: Weak<ZaloBridge>,
    request_id: Value,
    task_type: String,
    handler_id: u64,
}

/// Routes `$zsharedWorker.runTask(task)` (`CLOUD_SCAN_V2`, offline sync, preview computation)
/// between the main hidden renderer (`index.html`) and the hidden `shared-worker.html` host.
pub struct SharedWorkerRouter {
    worker_bridge: RefCell<Option<Weak<ZaloBridge>>>,
    handlers: RefCell<HashMap<String, Weak<ZaloBridge>>>,
    pending: RefCell<HashMap<String, PendingWorkerTask>>,
    last_remote_config: RefCell<Option<Value>>,
    trace: RefCell<Option<Rc<dyn Fn(&str)>>>,
}

impl Default for SharedWorkerRouter {
    fn default() -> Self {
        Self {
            worker_bridge: RefCell::new(None),
            handlers: RefCell::new(HashMap::new()),
            pending: RefCell::new(HashMap::new()),
            last_remote_config: RefCell::new(None),
            trace: RefCell::new(None),
        }
    }
}

impl SharedWorkerRouter {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn set_trace(&self, cb: impl Fn(&str) + 'static) {
        *self.trace.borrow_mut() = Some(Rc::new(cb));
    }

    fn emit_trace(&self, msg: &str) {
        let cb = self.trace.borrow().clone();
        if let Some(cb) = cb {
            cb(msg);
        } else {
            tracing::debug!(target: "olaz::shared_worker", "{msg}");
        }
    }

    /// Registers the hidden `shared-worker.html` bridge; tasks fail with `"Worker is not ready"` until set.
    pub fn register_worker(&self, bridge: &Rc<ZaloBridge>) {
        let bid = bridge.id();
        *self.worker_bridge.borrow_mut() = Some(Rc::downgrade(bridge));
        self.handlers.borrow_mut().retain(|_, weak| {
            weak.upgrade()
                .is_some_and(|existing| existing.id() != bid)
        });
    }

    /// Drops handlers and fails in-flight tasks belonging to a closing or replaced worker bridge.
    pub fn reset_worker(&self, bridge: &Rc<ZaloBridge>) {
        let bid = bridge.id();
        let should_clear = self
            .worker_bridge
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|w| w.id() == bid);
        if should_clear {
            *self.worker_bridge.borrow_mut() = None;
        }

        self.handlers
            .borrow_mut()
            .retain(|_, weak| weak.upgrade().is_some_and(|b| b.id() != bid));

        let mut interrupted = Vec::new();
        {
            let mut pending = self.pending.borrow_mut();
            pending.retain(|_, entry| entry.requester.upgrade().is_some());
            let keys: Vec<String> = pending
                .iter()
                .filter(|(_, entry)| entry.handler_id == bid)
                .map(|(k, _)| k.clone())
                .collect();
            for k in keys {
                if let Some(entry) = pending.remove(&k) {
                    interrupted.push(entry);
                }
            }
        }

        for entry in interrupted {
            Self::respond(
                &entry,
                None,
                Some(json!({
                    "name": "WorkerInterrupted",
                    "message": "Worker is interrupted",
                })),
            );
        }
    }

    /// Clears per-account router state on logout.
    pub fn reset_session(&self) {
        *self.last_remote_config.borrow_mut() = None;
        self.pending.borrow_mut().clear();
    }

    /// Re-sends the latest remote config after `shared-worker.html` finishes loading.
    pub fn worker_did_finish_load(&self) {
        let cfg = self.last_remote_config.borrow().clone();
        if let Some(cfg) = cfg {
            if let Some(wb) = self.worker_bridge.borrow().as_ref().and_then(Weak::upgrade) {
                wb.send("shared-worker-remote-config-change", vec![cfg]);
            }
        }
    }

    /// Installs all 12 `shared-worker-*` IPC channels on `bridge`.
    pub fn install(self: &Rc<Self>, bridge: &Rc<ZaloBridge>) {
        bridge.acknowledge_channels(&[
            "show-shared-worker",
            "hide-shared-worker",
            "shared-worker-ping",
            "shared-worker-dispose",
        ]);

        // 1. `shared-worker-handle`: window registers itself as handler for `task.type`
        {
            let router = Rc::clone(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("shared-worker-handle", move |args| {
                let router = Rc::clone(&router);
                let bridge_weak = bridge_weak.clone();
                async move {
                    if let (Some(b), Some(task_type)) = (
                        bridge_weak.upgrade(),
                        args.first().and_then(Value::as_str),
                    ) {
                        router
                            .handlers
                            .borrow_mut()
                            .insert(task_type.to_string(), Rc::downgrade(&b));
                        router.emit_trace(&format!("register {task_type} from #{}", b.id()));
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // 2. `shared-worker-start`: after 100ms, interrupt stale pending work on other windows
        {
            let router = Rc::clone(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("shared-worker-start", move |_args| {
                let router = Rc::clone(&router);
                let bridge_weak = bridge_weak.clone();
                async move {
                    let Some(b) = bridge_weak.upgrade() else {
                        return Ok(wire::undefined());
                    };
                    let sender_id = b.id();
                    router.emit_trace(&format!("start from #{sender_id}"));
                    let before: HashSet<String> =
                        router.pending.borrow().keys().cloned().collect();
                    if !before.is_empty() {
                        let router_delayed = Rc::clone(&router);
                        glib::MainContext::default().spawn_local(async move {
                            glib::timeout_future(Duration::from_millis(100)).await;
                            let mut to_interrupt = Vec::new();
                            {
                                let mut pending = router_delayed.pending.borrow_mut();
                                for key in before {
                                    let stale = pending
                                        .get(&key)
                                        .is_some_and(|e| e.handler_id != sender_id);
                                    if stale {
                                        if let Some(entry) = pending.remove(&key) {
                                            to_interrupt.push(entry);
                                        }
                                    }
                                }
                            }
                            for entry in to_interrupt {
                                Self::respond(
                                    &entry,
                                    None,
                                    Some(json!({
                                        "name": "WorkerInterrupted",
                                        "message": "Worker is interrupted",
                                    })),
                                );
                            }
                        });
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // 3. `shared-worker-run`: route `{id, type, ...}` to registered handler window
        {
            let router = Rc::clone(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("shared-worker-run", move |args| {
                let router = Rc::clone(&router);
                let bridge_weak = bridge_weak.clone();
                async move {
                    let (Some(requester), Some(task_obj)) = (
                        bridge_weak.upgrade(),
                        args.first().and_then(Value::as_object).cloned(),
                    ) else {
                        return Ok(wire::undefined());
                    };
                    let request_id = task_obj.get("id").cloned().unwrap_or(Value::Null);
                    let task_type = task_obj
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();

                    let worker_alive = router
                        .worker_bridge
                        .borrow()
                        .as_ref()
                        .and_then(Weak::upgrade)
                        .is_some();
                    if !worker_alive {
                        Self::reply_error(&requester, request_id, "Worker is not ready");
                        return Ok(wire::undefined());
                    }

                    let handler_opt = router
                        .handlers
                        .borrow()
                        .get(&task_type)
                        .and_then(Weak::upgrade);
                    let Some(handler) = handler_opt else {
                        router.emit_trace(&format!("run {task_type} -> unsupported (no handler)"));
                        Self::reply_error(&requester, request_id, "Task type is not supported");
                        return Ok(wire::undefined());
                    };

                    let key = format!("{}_{}", requester.id(), value_id_str(&request_id));
                    router.pending.borrow_mut().insert(
                        key.clone(),
                        PendingWorkerTask {
                            requester: Rc::downgrade(&requester),
                            request_id,
                            task_type: task_type.clone(),
                            handler_id: handler.id(),
                        },
                    );
                    router.emit_trace(&format!(
                        "run {task_type} id={key} #{} -> #{}",
                        requester.id(),
                        handler.id()
                    ));

                    let mut routed = task_obj;
                    routed.insert("id".to_string(), Value::String(key));
                    handler.send("shared-worker-run", vec![Value::Object(routed)]);
                    Ok(wire::undefined())
                }
            });
        }

        // 4. `shared-worker-resp`: route `{id, result?, error?}` back to original requester
        {
            let router = Rc::clone(self);
            bridge.handle_channel("shared-worker-resp", move |args| {
                let router = Rc::clone(&router);
                async move {
                    let Some(msg) = args.first().and_then(Value::as_object) else {
                        return Ok(wire::undefined());
                    };
                    let Some(id_val) = msg.get("id") else {
                        return Ok(wire::undefined());
                    };
                    let key = value_id_str(id_val);
                    let Some(entry) = router.pending.borrow_mut().remove(&key) else {
                        return Ok(wire::undefined());
                    };
                    let result = msg.get("result").cloned();
                    let error = msg.get("error").cloned();
                    router.emit_trace(&format!(
                        "resp {} id={key} {}",
                        entry.task_type,
                        if error.is_none() { "ok" } else { "error" }
                    ));
                    Self::respond(&entry, result, error);
                    Ok(wire::undefined())
                }
            });
        }

        // 5. `shared-worker-progress`: forward `{id, type, progress?, checkpoint?}`
        {
            let router = Rc::clone(self);
            bridge.handle_channel("shared-worker-progress", move |args| {
                let router = Rc::clone(&router);
                async move {
                    let Some(msg) = args.first().and_then(Value::as_object) else {
                        return Ok(wire::undefined());
                    };
                    let Some(id_val) = msg.get("id") else {
                        return Ok(wire::undefined());
                    };
                    let key = value_id_str(id_val);
                    let maybe_fwd = {
                        let pending = router.pending.borrow();
                        pending.get(&key).and_then(|entry| {
                            let req = entry.requester.upgrade()?;
                            let mut fwd = Map::new();
                            fwd.insert("id".to_string(), entry.request_id.clone());
                            fwd.insert("type".to_string(), Value::String(entry.task_type.clone()));
                            if let Some(p) = msg.get("progress") {
                                fwd.insert("progress".to_string(), p.clone());
                            }
                            if let Some(c) = msg.get("checkpoint") {
                                fwd.insert("checkpoint".to_string(), c.clone());
                            }
                            Some((req, Value::Object(fwd)))
                        })
                    };
                    if let Some((req, payload)) = maybe_fwd {
                        req.send("shared-worker-progress", vec![payload]);
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // 6. `shared-worker-abort`
        {
            let router = Rc::clone(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("shared-worker-abort", move |args| {
                let router = Rc::clone(&router);
                let bridge_weak = bridge_weak.clone();
                async move {
                    let (Some(requester), Some(msg)) = (
                        bridge_weak.upgrade(),
                        args.first().and_then(Value::as_object),
                    ) else {
                        return Ok(wire::undefined());
                    };
                    let id_part = msg.get("id").map(value_id_str).unwrap_or_else(|| "null".into());
                    let key = format!("{}_{id_part}", requester.id());
                    if let Some(entry) = router.pending.borrow_mut().remove(&key) {
                        Self::respond(&entry, None, Some(Value::String("Aborted".to_string())));
                    }
                    if let Some(task_type) = msg.get("type").and_then(Value::as_str) {
                        let handler = router
                            .handlers
                            .borrow()
                            .get(task_type)
                            .and_then(Weak::upgrade);
                        if let Some(h) = handler {
                            h.send("shared-worker-abort", vec![json!({ "id": key })]);
                        }
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // 7. `shared-worker-update-params`
        {
            let router = Rc::clone(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("shared-worker-update-params", move |args| {
                let router = Rc::clone(&router);
                let bridge_weak = bridge_weak.clone();
                async move {
                    let (Some(requester), Some(msg)) = (
                        bridge_weak.upgrade(),
                        args.first().and_then(Value::as_object),
                    ) else {
                        return Ok(wire::undefined());
                    };
                    let Some(task_type) = msg.get("type").and_then(Value::as_str) else {
                        return Ok(wire::undefined());
                    };
                    let handler = router
                        .handlers
                        .borrow()
                        .get(task_type)
                        .and_then(Weak::upgrade);
                    if let Some(h) = handler {
                        let id_part = msg
                            .get("id")
                            .map(value_id_str)
                            .unwrap_or_else(|| "null".into());
                        let key = format!("{}_{id_part}", requester.id());
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        h.send(
                            "shared-worker-update-params",
                            vec![json!({ "id": key, "type": task_type, "params": params })],
                        );
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // 8. `shared-worker-remote-config-change`
        {
            let router = Rc::clone(self);
            bridge.handle_channel("shared-worker-remote-config-change", move |args| {
                let router = Rc::clone(&router);
                async move {
                    let cfg = args
                        .first()
                        .and_then(|v| v.as_object().and_then(|o| o.get("config")).or(Some(v)))
                        .cloned();
                    if let Some(cfg) = cfg {
                        *router.last_remote_config.borrow_mut() = Some(cfg.clone());
                        if let Some(wb) = router
                            .worker_bridge
                            .borrow()
                            .as_ref()
                            .and_then(Weak::upgrade)
                        {
                            wb.send("shared-worker-remote-config-change", vec![cfg]);
                        }
                    }
                    Ok(wire::undefined())
                }
            });
        }
    }

    fn respond(entry: &PendingWorkerTask, result: Option<Value>, error: Option<Value>) {
        let Some(requester) = entry.requester.upgrade() else {
            return;
        };
        let mut msg = Map::new();
        msg.insert("id".to_string(), entry.request_id.clone());
        if let Some(r) = result {
            msg.insert("result".to_string(), r);
        }
        if let Some(e) = error {
            msg.insert("error".to_string(), e);
        }
        requester.send("shared-worker-resp", vec![Value::Object(msg)]);
    }

    fn reply_error(requester: &ZaloBridge, id: Value, error: &str) {
        requester.send(
            "shared-worker-resp",
            vec![json!({ "id": id, "error": error })],
        );
    }
}

fn value_id_str(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

// ============================================================================
// ZaloMainProcess
// ============================================================================

/// Live UI / engine privacy state queried when delivering an OS notification.
#[derive(Debug, Clone)]
pub struct NotificationContext {
    pub window_focused: bool,
    pub window_visible: bool,
    pub open_conversation: Option<String>,
    pub idle_seconds: f64,
    pub notifications_enabled: bool,
    pub shows_preview: bool,
    pub sound_enabled: bool,
    pub hidden_known: bool,
    pub locked_ids: HashSet<String>,
}

impl Default for NotificationContext {
    fn default() -> Self {
        Self {
            window_focused: false,
            window_visible: true,
            open_conversation: None,
            idle_seconds: 0.0,
            notifications_enabled: true,
            shows_preview: true,
            sound_enabled: true,
            // Fail-closed until `convActions.hidden_known` arrives from the renderer!
            hidden_known: false,
            locked_ids: HashSet::new(),
        }
    }
}

/// High-level main-process events forwarded to [`crate::ui_window::OlazAppWindow`].
#[derive(Debug, Clone)]
pub enum MainProcessEvent {
    LoginSucceeded,
    LoggedOut { kickout_notice: Option<String> },
    QrScanned { name: String, avatar: Option<String> },
    QrFailed { message: String },
    Badge(i64),
    NotificationClicked { conv_id: String },
    #[allow(dead_code)]
    IncomingCall(Option<IncomingVoipCall>),
}

/// Window control commands emitted by `$zwindow.*` IPC channels and executed on the GTK main thread.
#[derive(Debug, Clone)]
pub enum WindowCommand {
    Minimize {
        window_id: Option<String>,
    },
    ToggleMaximize {
        window_id: Option<String>,
        target: Option<bool>,
    },
    ToggleFullscreen {
        window_id: Option<String>,
        target: Option<bool>,
    },
    CloseOrHide {
        window_id: Option<String>,
    },
    Hide {
        window_id: Option<String>,
    },
    Show {
        window_id: Option<String>,
        focus: bool,
    },
    Focus {
        window_id: Option<String>,
    },
    Quit,
    FlashFrame(bool),
    SetLoginSize,
    ExpandMainWindow,
    SetResizable(bool),
}

/// Thread-safe mirror of primary GTK window state + command queue for `$zwindow` sync/async IPCs.
#[derive(Default)]
pub struct MainWindowState {
    pub is_maximized: AtomicBool,
    pub is_fullscreen: AtomicBool,
    pub child_maximized: RwLock<HashMap<String, bool>>,
    cmd_tx: RwLock<Option<tokio::sync::mpsc::UnboundedSender<WindowCommand>>>,
}

impl MainWindowState {
    pub fn set_command_sender(&self, tx: tokio::sync::mpsc::UnboundedSender<WindowCommand>) {
        *self.cmd_tx.write() = Some(tx);
    }

    pub fn send(&self, cmd: WindowCommand) {
        if let Some(ref tx) = *self.cmd_tx.read() {
            let _ = tx.send(cmd);
        }
    }
}

#[derive(Clone)]
struct LiveNotificationBridge {
    bridge: Weak<ZaloBridge>,
    tag: String,
}

/// Stand-in for Zalo PC's Electron main process (`main-dist/main.js`).
pub struct ZaloMainProcess {
    platform: Arc<ZaloPlatform>,
    runtime: ZaloWebRuntime,
    cookie_jar: ZaloCookieJar,
    network: NetworkProxy,
    media_loader: MediaLoader,
    downloads: DownloadManager,
    crypto: Arc<CryptoBridge>,
    fs_bridge: FsBridge,
    safe_storage: SafeStorage,
    keychain: Arc<KeychainCipher<LinuxSecretBackend>>,
    notifier: LinuxNotifier,
    call_coordinator: Rc<RefCell<LinuxCallCoordinator>>,
    shared_worker: Rc<SharedWorkerRouter>,
    window_state: Arc<MainWindowState>,
    tokio_handle: tokio::runtime::Handle,

    host: RefCell<Option<Weak<OfficialClientHost>>>,
    worker_host: RefCell<Option<Rc<OfficialClientHost>>>,
    db_clients: RefCell<Vec<Weak<ZaloBridge>>>,
    scripts: RefCell<Vec<Weak<ZaloBridge>>>,
    announced_session: RefCell<Option<Value>>,
    session: RefCell<Option<Value>>,
    viewer_key: RefCell<Option<Value>>,
    pending_close_dbs: RefCell<Vec<oneshot::Sender<()>>>,
    push_login_success_after_load: RefCell<bool>,
    last_kickout_notice: Arc<Mutex<Option<String>>>,
    live_notif_bridges: Rc<RefCell<HashMap<String, LiveNotificationBridge>>>,
    notif_context_fn: RefCell<Option<Rc<dyn Fn() -> NotificationContext>>>,
    on_event: RefCell<Option<Rc<dyn Fn(MainProcessEvent)>>>,
}

impl ZaloMainProcess {
    pub fn new(
        platform: Arc<ZaloPlatform>,
        runtime: ZaloWebRuntime,
        cookie_jar: ZaloCookieJar,
        network: NetworkProxy,
        media_loader: MediaLoader,
        tokio_handle: tokio::runtime::Handle,
    ) -> Rc<Self> {
        let allowed_dl_roots = vec![
            platform.user_data().to_path_buf(),
            platform.temp_dir().to_path_buf(),
            platform.electron_path("downloads"),
            platform.electron_path("documents").join("Zalo Received Files"),
            platform.cache_dir().to_path_buf(),
        ];
        let downloads = DownloadManager::new(
            media_loader.clone(),
            platform.temp_dir().join("downloads"),
            Some(allowed_dl_roots),
        );
        let crypto = Arc::new(CryptoBridge::new());
        let fs_bridge = FsBridge::new(Arc::clone(&platform));
        let safe_storage = SafeStorage::new();
        let keychain = Arc::new(KeychainCipher::for_platform(&platform));
        let notifier = LinuxNotifier::new();
        let call_coordinator = Rc::new(RefCell::new(LinuxCallCoordinator::new()));
        let shared_worker = SharedWorkerRouter::new();
        let last_kickout_notice = Arc::new(Mutex::new(None));

        // Observe `/api/session/kickout-info` and QR polling responses on `NetworkProxy`.
        let (qr_event_tx, mut qr_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<MainProcessEvent>();
        {
            let kickout_slot = Arc::clone(&last_kickout_notice);
            let qr_tx = qr_event_tx.clone();
            network.set_response_observer(Some(Arc::new(move |url, status, body| {
                if !is_observed_zalo_endpoint(url) {
                    return;
                }
                tracing::debug!(status, %url, bytes = body.len(), "observed Zalo session/QR endpoint");
                if url.contains("/api/session/kickout-info") && status == 200 {
                    if let Ok(parsed) = serde_json::from_slice::<Value>(body) {
                        let msg = parsed
                            .get("data")
                            .and_then(|d| d.get("message").or_else(|| d.get("reason")))
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| DEFAULT_KICKOUT_NOTICE.to_string());
                        *kickout_slot.lock() = Some(msg);
                    }
                } else if status == 200
                    && url.contains("qr-id.")
                    && url.contains("/polling/qr/")
                {
                    if let Ok(parsed) = serde_json::from_slice::<Value>(body) {
                        let data = parsed.get("data").unwrap_or(&parsed);
                        let scanned_name = data
                            .get("display_name")
                            .or_else(|| data.get("displayName"))
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        if let Some(name) = scanned_name {
                            let avatar = data
                                .get("avatar")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned);
                            let _ = qr_tx.send(MainProcessEvent::QrScanned {
                                name: name.to_owned(),
                                avatar,
                            });
                        } else {
                            let err_code = parsed
                                .get("error_code")
                                .and_then(Value::as_i64)
                                .or_else(|| data.get("error_code").and_then(Value::as_i64))
                                .unwrap_or(0);
                            if err_code != 0 {
                                let message = parsed
                                    .get("error_message")
                                    .or_else(|| data.get("error_message"))
                                    .or_else(|| data.get("message"))
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| {
                                        format!(
                                            "Không thể xác thực mã QR (mã lỗi {err_code}). Vui lòng tạo lại mã QR."
                                        )
                                    });
                                let _ = qr_tx.send(MainProcessEvent::QrFailed { message });
                            }
                        }
                    }
                }
            })));
        }

        let live_notif_bridges: Rc<RefCell<HashMap<String, LiveNotificationBridge>>> =
            Rc::new(RefCell::new(HashMap::new()));

        let mp = Rc::new(Self {
            platform,
            runtime,
            cookie_jar,
            network,
            media_loader,
            downloads,
            crypto,
            fs_bridge,
            safe_storage,
            keychain,
            notifier: notifier.clone(),
            call_coordinator,
            shared_worker,
            window_state: Arc::new(MainWindowState::default()),
            tokio_handle,
            host: RefCell::new(None),
            worker_host: RefCell::new(None),
            db_clients: RefCell::new(Vec::new()),
            scripts: RefCell::new(Vec::new()),
            announced_session: RefCell::new(None),
            session: RefCell::new(None),
            viewer_key: RefCell::new(None),
            pending_close_dbs: RefCell::new(Vec::new()),
            push_login_success_after_load: RefCell::new(false),
            last_kickout_notice,
            live_notif_bridges: Rc::clone(&live_notif_bridges),
            notif_context_fn: RefCell::new(None),
            on_event: RefCell::new(None),
        });

        // Forward background `notify-rust` click/close threads back onto the GTK main thread.
        let (notif_tx, mut notif_rx) = tokio::sync::mpsc::unbounded_channel::<NotificationEvent>();
        notifier.set_on_event(move |ev| {
            let _ = notif_tx.send(ev);
        });

        let mp_weak = Rc::downgrade(&mp);
        glib::MainContext::default().spawn_local(async move {
            while let Some(ev) = notif_rx.recv().await {
                let Some(this) = mp_weak.upgrade() else {
                    break;
                };
                match ev {
                    NotificationEvent::Clicked { id, conv_id } => {
                        this.push_notif_ipc("_noti_on-click", &id, false);
                        this.emit_event(MainProcessEvent::NotificationClicked { conv_id });
                    }
                    NotificationEvent::Closed { id, .. } => {
                        this.push_notif_ipc("_noti_on-close", &id, true);
                    }
                }
            }
        });

        // Forward QR polling network observer events onto the GTK main thread.
        let mp_weak_qr = Rc::downgrade(&mp);
        glib::MainContext::default().spawn_local(async move {
            while let Some(ev) = qr_event_rx.recv().await {
                let Some(this) = mp_weak_qr.upgrade() else {
                    break;
                };
                this.emit_event(ev);
            }
        });

        mp
    }

    pub fn platform(&self) -> &Arc<ZaloPlatform> {
        &self.platform
    }

    pub fn runtime(&self) -> &ZaloWebRuntime {
        &self.runtime
    }

    pub fn media_loader(&self) -> &MediaLoader {
        &self.media_loader
    }

    pub fn notifier(&self) -> &LinuxNotifier {
        &self.notifier
    }

    pub fn call_coordinator(&self) -> &Rc<RefCell<LinuxCallCoordinator>> {
        &self.call_coordinator
    }

    pub fn shared_worker(&self) -> &Rc<SharedWorkerRouter> {
        &self.shared_worker
    }

    pub fn window_state(&self) -> &Arc<MainWindowState> {
        &self.window_state
    }

    pub fn attach_host(self: &Rc<Self>, host: &Rc<OfficialClientHost>) {
        *self.host.borrow_mut() = Some(Rc::downgrade(host));
        if host.is_direct_ui() {
            let mp_weak = Rc::downgrade(self);
            host.set_popup_bridge_factory(move |child_bridge, popup_win, parent_bridge| {
                if let Some(mp) = mp_weak.upgrade() {
                    mp.install_popup_bridge_on(child_bridge, popup_win, parent_bridge);
                }
            });
        }
    }

    pub fn host(&self) -> Option<Rc<OfficialClientHost>> {
        self.host.borrow().as_ref().and_then(Weak::upgrade)
    }

    pub fn set_on_event(&self, cb: impl Fn(MainProcessEvent) + 'static) {
        *self.on_event.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn set_notification_context_provider(
        &self,
        cb: impl Fn() -> NotificationContext + 'static,
    ) {
        *self.notif_context_fn.borrow_mut() = Some(Rc::new(cb));
    }

    fn notification_context(&self) -> NotificationContext {
        self.notif_context_fn
            .borrow()
            .as_ref()
            .map(|f| f())
            .unwrap_or_default()
    }

    fn emit_event(&self, ev: MainProcessEvent) {
        let cb = self.on_event.borrow().clone();
        if let Some(cb) = cb {
            cb(ev);
        }
    }

    pub fn has_session(&self) -> bool {
        self.cookie_jar.has_session()
    }

    pub fn page_query(&self, already_login: bool) -> BTreeMap<String, String> {
        self.platform.page_query(already_login)
    }

    /// Normalizes `database-config.json` to IndexedDB and navigates to `index.html` (if `zpw_sek` exists)
    /// or `login.html`.
    pub fn start(self: &Rc<Self>) {
        let Some(host) = self.host() else {
            return;
        };
        self.platform.normalize_database_adapter_state();
        let logged_in = self.has_session();
        if logged_in {
            self.window_state.send(WindowCommand::ExpandMainWindow);
            self.start_shared_worker();
            host.load(HostPage::Main, &self.page_query(false));
        } else {
            self.window_state.send(WindowCommand::SetLoginSize);
            host.load(HostPage::Login, &self.page_query(false));
        }
    }

    /// Spawns the hidden `shared-worker.html` `OfficialClientHost` sharing `ZaloWebRuntime`'s `WebContext`.
    pub fn start_shared_worker(self: &Rc<Self>) {
        self.stop_shared_worker();
        if std::env::var_os("OLAZ_NO_SHARED_WORKER").is_some() {
            tracing::info!("OLAZ_NO_SHARED_WORKER is set; skipping shared-worker.html host");
            return;
        }

        let mp_for_bridge = Rc::clone(self);
        let worker = OfficialClientHost::new(
            &self.platform,
            self.runtime.clone(),
            self.network.clone(),
            self.tokio_handle.clone(),
            false,
            move |bridge| {
                mp_for_bridge.install_bridge_on(bridge);
            },
        );

        worker.set_on_console(|line| {
            let official = line.starts_with("[log]") || line.starts_with("[info]");
            if line.contains("do open indexeddb") {
                return;
            }
            if !official
                || line.contains("[adapter]")
                || line.contains("[chat]")
                || line.contains("[transfer]")
            {
                let truncated: String = line.chars().take(300).collect();
                tracing::debug!(target: "olaz::sw_console", "{truncated}");
            }
        });

        let mp_weak = Rc::downgrade(self);
        let worker_weak = Rc::downgrade(&worker);
        worker.set_on_page_event(move |ev| {
            tracing::debug!(?ev, "shared-worker page event");
            match ev {
                PageEvent::FinishedLoading(_) => {
                    if let (Some(mp), Some(w)) = (mp_weak.upgrade(), worker_weak.upgrade()) {
                        mp.shared_worker.worker_did_finish_load();
                        let vk = mp.viewer_key.borrow().clone();
                        if let Some(key) = vk {
                            w.bridge().send("receive-viewerkey", vec![key]);
                        }
                    }
                }
                PageEvent::Terminated => {
                    if let (Some(mp), Some(w)) = (mp_weak.upgrade(), worker_weak.upgrade()) {
                        if w.allow_crash_recovery() {
                            tracing::warn!(
                                "shared-worker WebProcess terminated; reloading shared-worker.html"
                            );
                            w.load(HostPage::SharedWorker, &mp.page_query(true));
                        } else {
                            tracing::error!(
                                "shared-worker WebProcess crashed repeatedly; suppressing reload"
                            );
                        }
                    }
                }
                PageEvent::StartedLoading | PageEvent::Failed(_) => {}
            }
        });

        self.shared_worker.register_worker(worker.bridge());
        *self.worker_host.borrow_mut() = Some(Rc::clone(&worker));
        worker.load(HostPage::SharedWorker, &self.page_query(true));
    }

    /// Stops and disposes the hidden `shared-worker.html` host.
    pub fn stop_shared_worker(&self) {
        let Some(worker) = self.worker_host.borrow_mut().take() else {
            return;
        };
        let wid = worker.bridge().id();
        self.db_clients
            .borrow_mut()
            .retain(|w| w.upgrade().is_some_and(|b| b.id() != wid));
        self.scripts
            .borrow_mut()
            .retain(|w| w.upgrade().is_some_and(|b| b.id() != wid));
        self.shared_worker.reset_worker(worker.bridge());
        worker.shutdown();
    }

    pub fn page_did_start_loading(&self) {
        if let Some(host) = self.host() {
            host.bridge().send("unload-zlog", vec![]);
        }
    }

    pub fn page_did_finish_loading(&self, page: HostPage) {
        let Some(host) = self.host() else {
            return;
        };
        let bridge = host.bridge();
        bridge.send("visibility-change", vec![json!("show")]);
        bridge.send("visibility-change", vec![json!("focus"), json!("1")]);
        if page == HostPage::Main && *self.push_login_success_after_load.borrow() {
            *self.push_login_success_after_load.borrow_mut() = false;
            bridge.send("login-success", vec![]);
        }
    }

    /// Clears all account-tied main-process state (`session`, `viewer_key`, `downloads`, `shared_worker`).
    pub fn reset_account_state(&self) {
        *self.session.borrow_mut() = None;
        *self.announced_session.borrow_mut() = None;
        *self.viewer_key.borrow_mut() = None;
        self.downloads.reset();
        self.call_coordinator.borrow_mut().clear_incoming();
        self.stop_shared_worker();
        self.shared_worker.reset_session();
    }

    /// Initiates a clean user logout: clears Zalo cookies, resets account state, and loads `login.html`.
    pub fn logout(self: &Rc<Self>) {
        let _ = self.cookie_jar.clear_cookies();
        *self.last_kickout_notice.lock() = None;
        self.reset_account_state();
        self.window_state.send(WindowCommand::SetLoginSize);
        if let Some(host) = self.host() {
            host.load(HostPage::Login, &self.page_query(false));
        }
        self.emit_event(MainProcessEvent::LoggedOut {
            kickout_notice: None,
        });
    }

    fn on_login_succeeded(self: &Rc<Self>) {
        tracing::info!("login-success -> starting shared-worker and loading main index.html");
        *self.last_kickout_notice.lock() = None;
        *self.push_login_success_after_load.borrow_mut() = true;
        self.window_state.send(WindowCommand::ExpandMainWindow);
        self.start_shared_worker();
        if let Some(host) = self.host() {
            host.load(HostPage::Main, &self.page_query(true));
        }
        self.emit_event(MainProcessEvent::LoginSucceeded);
    }

    fn on_back_to_login(&self) {
        let notice = self.last_kickout_notice.lock().take();
        self.reset_account_state();
        self.window_state.send(WindowCommand::SetLoginSize);
        if let Some(host) = self.host() {
            host.load(HostPage::Login, &self.page_query(false));
        }
        self.emit_event(MainProcessEvent::LoggedOut {
            kickout_notice: notice,
        });
    }

    fn finish_close_dbs(&self) {
        let waiters: Vec<oneshot::Sender<()>> =
            self.pending_close_dbs.borrow_mut().drain(..).collect();
        for tx in waiters {
            let _ = tx.send(());
        }
    }

    fn push_notif_ipc(&self, channel: &str, id: &str, remove: bool) {
        let entry = if remove {
            self.live_notif_bridges.borrow_mut().remove(id)
        } else {
            self.live_notif_bridges.borrow().get(id).cloned()
        };
        let tag = entry.as_ref().map(|e| e.tag.as_str()).unwrap_or("");
        let bridge = entry
            .as_ref()
            .and_then(|e| e.bridge.upgrade())
            .or_else(|| self.host().map(|h| Rc::clone(h.bridge())));
        if let Some(b) = bridge {
            b.send(channel, vec![json!({ "id": id, "tag": tag })]);
        }
    }

    /// Installs both main-process Electron IPC channels and preload-local `$z*` handlers onto `bridge`.
    pub fn install_bridge_on(self: &Rc<Self>, bridge: &Rc<ZaloBridge>) {
        self.install_ipc_channels(bridge);
        self.install_preload_local_handlers(bridge);
    }

    fn install_ipc_channels(self: &Rc<Self>, bridge: &Rc<ZaloBridge>) {
        let platform = Arc::clone(&self.platform);

        // ---- 1. App / paths (spec §2.3) ----
        {
            let p = Arc::clone(&platform);
            bridge.handle_channel_sync("_electron_get-path-sync", move |args| {
                let name = args.first().and_then(Value::as_str).unwrap_or("userData");
                Ok(json!(p.electron_path(name).to_string_lossy()))
            });
        }
        {
            let p = Arc::clone(&platform);
            bridge.handle_channel("_electron_get-path", move |args| {
                let p = Arc::clone(&p);
                async move {
                    let name = args.first().and_then(Value::as_str).unwrap_or("userData");
                    Ok(json!(p.electron_path(name).to_string_lossy()))
                }
            });
        }
        {
            let p = Arc::clone(&platform);
            bridge.handle_channel_sync("_electron_get-app-version", move |_| {
                Ok(json!(p.app_version()))
            });
        }
        {
            let user_data_str = platform.user_data().to_string_lossy().into_owned();
            let ud2 = user_data_str.clone();
            bridge.handle_channel_sync("_electron_get-app-path-sync", move |_| {
                Ok(json!(user_data_str))
            });
            bridge.handle_channel_sync("_electron_get-app-path", move |_| Ok(json!(ud2)));
        }
        {
            let cwd = platform.user_data().to_string_lossy().into_owned();
            let p = Arc::clone(&platform);
            bridge.handle_channel_sync("electron-store-get-data", move |_| {
                Ok(json!({ "defaultCwd": cwd, "appVersion": p.app_version() }))
            });
        }
        bridge.handle_channel_sync("get-child-url", |_| {
            Ok(json!(format!("{ZALO_APP_SCHEME}://local/child.html")))
        });
        bridge.handle_channel_sync("get-photo-viewer-url", |_| {
            Ok(json!(format!("{ZALO_APP_SCHEME}://local/popup-viewer.html")))
        });
        bridge.handle_channel_sync("get-resource-dir", |_| {
            Ok(json!(format!("{ZALO_APP_SCHEME}://local/")))
        });

        // ---- 2. Storage selection: force IndexedDB (spec §2.2, §4) ----
        bridge.handle_channel_sync("sqlite-available", |_| Ok(json!(false)));
        bridge.handle_channel_sync("fail-load-sqlite3", |_| Ok(json!(true)));
        bridge.handle_channel_sync("is-migrate-full-done", |_| Ok(json!(false)));
        bridge.handle_channel_sync("get-migrate-local-info", |_| {
            Ok(json!({ "disable_migrate": true }))
        });
        bridge.handle_channel_sync("migratedb:check_need_wait_bu", |_| Ok(json!(false)));
        bridge.handle_channel_sync("z-db-corruption-detector", |_| Ok(json!(0)));
        bridge.handle_channel_sync(
            "get-name-of-target-script-for-dal-channel-from-host",
            |_| Ok(json!("preload-sqlite")),
        );

        {
            let mp_weak = Rc::downgrade(self);
            bridge.handle_channel("migratedb:signal_restart", move |_| {
                let mp_weak = mp_weak.clone();
                async move {
                    if let Some(mp) = mp_weak.upgrade() {
                        tracing::info!(
                            "renderer requested DB migration restart -> reloading on IndexedDB"
                        );
                        mp.platform.normalize_database_adapter_state();
                        if mp.has_session() {
                            mp.start_shared_worker();
                            if let Some(h) = mp.host() {
                                h.load(HostPage::Main, &mp.page_query(false));
                            }
                        } else {
                            mp.stop_shared_worker();
                            if let Some(h) = mp.host() {
                                h.load(HostPage::Login, &mp.page_query(false));
                            }
                        }
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // ---- 3. Cookies (spec §2.1) ----
        for ch in [
            "_electron_set-app-cookie",
            "_electron_set-cookie",
            "_electron_clear-cookie",
            "_electron_get-cookies",
            "_electron_delete-cookie",
        ] {
            let jar = self.cookie_jar.clone();
            let ch_owned = ch.to_string();
            bridge.handle_channel_sync(ch, move |args| {
                jar.handle_ipc(&ch_owned, args).map_err(|e| e.to_string())
            });
        }

        // ---- 4. Login & multi-window session coordination ----
        {
            let mp_weak = Rc::downgrade(self);
            bridge.handle_channel("login-success", move |_| {
                let mp_weak = mp_weak.clone();
                async move {
                    if let Some(mp) = mp_weak.upgrade() {
                        mp.on_login_succeeded();
                    }
                    Ok(wire::undefined())
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            bridge.handle_channel("login", move |_| {
                let mp_weak = mp_weak.clone();
                async move {
                    if let Some(mp) = mp_weak.upgrade() {
                        mp.on_back_to_login();
                    }
                    Ok(wire::undefined())
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("script-connected", move |_| {
                let mp_weak = mp_weak.clone();
                let bridge_weak = bridge_weak.clone();
                async move {
                    if let (Some(mp), Some(b)) = (mp_weak.upgrade(), bridge_weak.upgrade()) {
                        let bid = b.id();
                        let mut scripts = mp.scripts.borrow_mut();
                        scripts.retain(|w| w.upgrade().is_some());
                        if !scripts
                            .iter()
                            .any(|w| w.upgrade().is_some_and(|x| x.id() == bid))
                        {
                            scripts.push(Rc::downgrade(&b));
                        }
                        drop(scripts);
                        if let Some(announced) = mp.announced_session.borrow().clone() {
                            b.send("new-session", vec![announced]);
                        }
                    }
                    Ok(wire::undefined())
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("new-session", move |args| {
                let mp_weak = mp_weak.clone();
                let bridge_weak = bridge_weak.clone();
                async move {
                    let Some(mp) = mp_weak.upgrade() else {
                        return Ok(wire::undefined());
                    };
                    let payload = args.first().cloned();
                    *mp.session.borrow_mut() = payload.clone();

                    let is_empty = match &payload {
                        None => true,
                        Some(v) if wire::is_null_or_undefined(v) => true,
                        Some(Value::Object(m)) => m.is_empty(),
                        _ => false,
                    };

                    if is_empty {
                        *mp.announced_session.borrow_mut() = None;
                    } else if let Some(sess) = payload {
                        *mp.announced_session.borrow_mut() = Some(sess.clone());
                        let sender_id = bridge_weak.upgrade().map(|b| b.id());
                        let targets: Vec<Rc<ZaloBridge>> = mp
                            .scripts
                            .borrow()
                            .iter()
                            .filter_map(Weak::upgrade)
                            .filter(|b| Some(b.id()) != sender_id)
                            .collect();
                        for t in targets {
                            t.send("new-session", vec![sess.clone()]);
                        }
                    }
                    Ok(wire::undefined())
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("db-authenticated", move |args| {
                let mp_weak = mp_weak.clone();
                let bridge_weak = bridge_weak.clone();
                async move {
                    let Some(mp) = mp_weak.upgrade() else {
                        return Ok(wire::undefined());
                    };
                    let payload = args.first().cloned();
                    *mp.session.borrow_mut() = payload.clone();
                    let Some(sess) = payload.filter(|v| !wire::is_null_or_undefined(v)) else {
                        return Ok(wire::undefined());
                    };
                    let mut targets: Vec<Rc<ZaloBridge>> = mp
                        .db_clients
                        .borrow()
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect();
                    if let Some(b) = bridge_weak.upgrade() {
                        if !targets.iter().any(|x| x.id() == b.id()) {
                            targets.push(b);
                        }
                    }
                    for t in targets {
                        t.send("db-authenticated", vec![sess.clone()]);
                    }
                    Ok(wire::undefined())
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("db-client-connected", move |_| {
                let mp_weak = mp_weak.clone();
                let bridge_weak = bridge_weak.clone();
                async move {
                    if let (Some(mp), Some(b)) = (mp_weak.upgrade(), bridge_weak.upgrade()) {
                        let bid = b.id();
                        let mut clients = mp.db_clients.borrow_mut();
                        clients.retain(|w| w.upgrade().is_some());
                        if !clients
                            .iter()
                            .any(|w| w.upgrade().is_some_and(|x| x.id() == bid))
                        {
                            clients.push(Rc::downgrade(&b));
                        }
                        drop(clients);
                        if let Some(sess) = mp
                            .session
                            .borrow()
                            .clone()
                            .filter(|v| !wire::is_null_or_undefined(v))
                        {
                            b.send("db-authenticated", vec![sess]);
                        }
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // ---- 5. Shared worker & DownloadManager ----
        self.shared_worker.install(bridge);

        {
            let dl = self.downloads.clone();
            let rt_handle = self.tokio_handle.clone();
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel(DOWNLOAD_IPC_CHANNEL, move |args| {
                let dl = dl.clone();
                let rt_handle = rt_handle.clone();
                let bridge_weak = bridge_weak.clone();
                async move {
                    if let Some(req) = args.first().cloned() {
                        let (tx, rx) = oneshot::channel();
                        rt_handle.spawn(async move {
                            let resp = dl.handle_message(req).await;
                            let _ = tx.send(resp);
                        });
                        glib::MainContext::default().spawn_local(async move {
                            if let Ok(Some(resp)) = rx.await {
                                if let Some(b) = bridge_weak.upgrade() {
                                    b.send(DOWNLOAD_IPC_CHANNEL, vec![resp]);
                                }
                            }
                        });
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // ---- 6. Viewer key & DB close barrier ----
        {
            let mp_weak = Rc::downgrade(self);
            bridge.handle_channel("receive-viewerkey", move |args| {
                let mp_weak = mp_weak.clone();
                async move {
                    if let (Some(mp), Some(key)) = (mp_weak.upgrade(), args.first().cloned()) {
                        *mp.viewer_key.borrow_mut() = Some(key.clone());
                        if let Some(ref w) = *mp.worker_host.borrow() {
                            w.bridge().send("receive-viewerkey", vec![key]);
                        }
                    }
                    Ok(wire::undefined())
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("db-start-close-dbs", move |args| {
                let mp_weak = mp_weak.clone();
                let bridge_weak = bridge_weak.clone();
                async move {
                    let Some(b) = bridge_weak.upgrade() else {
                        return Ok(wire::undefined());
                    };
                    let subscribed = b.send("db-start-close-dbs", args);
                    let Some(mp) = mp_weak.upgrade() else {
                        return Ok(wire::undefined());
                    };
                    if !subscribed {
                        return Ok(wire::undefined());
                    }
                    let (tx, rx) = oneshot::channel();
                    mp.pending_close_dbs.borrow_mut().push(tx);
                    let mp_timeout = Rc::downgrade(&mp);
                    glib::MainContext::default().spawn_local(async move {
                        glib::timeout_future(Duration::from_secs(5)).await;
                        if let Some(m) = mp_timeout.upgrade() {
                            m.finish_close_dbs();
                        }
                    });
                    let _ = rx.await;
                    Ok(wire::undefined())
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            bridge.handle_channel("db-finish-close-dbs", move |_| {
                let mp_weak = mp_weak.clone();
                async move {
                    if let Some(mp) = mp_weak.upgrade() {
                        mp.finish_close_dbs();
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // ---- 7. Credential vaults (`zsafestorage-execute` + `zproto-*-with-keychain`) ----
        {
            let safe = self.safe_storage;
            let p = Arc::clone(&platform);
            bridge.handle_channel_sync("zsafestorage-execute", move |args| {
                let method = args.first().and_then(Value::as_str).unwrap_or("");
                let rest = if args.len() > 1 { &args[1..] } else { &[] };
                safe.execute(&p, method, rest)
            });
        }
        {
            let kc = Arc::clone(&self.keychain);
            let rt_handle = self.tokio_handle.clone();
            bridge.handle_channel("zproto-encrypt-with-keychain", move |args| {
                let kc = Arc::clone(&kc);
                let rt_handle = rt_handle.clone();
                async move {
                    let (tx, rx) = oneshot::channel();
                    rt_handle.spawn(async move {
                        let _ = tx.send(kc.encrypt_wire(&args).await);
                    });
                    rx.await.map_err(|e| e.to_string())?
                }
            });
        }
        {
            let kc = Arc::clone(&self.keychain);
            let rt_handle = self.tokio_handle.clone();
            bridge.handle_channel("zproto-decrypt-with-keychain", move |args| {
                let kc = Arc::clone(&kc);
                let rt_handle = rt_handle.clone();
                async move {
                    let (tx, rx) = oneshot::channel();
                    rt_handle.spawn(async move {
                        let _ = tx.send(kc.decrypt_wire(&args).await);
                    });
                    rx.await.map_err(|e| e.to_string())?
                }
            });
        }

        // ---- 8. Mainless worker fallbacks, CPU metrics, badge, notifications, shell, calls ----
        for ch in ["exec", "exec_raw"] {
            bridge.handle_channel_sync(ch, |_| {
                Ok(json!({
                    "error": {
                        "name": "NotSupported",
                        "message": "Not available in Olaz",
                        "code": "ENOTSUP",
                    }
                }))
            });
        }

        bridge.handle_channel_sync("get-cpu-infos", |_| {
            let pid = std::process::id();
            Ok(json!([{
                "pid": pid,
                "type": "Browser",
                "cpu": { "percentCPUUsage": 0, "idleWakeupsPerSecond": 0 },
                "memory": { "workingSetSize": 0, "peakWorkingSetSize": 0 },
            }]))
        });

        {
            let mp_weak = Rc::downgrade(self);
            let notifier = self.notifier.clone();
            let rt_handle = self.tokio_handle.clone();
            bridge.handle_channel("badge-count", move |args| {
                let mp_weak = mp_weak.clone();
                let notifier = notifier.clone();
                let rt_handle = rt_handle.clone();
                async move {
                    let count = args.first().and_then(Value::as_i64).unwrap_or(0).max(0);
                    let ucount = count as u64;
                    rt_handle.spawn(async move {
                        let _ = notifier.emit_unity_badge(ucount).await;
                    });
                    if let Some(mp) = mp_weak.upgrade() {
                        mp.emit_event(MainProcessEvent::Badge(count));
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // Desktop notifications (`_noti_*`)
        bridge.handle_channel_sync("_noti_is-supported", |_| Ok(json!(true)));
        {
            let mp_weak = Rc::downgrade(self);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("_noti_create", move |args| {
                let mp_weak = mp_weak.clone();
                let bridge_weak = bridge_weak.clone();
                async move {
                    let Some(mp) = mp_weak.upgrade() else {
                        return Ok(Value::Null);
                    };
                    let ctx = mp.notification_context();
                    if !ctx.notifications_enabled {
                        return Ok(Value::Null);
                    }
                    let Some(mut req) = NotificationRequest::parse_ipc_args(&Value::Array(args))
                    else {
                        return Ok(Value::Null);
                    };
                    if !ctx.sound_enabled {
                        req.silent = true;
                    }

                    let avatar_path = mp.resolve_notification_avatar(&req.icon).await;
                    let res = mp.notifier.show_notification(
                        &req,
                        ctx.window_focused,
                        ctx.window_visible,
                        ctx.open_conversation.as_deref(),
                        ctx.idle_seconds,
                        ctx.shows_preview,
                        ctx.hidden_known,
                        &ctx.locked_ids,
                        avatar_path.as_deref(),
                    );

                    match res {
                        Ok(Some(id)) => {
                            if let Some(b) = bridge_weak.upgrade() {
                                mp.live_notif_bridges.borrow_mut().insert(
                                    id.clone(),
                                    LiveNotificationBridge {
                                        bridge: Rc::downgrade(&b),
                                        tag: req.tag.clone(),
                                    },
                                );
                                let mp_show = Rc::downgrade(&mp);
                                let id_show = id.clone();
                                glib::MainContext::default().spawn_local(async move {
                                    glib::timeout_future(Duration::from_millis(60)).await;
                                    if let Some(m) = mp_show.upgrade() {
                                        m.push_notif_ipc("_noti_on-show", &id_show, false);
                                    }
                                });
                            }
                            Ok(Value::String(id))
                        }
                        _ => Ok(Value::Null),
                    }
                }
            });
        }
        {
            let mp_weak = Rc::downgrade(self);
            bridge.handle_channel("_noti_close", move |args| {
                let mp_weak = mp_weak.clone();
                async move {
                    if let (Some(mp), Some(id)) =
                        (mp_weak.upgrade(), args.first().and_then(Value::as_str))
                    {
                        mp.push_notif_ipc("_noti_on-close", id, true);
                    }
                    Ok(wire::undefined())
                }
            });
        }

        // `__shell__` (`openPath` / `openExternal` / `showItemInFolder`) with executable guard!
        bridge.handle_channel("__shell__", |args| async move {
            let op = args.first().and_then(Value::as_str).unwrap_or("");
            let target = args.get(1).and_then(Value::as_str).unwrap_or("").trim();
            Ok(json!(exec_shell_operation(op, target)))
        });

        bridge.handle_channel_sync("__child_process__", |_| {
            Err("EPERM: child_process is not available".to_string())
        });

        // Voice / video calls (`$zcall`) Phase 1 graceful Linux stub
        bridge.acknowledge_channels(&["call-send-to-native", "call-init"]);
        bridge.handle_channel_sync("request-compressed-log-file", |_| {
            Ok(json!({
                "status": 2,
                "data": { "message": "Not available in Olaz", "errorData": null },
            }))
        });

        // ---- 9. `$zwindow.*` frameless window controls & screen focus key ----
        bridge.handle_channel_sync("_electron_get-info-screen-of-mouse-sync", |_| {
            Ok(json!("1"))
        });

        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("minimize", move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ws.send(WindowCommand::Minimize { window_id });
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("window-minimize", move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ws.send(WindowCommand::Minimize { window_id });
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("toggle-maximize", move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let target = args.get(1).and_then(Value::as_bool);
                ws.send(WindowCommand::ToggleMaximize { window_id, target });
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("window-maximize", move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ws.send(WindowCommand::ToggleMaximize {
                    window_id,
                    target: None,
                });
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("check-maximize", move |args| {
                if let Some(wid) = args.first().and_then(Value::as_str) {
                    if let Some(&m) = ws.child_maximized.read().get(wid) {
                        return Ok(json!(m));
                    }
                    if matches!(wid, "photo-viewer" | "file-viewer") {
                        return Ok(json!(false));
                    }
                }
                Ok(json!(ws.is_maximized.load(Ordering::Relaxed)))
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("toggle-fullscreen", move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let target = args.get(1).and_then(Value::as_bool);
                ws.send(WindowCommand::ToggleFullscreen { window_id, target });
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("check-fullscreen", move |_| {
                Ok(json!(ws.is_fullscreen.load(Ordering::Relaxed)))
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("close-window", move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ws.send(WindowCommand::CloseOrHide { window_id });
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("window-close", move |_| {
                ws.send(WindowCommand::CloseOrHide { window_id: None });
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            let bridge_weak = Rc::downgrade(bridge);
            bridge.handle_channel("hide-window", move |args| {
                let ws = Arc::clone(&ws);
                let bridge_weak = bridge_weak.clone();
                async move {
                    let window_id = args
                        .first()
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if let Some(ref wid) = window_id {
                        if matches!(wid.as_str(), "photo-viewer" | "file-viewer") {
                            if let Some(b) = bridge_weak.upgrade() {
                                b.send("hide-window-complete", vec![json!({ "windowId": wid })]);
                            }
                        }
                    }
                    ws.send(WindowCommand::Hide { window_id });
                    Ok(wire::undefined())
                }
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("window-hide", move |_| {
                ws.send(WindowCommand::Hide { window_id: None });
                Ok(wire::undefined())
            });
        }
        for (ch, focus) in [
            ("show", true),
            ("window-show", true),
            ("window-restore", true),
            ("show-inactive", false),
        ] {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync(ch, move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ws.send(WindowCommand::Show { window_id, focus });
                Ok(wire::undefined())
            });
        }
        for ch in ["focus-window", "window-focus"] {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync(ch, move |args| {
                let window_id = args
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ws.send(WindowCommand::Focus { window_id });
                Ok(wire::undefined())
            });
        }
        for ch in ["quit", "do-quit"] {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync(ch, move |_| {
                ws.send(WindowCommand::Quit);
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("flash-frame", move |args| {
                let flag = args.first().and_then(Value::as_bool).unwrap_or(true);
                ws.send(WindowCommand::FlashFrame(flag));
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("set-login-size", move |_| {
                ws.send(WindowCommand::SetLoginSize);
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("main-window-expand", move |_| {
                ws.send(WindowCommand::ExpandMainWindow);
                Ok(wire::undefined())
            });
        }
        {
            let ws = Arc::clone(&self.window_state);
            bridge.handle_channel_sync("set-main-window-resizable", move |args| {
                let resizable = args.first().and_then(Value::as_bool).unwrap_or(true);
                ws.send(WindowCommand::SetResizable(resizable));
                Ok(wire::undefined())
            });
        }

        // ---- 10. XDG FileChooser dialogs (`show-open-dialog*` / `show-save-dialog*`) ----
        {
            let p = Arc::clone(&platform);
            let rt_handle = self.tokio_handle.clone();
            bridge.handle_channel("show-open-dialog", move |args| {
                let p = Arc::clone(&p);
                let rt_handle = rt_handle.clone();
                async move {
                    let paths = run_open_dialog_portal(&p, &rt_handle, &args).await;
                    let strings: Vec<String> = paths
                        .iter()
                        .map(|x| x.to_string_lossy().into_owned())
                        .collect();
                    Ok(json!({
                        "canceled": strings.is_empty(),
                        "filePaths": strings,
                    }))
                }
            });
        }
        {
            let p = Arc::clone(&platform);
            let rt_handle = self.tokio_handle.clone();
            bridge.handle_channel("show-open-dialog-sync", move |args| {
                let p = Arc::clone(&p);
                let rt_handle = rt_handle.clone();
                async move {
                    let paths = run_open_dialog_portal(&p, &rt_handle, &args).await;
                    if paths.is_empty() {
                        Ok(wire::undefined())
                    } else {
                        let strings: Vec<String> = paths
                            .iter()
                            .map(|x| x.to_string_lossy().into_owned())
                            .collect();
                        Ok(json!(strings))
                    }
                }
            });
        }
        {
            let p = Arc::clone(&platform);
            let rt_handle = self.tokio_handle.clone();
            bridge.handle_channel("show-save-dialog", move |args| {
                let p = Arc::clone(&p);
                let rt_handle = rt_handle.clone();
                async move {
                    match run_save_dialog_portal(&p, &rt_handle, &args).await {
                        Some(dest) => Ok(json!({
                            "canceled": false,
                            "filePath": dest.to_string_lossy(),
                        })),
                        None => Ok(json!({
                            "canceled": true,
                            "filePath": "",
                        })),
                    }
                }
            });
        }
        {
            let p = Arc::clone(&platform);
            let rt_handle = self.tokio_handle.clone();
            bridge.handle_channel("show-save-dialog-sync", move |args| {
                let p = Arc::clone(&p);
                let rt_handle = rt_handle.clone();
                async move {
                    match run_save_dialog_portal(&p, &rt_handle, &args).await {
                        Some(dest) => Ok(json!(dest.to_string_lossy())),
                        None => Ok(wire::undefined()),
                    }
                }
            });
        }

        // ---- 11. `$zscreencap.getClipboard` & `$zFileManager` trust IPCs ----
        {
            let p = Arc::clone(&platform);
            bridge.handle_channel("get-clipboard", move |args| {
                let p = Arc::clone(&p);
                async move {
                    let req_id = args
                        .first()
                        .and_then(|v| {
                            v.get("idReq")
                                .or_else(|| v.get("requestId"))
                                .cloned()
                                .or_else(|| v.is_string().then(|| v.clone()))
                        })
                        .unwrap_or_else(|| json!(""));
                    let content = read_clipboard_paths_or_staged_png(&p);
                    Ok(json!({
                        "requestId": req_id,
                        "content": content,
                    }))
                }
            });
        }
        {
            let p = Arc::clone(&platform);
            bridge.handle_channel_sync("is_trust_path", move |args| {
                if let Some(raw_path) = args.first().and_then(Value::as_str) {
                    let path = Path::new(raw_path);
                    if path.exists() {
                        p.grant_readable_path(path);
                    }
                }
                Ok(json!(true))
            });
        }
        bridge.handle_channel_sync("trust_path_from_blob", |_| Ok(json!(true)));
        bridge.handle_channel_sync("verify_zalo_pc_hardlink_support", |_| Ok(json!(false)));

        // Benign Electron updater / power / analytics / content-protection channels acknowledged cleanly
        bridge.acknowledge_channels(&[
            "set-content-protection",
            "open-photo-viewer-devtool",
            "check-for-updates",
            "quit-and-install",
            "power-save-blocker",
            "sentry-ipc",
        ]);
    }

    fn install_preload_local_handlers(&self, bridge: &Rc<ZaloBridge>) {
        let platform = Arc::clone(&self.platform);

        // 1. `electron-store` backing (`$zelectron.store*`)
        {
            let p = Arc::clone(&platform);
            bridge.register_sync("$zelectron.storePath", move |args| {
                let name = args.first().and_then(Value::as_str).unwrap_or("config");
                Ok(json!(p.store_path(name).to_string_lossy()))
            });
        }
        {
            let p = Arc::clone(&platform);
            bridge.register_sync("$zelectron.storeRead", move |args| {
                let name = args.first().and_then(Value::as_str).unwrap_or("config");
                Ok(Value::Object(p.read_store(name)))
            });
        }
        {
            let p = Arc::clone(&platform);
            bridge.register_sync("$zelectron.storeWrite", move |args| {
                let name = args.first().and_then(Value::as_str).unwrap_or("config");
                let empty = Map::new();
                let obj = args.get(1).and_then(Value::as_object).unwrap_or(&empty);
                p.write_store(name, obj).map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            });
        }

        // 2. `$zelectronNative` GTK3 clipboard (`SELECTION_CLIPBOARD`)
        // Note: `handle_sync_request` runs on the GTK main thread inside WebKit2GTK's custom URI scheme handler.
        bridge.register_sync("$zelectronNative.clipboardText", |_| {
            let text = glib::MainContext::default()
                .is_owner()
                .then(|| {
                    let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                    cb.wait_for_text().map(|s| s.to_string())
                })
                .flatten()
                .unwrap_or_default();
            Ok(Value::String(text))
        });
        bridge.register_sync("$zelectronNative.clipboardWriteText", |args| {
            let text = args
                .first()
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if glib::MainContext::default().is_owner() {
                let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                cb.set_text(&text);
            } else {
                glib::MainContext::default().invoke(move || {
                    let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                    cb.set_text(&text);
                });
            }
            Ok(wire::undefined())
        });
        bridge.register_sync("$zelectronNative.clipboardFormats", |_| {
            let mut formats = Vec::new();
            if glib::MainContext::default().is_owner() {
                let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                if cb.wait_is_text_available() {
                    formats.push("text/plain");
                }
                if cb.wait_is_image_available() {
                    formats.push("image/png");
                }
                if cb.wait_is_uris_available() {
                    formats.push("text/uri-list");
                }
            }
            Ok(json!(formats))
        });
        bridge.register_sync("$zelectronNative.clipboardImagePng", |_| {
            if !glib::MainContext::default().is_owner() {
                return Ok(Value::Null);
            }
            let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
            let Some(pixbuf) = cb.wait_for_image() else {
                return Ok(Value::Null);
            };
            match pixbuf.save_to_bufferv("png", &[]) {
                Ok(png_bytes) if !png_bytes.is_empty() => Ok(wire::encode_bytes(&png_bytes)),
                _ => Ok(Value::Null),
            }
        });
        bridge.register_sync("$zelectronNative.clipboardWriteImagePng", |args| {
            let Some(first) = args.first() else {
                return Ok(wire::undefined());
            };
            let Some(bytes) = wire::decode_bytes(first) else {
                return Ok(wire::undefined());
            };
            if bytes.is_empty() {
                return Ok(wire::undefined());
            }
            let apply_png = move || {
                let loader = gdk::gdk_pixbuf::PixbufLoader::new();
                if loader.write(&bytes).is_ok() {
                    let _ = loader.close();
                    if let Some(pixbuf) = loader.pixbuf() {
                        let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                        cb.set_image(&pixbuf);
                    }
                } else {
                    let _ = loader.close();
                }
            };
            if glib::MainContext::default().is_owner() {
                apply_png();
            } else {
                glib::MainContext::default().invoke(apply_png);
            }
            Ok(wire::undefined())
        });

        // 2b. `$znode.shell.*` (`openPath` / `openExternal` / `showItemInFolder`)
        bridge.register_async("$znode.shell.openPath", |args| async move {
            let target = args.first().and_then(Value::as_str).unwrap_or("").trim();
            let ok = exec_shell_operation("openPath", target);
            Ok(json!(if ok { "" } else { "Failed to open path" }))
        });
        bridge.register_async("$znode.shell.openExternal", |args| async move {
            let target = args.first().and_then(Value::as_str).unwrap_or("").trim();
            Ok(json!(exec_shell_operation("openExternal", target)))
        });
        bridge.register_async("$znode.shell.showItemInFolder", |args| async move {
            let target = args.first().and_then(Value::as_str).unwrap_or("").trim();
            let _ = exec_shell_operation("showItemInFolder", target);
            Ok(wire::undefined())
        });

        // 2c. `$znode.nativelibs.zfileAddon.*` (`canRead`, `canWrite`, `canReadAndWrite`, `getDiskInfo`, `getInfo`, `copyFolder`, `cancelCopy`)
        for path in [
            "$znode.nativelibs.zfileAddon.canRead",
            "$znode.nativelibs.zfileAddon.canWrite",
            "$znode.nativelibs.zfileAddon.canReadAndWrite",
        ] {
            let p = Arc::clone(&platform);
            let path_owned = path.to_string();
            bridge.register_async(path, move |args| {
                let p = Arc::clone(&p);
                let path_owned = path_owned.clone();
                async move {
                    let Some(raw) = args.first().and_then(Value::as_str) else {
                        return Ok(json!(false));
                    };
                    let target = Path::new(raw);
                    let ok = if path_owned.ends_with("canRead") {
                        target.exists()
                    } else {
                        p.is_writable(target)
                            || target
                                .parent()
                                .is_some_and(|par| par.exists() && p.is_writable(par))
                    };
                    Ok(json!(ok))
                }
            });
        }
        bridge.register_async("$znode.nativelibs.zfileAddon.getDiskInfo", |_| async move {
            Ok(json!({
                "free": 50_000_000_000_u64,
                "total": 200_000_000_000_u64,
                "available": 50_000_000_000_u64,
            }))
        });
        bridge.register_async("$znode.nativelibs.zfileAddon.getInfo", |args| async move {
            let Some(raw) = args.first().and_then(Value::as_str) else {
                return Ok(Value::Null);
            };
            match std::fs::metadata(raw) {
                Ok(meta) => Ok(json!({
                    "size": meta.len(),
                    "isDirectory": meta.is_dir(),
                    "isFile": meta.is_file(),
                })),
                Err(_) => Ok(Value::Null),
            }
        });
        {
            let p = Arc::clone(&platform);
            let rt_handle = self.tokio_handle.clone();
            bridge.register_async("$znode.nativelibs.zfileAddon.copyFolder", move |args| {
                let p = Arc::clone(&p);
                let rt_handle = rt_handle.clone();
                async move {
                    let src = args
                        .first()
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let dst = args
                        .get(1)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if src.is_empty() || dst.is_empty() || !p.is_writable(Path::new(&dst)) {
                        return Ok(json!(false));
                    }
                    let (tx, rx) = oneshot::channel();
                    rt_handle.spawn_blocking(move || {
                        let ok = copy_dir_recursive(Path::new(&src), Path::new(&dst)).is_ok();
                        let _ = tx.send(ok);
                    });
                    Ok(json!(rx.await.unwrap_or(false)))
                }
            });
        }
        bridge.register_sync("$znode.nativelibs.zfileAddon.cancelCopy", |_| {
            Ok(wire::undefined())
        });

        // 3. `$zFileManager.getSentryConfig` (sync)
        bridge.register_sync("$zFileManager.getSentryConfig", |_| Ok(Value::Null));

        // 4. `$znode.crypto.*` (sync)
        for path in [
            "$znode.crypto.encrypt",
            "$znode.crypto.decrypt",
            "$znode.crypto.hash",
            "$znode.crypto.randomBytes",
            "$znode.crypto.__cipherCreate",
            "$znode.crypto.__cipherUpdate",
            "$znode.crypto.__cipherFinal",
            "$znode.crypto.__cipherOption",
            "$znode.crypto.__cipherSetAutoPadding",
            "$znode.crypto.__cipherSetAAD",
            "$znode.crypto.__cipherSetAuthTag",
            "$znode.crypto.__cipherGetAuthTag",
            "$znode.crypto.__hashCreate",
            "$znode.crypto.__hashUpdate",
            "$znode.crypto.__hashDigest",
        ] {
            let crypto = Arc::clone(&self.crypto);
            let path_owned = path.to_string();
            bridge.register_sync(path, move |args| crypto.dispatch_sync(&path_owned, args));
        }

        // 5. `$znode.fs*` synchronous calls
        for path in [
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
        ] {
            let fsb = self.fs_bridge.clone();
            let path_owned = path.to_string();
            bridge.register_sync(path, move |args| fsb.dispatch_sync(&path_owned, args));
        }

        // 6. `$znode.fs*` and `$zFileManager.*` asynchronous calls (offloaded onto Tokio blocking pool)
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
            "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync",
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
        ] {
            let fsb = self.fs_bridge.clone();
            let rt_handle = self.tokio_handle.clone();
            let path_owned = path.to_string();
            bridge.register_async(path, move |args| {
                let fsb = fsb.clone();
                let rt_handle = rt_handle.clone();
                let path_owned = path_owned.clone();
                async move {
                    let (tx, rx) = oneshot::channel();
                    rt_handle.spawn(async move {
                        let out = fsb.dispatch_async(&path_owned, &args).await;
                        let _ = tx.send(out);
                    });
                    rx.await.map_err(|e| format!("EIO: {e}"))?
                }
            });
        }
    }

    async fn resolve_notification_avatar(&self, icon: &Option<NotificationIcon>) -> Option<PathBuf> {
        let icon = icon.as_ref()?;
        let data: Vec<u8> = match icon {
            NotificationIcon::Inline(bytes) => bytes.clone(),
            NotificationIcon::Bundled(rel) => {
                let asar = self.runtime.asar()?;
                let lookup = format!("pc-dist/{}", rel.trim_start_matches('/'));
                asar.read_or_unpacked(&lookup)?.into_owned()
            }
            NotificationIcon::Remote(url) => {
                let loader = self.media_loader.clone();
                let url_owned = url.clone();
                let (tx, rx) = oneshot::channel();
                self.tokio_handle.spawn(async move {
                    let res = tokio::time::timeout(
                        Duration::from_secs(3),
                        loader.fetch_bytes(&url_owned, DEFAULT_IMAGE_DEADLINE),
                    )
                    .await;
                    let _ = tx.send(res);
                });
                match rx.await {
                    Ok(Ok(Ok(bytes))) => bytes.to_vec(),
                    _ => return None,
                }
            }
        };

        let (_, ext) = NotificationPolicy::attachment_type(&data)?;
        let dir = self.platform.temp_dir().join("notifications");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
        std::fs::write(&file, &data).ok()?;
        Some(file)
    }

    /// Installs full bridge handlers onto a secondary popup (`MediaViewer` / `child.html`) bridge,
    /// overriding `$zwindow.*` window-control channels so they target `popup_win` rather than the main window.
    pub fn install_popup_bridge_on(
        self: &Rc<Self>,
        child_bridge: &Rc<ZaloBridge>,
        popup_win: &gtk::Window,
        _parent_bridge: &Rc<ZaloBridge>,
    ) {
        self.install_bridge_on(child_bridge);

        let popup_state = Arc::new(MainWindowState::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WindowCommand>();
        popup_state.set_command_sender(tx);

        let ps_for_event = Arc::clone(&popup_state);
        let main_ws_for_event = Arc::clone(&self.window_state);
        popup_win.connect_window_state_event(move |_, ev| {
            let ns = ev.new_window_state();
            let is_max = ns.contains(gdk::WindowState::MAXIMIZED);
            ps_for_event.is_maximized.store(is_max, Ordering::Relaxed);
            ps_for_event
                .is_fullscreen
                .store(ns.contains(gdk::WindowState::FULLSCREEN), Ordering::Relaxed);
            main_ws_for_event
                .child_maximized
                .write()
                .insert("photo-viewer".to_owned(), is_max);
            glib::Propagation::Proceed
        });
        let main_ws_for_destroy = Arc::clone(&self.window_state);
        popup_win.connect_destroy(move |_| {
            main_ws_for_destroy
                .child_maximized
                .write()
                .remove("photo-viewer");
        });

        let win_weak = {
            let w = glib::WeakRef::new();
            w.set(Some(popup_win));
            w
        };
        glib::MainContext::default().spawn_local(async move {
            while let Some(cmd) = rx.recv().await {
                let Some(win) = win_weak.upgrade() else {
                    break;
                };
                match cmd {
                    WindowCommand::Minimize { .. } => win.iconify(),
                    WindowCommand::ToggleMaximize { target, .. } => {
                        let should_max = target.unwrap_or_else(|| !win.is_maximized());
                        if should_max {
                            win.maximize();
                        } else {
                            win.unmaximize();
                        }
                    }
                    WindowCommand::ToggleFullscreen { target, .. } => {
                        let cur = win
                            .window()
                            .is_some_and(|w| w.state().contains(gdk::WindowState::FULLSCREEN));
                        let should_fs = target.unwrap_or(!cur);
                        if should_fs {
                            win.fullscreen();
                        } else {
                            win.unfullscreen();
                        }
                    }
                    WindowCommand::CloseOrHide { .. }
                    | WindowCommand::Hide { .. }
                    | WindowCommand::Quit => {
                        win.close();
                    }
                    WindowCommand::Show { focus, .. } => {
                        win.show_all();
                        if focus {
                            win.present();
                        }
                    }
                    WindowCommand::Focus { .. } => {
                        win.present();
                    }
                    WindowCommand::FlashFrame(flag) => {
                        win.set_urgency_hint(flag);
                    }
                    WindowCommand::SetLoginSize
                    | WindowCommand::ExpandMainWindow
                    | WindowCommand::SetResizable(_) => {}
                }
            }
        });

        for ch in ["minimize", "window-minimize"] {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync(ch, move |_| {
                ps.send(WindowCommand::Minimize { window_id: None });
                Ok(wire::undefined())
            });
        }
        for ch in ["toggle-maximize", "window-maximize"] {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync(ch, move |args| {
                let target = args.get(1).and_then(Value::as_bool);
                ps.send(WindowCommand::ToggleMaximize {
                    window_id: None,
                    target,
                });
                Ok(wire::undefined())
            });
        }
        {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync("check-maximize", move |_| {
                Ok(json!(ps.is_maximized.load(Ordering::Relaxed)))
            });
        }
        {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync("toggle-fullscreen", move |args| {
                let target = args.get(1).and_then(Value::as_bool);
                ps.send(WindowCommand::ToggleFullscreen {
                    window_id: None,
                    target,
                });
                Ok(wire::undefined())
            });
        }
        {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync("check-fullscreen", move |_| {
                Ok(json!(ps.is_fullscreen.load(Ordering::Relaxed)))
            });
        }
        for ch in [
            "close-window",
            "window-close",
            "hide-window",
            "window-hide",
            "quit",
            "do-quit",
        ] {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync(ch, move |_| {
                ps.send(WindowCommand::CloseOrHide { window_id: None });
                Ok(wire::undefined())
            });
        }
        for (ch, focus) in [
            ("show", true),
            ("window-show", true),
            ("window-restore", true),
            ("show-inactive", false),
        ] {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync(ch, move |_| {
                ps.send(WindowCommand::Show {
                    window_id: None,
                    focus,
                });
                Ok(wire::undefined())
            });
        }
        for ch in ["focus-window", "window-focus"] {
            let ps = Arc::clone(&popup_state);
            child_bridge.override_channel_sync(ch, move |_| {
                ps.send(WindowCommand::Focus { window_id: None });
                Ok(wire::undefined())
            });
        }
    }

    /// Plays a notification sound URL if allowed by [`NotificationPolicy::is_sound_url_allowed`]
    /// and sound notifications are enabled in the user's preferences.
    pub fn play_notification_sound_if_allowed(&self, raw_url: &str) {
        if !self.notification_context().sound_enabled
            || !NotificationPolicy::is_sound_url_allowed(raw_url)
        {
            return;
        }
        let loader = self.media_loader.clone();
        let notifier = self.notifier.clone();
        let url_owned = raw_url.to_string();
        self.tokio_handle.spawn(async move {
            if let Ok(path) = loader.local_file(&url_owned).await {
                notifier.play_chime_file(&path);
            }
        });
    }
}

fn exec_shell_operation(op: &str, target: &str) -> bool {
    if target.is_empty() {
        return false;
    }
    if op == "openExternal" {
        if let Ok(parsed) = url::Url::parse(target) {
            if matches!(parsed.scheme(), "http" | "https" | "mailto") {
                let _ = gio::AppInfo::launch_default_for_uri(
                    parsed.as_str(),
                    gio::AppLaunchContext::NONE,
                );
                return true;
            }
        }
        return false;
    }

    let path = PathBuf::from(target);
    let reveal_only = op == "showItemInFolder" || FileCard::may_execute(target);
    let to_open = if reveal_only {
        path.parent().map(PathBuf::from).unwrap_or(path)
    } else {
        path
    };
    if let Ok(uri) = url::Url::from_file_path(&to_open) {
        let _ = gio::AppInfo::launch_default_for_uri(uri.as_str(), gio::AppLaunchContext::NONE);
        return true;
    }
    false
}

fn extract_dialog_options(args: &[Value]) -> &Value {
    args.iter()
        .find(|v| v.is_object())
        .unwrap_or(&Value::Null)
}

async fn run_open_dialog_portal(
    platform: &Arc<ZaloPlatform>,
    tokio_handle: &tokio::runtime::Handle,
    args: &[Value],
) -> Vec<PathBuf> {
    let opts = extract_dialog_options(args);
    let title = opts
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Select File")
        .to_string();
    let default_path = opts
        .get("defaultPath")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let mut directory = false;
    let mut multiple = false;
    if let Some(props) = opts.get("properties").and_then(Value::as_array) {
        for prop in props.iter().filter_map(Value::as_str) {
            match prop {
                "openDirectory" => directory = true,
                "multiSelections" => multiple = true,
                _ => {}
            }
        }
    }
    let mut filters: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(raw_filters) = opts.get("filters").and_then(Value::as_array) {
        for f in raw_filters {
            let name = f
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Files")
                .to_string();
            let exts: Vec<String> = f
                .get("extensions")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if !exts.is_empty() {
                filters.push((name, exts));
            }
        }
    }

    let (tx, rx) = oneshot::channel();
    tokio_handle.spawn(async move {
        let picked = pick_open_dialog(
            &title,
            multiple,
            directory,
            default_path.as_deref(),
            &filters,
        )
        .await;
        let _ = tx.send(picked);
    });
    let paths = match rx.await {
        Ok(Ok(list)) => list,
        _ => Vec::new(),
    };
    for p in &paths {
        if directory {
            platform.grant_writable_path(p);
        }
        platform.grant_readable_path(p);
    }
    paths
}

async fn run_save_dialog_portal(
    platform: &Arc<ZaloPlatform>,
    tokio_handle: &tokio::runtime::Handle,
    args: &[Value],
) -> Option<PathBuf> {
    let opts = extract_dialog_options(args);
    let title = opts
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Save File")
        .to_string();
    let default_raw = opts
        .get("defaultPath")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());

    let (initial_dir, suggested_name) = match default_raw {
        Some(raw) => {
            let p = Path::new(raw);
            if p.is_dir() {
                (Some(p.to_path_buf()), "download".to_string())
            } else {
                let dir = p
                    .parent()
                    .filter(|par| !par.as_os_str().is_empty() && par.exists())
                    .map(Path::to_path_buf)
                    .or_else(|| Some(platform.downloads_dir().to_path_buf()));
                let fname = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("download")
                    .to_string();
                (dir, fname)
            }
        }
        None => (
            Some(platform.downloads_dir().to_path_buf()),
            "download".to_string(),
        ),
    };

    let (tx, rx) = oneshot::channel();
    tokio_handle.spawn(async move {
        let res = pick_save_file_with_dir(&title, &suggested_name, initial_dir.as_deref()).await;
        let _ = tx.send(res);
    });

    let dest = rx.await.ok()?.ok().flatten()?;
    platform.grant_writable_path(&dest);
    Some(dest)
}

fn read_clipboard_paths_or_staged_png(platform: &ZaloPlatform) -> Value {
    if !glib::MainContext::default().is_owner() {
        return Value::Null;
    }
    let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
    let uris = cb.wait_for_uris();
    if !uris.is_empty() {
        let mut local_paths = Vec::new();
        for u in uris {
            if let Ok(parsed) = url::Url::parse(u.as_str()) {
                if let Ok(path) = parsed.to_file_path() {
                    if path.exists() {
                        platform.grant_readable_path(&path);
                        local_paths.push(path.to_string_lossy().into_owned());
                    }
                }
            }
        }
        if !local_paths.is_empty() {
            return Value::String(local_paths.join("|"));
        }
    }

    if let Some(pixbuf) = cb.wait_for_image() {
        if let Ok(png_bytes) = pixbuf.save_to_bufferv("png", &[]) {
            if !png_bytes.is_empty() {
                let dir = platform.temp_dir().join("olaz-clipboard");
                let _ = std::fs::create_dir_all(&dir);
                let file = dir.join(format!("clip-{}.png", uuid::Uuid::new_v4().simple()));
                if std::fs::write(&file, &png_bytes).is_ok() {
                    platform.grant_readable_path(&file);
                    return Value::String(file.to_string_lossy().into_owned());
                }
            }
        }
    }

    Value::Null
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_worker_router_registers_and_resets_cleanly() {
        let router = SharedWorkerRouter::new();
        let main_bridge = ZaloBridge::new();
        let worker_bridge = ZaloBridge::new();
        router.install(&main_bridge);
        router.install(&worker_bridge);

        // Before worker registration, worker_bridge is None.
        assert!(router.worker_bridge.borrow().is_none());

        router.register_worker(&worker_bridge);
        assert!(router
            .worker_bridge
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some());

        // Insert a handler and a pending task on worker_bridge, then reset_worker.
        router
            .handlers
            .borrow_mut()
            .insert("CLOUD_SCAN_V2".to_string(), Rc::downgrade(&worker_bridge));
        router.pending.borrow_mut().insert(
            format!("{}_req1", main_bridge.id()),
            PendingWorkerTask {
                requester: Rc::downgrade(&main_bridge),
                request_id: json!("req1"),
                task_type: "CLOUD_SCAN_V2".to_string(),
                handler_id: worker_bridge.id(),
            },
        );

        router.reset_worker(&worker_bridge);
        assert!(router.worker_bridge.borrow().is_none());
        assert!(router.handlers.borrow().is_empty());
        assert!(router.pending.borrow().is_empty());
    }

    #[test]
    fn value_id_str_formats_strings_and_numbers_without_quotes() {
        assert_eq!(value_id_str(&json!("abc-123")), "abc-123");
        assert_eq!(value_id_str(&json!(42)), "42");
        assert_eq!(value_id_str(&Value::Null), "null");
    }
}
