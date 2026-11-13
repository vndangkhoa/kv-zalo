//! Self-contained GTK3 + WebKit2GTK VoIP Call UI controller (`CallUiController`) managing the
//! 3 frameless call windows (`call`: `496x720` / `640x720` video `always_on_top`,
//! `incoming`: `360x560` `always_on_top`, `devices`: `420x600`).

#![allow(dead_code)]

use base64::Engine as _;
use gtk::prelude::*;
use olaz_linux::call::{
    default_recordings_dir, get_ui_asset, AudioDeviceList, CallPartnerInfo, CallUiEvent,
    OlazCallEngine, CALL_UI_CALL_URL, CALL_UI_DEVICES_URL, CALL_UI_INCOMING_URL, CALL_UI_SCHEME,
    PRELOAD_BRIDGE_JS,
};
use serde_json::{json, Value};
use soup::{MessageHeaders, MessageHeadersType};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;
use webkit2gtk::{
    HardwareAccelerationPolicy, LoadEvent, SecurityManagerExt, Settings as WebKitSettings,
    SettingsExt, URISchemeRequest, URISchemeRequestExt, URISchemeResponse, URISchemeResponseExt,
    UserContentInjectedFrames, UserContentManager, UserContentManagerExt, UserScript,
    UserScriptInjectionTime, WebContext, WebContextExt, WebView, WebViewExt,
};

/// Audio call window dimensions (`496x720`).
pub const CALL_WINDOW_AUDIO_WIDTH: i32 = 496;
pub const CALL_WINDOW_HEIGHT: i32 = 720;
/// Video / screen-share call window dimensions (`640x720`).
pub const CALL_WINDOW_VIDEO_WIDTH: i32 = 640;

/// Incoming call popup window dimensions (`360x560`).
pub const INCOMING_WINDOW_WIDTH: i32 = 360;
pub const INCOMING_WINDOW_HEIGHT: i32 = 560;

/// Audio devices dialog dimensions (`420x600`).
pub const DEVICES_WINDOW_WIDTH: i32 = 420;
pub const DEVICES_WINDOW_HEIGHT: i32 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedWindowKind {
    Call,
    Incoming,
    Devices,
}

struct ManagedCallWindow {
    window: gtk::Window,
    webview: WebView,
    ready: Cell<bool>,
    pending_js: RefCell<Vec<String>>,
    suppress_delete_action: Cell<bool>,
    last_press: Rc<Cell<Option<crate::client_host::LastButtonPress>>>,
}

impl ManagedCallWindow {
    fn eval_or_queue(&self, js: String) {
        if self.ready.get() {
            self.webview
                .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
        } else {
            self.pending_js.borrow_mut().push(js);
        }
    }

    fn flush_pending(&self) {
        self.ready.set(true);
        let queued: Vec<String> = self.pending_js.borrow_mut().drain(..).collect();
        for js in queued {
            self.webview
                .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
        }
    }

    fn destroy_quietly(&self) {
        self.suppress_delete_action.set(true);
        self.window.close();
    }
}

/// GTK3 + WebKit2GTK controller for the `call`, `incoming`, and `devices` frameless windows.
pub struct CallUiController {
    web_context: WebContext,
    action_handler: RefCell<Option<Arc<dyn Fn(&str, Value) + Send + Sync>>>,
    call_win: RefCell<Option<Rc<ManagedCallWindow>>>,
    incoming_win: RefCell<Option<Rc<ManagedCallWindow>>>,
    devices_win: RefCell<Option<Rc<ManagedCallWindow>>>,
    last_partner: RefCell<Option<CallPartnerInfo>>,
    last_state: RefCell<Option<Value>>,
    last_devices: RefCell<Option<AudioDeviceList>>,
    last_recording: RefCell<Option<Value>>,
    last_local_frame_at: Cell<Option<Instant>>,
    last_remote_frame_at: Cell<Option<Instant>>,
}

impl CallUiController {
    /// Creates a `CallUiController` on the GTK main thread, optionally serving fallback
    /// `zalo-app://` assets from `optional_zalo_app_dir` if present.
    pub fn new(optional_zalo_app_dir: Option<PathBuf>) -> Rc<Self> {
        let web_context = build_call_ui_web_context(optional_zalo_app_dir);
        Rc::new(Self {
            web_context,
            action_handler: RefCell::new(None),
            call_win: RefCell::new(None),
            incoming_win: RefCell::new(None),
            devices_win: RefCell::new(None),
            last_partner: RefCell::new(None),
            last_state: RefCell::new(None),
            last_devices: RefCell::new(None),
            last_recording: RefCell::new(None),
            last_local_frame_at: Cell::new(None),
            last_remote_frame_at: Cell::new(None),
        })
    }

    /// Creates a `CallUiController` together with a thread-safe `Send + Sync` event callback
    /// suitable for passing directly to `CallEngineConfig::emit_ui_event`.
    pub fn create_with_event_sink(
        optional_zalo_app_dir: Option<PathBuf>,
    ) -> (Rc<Self>, Arc<dyn Fn(CallUiEvent) + Send + Sync>) {
        let controller = Self::new(optional_zalo_app_dir);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CallUiEvent>();
        let weak_ctrl = Rc::downgrade(&controller);
        glib::MainContext::default().spawn_local(async move {
            while let Some(ev) = rx.recv().await {
                let Some(ctrl) = weak_ctrl.upgrade() else {
                    break;
                };
                ctrl.handle_event(ev);
            }
        });
        let sink = Arc::new(move |ev: CallUiEvent| {
            let _ = tx.send(ev);
        });
        (controller, sink)
    }

    /// Wires bidirectional action routing between this `CallUiController` and an `OlazCallEngine`.
    pub fn bind_engine(&self, engine: Arc<OlazCallEngine>) {
        self.set_action_handler(move |action, value| {
            engine.handle_ui_action(action, value);
        });
    }

    /// Registers a callback invoked whenever `window.zcallUI.action(action, value)` fires.
    pub fn set_action_handler<F>(&self, handler: F)
    where
        F: Fn(&str, Value) + Send + Sync + 'static,
    {
        *self.action_handler.borrow_mut() = Some(Arc::new(handler));
    }

    /// Applies a `CallUiEvent` on the GTK main thread.
    pub fn handle_event(self: &Rc<Self>, event: CallUiEvent) {
        match event {
            CallUiEvent::ShowCall {
                partner, is_video, ..
            } => {
                *self.last_partner.borrow_mut() = Some(partner.clone());
                let win = self.ensure_window(ManagedWindowKind::Call, is_video);
                let target_w = if is_video {
                    CALL_WINDOW_VIDEO_WIDTH
                } else {
                    CALL_WINDOW_AUDIO_WIDTH
                };
                win.window.resize(target_w, CALL_WINDOW_HEIGHT);
                dispatch_to_window(&win, "partner", &serde_json::to_value(&partner).unwrap_or_default());
                win.window.show_all();
                win.window.present();
            }
            CallUiEvent::ShowIncoming { partner, .. } => {
                *self.last_partner.borrow_mut() = Some(partner.clone());
                let win = self.ensure_window(ManagedWindowKind::Incoming, false);
                dispatch_to_window(&win, "partner", &serde_json::to_value(&partner).unwrap_or_default());
                if let Some(ref st) = *self.last_state.borrow() {
                    dispatch_to_window(&win, "state", st);
                }
                win.window.show_all();
                win.window.present();
            }
            CallUiEvent::OpenDevices => {
                let win = self.ensure_window(ManagedWindowKind::Devices, false);
                if let Some(ref devs) = *self.last_devices.borrow() {
                    if let Ok(v) = serde_json::to_value(devs) {
                        dispatch_to_window(&win, "devices", &v);
                    }
                }
                win.window.show_all();
                win.window.present();
            }
            CallUiEvent::SetState {
                state,
                connected_at,
                outcome,
                is_video,
                camera_enabled,
                screen_sharing,
            } => {
                let payload = json!({
                    "state": state,
                    "connectedAt": connected_at,
                    "outcome": outcome,
                    "isVideo": is_video,
                    "cameraEnabled": camera_enabled,
                    "screenSharing": screen_sharing,
                });
                *self.last_state.borrow_mut() = Some(payload.clone());

                if let Some(ref w) = *self.call_win.borrow() {
                    if is_video {
                        w.window.resize(CALL_WINDOW_VIDEO_WIDTH, CALL_WINDOW_HEIGHT);
                    }
                    dispatch_to_window(w, "state", &payload);
                }
                if let Some(ref w) = *self.incoming_win.borrow() {
                    dispatch_to_window(w, "state", &payload);
                }
            }
            CallUiEvent::SetDevices(devs) => {
                *self.last_devices.borrow_mut() = Some(devs.clone());
                if let Ok(val) = serde_json::to_value(&devs) {
                    if let Some(ref w) = *self.call_win.borrow() {
                        dispatch_to_window(w, "devices", &val);
                    }
                    if let Some(ref w) = *self.devices_win.borrow() {
                        dispatch_to_window(w, "devices", &val);
                    }
                }
            }
            CallUiEvent::SetMicLevel { permille } => {
                let val = json!(permille);
                if let Some(ref w) = *self.call_win.borrow() {
                    if w.ready.get() {
                        dispatch_to_window(w, "micLevel", &val);
                    }
                }
                if let Some(ref w) = *self.devices_win.borrow() {
                    if w.ready.get() {
                        dispatch_to_window(w, "micLevel", &val);
                    }
                }
            }
            CallUiEvent::VideoFrame {
                is_local,
                width,
                height,
                rgba,
            } => {
                let Some(ref w) = *self.call_win.borrow() else {
                    return;
                };
                if !w.ready.get() {
                    return;
                }
                // Cap canvas frame updates to ~20 fps per stream to avoid WebProcess IPC backlog.
                let now = Instant::now();
                let last_cell = if is_local {
                    &self.last_local_frame_at
                } else {
                    &self.last_remote_frame_at
                };
                if let Some(prev) = last_cell.get() {
                    if now.duration_since(prev) < Duration::from_millis(45) {
                        return;
                    }
                }
                last_cell.set(Some(now));

                let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
                let payload = json!({
                    "isLocal": is_local,
                    "width": width,
                    "height": height,
                    "rgbaBase64": b64,
                });
                dispatch_to_window(w, "videoFrame", &payload);
            }
            CallUiEvent::SetRecording {
                active,
                started_at_ms,
                path,
                format,
                error,
            } => {
                let payload = json!({
                    "active": active,
                    "startedAtMs": started_at_ms,
                    "path": path,
                    "format": format,
                    "error": error,
                });
                *self.last_recording.borrow_mut() = Some(payload.clone());
                if let Some(ref w) = *self.call_win.borrow() {
                    dispatch_to_window(w, "recording", &payload);
                }
            }
            CallUiEvent::CloseIncoming => {
                if let Some(w) = self.incoming_win.borrow_mut().take() {
                    w.destroy_quietly();
                }
            }
            CallUiEvent::CloseCall => {
                if let Some(dw) = self.devices_win.borrow_mut().take() {
                    dw.destroy_quietly();
                }
                if let Some(cw) = self.call_win.borrow_mut().take() {
                    cw.destroy_quietly();
                }
                *self.last_partner.borrow_mut() = None;
                *self.last_state.borrow_mut() = None;
                *self.last_recording.borrow_mut() = None;
            }
        }
    }

    /// Closes all open Call UI windows immediately.
    pub fn close_all(&self) {
        if let Some(iw) = self.incoming_win.borrow_mut().take() {
            iw.destroy_quietly();
        }
        if let Some(dw) = self.devices_win.borrow_mut().take() {
            dw.destroy_quietly();
        }
        if let Some(cw) = self.call_win.borrow_mut().take() {
            cw.destroy_quietly();
        }
    }

    fn ensure_window(
        self: &Rc<Self>,
        kind: ManagedWindowKind,
        is_video: bool,
    ) -> Rc<ManagedCallWindow> {
        let slot = match kind {
            ManagedWindowKind::Call => &self.call_win,
            ManagedWindowKind::Incoming => &self.incoming_win,
            ManagedWindowKind::Devices => &self.devices_win,
        };
        if let Some(existing) = slot.borrow().as_ref() {
            return Rc::clone(existing);
        }

        let (width, height, title, url, always_on_top) = match kind {
            ManagedWindowKind::Call => (
                if is_video {
                    CALL_WINDOW_VIDEO_WIDTH
                } else {
                    CALL_WINDOW_AUDIO_WIDTH
                },
                CALL_WINDOW_HEIGHT,
                "Zalo Call",
                CALL_UI_CALL_URL,
                true,
            ),
            ManagedWindowKind::Incoming => (
                INCOMING_WINDOW_WIDTH,
                INCOMING_WINDOW_HEIGHT,
                "Cuộc gọi đến",
                CALL_UI_INCOMING_URL,
                true,
            ),
            ManagedWindowKind::Devices => (
                DEVICES_WINDOW_WIDTH,
                DEVICES_WINDOW_HEIGHT,
                "Tình trạng thiết bị",
                CALL_UI_DEVICES_URL,
                false,
            ),
        };

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title(title);
        window.set_default_size(width, height);
        window.set_decorated(false);
        window.set_position(gtk::WindowPosition::Center);
        if always_on_top {
            window.set_keep_above(true);
        }

        let ucm = UserContentManager::new();
        ucm.register_script_message_handler("zcallUi");
        ucm.add_script(&UserScript::new(
            PRELOAD_BRIDGE_JS,
            UserContentInjectedFrames::TopFrame,
            UserScriptInjectionTime::Start,
            &[],
            &[],
        ));

        let settings = WebKitSettings::new();
        settings.set_enable_write_console_messages_to_stdout(false);
        settings.set_javascript_can_open_windows_automatically(false);
        settings.set_enable_back_forward_navigation_gestures(false);
        settings.set_enable_webaudio(true);
        if std::env::var_os("OLAZ_ENABLE_WEBKIT_HWACCEL").is_none() {
            settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
        }

        let webview = WebView::builder()
            .web_context(&self.web_context)
            .user_content_manager(&ucm)
            .settings(&settings)
            .build();
        webview.set_hexpand(true);
        webview.set_vexpand(true);
        let last_press = crate::client_host::track_last_button_press(&webview);
        window.add(&webview);

        let managed = Rc::new(ManagedCallWindow {
            window: window.clone(),
            webview: webview.clone(),
            ready: Cell::new(false),
            pending_js: RefCell::new(Vec::new()),
            suppress_delete_action: Cell::new(false),
            last_press,
        });

        // Flush queued JS AND re-deliver `last_partner`, `last_state`, and `last_devices` on load finish
        // (eliminating the `incoming.html` ringtone load race and `devices.html` blank-select race).
        let ctrl_weak_load = Rc::downgrade(self);
        let managed_weak_load = Rc::downgrade(&managed);
        webview.connect_load_changed(move |_, load_event| {
            if load_event != LoadEvent::Finished {
                return;
            }
            let Some(ctrl) = ctrl_weak_load.upgrade() else {
                return;
            };
            let Some(mw) = managed_weak_load.upgrade() else {
                return;
            };
            mw.flush_pending();

            if matches!(kind, ManagedWindowKind::Call | ManagedWindowKind::Incoming) {
                if let Some(ref partner) = *ctrl.last_partner.borrow() {
                    if let Ok(v) = serde_json::to_value(partner) {
                        dispatch_to_window(&mw, "partner", &v);
                    }
                }
                if let Some(ref st) = *ctrl.last_state.borrow() {
                    dispatch_to_window(&mw, "state", st);
                }
            }
            if matches!(kind, ManagedWindowKind::Call | ManagedWindowKind::Devices) {
                if let Some(ref devs) = *ctrl.last_devices.borrow() {
                    if let Ok(v) = serde_json::to_value(devs) {
                        dispatch_to_window(&mw, "devices", &v);
                    }
                }
            }
            if matches!(kind, ManagedWindowKind::Call) {
                if let Some(ref rec) = *ctrl.last_recording.borrow() {
                    dispatch_to_window(&mw, "recording", rec);
                }
            }
        });

        // Route `window.zcallUI.action(action, value)` via `UserContentManager` `"zcallUi"`.
        let ctrl_weak_msg = Rc::downgrade(self);
        let managed_weak_msg = Rc::downgrade(&managed);
        ucm.connect_script_message_received(Some("zcallUi"), move |_, js_res| {
            let Some(ctrl) = ctrl_weak_msg.upgrade() else {
                return;
            };
            let Some(val) = js_res.js_value() else {
                return;
            };
            let Some(parsed) = crate::client_host::decode_jsc_json_value(&val) else {
                return;
            };
            let action = parsed.get("action").and_then(Value::as_str).unwrap_or("");
            let value = parsed.get("value").cloned().unwrap_or(Value::Null);
            ctrl.on_script_action(kind, managed_weak_msg.upgrade(), action, value);
        });

        // Closing a window via WM shortcut (`Alt+F4`) cleans up its slot and hangs up if needed.
        let ctrl_weak_del = Rc::downgrade(self);
        let managed_weak_del = Rc::downgrade(&managed);
        window.connect_delete_event(move |_, _| {
            if let Some(ctrl) = ctrl_weak_del.upgrade() {
                let suppressed = managed_weak_del
                    .upgrade()
                    .map(|m| m.suppress_delete_action.get())
                    .unwrap_or(false);
                match kind {
                    ManagedWindowKind::Call => {
                        let _ = ctrl.call_win.borrow_mut().take();
                        if !suppressed {
                            ctrl.emit_action("end", Value::Null);
                        }
                    }
                    ManagedWindowKind::Incoming => {
                        let _ = ctrl.incoming_win.borrow_mut().take();
                        if !suppressed {
                            ctrl.emit_action("decline", Value::Null);
                        }
                    }
                    ManagedWindowKind::Devices => {
                        let _ = ctrl.devices_win.borrow_mut().take();
                    }
                }
            }
            glib::Propagation::Proceed
        });

        webview.load_uri(url);
        *slot.borrow_mut() = Some(Rc::clone(&managed));
        managed
    }

    fn on_script_action(
        self: &Rc<Self>,
        kind: ManagedWindowKind,
        win_opt: Option<Rc<ManagedCallWindow>>,
        action: &str,
        value: Value,
    ) {
        match action {
            "windowDrag" => {
                if let Some(mw) = win_opt {
                    let cmd = crate::client_host::parse_win_drag_command(&value).unwrap_or(
                        crate::client_host::WinDragCommand::Move {
                            button: 1,
                            screen_x: 0,
                            screen_y: 0,
                        },
                    );
                    crate::client_host::execute_win_drag_command(
                        &mw.window,
                        Some(mw.last_press.as_ref()),
                        cmd,
                    );
                }
            }
            "win" => {
                if let Some(mw) = win_opt {
                    match value.as_str().unwrap_or("") {
                        "minimize" => mw.window.iconify(),
                        "maximize" => {
                            if mw.window.is_maximized() {
                                mw.window.unmaximize();
                            } else {
                                mw.window.maximize();
                            }
                        }
                        "close" => {
                            if kind == ManagedWindowKind::Call {
                                self.emit_action("end", Value::Null);
                            }
                            mw.destroy_quietly();
                        }
                        _ => {}
                    }
                }
            }
            "devwin" => match value.as_str().unwrap_or("") {
                "minimize" => {
                    if let Some(ref dw) = *self.devices_win.borrow() {
                        dw.window.iconify();
                    }
                }
                "maximize" => {
                    if let Some(ref dw) = *self.devices_win.borrow() {
                        if dw.window.is_maximized() {
                            dw.window.unmaximize();
                        } else {
                            dw.window.maximize();
                        }
                    }
                }
                "close" => {
                    if let Some(dw) = self.devices_win.borrow_mut().take() {
                        dw.destroy_quietly();
                    }
                }
                _ => {}
            },
            "openDevices" => {
                self.handle_event(CallUiEvent::OpenDevices);
                self.emit_action("openDevices", Value::Null);
            }
            "openRecordingFolder" => {
                open_recording_parent_folder(&value);
            }
            other => {
                self.emit_action(other, value);
            }
        }
    }

    fn emit_action(&self, action: &str, value: Value) {
        let handler_opt = self.action_handler.borrow().clone();
        if let Some(h) = handler_opt {
            h(action, value);
        }
    }
}

fn open_recording_parent_folder(value: &Value) {
    let raw_path = value
        .as_str()
        .or_else(|| value.get("path").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let target_dir = match raw_path {
        Some(raw) => {
            let p = Path::new(raw);
            if p.is_dir() {
                p.to_path_buf()
            } else {
                p.parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(default_recordings_dir)
            }
        }
        None => default_recordings_dir(),
    };

    if !target_dir.exists() {
        return;
    }

    let _ = Command::new("xdg-open")
        .arg(&target_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn dispatch_to_window(win: &ManagedCallWindow, channel: &str, payload: &Value) {
    let ch_json = serde_json::to_string(channel).unwrap_or_else(|_| "\"\"".to_string());
    let val_json = serde_json::to_string(payload).unwrap_or_else(|_| "null".to_string());
    let js = format!(
        "window.zcallUI && window.zcallUI.__dispatch && window.zcallUI.__dispatch({ch_json}, {val_json});"
    );
    win.eval_or_queue(js);
}

fn build_call_ui_web_context(optional_zalo_app_dir: Option<PathBuf>) -> WebContext {
    let context = WebContext::new();
    if let Some(sec) = context.security_manager() {
        sec.register_uri_scheme_as_secure(CALL_UI_SCHEME);
        sec.register_uri_scheme_as_cors_enabled(CALL_UI_SCHEME);
        sec.register_uri_scheme_as_secure("zalo-app");
        sec.register_uri_scheme_as_cors_enabled("zalo-app");
    }

    context.register_uri_scheme(CALL_UI_SCHEME, |request| {
        let raw_uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
        let path = Url::parse(&raw_uri)
            .ok()
            .map(|u| u.path().to_string())
            .or_else(|| request.path().map(|p| p.to_string()))
            .unwrap_or_default();

        match get_ui_asset(&path) {
            Some((mime, bytes)) => finish_uri_response(request, 200, "OK", mime, bytes.to_vec()),
            None => finish_uri_response(
                request,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            ),
        }
    });

    if let Some(app_root) = optional_zalo_app_dir {
        context.register_uri_scheme("zalo-app", move |request| {
            let raw_uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
            let rel_path = Url::parse(&raw_uri)
                .ok()
                .map(|u| u.path().trim_start_matches('/').to_string())
                .unwrap_or_default();
            let candidate = app_root.join(&rel_path);
            if let (Ok(canon_root), Ok(canon_file)) =
                (app_root.canonicalize(), candidate.canonicalize())
            {
                if canon_file.starts_with(&canon_root) && canon_file.is_file() {
                    if let Ok(bytes) = std::fs::read(&canon_file) {
                        let mime = mime_guess::from_path(&canon_file)
                            .first_raw()
                            .unwrap_or("application/octet-stream");
                        finish_uri_response(request, 200, "OK", mime, bytes);
                        return;
                    }
                }
            }
            finish_uri_response(
                request,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            );
        });
    }

    context
}

fn finish_uri_response(
    request: &URISchemeRequest,
    status: u32,
    reason: &str,
    content_type: &str,
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
    response.set_http_headers(headers);

    request.finish_with_response(&response);
}
