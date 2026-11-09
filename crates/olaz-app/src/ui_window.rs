//! Native GTK3 + WebKit2GTK UI window (`OlazAppWindow`), `olaz-ui://` embedded asset scheme,
//! `olaz-media://` HTTP 206 byte-range media scheme, `olaz_ui` bidirectional command bridge,
//! Omarchy live theme watcher, `StatusNotifierItem` tray integration, and `ZaloEngineState` -> UI store serializer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use base64::Engine as _;
use gtk::prelude::*;
use javascriptcore::ValueExt as JscValueExt;
use olaz_core::{AsarArchive, InstallProgress, ProfileRegistry, ZaloPlatform, ZaloRuntimeManager};
use olaz_linux::api_mcp::MultiAccountControlBackend;
use olaz_linux::{
    capture_interactive_screenshot, has_gstreamer_audio_sink, load_omarchy_theme, pick_open_dialog,
    pick_open_files, spawn_local_api_server, spawn_omarchy_watcher, spawn_tray,
    stage_screenshot_file, unix_now_ms, CallRecordingSettings, ComposerDraft, Conversation,
    DesktopTheme, EnginePhase, FileCard, IncomingVoipCall, InstanceEvent, LocalApiServerHandle,
    Mention, NotificationPolicy, OlazControlBackend, OlazTrayEvent, OlazTrayHandle,
    OmarchyWatcherHandle, ProfileInstanceEvent, WaitingConfirm, ZaloEngineState,
    SCREENSHOT_STAGING_DIR,
};
use olaz_net::{serve_local_media_range, MediaLoader, MediaRangeResponse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use soup::{MessageHeaders, MessageHeadersType};
use tokio::sync::oneshot;
use url::Url;
use webkit2gtk::{
    HardwareAccelerationPolicy, LoadEvent, NavigationPolicyDecision, NavigationPolicyDecisionExt,
    NetworkProxyMode, NetworkProxySettings, PolicyDecisionExt, PolicyDecisionType,
    SecurityManagerExt, Settings as WebKitSettings, SettingsExt, URIRequestExt, URISchemeRequest,
    URISchemeRequestExt, URISchemeResponse, URISchemeResponseExt, UserContentInjectedFrames,
    UserContentManager, UserContentManagerExt, UserScript, UserScriptInjectionTime, WebContext,
    WebContextExt, WebView, WebViewExt, WebsiteDataManager, WebsiteDataManagerExt,
};

use crate::account_session::{
    privacy_config_from_tray, resolve_instance_root_dir, save_legacy_tray_privacy,
    tray_privacy_from_config, AccountOutboundCall, AccountOutboundSend, AccountPrivacyEvent,
    AccountSession, WindowPageMode,
};
use crate::client_host::{HostPage, OfficialClientHost, PageEvent};
use crate::main_process::{MainProcessEvent, NotificationContext, WindowCommand, ZaloMainProcess};

/// Custom URI scheme serving the embedded Liquid Glass frontend (`crates/olaz-app/ui/*`).
pub const OLAZ_UI_SCHEME: &str = "olaz-ui";

/// Custom URI scheme serving cached Zalo CDN media (`olaz-media://cdn?url=...`) and
/// sandbox-confined local/E2EE media (`olaz-media://file?path=...`) with HTTP `206 Partial Content`.
pub const OLAZ_MEDIA_SCHEME: &str = "olaz-media";

const ASSET_INDEX_HTML: &[u8] = include_bytes!("../ui/index.html");
const ASSET_STYLES_CSS: &[u8] = include_bytes!("../ui/styles.css");
const ASSET_APP_JS: &[u8] = include_bytes!("../ui/app.js");
const ASSET_ICONS_JS: &[u8] = include_bytes!("../ui/icons.js");
const ASSET_COMPONENTS_JS: &[u8] = include_bytes!("../ui/components.js");
const ASSET_DEMO_JS: &[u8] = include_bytes!("../ui/demo.js");

/// Preamble injected into the UI `WebView` at `UserScriptInjectionTime::Start` so
/// `window.webkit.messageHandlers.olaz_ui.postMessage(...)` is available before ES modules execute.
const OLAZ_UI_BRIDGE_PREAMBLE: &str = r#"(() => {
  'use strict';
  if (window.__olazUiBridgeInstalled) return;
  window.__olazUiBridgeInstalled = true;

  const w = window.webkit;
  const mh = w && w.messageHandlers;
  if (!w || !mh) return;
  Object.defineProperty(window, '__olazUiKeepAlive', {
    value: [w, mh],
    configurable: false,
    enumerable: false,
    writable: false,
  });

  const raw = mh.__olaz_ui_raw;
  mh.olaz_ui = {
    postMessage(payload) {
      if (raw && typeof raw.postMessage === 'function') {
        const p = raw.postMessage(
          typeof payload === 'string' ? payload : JSON.stringify(payload)
        );
        if (p && typeof p.catch === 'function') p.catch(() => {});
      }
    },
  };

  const forwardUiError = (message, source, stack) => {
    try {
      mh.olaz_ui.postMessage({
        id: '__err',
        cmd: '__ui_log_error',
        args: {
          message: String(message || 'Unknown UI JS error'),
          source: String(source || ''),
          stack: String(stack || ''),
        },
      });
    } catch {}
  };

  window.addEventListener('error', (ev) => {
    const err = ev && ev.error;
    const msg = (ev && ev.message) || (err && err.message) || 'Uncaught UI error';
    const src =
      ev && ev.filename ? `${ev.filename}:${ev.lineno || 0}:${ev.colno || 0}` : '';
    const stk = err && err.stack ? err.stack : '';
    forwardUiError(msg, src, stk);
  });

  window.addEventListener('unhandledrejection', (ev) => {
    const reason = ev ? ev.reason : undefined;
    const msg =
      reason instanceof Error
        ? reason.message
        : typeof reason === 'string'
          ? reason
          : (() => {
              try {
                return JSON.stringify(reason);
              } catch {
                return String(reason);
              }
            })();
    const stk = reason instanceof Error && reason.stack ? reason.stack : '';
    forwardUiError(msg || 'Unhandled Promise rejection', 'unhandledrejection', stk);
  });
})();
//# sourceURL=olaz-ui-preamble.js"#;

/// User-configurable Olaz preferences persisted in `<user_data>/olaz-ui-settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSettings {
    pub theme: String,
    pub language: String,
    pub notifications: bool,
    pub sound: bool,
    pub sound_volume: u32,
    pub enter_to_send: bool,
    pub sticker_suggest: bool,
    pub auto_download_mb: u32,
    pub omarchy_sync: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            language: "vi".to_string(),
            notifications: true,
            sound: true,
            sound_volume: 80,
            enter_to_send: true,
            sticker_suggest: true,
            auto_download_mb: 25,
            omarchy_sync: true,
        }
    }
}

impl UiSettings {
    fn load(path: &Path, default_lang: &str) -> Self {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(parsed) = serde_json::from_slice::<Self>(&bytes) {
                return parsed;
            }
        }
        Self {
            language: default_lang.to_string(),
            ..Self::default()
        }
    }

    fn save(&self, path: &Path) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }

    fn apply_patch(&mut self, args: &Value) {
        let Some(obj) = args.as_object() else {
            return;
        };
        if let Some(v) = obj.get("theme").and_then(Value::as_str) {
            self.theme = v.to_string();
        }
        if let Some(v) = obj.get("language").and_then(Value::as_str) {
            self.language = v.to_string();
        }
        if let Some(v) = obj.get("notifications").and_then(Value::as_bool) {
            self.notifications = v;
        }
        if let Some(v) = obj.get("sound").and_then(Value::as_bool) {
            self.sound = v;
        }
        if let Some(v) = obj.get("soundVolume").and_then(Value::as_u64) {
            self.sound_volume = (v.min(100)) as u32;
        }
        if let Some(v) = obj.get("enterToSend").and_then(Value::as_bool) {
            self.enter_to_send = v;
        }
        if let Some(v) = obj.get("stickerSuggest").and_then(Value::as_bool) {
            self.sticker_suggest = v;
        }
        if let Some(v) = obj.get("autoDownloadMb").and_then(Value::as_u64) {
            self.auto_download_mb = (v.min(2048)) as u32;
        }
        if let Some(v) = obj.get("omarchySync").and_then(Value::as_bool) {
            self.omarchy_sync = v;
        }
    }
}

/// Live runtime installation progress tracked for the `'preparing'` lifecycle screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiRuntimeProgress {
    pub step: String,
    pub received: u64,
    pub total: u64,
    pub failure: Option<String>,
}

impl UiRuntimeProgress {
    fn from_install_progress(p: InstallProgress) -> Self {
        match p {
            InstallProgress::Looking => Self {
                step: "looking".to_string(),
                received: 0,
                total: 0,
                failure: None,
            },
            InstallProgress::Downloading { received, total } => Self {
                step: "downloading".to_string(),
                received,
                total: total.unwrap_or(0),
                failure: None,
            },
            InstallProgress::Verifying => Self {
                step: "verifying".to_string(),
                received: 0,
                total: 0,
                failure: None,
            },
            InstallProgress::Extracting | InstallProgress::Installing | InstallProgress::Complete => {
                Self {
                    step: "installing".to_string(),
                    received: 0,
                    total: 0,
                    failure: None,
                }
            }
        }
    }
}

/// Minimum width/height allowed when persisting tiled/floating main-window geometry.
const MIN_MAIN_WINDOW_DIM: i32 = 420;

/// Persisted main-window geometry stored at `<user_data>/olaz-window-state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedWindowState {
    width: i32,
    height: i32,
    maximized: bool,
    #[serde(default)]
    pos_x: Option<i32>,
    #[serde(default)]
    pos_y: Option<i32>,
}

impl Default for SavedWindowState {
    fn default() -> Self {
        Self {
            width: 1180,
            height: 760,
            maximized: false,
            pos_x: None,
            pos_y: None,
        }
    }
}

impl SavedWindowState {
    fn load(path: &Path) -> Self {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(mut parsed) = serde_json::from_slice::<Self>(&bytes) {
                parsed.width = parsed.width.max(MIN_MAIN_WINDOW_DIM);
                parsed.height = parsed.height.max(MIN_MAIN_WINDOW_DIM);
                return parsed;
            }
        }
        Self::default()
    }

    fn save(&self, path: &Path) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

const ACCOUNT_RAIL_CSS: &str = "\
.olaz-account-rail {\
  background-color: #0068ff;\
  border-right: 1px solid rgba(0, 0, 0, 0.08);\
}\
.olaz-rail-indicator {\
  background-color: transparent;\
  border-radius: 0 3px 3px 0;\
  min-width: 3px;\
  min-height: 20px;\
}\
.olaz-rail-indicator-active {\
  background-color: #ffffff;\
  min-height: 28px;\
}\
.olaz-rail-btn {\
  background-image: none;\
  background-color: rgba(255, 255, 255, 0.16);\
  color: #ffffff;\
  border: 1.5px solid rgba(255, 255, 255, 0.72);\
  border-radius: 22px;\
  padding: 0;\
  min-width: 44px;\
  min-height: 44px;\
  font-weight: 700;\
  font-size: 13px;\
}\
.olaz-rail-btn:hover {\
  background-color: rgba(255, 255, 255, 0.28);\
  border-color: #ffffff;\
}\
.olaz-rail-btn-active {\
  background-color: #0043a8;\
  color: #ffffff;\
  border-color: #ffffff;\
  border-radius: 12px;\
}\
.olaz-rail-badge {\
  background-color: #ef4444;\
  color: #ffffff;\
  border-radius: 9px;\
  padding: 1px 5px;\
  font-size: 10px;\
  font-weight: 700;\
  min-width: 16px;\
  min-height: 16px;\
}\
.olaz-rail-add-btn {\
  background-image: none;\
  background-color: transparent;\
  color: #ffffff;\
  border: none;\
  border-radius: 6px;\
  padding: 0;\
  min-width: 48px;\
  min-height: 48px;\
  font-size: 22px;\
  font-weight: 500;\
}\
.olaz-rail-add-btn:hover {\
  background-color: rgba(255, 255, 255, 0.16);\
  color: #ffffff;\
}\
.olaz-account-page {\
  background-color: #eef3fa;\
}\
.olaz-close-login-tab-btn {\
  background-image: none;\
  background-color: #ffffff;\
  color: #081b3a;\
  border: 1px solid rgba(8, 27, 58, 0.16);\
  border-radius: 8px;\
  padding: 6px 12px;\
  font-size: 12px;\
  font-weight: 600;\
}\
.olaz-close-login-tab-btn:hover {\
  background-color: #dc2626;\
  color: #ffffff;\
  border-color: #dc2626;\
}\
.olaz-workspace-toast {\
  background-color: rgba(15, 23, 42, 0.94);\
  color: #f8fafc;\
  border: 1px solid rgba(255, 255, 255, 0.18);\
  border-radius: 10px;\
  padding: 8px 16px;\
  font-size: 13px;\
  font-weight: 600;\
}";

fn install_account_rail_css() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let provider = gtk::CssProvider::new();
        if let Err(err) = provider.load_from_data(ACCOUNT_RAIL_CSS.as_bytes()) {
            tracing::warn!(%err, "failed to load AccountRail GTK CSS");
            return;
        }
        if let Some(screen) = gdk::Screen::default() {
            gtk::StyleContext::add_provider_for_screen(
                &screen,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
}

struct AccountRailWidgets {
    row: gtk::Box,
    indicator: gtk::Box,
    button: gtk::Button,
    initials_label: gtk::Label,
    badge_label: gtk::Label,
}

/// Primary GTK3 application window hosting one or more isolated Zalo account sessions inside an
/// instant-switching `gtk::Stack` with an auto-showing `AccountRail` sidebar, falling back to the
/// `olaz-ui://` setup view during first-run DMG installation or `--demo`.
pub struct OlazAppWindow {
    window: gtk::Window,
    #[allow(dead_code)]
    root_overlay: gtk::Overlay,
    toast_label: gtk::Label,
    toast_source: RefCell<Option<glib::SourceId>>,
    stack: gtk::Stack,
    workspace_hbox: gtk::Box,
    account_rail: gtk::Box,
    rail_accounts_box: gtk::Box,
    rail_widgets: RefCell<HashMap<String, AccountRailWidgets>>,
    accounts_stack: gtk::Stack,
    ui_webview: WebView,

    instance_root_dir: PathBuf,
    profile_registry: RefCell<ProfileRegistry>,
    sessions: RefCell<Vec<Rc<AccountSession>>>,
    active_slug: RefCell<String>,

    primary_platform: Arc<ZaloPlatform>,
    runtime_mgr: ZaloRuntimeManager,
    shared_asar: RefCell<Option<Arc<AsarArchive>>>,
    tokio_handle: tokio::runtime::Handle,
    demo_mode: bool,

    runtime_progress: RefCell<Option<UiRuntimeProgress>>,
    installing_runtime: Cell<bool>,
    last_delete_undo_id: RefCell<Option<String>>,
    settings: RefCell<UiSettings>,
    settings_path: PathBuf,
    saved_win_state: RefCell<SavedWindowState>,
    win_state_path: PathBuf,
    win_state_save_source: RefCell<Option<glib::SourceId>>,
    window_size_restored: Cell<bool>,
    in_login_mode: Cell<bool>,
    desktop_theme: RefCell<DesktopTheme>,
    ui_loaded: Cell<bool>,
    ui_ready: Cell<bool>,
    ui_crash_timestamps: RefCell<Vec<Instant>>,
    suppress_renderer_auto_show: Cell<bool>,
    quitting: Cell<bool>,

    outbound_send_tx: tokio::sync::mpsc::UnboundedSender<AccountOutboundSend>,
    outbound_call_tx: tokio::sync::mpsc::UnboundedSender<AccountOutboundCall>,
    privacy_obs_tx: tokio::sync::mpsc::UnboundedSender<AccountPrivacyEvent>,
    multi_control_backend: Arc<MultiAccountControlBackend>,
    control_backend: Arc<dyn OlazControlBackend>,
    _api_server: RefCell<Option<LocalApiServerHandle>>,

    tray_handle: RefCell<Option<OlazTrayHandle>>,
    _omarchy_watcher: OmarchyWatcherHandle,
}

impl OlazAppWindow {
    const UI_CRASH_WINDOW: Duration = Duration::from_secs(15);
    const MAX_UI_CRASH_RELOADS: usize = 3;
    const QR_EXPIRY_SECS: u32 = 100;

    /// Returns the shared control-plane backend (`EngineControlBackend`) for local REST, MCP, and Unix-socket RPC.
    pub fn control_backend(&self) -> Arc<dyn OlazControlBackend> {
        Arc::clone(&self.control_backend)
    }

    /// Constructs and presents the native Olaz GTK3 window, starts the Omarchy watcher and tray,
    /// and boots (or installs) the official Zalo runtime (unless `demo_mode` is `true`).
    #[allow(dead_code)]
    pub fn launch(
        platform: Arc<ZaloPlatform>,
        runtime_mgr: ZaloRuntimeManager,
        host: Rc<OfficialClientHost>,
        main_process: Rc<ZaloMainProcess>,
        tokio_handle: tokio::runtime::Handle,
        demo_mode: bool,
    ) -> Rc<Self> {
        Self::launch_with_profile_options(
            platform,
            runtime_mgr,
            host,
            main_process,
            tokio_handle,
            demo_mode,
            false,
            None,
            None,
        )
    }

    /// Constructs the native Olaz GTK3 window with optional `--minimized` start-in-tray lifecycle
    /// and single-instance re-activation channel.
    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn launch_with_options(
        platform: Arc<ZaloPlatform>,
        runtime_mgr: ZaloRuntimeManager,
        host: Rc<OfficialClientHost>,
        main_process: Rc<ZaloMainProcess>,
        tokio_handle: tokio::runtime::Handle,
        demo_mode: bool,
        start_minimized: bool,
        instance_rx: Option<tokio::sync::mpsc::UnboundedReceiver<InstanceEvent>>,
    ) -> Rc<Self> {
        let mapped_rx = instance_rx.map(|mut rx| {
            let (tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio_handle.spawn(async move {
                while let Some(ev) = rx.recv().await {
                    let mapped = match ev {
                        InstanceEvent::PresentWindow => ProfileInstanceEvent::PresentWindow,
                        InstanceEvent::BackgroundPing => ProfileInstanceEvent::BackgroundPing,
                    };
                    if tx.send(mapped).is_err() {
                        break;
                    }
                }
            });
            out_rx
        });
        Self::launch_with_profile_options(
            platform,
            runtime_mgr,
            host,
            main_process,
            tokio_handle,
            demo_mode,
            start_minimized,
            mapped_rx,
            None,
        )
    }

    /// Constructs the native Olaz GTK3 window with profile-aware activation/deep-link events
    /// and optional loopback HTTP REST/MCP server (`--api-port`).
    #[allow(clippy::too_many_arguments)]
    pub fn launch_with_profile_options(
        platform: Arc<ZaloPlatform>,
        runtime_mgr: ZaloRuntimeManager,
        host: Rc<OfficialClientHost>,
        main_process: Rc<ZaloMainProcess>,
        tokio_handle: tokio::runtime::Handle,
        demo_mode: bool,
        start_minimized: bool,
        instance_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ProfileInstanceEvent>>,
        api_port: Option<u16>,
    ) -> Rc<Self> {
        gtk::Window::set_default_icon_name("olaz");
        install_account_rail_css();

        let hide_on_startup = start_minimized && !demo_mode;
        main_process
            .window_state()
            .is_visible
            .store(!hide_on_startup, Ordering::Relaxed);

        let initial_theme = load_omarchy_theme();
        let settings_path = platform.user_data().join("olaz-ui-settings.json");
        let settings = UiSettings::load(&settings_path, &initial_theme.language);
        let win_state_path = platform.user_data().join("olaz-window-state.json");
        let saved_win_state = SavedWindowState::load(&win_state_path);

        let instance_root_dir = resolve_instance_root_dir(&platform);
        let mut initial_registry = ProfileRegistry::load_or_discover(&instance_root_dir);
        let primary_slug = platform.profile_slug().to_string();
        if let Ok(meta) = initial_registry.ensure_profile(&primary_slug) {
            if meta.proxy_url.is_none() {
                meta.proxy_url = platform.proxy_url().map(str::to_owned);
            }
        }
        if primary_slug != "default" {
            initial_registry.active_slug = primary_slug.clone();
        }
        if !demo_mode {
            let _ = initial_registry.save_to(&instance_root_dir);
        }

        let (outbound_send_tx, mut outbound_send_rx) =
            tokio::sync::mpsc::unbounded_channel::<AccountOutboundSend>();
        let (outbound_call_tx, mut outbound_call_rx) =
            tokio::sync::mpsc::unbounded_channel::<AccountOutboundCall>();
        let (privacy_obs_tx, mut privacy_obs_rx) =
            tokio::sync::mpsc::unbounded_channel::<AccountPrivacyEvent>();

        let primary_session = AccountSession::from_existing(
            Arc::clone(&platform),
            host,
            Rc::clone(&main_process),
            &tokio_handle,
            demo_mode,
            outbound_send_tx.clone(),
            outbound_call_tx.clone(),
            privacy_obs_tx.clone(),
        );

        let multi_control_backend = Arc::new(MultiAccountControlBackend::new(&primary_slug));
        let control_backend: Arc<dyn OlazControlBackend> =
            Arc::clone(&multi_control_backend) as Arc<dyn OlazControlBackend>;

        let ui_context = build_ui_web_context(
            &platform,
            main_process.media_loader().clone(),
            tokio_handle.clone(),
        );

        let ucm = UserContentManager::new();
        ucm.register_script_message_handler("__olaz_ui_raw");
        ucm.add_script(&UserScript::new(
            OLAZ_UI_BRIDGE_PREAMBLE,
            UserContentInjectedFrames::TopFrame,
            UserScriptInjectionTime::Start,
            &[],
            &[],
        ));

        let wk_settings = WebKitSettings::new();
        wk_settings.set_enable_developer_extras(true);
        wk_settings.set_enable_write_console_messages_to_stdout(false);
        wk_settings.set_javascript_can_open_windows_automatically(false);
        wk_settings.set_enable_back_forward_navigation_gestures(false);
        if std::env::var_os("OLAZ_ENABLE_WEBKIT_HWACCEL").is_none() {
            wk_settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
        }

        let ui_webview = WebView::builder()
            .web_context(&ui_context)
            .user_content_manager(&ucm)
            .settings(&wk_settings)
            .build();
        ui_webview.set_hexpand(true);
        ui_webview.set_vexpand(true);

        configure_ui_navigation_policy(&ui_webview);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.set_hexpand(true);
        stack.set_vexpand(true);

        let workspace_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        workspace_hbox.set_hexpand(true);
        workspace_hbox.set_vexpand(true);

        let account_rail = gtk::Box::new(gtk::Orientation::Vertical, 0);
        account_rail.set_width_request(64);
        account_rail.set_vexpand(true);
        account_rail.style_context().add_class("olaz-account-rail");
        account_rail.set_no_show_all(true);
        account_rail.hide();

        let rail_accounts_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        rail_accounts_box.set_margin_top(10);
        rail_accounts_box.set_margin_bottom(8);
        rail_accounts_box.show();

        let rail_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rail_spacer.set_vexpand(true);
        rail_spacer.show();

        let add_btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        add_btn_row.set_halign(gtk::Align::Center);
        add_btn_row.set_margin_bottom(12);
        let add_account_btn = gtk::Button::with_label("+");
        add_account_btn
            .style_context()
            .add_class("olaz-rail-add-btn");
        add_account_btn.set_tooltip_text(Some("Thêm tài khoản Zalo (Ctrl+Shift+N)"));
        add_account_btn.show();
        add_btn_row.pack_start(&add_account_btn, false, false, 0);
        add_btn_row.show();

        account_rail.pack_start(&rail_accounts_box, false, false, 0);
        account_rail.pack_start(&rail_spacer, true, true, 0);
        account_rail.pack_end(&add_btn_row, false, false, 0);

        let accounts_stack = gtk::Stack::new();
        accounts_stack.set_transition_type(gtk::StackTransitionType::None);
        accounts_stack.set_hexpand(true);
        accounts_stack.set_vexpand(true);

        workspace_hbox.pack_start(&account_rail, false, false, 0);
        workspace_hbox.pack_start(&accounts_stack, true, true, 0);

        stack.add_named(&ui_webview, "setup");
        stack.add_named(&workspace_hbox, "zalo");
        stack.set_visible_child_name("setup");

        let root_overlay = gtk::Overlay::new();
        root_overlay.set_hexpand(true);
        root_overlay.set_vexpand(true);
        root_overlay.add(&stack);

        let toast_label = gtk::Label::new(None);
        toast_label
            .style_context()
            .add_class("olaz-workspace-toast");
        toast_label.set_halign(gtk::Align::Center);
        toast_label.set_valign(gtk::Align::Start);
        toast_label.set_margin_top(18);
        toast_label.set_no_show_all(true);
        toast_label.hide();
        root_overlay.add_overlay(&toast_label);
        root_overlay.set_overlay_pass_through(&toast_label, true);

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        if demo_mode {
            window.set_title("KV Zalo");
            window.set_default_size(1180, 760);
            window.set_size_request(960, 620);
        } else {
            if primary_slug == "default" {
                window.set_title("Zalo");
            } else {
                window.set_title(&format!("Zalo ({primary_slug})"));
            }
            // Frameless window so Zalo's patched Windows-style `#titleBar` and login bar are the sole chrome!
            window.set_decorated(false);
            window.set_default_size(saved_win_state.width, saved_win_state.height);
            window.set_size_request(400, 560);
        }
        window.add(&root_overlay);

        // Spawn Omarchy live theme/language watcher bridging to the GTK main context.
        let (theme_tx, mut theme_rx) = tokio::sync::mpsc::unbounded_channel::<DesktopTheme>();
        let omarchy_watcher = spawn_omarchy_watcher(move |next_theme| {
            let _ = theme_tx.send(next_theme);
        });

        let app = Rc::new(Self {
            window: window.clone(),
            root_overlay,
            toast_label,
            toast_source: RefCell::new(None),
            stack,
            workspace_hbox,
            account_rail,
            rail_accounts_box,
            rail_widgets: RefCell::new(HashMap::new()),
            accounts_stack,
            ui_webview: ui_webview.clone(),
            instance_root_dir,
            profile_registry: RefCell::new(initial_registry),
            sessions: RefCell::new(Vec::new()),
            active_slug: RefCell::new(primary_slug),
            primary_platform: platform,
            runtime_mgr,
            shared_asar: RefCell::new(None),
            tokio_handle: tokio_handle.clone(),
            demo_mode,
            runtime_progress: RefCell::new(None),
            installing_runtime: Cell::new(false),
            last_delete_undo_id: RefCell::new(None),
            settings: RefCell::new(settings),
            settings_path,
            saved_win_state: RefCell::new(saved_win_state),
            win_state_path,
            win_state_save_source: RefCell::new(None),
            window_size_restored: Cell::new(false),
            in_login_mode: Cell::new(false),
            desktop_theme: RefCell::new(initial_theme),
            ui_loaded: Cell::new(false),
            ui_ready: Cell::new(false),
            ui_crash_timestamps: RefCell::new(Vec::new()),
            suppress_renderer_auto_show: Cell::new(hide_on_startup),
            quitting: Cell::new(false),
            outbound_send_tx,
            outbound_call_tx,
            privacy_obs_tx,
            multi_control_backend,
            control_backend: Arc::clone(&control_backend),
            _api_server: RefCell::new(None),
            tray_handle: RefCell::new(None),
            _omarchy_watcher: omarchy_watcher,
        });

        {
            let app_weak = Rc::downgrade(&app);
            add_account_btn.connect_clicked(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    app.add_new_account_tab(None);
                }
            });
        }

        app.register_session_in_ui(&primary_session);
        app.install_window_lifecycle();
        app.install_ui_rpc(&ucm);
        if !demo_mode {
            app.install_auto_relock_timer();
        }

        // Handle outbound message sends from Scheduler, Auto-Reply, REST `/api/v1/send`, and MCP `zalo_send_message`.
        {
            let app_weak = Rc::downgrade(&app);
            glib::MainContext::default().spawn_local(async move {
                while let Some(req) = outbound_send_rx.recv().await {
                    let Some(app) = app_weak.upgrade() else {
                        if let Some(tx) = req.reply_tx {
                            let _ = tx.send(Err(anyhow::anyhow!("Olaz window closed")));
                        }
                        break;
                    };
                    let res = app
                        .execute_control_send_for(&req.slug, &req.conv_id, &req.text)
                        .await;
                    if let Some(tx) = req.reply_tx {
                        let _ = tx.send(res);
                    }
                }
            });
        }

        // Handle outbound call requests from REST `/api/v1/call` and MCP `zalo_start_call`.
        {
            let app_weak = Rc::downgrade(&app);
            glib::MainContext::default().spawn_local(async move {
                while let Some(req) = outbound_call_rx.recv().await {
                    let Some(app) = app_weak.upgrade() else {
                        let _ = req.reply_tx.send(Err(anyhow::anyhow!("Olaz window closed")));
                        break;
                    };
                    let res = app
                        .execute_control_call_for(&req.slug, &req.target_id, req.video)
                        .await;
                    let _ = req.reply_tx.send(res);
                }
            });
        }

        // Synchronize privacy updates originating from REST `/api/v1/privacy` or MCP `zalo_set_privacy`
        // onto the target session's WebView, `olaz-privacy.json`, and (if active) system tray checkboxes.
        {
            let app_weak = Rc::downgrade(&app);
            glib::MainContext::default().spawn_local(async move {
                while let Some(ev) = privacy_obs_rx.recv().await {
                    let Some(app) = app_weak.upgrade() else {
                        break;
                    };
                    let Some(sess) = app.session_by_slug(&ev.slug) else {
                        continue;
                    };
                    let changed = {
                        let mut cur = sess.privacy.borrow_mut();
                        if *cur == ev.config {
                            false
                        } else {
                            *cur = ev.config;
                            true
                        }
                    };
                    if changed {
                        let tray_state = tray_privacy_from_config(ev.config);
                        save_legacy_tray_privacy(&tray_state, &sess.privacy_path);
                        sess.sync_privacy_to_webview();
                        if app.active_slug() == ev.slug {
                            if let Some(tray) = app.tray_handle.borrow().clone() {
                                app.tokio_handle.spawn(async move {
                                    tray.set_privacy_state(tray_state).await;
                                });
                            }
                        }
                    }
                }
            });
        }

        if let Some(port) = api_port.filter(|_| !demo_mode) {
            let app_weak = Rc::downgrade(&app);
            let backend_for_srv = Arc::clone(&control_backend);
            let (srv_tx, srv_rx) = oneshot::channel();
            tokio_handle.spawn(async move {
                let _ = srv_tx.send(spawn_local_api_server(backend_for_srv, port).await);
            });
            glib::MainContext::default().spawn_local(async move {
                match srv_rx.await {
                    Ok(Ok(handle)) => {
                        tracing::info!(
                            addr = %handle.local_addr(),
                            "Olaz local REST & HTTP-MCP server listening"
                        );
                        if let Some(app) = app_weak.upgrade() {
                            *app._api_server.borrow_mut() = Some(handle);
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::error!(port, %err, "failed to bind Olaz local REST API server");
                    }
                    Err(_) => {}
                }
            });
        }

        if let Some(mut rx) = instance_rx {
            let app_weak = Rc::downgrade(&app);
            glib::MainContext::default().spawn_local(async move {
                while let Some(ev) = rx.recv().await {
                    let Some(app) = app_weak.upgrade() else {
                        break;
                    };
                    match ev {
                        ProfileInstanceEvent::PresentWindow => {
                            tracing::info!(
                                "presenting primary window for secondary olaz invocation"
                            );
                            app.present_window();
                            app.active_session()
                                .host
                                .bridge()
                                .send("show-from-tray", vec![]);
                        }
                        ProfileInstanceEvent::BackgroundPing => {
                            tracing::debug!(
                                "ignoring duplicate --minimized background invocation"
                            );
                        }
                        ProfileInstanceEvent::OpenDeepLink(url) => {
                            tracing::info!(%url, "forwarding zalo:// deep link to primary window");
                            app.present_window();
                            app.active_session()
                                .host
                                .bridge()
                                .send("show-from-tray", vec![]);
                            app.handle_deep_link(&url);
                        }
                    }
                }
            });
        }
        app.spawn_omarchy_listener(&mut theme_rx);
        app.spawn_status_tray();

        if demo_mode {
            app.ensure_setup_ui_loaded();
        } else {
            app.bootstrap_runtime();
        }

        if app.suppress_renderer_auto_show.get() {
            app.stack.show_all();
            tracing::info!("started minimized to system tray (--minimized)");
        } else {
            window.show_all();
            app.refresh_workspace_chrome();
            window.present();
        }

        app
    }

    fn active_slug(&self) -> String {
        self.active_slug.borrow().clone()
    }

    fn session_count(&self) -> usize {
        self.sessions.borrow().len()
    }

    fn session_by_slug(&self, slug: &str) -> Option<Rc<AccountSession>> {
        self.sessions
            .borrow()
            .iter()
            .find(|s| s.slug == slug)
            .cloned()
    }

    fn active_session(&self) -> Rc<AccountSession> {
        let cur = self.active_slug.borrow().clone();
        self.session_by_slug(&cur).unwrap_or_else(|| {
            self.sessions
                .borrow()
                .first()
                .cloned()
                .expect("OlazAppWindow always keeps at least one AccountSession")
        })
    }

    fn account_effective_name(&self, slug: &str) -> String {
        self.profile_registry
            .borrow()
            .get(slug)
            .map(|m| m.effective_name())
            .unwrap_or_else(|| slug.to_string())
    }

    fn account_initials(&self, slug: &str) -> String {
        self.profile_registry
            .borrow()
            .get(slug)
            .map(|m| m.initials())
            .unwrap_or_else(|| "Z".to_string())
    }

    fn save_profile_registry(&self) {
        if self.demo_mode {
            return;
        }
        let _ = self.profile_registry.borrow().save_to(&self.instance_root_dir);
    }

    fn show_workspace_toast(self: &Rc<Self>, text: &str) {
        if let Some(prev) = self.toast_source.borrow_mut().take() {
            prev.remove();
        }
        self.toast_label.set_text(text);
        self.toast_label.show();
        let app_weak = Rc::downgrade(self);
        let source_id = glib::timeout_add_local_once(Duration::from_millis(3600), move || {
            if let Some(app) = app_weak.upgrade() {
                app.toast_source.borrow_mut().take();
                app.toast_label.hide();
            }
        });
        *self.toast_source.borrow_mut() = Some(source_id);
    }

    fn refresh_rail_item_visuals(&self, slug: &str) {
        let is_active = self.active_slug() == slug;
        let initials = self.account_initials(slug);
        let eff_name = self.account_effective_name(slug);
        let unread = self
            .session_by_slug(slug)
            .map(|s| s.unread_count.get())
            .unwrap_or(0);
        let idx_opt = self
            .sessions
            .borrow()
            .iter()
            .position(|s| s.slug == slug);

        let widgets_map = self.rail_widgets.borrow();
        let Some(w) = widgets_map.get(slug) else {
            return;
        };
        w.initials_label.set_text(&initials);
        let shortcut_hint = idx_opt
            .filter(|&i| i < 9)
            .map(|i| format!(" (Ctrl+{})", i + 1))
            .unwrap_or_default();
        if unread > 0 {
            w.button.set_tooltip_text(Some(&format!(
                "{eff_name} — {unread} tin chưa đọc{shortcut_hint}"
            )));
            let badge_txt = if unread > 99 {
                "99+".to_string()
            } else {
                unread.to_string()
            };
            w.badge_label.set_text(&badge_txt);
            w.badge_label.show();
        } else {
            w.button
                .set_tooltip_text(Some(&format!("{eff_name}{shortcut_hint}")));
            w.badge_label.hide();
        }

        let ind_ctx = w.indicator.style_context();
        let btn_ctx = w.button.style_context();
        if is_active {
            ind_ctx.add_class("olaz-rail-indicator-active");
            btn_ctx.add_class("olaz-rail-btn-active");
        } else {
            ind_ctx.remove_class("olaz-rail-indicator-active");
            btn_ctx.remove_class("olaz-rail-btn-active");
        }
    }

    fn sync_accounts_to_all_webviews(&self) {
        if self.demo_mode {
            return;
        }
        let sessions: Vec<Rc<AccountSession>> = self.sessions.borrow().clone();
        let active = self.active_slug();
        let reg = self.profile_registry.borrow();
        let accounts_arr: Vec<Value> = sessions
            .iter()
            .enumerate()
            .map(|(idx, sess)| {
                let meta = reg.get(&sess.slug);
                let logged_in = sess.main_process.has_session();
                let has_named_ident = meta
                    .and_then(|m| m.label.as_ref().or(m.display_name.as_ref()))
                    .is_some_and(|s| !s.trim().is_empty());
                let name = if !logged_in && !has_named_ident {
                    format!("Tài khoản mới ({})", idx + 1)
                } else {
                    meta.map(|m| m.effective_name())
                        .unwrap_or_else(|| sess.slug.clone())
                };
                let initials = if !logged_in && !has_named_ident {
                    "QR".to_string()
                } else {
                    meta.map(|m| m.initials())
                        .unwrap_or_else(|| "Z".to_string())
                };
                let avatar = sess
                    .engine_state
                    .borrow()
                    .user
                    .as_ref()
                    .and_then(|u| u.avatar.clone())
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| meta.and_then(|m| m.avatar_url.clone()));
                json!({
                    "slug": sess.slug,
                    "name": name,
                    "initials": initials,
                    "avatar": avatar,
                    "unread": sess.unread_count.get(),
                    "loggedIn": logged_in,
                    "shortcutIndex": idx + 1,
                })
            })
            .collect();
        drop(reg);

        for sess in &sessions {
            sess.sync_accounts_to_webview(&json!({
                "selfSlug": sess.slug,
                "activeSlug": active,
                "accounts": accounts_arr,
            }));
        }
    }

    fn refresh_workspace_chrome(&self) {
        let multi = self.session_count() >= 2;
        let active_sess = self.active_session();
        // On `index.html` (`WindowPageMode::Main`), Zalo's native 64px `#main-tab` sidebar hosts
        // `#olaz-sidebar-accounts` directly, so hide `account_rail` to prevent a double sidebar.
        // Show `account_rail` (styled identically at 64px `#0068ff`) when on `login.html` (`WindowPageMode::Login`)!
        if multi
            && !self.demo_mode
            && (!active_sess.host.is_direct_ui()
                || active_sess.page_mode.get() == WindowPageMode::Login)
        {
            self.account_rail.show();
        } else {
            self.account_rail.hide();
        }

        let sessions: Vec<Rc<AccountSession>> = self.sessions.borrow().clone();
        for sess in &sessions {
            sess.apply_layout_mode(multi);
            self.refresh_rail_item_visuals(&sess.slug);
        }
        self.sync_accounts_to_all_webviews();

        if !self.demo_mode {
            let active = self.active_slug();
            let eff_name = self.account_effective_name(&active);
            if !multi && active == "default" {
                self.window.set_title("Zalo");
            } else {
                self.window.set_title(&format!("Zalo — {eff_name}"));
            }
        }
    }

    fn register_session_in_ui(self: &Rc<Self>, session: &Rc<AccountSession>) {
        let slug = session.slug.clone();
        self.sessions.borrow_mut().push(Rc::clone(session));
        self.accounts_stack
            .add_named(&session.page_container, &slug);
        session.page_container.show_all();

        let label_for_mcp = self
            .profile_registry
            .borrow()
            .get(&slug)
            .and_then(|m| m.label.clone().or_else(|| m.display_name.clone()));
        self.multi_control_backend.register_account(
            slug.clone(),
            label_for_mcp,
            Arc::clone(&session.control_backend) as Arc<dyn OlazControlBackend>,
        );

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.set_margin_end(6);

        let indicator = gtk::Box::new(gtk::Orientation::Vertical, 0);
        indicator.set_valign(gtk::Align::Center);
        indicator.style_context().add_class("olaz-rail-indicator");
        indicator.show();

        let overlay = gtk::Overlay::new();
        overlay.set_halign(gtk::Align::Center);
        overlay.set_valign(gtk::Align::Center);

        let initials_label = gtk::Label::new(Some(&self.account_initials(&slug)));
        initials_label.show();

        let button = gtk::Button::new();
        button.style_context().add_class("olaz-rail-btn");
        button.add(&initials_label);
        button.show();

        let badge_label = gtk::Label::new(None);
        badge_label.style_context().add_class("olaz-rail-badge");
        badge_label.set_halign(gtk::Align::End);
        badge_label.set_valign(gtk::Align::Start);
        badge_label.set_no_show_all(true);
        badge_label.hide();

        overlay.add(&button);
        overlay.add_overlay(&badge_label);
        overlay.set_overlay_pass_through(&badge_label, true);
        overlay.show();

        row.pack_start(&indicator, false, false, 0);
        row.pack_start(&overlay, true, false, 0);
        row.show();
        self.rail_accounts_box.pack_start(&row, false, false, 0);

        {
            let app_weak = Rc::downgrade(self);
            let slug_click = slug.clone();
            button.connect_clicked(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    app.switch_to_account(&slug_click);
                }
            });
        }
        {
            let app_weak = Rc::downgrade(self);
            let slug_ctx = slug.clone();
            button.connect_button_press_event(move |_, ev| {
                if ev.button() == 3 {
                    if let Some(app) = app_weak.upgrade() {
                        app.show_account_context_menu(&slug_ctx, Some(ev));
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        {
            let app_weak = Rc::downgrade(self);
            let slug_close = slug.clone();
            session.connect_close_login_tab(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.remove_account_session(&slug_close);
                }
            });
        }

        self.rail_widgets.borrow_mut().insert(
            slug.clone(),
            AccountRailWidgets {
                row,
                indicator,
                button,
                initials_label,
                badge_label,
            },
        );

        if !self.demo_mode {
            self.install_session_window_commands(session);
            self.install_session_hooks(session);
        }
        self.refresh_rail_item_visuals(&slug);
    }

    fn show_account_context_menu(self: &Rc<Self>, slug: &str, ev: Option<&gdk::EventButton>) {
        let menu = gtk::Menu::new();

        let item_switch = gtk::MenuItem::with_label("Chuyển sang tài khoản này");
        {
            let app_weak = Rc::downgrade(self);
            let slug_s = slug.to_string();
            item_switch.connect_activate(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    app.switch_to_account(&slug_s);
                }
            });
        }
        menu.append(&item_switch);

        let item_rename = gtk::MenuItem::with_label("Đổi tên gợi nhớ...");
        {
            let app_weak = Rc::downgrade(self);
            let slug_r = slug.to_string();
            item_rename.connect_activate(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    app.prompt_rename_account(&slug_r);
                }
            });
        }
        menu.append(&item_rename);

        let item_reload = gtk::MenuItem::with_label("Tải lại (Reload)");
        {
            let app_weak = Rc::downgrade(self);
            let slug_rel = slug.to_string();
            item_reload.connect_activate(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    if let Some(sess) = app.session_by_slug(&slug_rel) {
                        let target = if sess.main_process.has_session() {
                            HostPage::Main
                        } else {
                            HostPage::Login
                        };
                        sess.host
                            .load(target, &sess.main_process.page_query(target == HostPage::Main));
                    }
                }
            });
        }
        menu.append(&item_reload);

        menu.append(&gtk::SeparatorMenuItem::new());

        let item_remove = gtk::MenuItem::with_label("Xóa tài khoản khỏi KV Zalo");
        {
            let app_weak = Rc::downgrade(self);
            let slug_rm = slug.to_string();
            item_remove.connect_activate(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    app.remove_account_session(&slug_rm);
                }
            });
        }
        menu.append(&item_remove);

        menu.show_all();
        menu.popup_at_pointer(ev.map(|v| &**v));
    }

    fn prompt_rename_account(self: &Rc<Self>, slug: &str) {
        let current = self
            .profile_registry
            .borrow()
            .get(slug)
            .and_then(|m| m.label.clone())
            .unwrap_or_default();

        let dialog = gtk::Dialog::with_buttons(
            Some("Đổi tên gợi nhớ tài khoản"),
            Some(&self.window),
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            &[
                ("Hủy", gtk::ResponseType::Cancel),
                ("Lưu", gtk::ResponseType::Accept),
            ],
        );
        dialog.set_default_response(gtk::ResponseType::Accept);
        dialog.set_default_size(360, 120);

        let content = dialog.content_area();
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(16);
        content.set_margin_end(16);
        content.set_spacing(8);

        let hint = gtk::Label::new(Some("Nhập tên gợi nhớ (để trống để dùng tên Zalo):"));
        hint.set_halign(gtk::Align::Start);
        let entry = gtk::Entry::new();
        entry.set_text(&current);
        entry.set_activates_default(true);
        content.pack_start(&hint, false, false, 0);
        content.pack_start(&entry, false, false, 0);
        content.show_all();

        if dialog.run() == gtk::ResponseType::Accept {
            let new_label = entry.text().to_string();
            let changed = self
                .profile_registry
                .borrow_mut()
                .set_label(slug, Some(new_label));
            if changed {
                self.save_profile_registry();
                let updated_lbl = self
                    .profile_registry
                    .borrow()
                    .get(slug)
                    .and_then(|m| m.label.clone().or_else(|| m.display_name.clone()));
                self.multi_control_backend
                    .set_account_label(slug, updated_lbl);
                self.refresh_workspace_chrome();
            }
        }
        unsafe {
            dialog.destroy();
        }
    }

    pub fn add_new_account_tab(self: &Rc<Self>, explicit_proxy: Option<String>) {
        if self.demo_mode || self.installing_runtime.get() {
            return;
        }
        let slug = {
            let mut reg = self.profile_registry.borrow_mut();
            let allocated = reg.allocate_new_slug();
            if let Some(meta) = reg.get_mut(&allocated) {
                meta.proxy_url = explicit_proxy.clone();
            }
            reg.active_slug = allocated.clone();
            allocated
        };
        self.save_profile_registry();

        let shared_asar = self.shared_asar.borrow().clone();
        let direct_ui = self.active_session().host.is_direct_ui();
        let session = match AccountSession::spawn_secondary(
            &slug,
            explicit_proxy,
            &self.primary_platform,
            &self.instance_root_dir,
            shared_asar,
            &self.tokio_handle,
            direct_ui,
            self.outbound_send_tx.clone(),
            self.outbound_call_tx.clone(),
            self.privacy_obs_tx.clone(),
        ) {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(%slug, %err, "failed to create secondary AccountSession");
                self.profile_registry.borrow_mut().remove_profile(&slug);
                self.save_profile_registry();
                return;
            }
        };

        self.register_session_in_ui(&session);
        self.start_account_session(&session);
        self.apply_main_window_size();
        self.refresh_workspace_chrome();
        self.switch_to_account(&slug);
        self.present_window();
    }

    pub fn switch_to_account(self: &Rc<Self>, slug: &str) {
        let Some(target) = self.session_by_slug(slug) else {
            return;
        };
        let prev_slug = self.active_slug();
        if prev_slug != slug {
            if let Some(prev) = self.session_by_slug(&prev_slug) {
                prev.host
                    .bridge()
                    .send("visibility-change", vec![json!("blur"), json!("1")]);
            }
            *self.active_slug.borrow_mut() = slug.to_string();
            {
                let mut reg = self.profile_registry.borrow_mut();
                reg.active_slug = slug.to_string();
            }
            self.save_profile_registry();
            self.multi_control_backend.set_active_slug(slug);
        }

        if !self.demo_mode && target.host.is_direct_ui() && !self.installing_runtime.get() {
            if self.window.is_visible() {
                target.mount_into_page();
            }
            self.accounts_stack.set_visible_child_name(slug);
            self.stack.set_visible_child_name("zalo");
        }

        self.refresh_workspace_chrome();

        if self.session_count() == 1 && !self.demo_mode && self.window.is_visible() {
            if target.page_mode.get() == WindowPageMode::Login {
                self.apply_login_window_size();
            } else {
                self.apply_main_window_size();
            }
        }

        if self.window.is_visible() {
            target.host.webview().grab_focus();
            target
                .host
                .bridge()
                .send("visibility-change", vec![json!("show")]);
            target
                .host
                .bridge()
                .send("visibility-change", vec![json!("focus"), json!("1")]);
        }

        let tray_privacy = tray_privacy_from_config(*target.privacy.borrow());
        if let Some(tray) = self.tray_handle.borrow().clone() {
            self.tokio_handle.spawn(async move {
                tray.set_privacy_state(tray_privacy).await;
            });
        }

        if !target.host.is_direct_ui() && !self.demo_mode {
            let snap = self.full_ui_snapshot();
            self.push_ui_state(&snap);
        }
    }

    fn cycle_next_account(self: &Rc<Self>, reverse: bool) {
        let slugs: Vec<String> = self
            .sessions
            .borrow()
            .iter()
            .map(|s| s.slug.clone())
            .collect();
        if slugs.len() < 2 {
            return;
        }
        let cur = self.active_slug();
        let idx = slugs.iter().position(|s| s == &cur).unwrap_or(0);
        let next_idx = if reverse {
            if idx == 0 {
                slugs.len() - 1
            } else {
                idx - 1
            }
        } else {
            (idx + 1) % slugs.len()
        };
        self.switch_to_account(&slugs[next_idx]);
    }

    fn remove_account_session(self: &Rc<Self>, slug: &str) {
        if self.session_count() <= 1 {
            // Sole account in window: log out cleanly rather than leaving zero sessions.
            if let Some(sole) = self.session_by_slug(slug) {
                if sole.main_process.has_session() {
                    sole.main_process.logout();
                }
                self.profile_registry.borrow_mut().remove_profile(slug);
                self.save_profile_registry();
                self.refresh_workspace_chrome();
            }
            return;
        }

        if slug == "default" {
            // Keep the primary `"default"` profile slot if user tries to delete it while other tabs exist:
            // log out `"default"` and switch to the next open session, or if already logged out, keep `"default"` only when needed.
            // Wait: allow removing secondary profiles cleanly; for `"default"`, log it out and switch to another tab!
            if let Some(def_sess) = self.session_by_slug("default") {
                if def_sess.main_process.has_session() {
                    def_sess.main_process.logout();
                }
                self.profile_registry.borrow_mut().remove_profile("default");
                self.save_profile_registry();
                if let Some(other) = self
                    .sessions
                    .borrow()
                    .iter()
                    .find(|s| s.slug != "default")
                    .map(|s| s.slug.clone())
                {
                    self.switch_to_account(&other);
                }
                self.refresh_workspace_chrome();
                self.refresh_total_unread_indicators();
                return;
            }
        }

        let fallback_slug = self
            .sessions
            .borrow()
            .iter()
            .find(|s| s.slug != slug)
            .map(|s| s.slug.clone())
            .unwrap_or_else(|| "default".to_string());

        if self.active_slug() == slug {
            self.switch_to_account(&fallback_slug);
        }

        let removed_opt = {
            let mut list = self.sessions.borrow_mut();
            list.iter()
                .position(|s| s.slug == slug)
                .map(|idx| list.remove(idx))
        };
        let Some(removed) = removed_opt else {
            return;
        };

        self.multi_control_backend.unregister_account(slug);
        self.profile_registry.borrow_mut().remove_profile(slug);
        self.save_profile_registry();

        let _ = removed.main_process.autostart().set_open_at_login(false);
        self.accounts_stack.remove(&removed.page_container);
        if let Some(w) = self.rail_widgets.borrow_mut().remove(slug) {
            self.rail_accounts_box.remove(&w.row);
        }

        let base_dir = removed.platform.base_dir().to_path_buf();
        let cfg_dir = removed.platform.config_dir().to_path_buf();
        let cache_dir = removed.platform.cache_dir().to_path_buf();
        removed.shutdown();

        if slug != "default" {
            let expected_profiles_root = self.instance_root_dir.join("profiles");
            if base_dir.starts_with(&expected_profiles_root) {
                let _ = std::fs::remove_dir_all(&base_dir);
            }
            if cfg_dir.ends_with(Path::new("profiles").join(slug)) {
                let _ = std::fs::remove_dir_all(&cfg_dir);
            }
            if cache_dir.ends_with(Path::new("profiles").join(slug)) {
                let _ = std::fs::remove_dir_all(&cache_dir);
            }
        }

        self.refresh_workspace_chrome();
        if self.session_count() == 1 && self.window.is_visible() {
            let active_sess = self.active_session();
            if active_sess.page_mode.get() == WindowPageMode::Login {
                self.apply_login_window_size();
            } else {
                self.apply_main_window_size();
            }
        }
        self.refresh_total_unread_indicators();
    }

    fn ensure_setup_ui_loaded(&self) {
        if !self.ui_loaded.replace(true) {
            self.ui_webview.load_uri(&self.ui_entry_uri());
        }
    }

    fn ui_entry_uri(&self) -> String {
        if self.demo_mode {
            format!("{OLAZ_UI_SCHEME}://app/index.html?demo=1")
        } else {
            format!("{OLAZ_UI_SCHEME}://app/index.html")
        }
    }

    fn install_window_lifecycle(self: &Rc<Self>) {
        // Close-to-tray when a StatusNotifierItem tray is active; quit cleanly otherwise.
        let app_weak = Rc::downgrade(self);
        self.window.connect_delete_event(move |_, _| {
            let Some(app) = app_weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            app.persist_window_geometry();
            let has_live_tray = app
                .tray_handle
                .borrow()
                .as_ref()
                .is_some_and(|h| !h.is_closed());
            if !app.quitting.get() && has_live_tray {
                app.hide_to_tray();
                return glib::Propagation::Stop;
            }
            app.shutdown_and_quit();
            glib::Propagation::Proceed
        });

        // Keyboard shortcuts: Ctrl+1..9 switch accounts, Ctrl+Tab / Ctrl+Shift+Tab cycle accounts,
        // and Ctrl+Shift+N opens a brand-new account tab in-window!
        let app_weak_keys = Rc::downgrade(self);
        self.window.connect_key_press_event(move |_, ev| {
            let Some(app) = app_weak_keys.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if app.demo_mode {
                return glib::Propagation::Proceed;
            }
            let state = ev.state();
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            let alt = state.contains(gdk::ModifierType::MOD1_MASK);
            if !ctrl || alt {
                return glib::Propagation::Proceed;
            }

            let keyval = ev.keyval();
            if shift && (keyval == gdk::keys::constants::N || keyval == gdk::keys::constants::n) {
                app.add_new_account_tab(None);
                return glib::Propagation::Stop;
            }

            if keyval == gdk::keys::constants::Tab || keyval == gdk::keys::constants::ISO_Left_Tab
            {
                if app.session_count() >= 2 {
                    let rev = shift || keyval == gdk::keys::constants::ISO_Left_Tab;
                    app.cycle_next_account(rev);
                    return glib::Propagation::Stop;
                }
            }

            if !shift {
                let digit_idx = match keyval {
                    gdk::keys::constants::_1 => Some(0usize),
                    gdk::keys::constants::_2 => Some(1usize),
                    gdk::keys::constants::_3 => Some(2usize),
                    gdk::keys::constants::_4 => Some(3usize),
                    gdk::keys::constants::_5 => Some(4usize),
                    gdk::keys::constants::_6 => Some(5usize),
                    gdk::keys::constants::_7 => Some(6usize),
                    gdk::keys::constants::_8 => Some(7usize),
                    gdk::keys::constants::_9 => Some(8usize),
                    _ => None,
                };
                if let Some(idx) = digit_idx {
                    let target_slug = app
                        .sessions
                        .borrow()
                        .get(idx)
                        .map(|s| s.slug.clone());
                    if let Some(slug) = target_slug {
                        app.switch_to_account(&slug);
                        return glib::Propagation::Stop;
                    }
                }
            }

            glib::Propagation::Proceed
        });

        // Track window state (`maximized`, `fullscreen`) and forward `visibility-change` to Zalo's `#titleBar`.
        let app_weak_state = Rc::downgrade(self);
        self.window.connect_window_state_event(move |_, ev| {
            let Some(app) = app_weak_state.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let ns = ev.new_window_state();
            let max_now = ns.contains(gdk::WindowState::MAXIMIZED);
            let fs_now = ns.contains(gdk::WindowState::FULLSCREEN);
            for sess in app.sessions.borrow().iter() {
                let ws = sess.main_process.window_state();
                ws.is_maximized.store(max_now, Ordering::Relaxed);
                ws.is_fullscreen.store(fs_now, Ordering::Relaxed);
            }

            if ev.changed_mask().contains(gdk::WindowState::MAXIMIZED) {
                if !app.in_login_mode.get() {
                    app.saved_win_state.borrow_mut().maximized = max_now;
                    app.persist_window_geometry();
                }
                let ev_name = if max_now { "maximize" } else { "unmaximize" };
                for sess in app.sessions.borrow().iter() {
                    sess.host
                        .bridge()
                        .send("visibility-change", vec![json!(ev_name)]);
                }
            }
            glib::Propagation::Proceed
        });

        let app_weak_focus_in = Rc::downgrade(self);
        self.window.connect_focus_in_event(move |win, _| {
            win.set_urgency_hint(false);
            if let Some(app) = app_weak_focus_in.upgrade() {
                app.active_session()
                    .host
                    .bridge()
                    .send("visibility-change", vec![json!("focus"), json!("1")]);
            }
            glib::Propagation::Proceed
        });

        let app_weak_focus_out = Rc::downgrade(self);
        self.window.connect_focus_out_event(move |_, _| {
            if let Some(app) = app_weak_focus_out.upgrade() {
                app.active_session()
                    .host
                    .bridge()
                    .send("visibility-change", vec![json!("blur"), json!("1")]);
            }
            glib::Propagation::Proceed
        });

        let app_weak_cfg = Rc::downgrade(self);
        self.window.connect_configure_event(move |win, ev| {
            if let Some(app) = app_weak_cfg.upgrade() {
                if !app.in_login_mode.get() && !win.is_maximized() {
                    let (w, h) = ev.size();
                    let min_dim = MIN_MAIN_WINDOW_DIM as u32;
                    if w >= min_dim && h >= min_dim {
                        let (wx, wy) = win.position();
                        let (x, y) = if wx != 0 || wy != 0 {
                            (wx, wy)
                        } else {
                            ev.position()
                        };
                        {
                            let mut st = app.saved_win_state.borrow_mut();
                            st.width = w as i32;
                            st.height = h as i32;
                            if x != 0 || y != 0 {
                                st.pos_x = Some(x);
                                st.pos_y = Some(y);
                            }
                        }
                        app.schedule_debounced_window_state_save();
                    }
                }
            }
            false
        });

        // Push theme + initial snapshot as soon as `olaz-ui://app/index.html` finishes loading.
        let app_weak_load = Rc::downgrade(self);
        self.ui_webview.connect_load_changed(move |_, event| {
            if event != LoadEvent::Finished {
                return;
            }
            let Some(app) = app_weak_load.upgrade() else {
                return;
            };
            app.ui_ready.set(true);
            let theme = app.desktop_theme.borrow().clone();
            app.apply_desktop_theme(&theme);
            if !app.demo_mode {
                let snap = app.full_ui_snapshot();
                app.push_ui_state(&snap);
            }
        });

        // Recover automatically (up to 3 reloads within 15s) if `ui_webview`'s WebProcess crashes.
        let app_weak_crash = Rc::downgrade(self);
        self.ui_webview
            .connect_web_process_terminated(move |wv, reason| {
                let Some(app) = app_weak_crash.upgrade() else {
                    return;
                };
                if app.quitting.get() {
                    return;
                }
                app.ui_ready.set(false);
                let now = Instant::now();
                let allow_reload = {
                    let mut times = app.ui_crash_timestamps.borrow_mut();
                    times.retain(|t| now.saturating_duration_since(*t) < Self::UI_CRASH_WINDOW);
                    if times.len() < Self::MAX_UI_CRASH_RELOADS {
                        times.push(now);
                        true
                    } else {
                        false
                    }
                };
                if allow_reload {
                    let uri = app.ui_entry_uri();
                    tracing::error!(?reason, %uri, "Olaz UI WebProcess terminated; reloading UI");
                    wv.load_uri(&uri);
                } else {
                    tracing::error!(
                        ?reason,
                        "Olaz UI WebProcess crashed >3 times within 15s; halting automatic UI reload"
                    );
                }
            });
    }

    fn install_session_window_commands(self: &Rc<Self>, session: &Rc<AccountSession>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WindowCommand>();
        session.main_process.window_state().set_command_sender(tx);
        let slug = session.slug.clone();

        let app_weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            while let Some(cmd) = rx.recv().await {
                let Some(app) = app_weak.upgrade() else {
                    break;
                };
                let Some(sess) = app.session_by_slug(&slug) else {
                    break;
                };
                match cmd {
                    WindowCommand::Minimize { window_id } => {
                        if let Some(ref wid) = window_id {
                            if sess.host.minimize_child_window(wid)
                                || matches!(wid.as_str(), "photo-viewer" | "file-viewer")
                            {
                                continue;
                            }
                        }
                        app.window.iconify();
                    }
                    WindowCommand::ToggleMaximize { window_id, target } => {
                        if let Some(ref wid) = window_id {
                            if sess.host.toggle_maximize_child_window(wid, target)
                                || matches!(wid.as_str(), "photo-viewer" | "file-viewer")
                            {
                                continue;
                            }
                        }
                        let should_max = target.unwrap_or_else(|| !app.window.is_maximized());
                        if should_max {
                            app.window.maximize();
                        } else {
                            app.window.unmaximize();
                        }
                    }
                    WindowCommand::ToggleFullscreen { window_id, target } => {
                        if let Some(ref wid) = window_id {
                            if sess.host.toggle_fullscreen_child_window(wid, target)
                                || matches!(wid.as_str(), "photo-viewer" | "file-viewer")
                            {
                                continue;
                            }
                        }
                        let cur = sess
                            .main_process
                            .window_state()
                            .is_fullscreen
                            .load(Ordering::Relaxed);
                        let should_fs = target.unwrap_or(!cur);
                        if should_fs {
                            app.window.fullscreen();
                        } else {
                            app.window.unfullscreen();
                        }
                    }
                    WindowCommand::CloseOrHide { window_id }
                    | WindowCommand::Hide { window_id } => {
                        if let Some(ref wid) = window_id {
                            if sess.host.close_child_window(wid)
                                || matches!(wid.as_str(), "photo-viewer" | "file-viewer")
                            {
                                continue;
                            }
                        }
                        app.persist_window_geometry();
                        let has_live_tray = app
                            .tray_handle
                            .borrow()
                            .as_ref()
                            .is_some_and(|h| !h.is_closed());
                        if !app.quitting.get() && has_live_tray {
                            app.hide_to_tray();
                        } else {
                            app.quitting.set(true);
                            app.shutdown_and_quit();
                            break;
                        }
                    }
                    WindowCommand::Show { window_id, focus } => {
                        if let Some(ref wid) = window_id {
                            if sess.host.show_child_window(wid, focus)
                                || matches!(wid.as_str(), "photo-viewer" | "file-viewer")
                            {
                                continue;
                            }
                        }
                        if app.suppress_renderer_auto_show.get() {
                            tracing::debug!(
                                "suppressing renderer-initiated main window show while started minimized in tray"
                            );
                            continue;
                        }
                        if app.active_slug() != slug {
                            // Keep background sessions mounted inside `accounts_stack` without stealing active tab!
                            if sess.host.is_direct_ui() && !app.installing_runtime.get() {
                                sess.mount_into_page();
                            }
                            continue;
                        }
                        if focus {
                            app.present_window();
                        } else {
                            let was_visible = app.window.is_visible();
                            if !app.demo_mode
                                && sess.host.is_direct_ui()
                                && !app.installing_runtime.get()
                            {
                                sess.mount_into_page();
                                app.accounts_stack.set_visible_child_name(&slug);
                                app.stack.set_visible_child_name("zalo");
                                if !was_visible {
                                    if app.session_count() == 1
                                        && sess.page_mode.get() == WindowPageMode::Login
                                    {
                                        app.apply_login_window_size();
                                    } else {
                                        app.apply_main_window_size();
                                    }
                                }
                            }
                            sess.main_process
                                .window_state()
                                .is_visible
                                .store(true, Ordering::Relaxed);
                            app.window.show_all();
                            app.refresh_workspace_chrome();
                            app.window.deiconify();
                            sess.host
                                .bridge()
                                .send("visibility-change", vec![json!("show")]);
                        }
                    }
                    WindowCommand::Focus { window_id } => {
                        if let Some(ref wid) = window_id {
                            if sess.host.focus_child_window(wid)
                                || matches!(wid.as_str(), "photo-viewer" | "file-viewer")
                            {
                                continue;
                            }
                        }
                        if app.suppress_renderer_auto_show.get() || app.active_slug() != slug {
                            continue;
                        }
                        app.present_window();
                    }
                    WindowCommand::Quit => {
                        app.quitting.set(true);
                        app.shutdown_and_quit();
                        break;
                    }
                    WindowCommand::FlashFrame(flag) => {
                        app.window.set_urgency_hint(flag);
                    }
                    WindowCommand::SetLoginSize => {
                        sess.page_mode.set(WindowPageMode::Login);
                        app.refresh_workspace_chrome();
                        if app.session_count() == 1 && app.active_slug() == slug {
                            app.apply_login_window_size();
                        }
                    }
                    WindowCommand::ExpandMainWindow => {
                        sess.page_mode.set(WindowPageMode::Main);
                        app.refresh_workspace_chrome();
                        if app.active_slug() == slug || app.session_count() >= 2 {
                            app.apply_main_window_size();
                        }
                    }
                    WindowCommand::SetResizable(resizable) => {
                        if app.session_count() == 1 {
                            app.window.set_resizable(resizable);
                        } else {
                            app.window.set_resizable(true);
                        }
                    }
                }
            }
        });
    }

    fn install_ui_rpc(self: &Rc<Self>, ucm: &UserContentManager) {
        let app_weak = Rc::downgrade(self);
        ucm.connect_script_message_received(Some("__olaz_ui_raw"), move |_, js_res| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let Some(val) = js_res.js_value() else {
                return;
            };
            let raw = val.to_str();
            let Ok(envelope) = serde_json::from_str::<Value>(raw.as_str()) else {
                tracing::warn!("ignoring malformed olaz_ui command payload");
                return;
            };
            let id = envelope
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let cmd = envelope
                .get("cmd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let args = envelope.get("args").cloned().unwrap_or_else(|| json!({}));

            if id.is_empty() || cmd.is_empty() {
                return;
            }

            if cmd == "__ui_log_error" {
                log_ui_js_error(&args);
                if id != "__err" {
                    app.send_ui_reply(&id, Ok(json!({ "ok": true })));
                }
                return;
            }

            let app_for_task = Rc::clone(&app);
            glib::MainContext::default().spawn_local(async move {
                let res = app_for_task.dispatch_ui_command(&cmd, args).await;
                app_for_task.send_ui_reply(&id, res);
            });
        });
    }

    fn install_session_hooks(self: &Rc<Self>, session: &Rc<AccountSession>) {
        let slug = session.slug.clone();

        // 1. OfficialClientHost console & page lifecycle
        {
            let slug_log = slug.clone();
            session.host.set_on_console(move |line| {
                let official = line.starts_with("[log]") || line.starts_with("[info]");
                if line.contains("do open indexeddb") {
                    return;
                }
                if !official
                    || line.contains("[adapter]")
                    || line.contains("[chat]")
                    || line.contains("[transfer]")
                    || line.contains("[bridge]")
                {
                    let truncated: String = line.chars().take(320).collect();
                    tracing::debug!(
                        target: "olaz::zalo_console",
                        profile = %slug_log,
                        "{truncated}"
                    );
                }
            });
        }

        {
            let app_weak = Rc::downgrade(self);
            let slug_page = slug.clone();
            session.host.set_on_page_event(move |ev| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                let Some(sess) = app.session_by_slug(&slug_page) else {
                    return;
                };
                match ev {
                    PageEvent::StartedLoading => {
                        sess.main_process.page_did_start_loading();
                    }
                    PageEvent::FinishedLoading(page) => {
                        sess.main_process.page_did_finish_loading(page);
                        sess.sync_privacy_to_webview();
                        sess.sync_call_recording_settings_to_webview();
                        match page {
                            HostPage::Main => {
                                sess.page_mode.set(WindowPageMode::Main);
                                app.refresh_workspace_chrome();
                                if app.window.is_visible() && !app.suppress_renderer_auto_show.get()
                                {
                                    app.apply_main_window_size();
                                }
                            }
                            HostPage::Login => {
                                sess.page_mode.set(WindowPageMode::Login);
                                app.refresh_workspace_chrome();
                                if app.session_count() == 1
                                    && app.active_slug() == slug_page
                                    && app.window.is_visible()
                                    && !app.suppress_renderer_auto_show.get()
                                {
                                    app.apply_login_window_size();
                                }
                            }
                            HostPage::SharedWorker => {}
                        }
                        if page == HostPage::Main && sess.main_process.has_session() {
                            let needs_ready = sess.signing_in.get()
                                || matches!(sess.engine_state.borrow().phase, EnginePhase::Booting);
                            if needs_ready {
                                sess.signing_in.set(false);
                                sess.engine_state.borrow_mut().phase = EnginePhase::Ready;
                                sess.sync_engine_state_mirror();
                                if app.active_slug() == slug_page {
                                    let snap = app.full_ui_snapshot();
                                    app.push_ui_state(&snap);
                                }
                            }
                            let host_refresh = Rc::clone(&sess.host);
                            glib::MainContext::default().spawn_local(async move {
                                let _ = host_refresh
                                    .evaluate(
                                        "if (window.__zaloConvActions && typeof window.__zaloConvActions.refresh === 'function') {\n\
                                           await window.__zaloConvActions.refresh();\n\
                                         }\n\
                                         return true;",
                                    )
                                    .await;
                            });
                        }
                    }
                    PageEvent::Failed(err) => {
                        tracing::warn!(profile = %slug_page, %err, "official Zalo host navigation failed");
                    }
                    PageEvent::Terminated => {
                        if app.quitting.get() {
                            return;
                        }
                        if sess.host.allow_crash_recovery() {
                            let target = if sess.main_process.has_session() {
                                HostPage::Main
                            } else {
                                HostPage::Login
                            };
                            tracing::warn!(
                                profile = %slug_page,
                                ?target,
                                "official Zalo host WebProcess terminated; reloading target page"
                            );
                            sess.host
                                .load(target, &sess.main_process.page_query(false));
                        } else {
                            tracing::error!(
                                profile = %slug_page,
                                "official Zalo host WebProcess crashed >3 times within 15s; halting automatic host reload"
                            );
                        }
                    }
                }
            });
        }

        // 2. Incoming `zstate` patches from official `HostScripts/*.js` & `LINUX_DIRECT_UI_SCRIPT`
        {
            let app_weak = Rc::downgrade(self);
            let slug_state = slug.clone();
            session.host.set_on_state(move |payload| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                app.handle_session_zstate_payload(&slug_state, payload);
            });
        }

        // 3. Notification privacy & focus context provider (background tabs are NEVER suppressed!)
        {
            let app_weak = Rc::downgrade(self);
            let slug_notif = slug.clone();
            session
                .main_process
                .set_notification_context_provider(move || {
                    let Some(app) = app_weak.upgrade() else {
                        return NotificationContext::default();
                    };
                    let Some(sess) = app.session_by_slug(&slug_notif) else {
                        return NotificationContext::default();
                    };
                    let is_active_tab = app.active_slug() == slug_notif;
                    let state = sess.engine_state.borrow();
                    let settings = app.settings.borrow();
                    let prefix = if app.session_count() > 1 {
                        Some(app.account_effective_name(&slug_notif))
                    } else {
                        None
                    };
                    NotificationContext {
                        window_focused: is_active_tab && app.window.is_active(),
                        window_visible: app.window.is_visible(),
                        open_conversation: if !is_active_tab || sess.host.is_direct_ui() {
                            None
                        } else {
                            state.open_conversation_id.clone()
                        },
                        idle_seconds: 0.0,
                        notifications_enabled: settings.notifications,
                        shows_preview: true,
                        sound_enabled: settings.sound,
                        // In direct-UI mode, Zalo's own official renderer gates hidden-chat previews
                        // before invoking `new Notification(...)`.
                        hidden_known: if sess.host.is_direct_ui() {
                            true
                        } else {
                            state.conv_actions.hidden_known
                        },
                        locked_ids: state.conv_actions.locked_ids(),
                        account_title_prefix: prefix,
                    }
                });
        }

        // 4. High-level main-process events (login, QR scan/failure, logout/kickout, badge, notification click, calls)
        {
            let app_weak = Rc::downgrade(self);
            let slug_ev = slug;
            session.main_process.set_on_event(move |ev| {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                let Some(sess) = app.session_by_slug(&slug_ev) else {
                    return;
                };
                let is_active = app.active_slug() == slug_ev;
                match ev {
                    MainProcessEvent::LoginSucceeded => {
                        sess.signing_in.set(true);
                        sess.page_mode.set(WindowPageMode::Main);
                        *sess.last_qr.borrow_mut() = None;
                        sess.qr_seq.set(sess.qr_seq.get().wrapping_add(1));
                        sess.engine_state.borrow_mut().phase = EnginePhase::Booting;
                        sess.sync_engine_state_mirror();
                        app.refresh_workspace_chrome();
                        if app.window.is_visible() && !app.suppress_renderer_auto_show.get() {
                            app.apply_main_window_size();
                        }
                        if is_active {
                            let snap = app.full_ui_snapshot();
                            app.push_ui_state(&snap);
                        }
                    }
                    MainProcessEvent::LoggedOut { kickout_notice } => {
                        sess.signing_in.set(false);
                        sess.page_mode.set(WindowPageMode::Login);
                        *sess.last_qr.borrow_mut() = None;
                        sess.qr_seq.set(sess.qr_seq.get().wrapping_add(1));
                        sess.autoreply_seen_ids.borrow_mut().clear();
                        sess.unread_count.set(0);
                        {
                            let mut state = sess.engine_state.borrow_mut();
                            state.reset_account_state(kickout_notice.clone());
                        }
                        sess.sync_engine_state_mirror();
                        app.refresh_workspace_chrome();
                        app.refresh_total_unread_indicators();
                        if app.session_count() == 1
                            && is_active
                            && app.window.is_visible()
                            && !app.suppress_renderer_auto_show.get()
                        {
                            app.apply_login_window_size();
                        }
                        if let Some(ref notice) = kickout_notice {
                            if !is_active {
                                let name = app.account_effective_name(&slug_ev);
                                app.show_workspace_toast(&format!("[{name}] {notice}"));
                            }
                        }
                        if is_active {
                            let mut snap = app.full_ui_snapshot();
                            if let (Some(notice), Some(obj)) = (kickout_notice, snap.as_object_mut())
                            {
                                obj.insert("toast".to_string(), json!({ "text": notice }));
                            }
                            app.push_ui_state(&snap);
                        }
                    }
                    MainProcessEvent::QrScanned { name, avatar } => {
                        let updated = {
                            let mut state = sess.engine_state.borrow_mut();
                            if let EnginePhase::Login {
                                qr_expired,
                                waiting_confirm,
                                error_notice,
                                ..
                            } = &mut state.phase
                            {
                                *qr_expired = false;
                                *error_notice = None;
                                *waiting_confirm = Some(WaitingConfirm { name, avatar });
                                true
                            } else {
                                false
                            }
                        };
                        if updated && is_active {
                            app.push_login_phase_patch(&sess);
                        }
                    }
                    MainProcessEvent::QrFailed { message } => {
                        let updated = {
                            let mut state = sess.engine_state.borrow_mut();
                            if let EnginePhase::Login { error_notice, .. } = &mut state.phase {
                                *error_notice = Some(message);
                                true
                            } else {
                                false
                            }
                        };
                        if updated && is_active {
                            app.push_login_phase_patch(&sess);
                        }
                    }
                    MainProcessEvent::Badge(count) => {
                        let unread = u64::try_from(count.max(0)).unwrap_or(0);
                        sess.unread_count.set(unread);
                        app.refresh_rail_item_visuals(&slug_ev);
                        app.sync_accounts_to_all_webviews();
                        app.refresh_total_unread_indicators();
                    }
                    MainProcessEvent::NotificationClicked { conv_id } => {
                        app.switch_to_account(&slug_ev);
                        app.present_window();
                        if !sess.host.is_direct_ui() && !conv_id.is_empty() {
                            let app_sel = Rc::clone(&app);
                            glib::MainContext::default().spawn_local(async move {
                                let _ = app_sel
                                    .dispatch_ui_command(
                                        "select_conversation",
                                        json!({ "convId": conv_id }),
                                    )
                                    .await;
                            });
                        }
                    }
                    MainProcessEvent::IncomingCall(call_opt) => {
                        if is_active {
                            let incoming_val = call_opt.as_ref().map(incoming_call_to_ui_json);
                            app.push_ui_state(&json!({ "incomingCall": incoming_val }));
                        }
                    }
                }
            });
        }
    }

    fn install_auto_relock_timer(self: &Rc<Self>) {
        let app_weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(10, move || {
            let Some(app) = app_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let active = app.active_slug();
            let sessions: Vec<Rc<AccountSession>> = app.sessions.borrow().clone();
            for sess in sessions {
                let relocked = sess
                    .engine_state
                    .borrow_mut()
                    .conv_actions
                    .check_auto_relock(Instant::now());
                if relocked {
                    sess.sync_engine_state_mirror();
                    if sess.slug == active {
                        let state = sess.engine_state.borrow();
                        let patch = build_ui_patch_for_slices(
                            &state,
                            &[
                                "conversations".to_string(),
                                "conv_actions".to_string(),
                            ],
                            None,
                            sess.signing_in.get(),
                        );
                        drop(state);
                        app.push_ui_state(&patch);
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    fn spawn_omarchy_listener(
        self: &Rc<Self>,
        rx_init: &mut tokio::sync::mpsc::UnboundedReceiver<DesktopTheme>,
    ) {
        let mut rx = std::mem::replace(rx_init, tokio::sync::mpsc::unbounded_channel().1);
        let app_weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            while let Some(next_theme) = rx.recv().await {
                let Some(app) = app_weak.upgrade() else {
                    break;
                };
                *app.desktop_theme.borrow_mut() = next_theme.clone();
                if app.settings.borrow().omarchy_sync {
                    app.apply_desktop_theme(&next_theme);
                }
            }
        });
    }

    fn spawn_status_tray(self: &Rc<Self>) {
        let (ready_tx, ready_rx) = oneshot::channel();
        self.tokio_handle.spawn(async move {
            let _ = ready_tx.send(spawn_tray().await);
        });

        let app_weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(Ok((handle, mut rx))) = ready_rx.await else {
                tracing::warn!("StatusNotifierItem tray unavailable on current session bus");
                if let Some(app) = app_weak.upgrade() {
                    if app.suppress_renderer_auto_show.get() {
                        tracing::warn!(
                            "presenting main window because StatusNotifierItem tray failed during --minimized startup"
                        );
                        app.present_window();
                    }
                }
                return;
            };
            if let Some(app) = app_weak.upgrade() {
                let unread = app.total_unread_across_sessions();
                let initial_privacy =
                    tray_privacy_from_config(*app.active_session().privacy.borrow());
                *app.tray_handle.borrow_mut() = Some(handle.clone());
                app.tokio_handle.spawn(async move {
                    handle.set_unread_count(unread).await;
                    handle.set_privacy_state(initial_privacy).await;
                });
            }

            while let Some(ev) = rx.recv().await {
                let Some(app) = app_weak.upgrade() else {
                    break;
                };
                match ev {
                    OlazTrayEvent::ShowWindow { .. } => {
                        app.present_window();
                        app.active_session()
                            .host
                            .bridge()
                            .send("show-from-tray", vec![]);
                    }
                    OlazTrayEvent::ToggleWindow { .. } => {
                        if app.window.is_visible() && app.window.is_active() {
                            app.hide_to_tray();
                        } else {
                            app.present_window();
                            app.active_session()
                                .host
                                .bridge()
                                .send("show-from-tray", vec![]);
                        }
                    }
                    OlazTrayEvent::SetUserStatusOnline => {
                        // Silent presence toggle: keep the window hidden if hidden in tray!
                        app.active_session()
                            .host
                            .bridge()
                            .send("change-user-mode-online", vec![]);
                        if let Some(tray) = app.tray_handle.borrow().clone() {
                            app.tokio_handle.spawn(async move {
                                tray.set_online_busy_index(0).await;
                            });
                        }
                    }
                    OlazTrayEvent::SetUserStatusBusy => {
                        // Silent DND/Busy toggle: keep the window hidden if hidden in tray!
                        app.active_session()
                            .host
                            .bridge()
                            .send("change-user-mode-busy", vec![]);
                        if let Some(tray) = app.tray_handle.borrow().clone() {
                            app.tokio_handle.spawn(async move {
                                tray.set_online_busy_index(1).await;
                            });
                        }
                    }
                    OlazTrayEvent::TogglePrivacy { field, enabled } => {
                        let sess = app.active_session();
                        let next_state = {
                            let mut st = tray_privacy_from_config(*sess.privacy.borrow());
                            st.set(field, enabled);
                            *sess.privacy.borrow_mut() = privacy_config_from_tray(st);
                            st
                        };
                        save_legacy_tray_privacy(&next_state, &sess.privacy_path);
                        let _ = sess
                            .privacy
                            .borrow()
                            .save_to(sess.platform.config_dir());
                        sess.sync_privacy_to_webview();
                        if let Some(tray) = app.tray_handle.borrow().clone() {
                            app.tokio_handle.spawn(async move {
                                tray.set_privacy_state(next_state).await;
                            });
                        }
                        let backend = Arc::clone(&sess.control_backend) as Arc<dyn OlazControlBackend>;
                        let req = json!({
                            "blockTyping": next_state.block_typing,
                            "blockSeen": next_state.block_seen,
                            "blockDelivered": next_state.block_delivered,
                            "antiRecall": next_state.anti_recall,
                        });
                        app.tokio_handle.spawn(async move {
                            let _ = backend.set_privacy(&req).await;
                        });
                    }
                    OlazTrayEvent::LaunchSecondaryProfile => {
                        app.add_new_account_tab(None);
                    }
                    OlazTrayEvent::Screenshot => {
                        let sess = app.active_session();
                        if sess.host.is_direct_ui() {
                            let rt = app.tokio_handle.clone();
                            let platform_shot = Arc::clone(&sess.platform);
                            let app_shot = Rc::clone(&app);
                            let (tx, rx_shot) = oneshot::channel();
                            rt.spawn(async move {
                                let res: anyhow::Result<Option<PathBuf>> =
                                    match capture_interactive_screenshot().await {
                                        Ok(Some(raw)) => stage_screenshot_file(
                                            &raw,
                                            Path::new(SCREENSHOT_STAGING_DIR),
                                        )
                                        .map(Some),
                                        Ok(None) => Ok(None),
                                        Err(err) => Err(err),
                                    };
                                let _ = tx.send(res);
                            });
                            glib::MainContext::default().spawn_local(async move {
                                if let Ok(Ok(Some(staged))) = rx_shot.await {
                                    platform_shot.grant_readable_path(&staged);
                                    if let Ok(pixbuf) =
                                        gdk::gdk_pixbuf::Pixbuf::from_file(&staged)
                                    {
                                        let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                                        cb.set_image(&pixbuf);
                                    }
                                    app_shot.present_window();
                                }
                            });
                        } else {
                            app.present_window();
                            let app_shot = Rc::clone(&app);
                            glib::MainContext::default().spawn_local(async move {
                                let target_conv = {
                                    let active_s = app_shot.active_session();
                                    let st = active_s.engine_state.borrow();
                                    st.open_conversation_id
                                        .clone()
                                        .or_else(|| st.self_conversation_id.clone())
                                        .unwrap_or_default()
                                };
                                let to_my_cloud = target_conv.is_empty();
                                let _ = app_shot
                                    .dispatch_ui_command(
                                        "capture_screenshot",
                                        json!({
                                            "convId": target_conv,
                                            "toMyCloud": to_my_cloud,
                                        }),
                                    )
                                    .await;
                            });
                        }
                    }
                    OlazTrayEvent::Quit => {
                        app.quitting.set(true);
                        app.shutdown_and_quit();
                        break;
                    }
                }
            }
        });
    }

    /// Starts the installed Zalo runtime immediately, or downloads and verifies the official DMG
    /// while streaming live `InstallProgress` updates to the UI `'preparing'` screen.
    pub fn bootstrap_runtime(self: &Rc<Self>) {
        if ZaloRuntimeManager::is_complete(&self.runtime_mgr.current_dir()) {
            match self.runtime_mgr.open_asar() {
                Ok(archive) => {
                    let asar_arc = Arc::new(archive);
                    *self.shared_asar.borrow_mut() = Some(Arc::clone(&asar_arc));
                    if let Some(ver) =
                        ZaloRuntimeManager::installed_version(&self.runtime_mgr.current_dir())
                    {
                        self.primary_platform.set_app_version(&ver);
                        for sess in self.sessions.borrow().iter() {
                            sess.platform.set_app_version(&ver);
                            sess.host.reload_user_scripts(&sess.platform);
                        }
                    }
                    for sess in self.sessions.borrow().iter() {
                        sess.main_process
                            .runtime()
                            .set_asar(Arc::clone(&asar_arc));
                    }
                    self.runtime_progress.replace(None);
                    self.begin_main_process();
                    return;
                }
                Err(err) => {
                    tracing::warn!(%err, "existing app.asar could not be opened; reinstalling runtime");
                }
            }
        }

        self.start_runtime_installation();
    }

    fn start_runtime_installation(self: &Rc<Self>) {
        if self.installing_runtime.get() {
            return;
        }
        self.installing_runtime.set(true);
        self.suppress_renderer_auto_show.set(false);
        for sess in self.sessions.borrow().iter() {
            sess.main_process
                .window_state()
                .is_visible
                .store(true, Ordering::Relaxed);
        }
        self.stack.set_visible_child_name("setup");
        self.ensure_setup_ui_loaded();

        let initial_prog = UiRuntimeProgress::from_install_progress(InstallProgress::Looking);
        *self.runtime_progress.borrow_mut() = Some(initial_prog.clone());
        {
            let active = self.active_session();
            let mut state = active.engine_state.borrow_mut();
            state.phase = EnginePhase::InstallingRuntime {
                step: "looking".to_string(),
                fraction: 0.0,
                detail: None,
                error: None,
            };
        }
        self.push_ui_state(&json!({
            "phase": "preparing",
            "runtimeProgress": initial_prog,
        }));

        let mgr = self.runtime_mgr.clone();
        let (prog_tx, mut prog_rx) = tokio::sync::mpsc::unbounded_channel::<InstallProgress>();
        let (done_tx, done_rx) = oneshot::channel::<anyhow::Result<String>>();

        self.tokio_handle.spawn(async move {
            let res = mgr
                .install(move |p| {
                    let _ = prog_tx.send(p);
                })
                .await;
            let _ = done_tx.send(res);
        });

        let app_weak_prog = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            while let Some(p) = prog_rx.recv().await {
                let Some(app) = app_weak_prog.upgrade() else {
                    break;
                };
                let ui_prog = UiRuntimeProgress::from_install_progress(p);
                *app.runtime_progress.borrow_mut() = Some(ui_prog.clone());
                app.push_ui_state(&json!({
                    "phase": "preparing",
                    "runtimeProgress": ui_prog,
                }));
            }
        });

        let app_weak_done = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(outcome) = done_rx.await else {
                return;
            };
            let Some(app) = app_weak_done.upgrade() else {
                return;
            };
            app.installing_runtime.set(false);

            match outcome
                .and_then(|version| app.runtime_mgr.open_asar().map(|archive| (version, archive)))
            {
                Ok((version, archive)) => {
                    tracing::info!(%version, "Zalo runtime installed; starting main process");
                    let asar_arc = Arc::new(archive);
                    *app.shared_asar.borrow_mut() = Some(Arc::clone(&asar_arc));
                    app.primary_platform.set_app_version(&version);
                    for sess in app.sessions.borrow().iter() {
                        sess.platform.set_app_version(&version);
                        sess.host.reload_user_scripts(&sess.platform);
                        sess.main_process
                            .runtime()
                            .set_asar(Arc::clone(&asar_arc));
                    }
                    *app.runtime_progress.borrow_mut() = None;
                    app.begin_main_process();
                }
                Err(err) => {
                    let msg = format!("{err:#}");
                    tracing::error!(error = %msg, "Zalo runtime installation failed");
                    let failed = UiRuntimeProgress {
                        step: "looking".to_string(),
                        received: 0,
                        total: 0,
                        failure: Some(msg.clone()),
                    };
                    *app.runtime_progress.borrow_mut() = Some(failed.clone());
                    {
                        let active = app.active_session();
                        let mut state = active.engine_state.borrow_mut();
                        state.phase = EnginePhase::InstallingRuntime {
                            step: "failed".to_string(),
                            fraction: 0.0,
                            detail: None,
                            error: Some(msg),
                        };
                    }
                    app.push_ui_state(&json!({
                        "phase": "preparing",
                        "runtimeProgress": failed,
                    }));
                }
            }
        });
    }

    fn start_account_session(&self, session: &Rc<AccountSession>) {
        let logged_in = session.main_process.has_session();
        if logged_in {
            session.page_mode.set(WindowPageMode::Main);
            session.signing_in.set(true);
            *session.last_qr.borrow_mut() = None;
            session.qr_seq.set(session.qr_seq.get().wrapping_add(1));
            session.engine_state.borrow_mut().phase = EnginePhase::Booting;
        } else {
            session.page_mode.set(WindowPageMode::Login);
            session.signing_in.set(false);
            *session.last_qr.borrow_mut() = None;
            session.qr_seq.set(session.qr_seq.get().wrapping_add(1));
            session.engine_state.borrow_mut().phase = EnginePhase::Login {
                qr: None,
                qr_expired: false,
                waiting_confirm: None,
                kickout_notice: None,
                error_notice: None,
            };
        }
        session.sync_engine_state_mirror();

        if session.host.is_direct_ui() {
            if logged_in {
                let st = self.saved_win_state.borrow().clone();
                session.host.resize_offscreen(
                    st.width.max(MIN_MAIN_WINDOW_DIM),
                    st.height.max(MIN_MAIN_WINDOW_DIM),
                );
            } else {
                session.host.resize_offscreen(400, 580);
            }
            if !self.suppress_renderer_auto_show.get() {
                session.mount_into_page();
            }
        }

        session
            .main_process
            .window_state()
            .is_visible
            .store(!self.suppress_renderer_auto_show.get(), Ordering::Relaxed);
        session.main_process.start();
    }

    fn restore_workspace_secondary_sessions(self: &Rc<Self>) {
        if self.demo_mode || self.primary_platform.profile_slug() != "default" {
            return;
        }
        let metas = self.profile_registry.borrow().workspace_profiles();
        let shared_asar = self.shared_asar.borrow().clone();
        let direct_ui = self.active_session().host.is_direct_ui();

        for meta in metas {
            if meta.slug == "default" || self.session_by_slug(&meta.slug).is_some() {
                continue;
            }
            // Only auto-restore secondary profiles that have an existing session or explicit registry entry
            // with `enabled_in_workspace: true`.
            match AccountSession::spawn_secondary(
                &meta.slug,
                meta.proxy_url.clone(),
                &self.primary_platform,
                &self.instance_root_dir,
                shared_asar.clone(),
                &self.tokio_handle,
                direct_ui,
                self.outbound_send_tx.clone(),
                self.outbound_call_tx.clone(),
                self.privacy_obs_tx.clone(),
            ) {
                Ok(sess) => {
                    self.register_session_in_ui(&sess);
                    self.start_account_session(&sess);
                }
                Err(err) => {
                    tracing::warn!(slug = %meta.slug, %err, "failed to restore secondary AccountSession");
                }
            }
        }
    }

    fn begin_main_process(self: &Rc<Self>) {
        let primary = self.active_session();
        self.start_account_session(&primary);
        self.restore_workspace_secondary_sessions();

        let desired_active = {
            let saved = self.profile_registry.borrow().active_slug.clone();
            if self.session_by_slug(&saved).is_some() {
                saved
            } else {
                primary.slug.clone()
            }
        };
        *self.active_slug.borrow_mut() = desired_active.clone();
        self.multi_control_backend.set_active_slug(&desired_active);

        let active_sess = self.active_session();
        let start_hidden = self.suppress_renderer_auto_show.get();
        if active_sess.host.is_direct_ui() {
            let multi = self.session_count() >= 2;
            if !multi && active_sess.page_mode.get() == WindowPageMode::Login {
                if start_hidden {
                    self.in_login_mode.set(true);
                    self.window_size_restored.set(false);
                } else {
                    self.apply_login_window_size();
                }
            } else if start_hidden {
                self.in_login_mode.set(false);
            } else {
                self.apply_main_window_size();
            }

            if !start_hidden {
                for sess in self.sessions.borrow().iter() {
                    sess.mount_into_page();
                }
                self.accounts_stack.set_visible_child_name(&desired_active);
                self.workspace_hbox.show_all();
                self.stack.set_visible_child_name("zalo");
            }
            self.refresh_workspace_chrome();
        } else {
            let snap = self.full_ui_snapshot();
            self.push_ui_state(&snap);
        }
    }

    fn schedule_qr_expiry_timer(self: &Rc<Self>, slug: String, seq: u64) {
        let app_weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local_once(Self::QR_EXPIRY_SECS, move || {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let Some(sess) = app.session_by_slug(&slug) else {
                return;
            };
            if sess.qr_seq.get() != seq {
                return;
            }
            let expired_now = {
                let mut state = sess.engine_state.borrow_mut();
                if let EnginePhase::Login {
                    qr_expired,
                    waiting_confirm: None,
                    ..
                } = &mut state.phase
                {
                    if !*qr_expired {
                        *qr_expired = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if expired_now && app.active_slug() == slug {
                app.push_login_phase_patch(&sess);
            }
        });
    }

    fn push_login_phase_patch(&self, sess: &AccountSession) {
        let ui_patch = {
            let state = sess.engine_state.borrow();
            let rt_prog = self.runtime_progress.borrow().clone();
            build_ui_patch_for_slices(
                &state,
                &["phase".to_string()],
                rt_prog.as_ref(),
                sess.signing_in.get(),
            )
        };
        if ui_patch.as_object().is_some_and(|m| !m.is_empty()) {
            self.push_ui_state(&ui_patch);
        }
    }

    fn handle_session_zstate_payload(self: &Rc<Self>, slug: &str, payload: Value) {
        let Some(sess) = self.session_by_slug(slug) else {
            return;
        };
        // Ignore stale `login.html` DOM polls once login succeeds and we are loading `index.html`.
        if sess.signing_in.get()
            && payload
                .get("page")
                .and_then(Value::as_str)
                .is_some_and(|p| p == "login")
        {
            return;
        }

        let is_active = self.active_slug() == slug;

        if let Some(obj) = payload.as_object() {
            // In-window titlebar actions (`toggleAntiRecall` / `addAccount`).
            if let Some(ui_act) = obj.get("uiAction").and_then(Value::as_str) {
                match ui_act {
                    "toggleAntiRecall" => {
                        let next_state = {
                            let mut st = tray_privacy_from_config(*sess.privacy.borrow());
                            let next_val = obj
                                .get("enabled")
                                .and_then(Value::as_bool)
                                .unwrap_or(!st.anti_recall);
                            st.set(olaz_linux::tray::PrivacyField::AntiRecall, next_val);
                            *sess.privacy.borrow_mut() = privacy_config_from_tray(st);
                            st
                        };
                        save_legacy_tray_privacy(&next_state, &sess.privacy_path);
                        let _ = sess.privacy.borrow().save_to(sess.platform.config_dir());
                        sess.sync_privacy_to_webview();
                        if is_active {
                            if let Some(tray) = self.tray_handle.borrow().clone() {
                                self.tokio_handle.spawn(async move {
                                    tray.set_privacy_state(next_state).await;
                                });
                            }
                        }
                        let backend =
                            Arc::clone(&sess.control_backend) as Arc<dyn OlazControlBackend>;
                        let req = json!({
                            "blockTyping": next_state.block_typing,
                            "blockSeen": next_state.block_seen,
                            "blockDelivered": next_state.block_delivered,
                            "antiRecall": next_state.anti_recall,
                        });
                        self.tokio_handle.spawn(async move {
                            let _ = backend.set_privacy(&req).await;
                        });
                        return;
                    }
                    "addAccount" => {
                        self.add_new_account_tab(None);
                        return;
                    }
                    "switchAccount" => {
                        if let Some(target_slug) = obj
                            .get("slug")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            self.switch_to_account(target_slug);
                        }
                        return;
                    }
                    "accountContextMenu" => {
                        if let Some(target_slug) = obj
                            .get("slug")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            self.show_account_context_menu(target_slug, None);
                        }
                        return;
                    }
                    "chooseCallRecordingsDir" => {
                        let current_dir = sess.main_process.call_engine().recordings_dir();
                        let rt = self.tokio_handle.clone();
                        let app_weak = Rc::downgrade(self);
                        let slug_owned = slug.to_string();
                        let (tx, rx) = oneshot::channel();
                        rt.spawn(async move {
                            let _ = tx.send(
                                pick_open_dialog(
                                    "Chọn thư mục lưu ghi âm cuộc gọi",
                                    false,
                                    true,
                                    Some(&current_dir),
                                    &[],
                                )
                                .await,
                            );
                        });
                        glib::MainContext::default().spawn_local(async move {
                            let Some(app) = app_weak.upgrade() else {
                                return;
                            };
                            let chosen_opt = match rx.await {
                                Ok(Ok(mut paths)) => paths.drain(..).next(),
                                _ => {
                                    let dialog = gtk::FileChooserNative::new(
                                        Some("Chọn thư mục lưu ghi âm cuộc gọi"),
                                        Some(&app.window),
                                        gtk::FileChooserAction::SelectFolder,
                                        Some("Chọn thư mục"),
                                        Some("Hủy"),
                                    );
                                    let cur = app
                                        .session_by_slug(&slug_owned)
                                        .map(|s| s.main_process.call_engine().recordings_dir());
                                    if let Some(ref d) = cur {
                                        let _ = std::fs::create_dir_all(d);
                                        let _ = dialog.set_current_folder(d);
                                    }
                                    if dialog.run() == gtk::ResponseType::Accept {
                                        dialog.filename()
                                    } else {
                                        None
                                    }
                                }
                            };
                            if let Some(chosen) = chosen_opt.filter(|p| !p.as_os_str().is_empty()) {
                                let _ = std::fs::create_dir_all(&chosen);
                                if let Some(target_sess) = app.session_by_slug(&slug_owned) {
                                    target_sess.platform.grant_readable_path(&chosen);
                                    target_sess
                                        .main_process
                                        .call_engine()
                                        .set_recordings_dir(chosen.clone());
                                    let default_dir =
                                        target_sess.platform.base_dir().join("recordings");
                                    let cfg = CallRecordingSettings {
                                        recordings_dir: if chosen == default_dir {
                                            None
                                        } else {
                                            Some(chosen)
                                        },
                                    };
                                    let _ = cfg.save_to(target_sess.platform.config_dir());
                                    target_sess.sync_call_recording_settings_to_webview();
                                }
                            }
                        });
                        return;
                    }
                    "openCallRecordingsDir" => {
                        let dir = sess.main_process.call_engine().recordings_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::process::Command::new("xdg-open")
                            .arg(&dir)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                        return;
                    }
                    "resetCallRecordingsDir" => {
                        let default_dir = sess.platform.base_dir().join("recordings");
                        let _ = std::fs::create_dir_all(&default_dir);
                        sess.platform.grant_readable_path(&default_dir);
                        sess.main_process
                            .call_engine()
                            .set_recordings_dir(default_dir);
                        let _ = CallRecordingSettings {
                            recordings_dir: None,
                        }
                        .save_to(sess.platform.config_dir());
                        sess.sync_call_recording_settings_to_webview();
                        return;
                    }
                    "queryCallRecordingsDir" => {
                        sess.sync_call_recording_settings_to_webview();
                        return;
                    }
                    _ => {}
                }
            }

            // Message delete undo token (`msgActions: { deleteUndo: { undoId, convId, msgId } }`).
            if let Some(undo_id) = obj
                .get("msgActions")
                .and_then(|ma| ma.get("deleteUndo"))
                .and_then(|du| du.get("undoId").or_else(|| du.get("id")))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                if is_active {
                    *self.last_delete_undo_id.borrow_mut() = Some(undo_id.to_string());
                }
            }

            // Notification chime sound from `notification-adapter.js`.
            if let Some(sound_url) = obj
                .get("notification")
                .and_then(|n| n.get("sound"))
                .and_then(Value::as_str)
            {
                if self.settings.borrow().sound {
                    sess.main_process.play_notification_sound_if_allowed(sound_url);
                }
            }

            // Incoming VoIP call signaling from `call-adapter.js` (`{call: ...}` or `{voip: ...}`).
            if obj.contains_key("call") || obj.contains_key("voip") {
                let incoming_opt = sess
                    .main_process
                    .call_coordinator()
                    .borrow_mut()
                    .handle_voip_event(&payload);
                if let Some(ref call) = incoming_opt {
                    if self.settings.borrow().notifications {
                        let mut req = call.to_notification_request();
                        if self.session_count() > 1 {
                            let prefix = self.account_effective_name(slug);
                            req.title = format!("[{prefix}] {}", req.title);
                        }
                        let locked = sess.engine_state.borrow().conv_actions.locked_ids();
                        let hk = sess.engine_state.borrow().conv_actions.hidden_known;
                        let _ = sess.main_process.notifier().show_notification(
                            &req,
                            false,
                            self.window.is_visible(),
                            None,
                            0.0,
                            true,
                            hk,
                            &locked,
                            None,
                        );
                    }
                }
                if is_active {
                    let active = sess
                        .main_process
                        .call_coordinator()
                        .borrow()
                        .active_incoming
                        .clone();
                    self.push_ui_state(&json!({
                        "incomingCall": active.as_ref().map(incoming_call_to_ui_json),
                    }));
                }
            }
        }

        let old_hidden = sess.engine_state.borrow().conv_actions.hidden_ids.clone();
        let changed = sess.engine_state.borrow_mut().apply_zstate_patch(&payload);
        let new_hidden = sess.engine_state.borrow().conv_actions.hidden_ids.clone();

        // Clear any OS notifications belonging to conversations that just became hidden.
        let newly_hidden = NotificationPolicy::newly_hidden(&old_hidden, &new_hidden);
        if !newly_hidden.is_empty() {
            let notifier = sess.main_process.notifier().clone();
            self.tokio_handle.spawn(async move {
                for cid in newly_hidden {
                    notifier.clear_conversation(&cid).await;
                }
            });
        }

        if matches!(
            sess.engine_state.borrow().phase,
            EnginePhase::Ready | EnginePhase::Login { .. }
        ) {
            sess.signing_in.set(false);
        }

        // Duplicate-UID guard + persistent ProfileRegistry user identity update!
        let user_ident = {
            let st = sess.engine_state.borrow();
            st.user.as_ref().map(|u| {
                (
                    u.user_id.trim().to_string(),
                    u.display_name.trim().to_string(),
                    u.avatar.clone(),
                )
            })
        };
        if let Some((uid, dname, ava)) = user_ident.filter(|(u, _, _)| !u.is_empty()) {
            let dup_slug_opt = self
                .sessions
                .borrow()
                .iter()
                .find(|other| {
                    if other.slug == slug {
                        return false;
                    }
                    let other_uid_live = other
                        .engine_state
                        .borrow()
                        .user
                        .as_ref()
                        .map(|u| u.user_id.trim().to_string());
                    if other_uid_live.as_deref() == Some(uid.as_str()) {
                        return true;
                    }
                    other.main_process.has_session()
                        && self
                            .profile_registry
                            .borrow()
                            .get(&other.slug)
                            .and_then(|m| m.zalo_uid.as_deref())
                            == Some(uid.as_str())
                })
                .map(|other| other.slug.clone());

            if let Some(existing_slug) = dup_slug_opt {
                if slug != "default" {
                    self.show_workspace_toast("Tài khoản này đã đăng nhập ở tab khác");
                    self.switch_to_account(&existing_slug);
                    let app_dup = Rc::clone(self);
                    let dup_to_remove = slug.to_string();
                    glib::idle_add_local_once(move || {
                        app_dup.remove_account_session(&dup_to_remove);
                    });
                    return;
                }
            }

            let ident_changed = self.profile_registry.borrow_mut().update_identity(
                slug,
                Some(&uid),
                (!dname.is_empty()).then_some(dname.as_str()),
                ava.as_deref(),
            );
            if ident_changed {
                self.save_profile_registry();
                let updated_lbl = self
                    .profile_registry
                    .borrow()
                    .get(slug)
                    .and_then(|m| m.label.clone().or_else(|| m.display_name.clone()));
                self.multi_control_backend
                    .set_account_label(slug, updated_lbl);
                self.refresh_workspace_chrome();
            }
        }

        // Arm a fresh 100s expiry timer whenever `EnginePhase::Login`'s QR image transitions to a new value.
        let new_qr_seq = {
            let state = sess.engine_state.borrow();
            match &state.phase {
                EnginePhase::Login { qr: Some(q), .. } => {
                    let changed_qr = sess.last_qr.borrow().as_deref() != Some(q.as_str());
                    if changed_qr {
                        *sess.last_qr.borrow_mut() = Some(q.clone());
                        let next = sess.qr_seq.get().wrapping_add(1);
                        sess.qr_seq.set(next);
                        Some(next)
                    } else {
                        None
                    }
                }
                _ => {
                    *sess.last_qr.borrow_mut() = None;
                    None
                }
            }
        };
        if let Some(seq) = new_qr_seq {
            self.schedule_qr_expiry_timer(slug.to_string(), seq);
        }

        if changed.is_empty() {
            return;
        }

        sess.sync_engine_state_mirror();

        if changed.iter().any(|s| s == "messages") {
            self.evaluate_autoreply_for_session(&sess);
        }

        if changed.iter().any(|s| s == "conversations" || s == "conv_actions") {
            let unread = sess.engine_state.borrow().total_unread();
            sess.unread_count.set(unread);
            self.refresh_rail_item_visuals(slug);
            self.sync_accounts_to_all_webviews();
            self.refresh_total_unread_indicators();
        }

        if is_active {
            let ui_patch = {
                let state = sess.engine_state.borrow();
                let rt_prog = self.runtime_progress.borrow().clone();
                build_ui_patch_for_slices(
                    &state,
                    &changed,
                    rt_prog.as_ref(),
                    sess.signing_in.get(),
                )
            };

            if ui_patch.as_object().is_some_and(|m| !m.is_empty()) {
                self.push_ui_state(&ui_patch);
            }
        }
    }

    fn total_unread_across_sessions(&self) -> u64 {
        self.sessions
            .borrow()
            .iter()
            .map(|s| s.unread_count.get())
            .sum()
    }

    fn refresh_total_unread_indicators(&self) {
        let total_unread = self.total_unread_across_sessions();
        let notifier = self.active_session().main_process.notifier().clone();
        let tray_opt = self.tray_handle.borrow().clone();
        self.tokio_handle.spawn(async move {
            let _ = notifier.emit_unity_badge(total_unread).await;
            if let Some(tray) = tray_opt {
                tray.set_unread_count(total_unread).await;
            }
        });
    }

    fn apply_login_window_size(&self) {
        if self.demo_mode || self.session_count() >= 2 {
            return;
        }
        self.persist_window_geometry();
        self.in_login_mode.set(true);
        self.window_size_restored.set(false);
        if self.window.is_maximized() {
            self.window.unmaximize();
        }
        self.window.set_resizable(true);
        self.window.set_size_request(400, 560);
        self.window.set_default_size(400, 580);
        self.window.resize(400, 580);
    }

    fn apply_main_window_size(&self) {
        if self.demo_mode {
            return;
        }
        let multi = self.session_count() >= 2;
        let min_w = if multi { 1040 } else { MIN_MAIN_WINDOW_DIM };
        let min_h = if multi { 700 } else { MIN_MAIN_WINDOW_DIM };
        self.in_login_mode.set(false);
        if self.window_size_restored.replace(true) {
            self.window.set_size_request(min_w, min_h);
            let (cur_w, cur_h) = self.window.size();
            if cur_w < min_w || cur_h < min_h {
                self.window.resize(cur_w.max(min_w), cur_h.max(min_h));
            }
            return;
        }
        let st = self.saved_win_state.borrow().clone();
        let w = st.width.max(min_w);
        let h = st.height.max(min_h);
        self.window.set_resizable(true);
        self.window.set_size_request(min_w, min_h);
        self.window.set_default_size(w, h);
        self.window.resize(w, h);
        if let (Some(x), Some(y)) = (st.pos_x, st.pos_y) {
            if x != 0 || y != 0 {
                self.window.move_(x, y);
            }
        }
        if st.maximized {
            self.window.maximize();
        }
    }

    fn schedule_debounced_window_state_save(self: &Rc<Self>) {
        if let Some(prev) = self.win_state_save_source.borrow_mut().take() {
            prev.remove();
        }
        let app_weak = Rc::downgrade(self);
        let source_id = glib::timeout_add_local_once(Duration::from_millis(250), move || {
            if let Some(app) = app_weak.upgrade() {
                app.win_state_save_source.borrow_mut().take();
                app.persist_window_geometry();
            }
        });
        *self.win_state_save_source.borrow_mut() = Some(source_id);
    }

    fn persist_window_geometry(&self) {
        if let Some(pending) = self.win_state_save_source.borrow_mut().take() {
            pending.remove();
        }
        if self.demo_mode || self.in_login_mode.get() {
            return;
        }
        self.saved_win_state.borrow().save(&self.win_state_path);
    }

    fn hide_to_tray(&self) {
        self.persist_window_geometry();
        let sessions: Vec<Rc<AccountSession>> = self.sessions.borrow().clone();
        if !self.demo_mode && !self.installing_runtime.get() {
            // Keep all Zalo WebViews mapped inside their `offscreen_window` while hidden in the tray
            // so WebKit2GTK never throttles WebSocket frames, timers, or IndexedDB writes.
            for sess in &sessions {
                if sess.host.is_direct_ui() {
                    sess.unmount_to_offscreen();
                }
            }
        }
        self.window.hide();
        for sess in &sessions {
            sess.main_process
                .window_state()
                .is_visible
                .store(false, Ordering::Relaxed);
            sess.host
                .bridge()
                .send("visibility-change", vec![json!("hide")]);
            sess.host
                .bridge()
                .send("visibility-change", vec![json!("blur"), json!("1")]);
        }
    }

    pub fn present_window(&self) {
        self.suppress_renderer_auto_show.set(false);
        let sessions: Vec<Rc<AccountSession>> = self.sessions.borrow().clone();
        for sess in &sessions {
            sess.main_process
                .window_state()
                .is_visible
                .store(true, Ordering::Relaxed);
        }
        let was_visible = self.window.is_visible();
        let active = self.active_session();
        if !self.demo_mode && active.host.is_direct_ui() && !self.installing_runtime.get() {
            for sess in &sessions {
                sess.mount_into_page();
            }
            self.accounts_stack.set_visible_child_name(&active.slug);
            self.workspace_hbox.show_all();
            self.stack.set_visible_child_name("zalo");
            if !was_visible {
                if self.session_count() == 1 && active.page_mode.get() == WindowPageMode::Login {
                    self.apply_login_window_size();
                } else {
                    self.apply_main_window_size();
                }
            }
        }
        self.window.show_all();
        self.refresh_workspace_chrome();
        self.window.deiconify();
        self.window.present();
        if active.host.is_direct_ui() {
            active.host.webview().grab_focus();
        }
        active
            .host
            .bridge()
            .send("visibility-change", vec![json!("show")]);
        active
            .host
            .bridge()
            .send("visibility-change", vec![json!("focus"), json!("1")]);
    }

    fn evaluate_autoreply_for_session(&self, sess: &AccountSession) {
        if self.demo_mode {
            return;
        }
        let now_ms = unix_now_ms();
        let mut pending_replies: Vec<(String, String)> = Vec::new();
        {
            let state = sess.engine_state.borrow();
            let self_uid = state.user.as_ref().map(|u| u.user_id.clone());
            let group_map: HashMap<&str, bool> = state
                .conversations
                .iter()
                .map(|c| (c.id.as_str(), c.is_group))
                .collect();
            let mut seen = sess.autoreply_seen_ids.borrow_mut();

            for (conv_id, list) in &state.messages {
                let is_group = group_map
                    .get(conv_id.as_str())
                    .copied()
                    .unwrap_or_else(|| conv_id.starts_with('g'));
                for msg in list {
                    let key = (conv_id.clone(), msg.id.clone());
                    if !seen.insert(key) {
                        continue;
                    }
                    let msg_ts_ms = if msg.timestamp > 0 && msg.timestamp < 1_000_000_000_000 {
                        msg.timestamp.saturating_mul(1000)
                    } else {
                        msg.timestamp
                    };
                    if msg.is_mine || msg.recalled || msg_ts_ms < sess.boot_time_ms {
                        continue;
                    }
                    if let Some(rule) = sess.autoreply.evaluate_chat_message(
                        msg,
                        is_group,
                        self_uid.as_deref(),
                        now_ms,
                    ) {
                        pending_replies.push((conv_id.clone(), rule.reply_text));
                    }
                }
            }
        }

        for (conv_id, reply_text) in pending_replies {
            let _ = sess.outbound_send_tx.send(AccountOutboundSend {
                slug: sess.slug.clone(),
                conv_id,
                text: reply_text,
                reply_tx: None,
            });
        }
    }

    async fn execute_control_send_for(
        &self,
        slug: &str,
        conv_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let Some(sess) = self.session_by_slug(slug) else {
            anyhow::bail!("account profile `{slug}` is not open");
        };
        let cid = conv_id.trim();
        let body = text.trim();
        if cid.is_empty() || body.is_empty() {
            anyhow::bail!("conv_id and text must not be empty");
        }
        let cid_json = serde_json::to_string(cid)?;
        let txt_json = serde_json::to_string(body)?;
        let js = format!(
            "const cid = {cid_json};\n\
             const txt = {txt_json};\n\
             if (window.__zaloChat && typeof window.__zaloChat.sendText === 'function') {{\n\
               return await window.__zaloChat.sendText(cid, txt, null, []);\n\
             }}\n\
             if (window.__olazControl && typeof window.__olazControl.sendText === 'function') {{\n\
               return await window.__olazControl.sendText(cid, txt);\n\
             }}\n\
             return 'not-ready';"
        );
        let out = sess
            .host
            .evaluate(&js)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if out.as_str() == Some("sent") || out.as_bool() == Some(true) {
            Ok(())
        } else {
            anyhow::bail!("Zalo renderer returned `{out}` when sending message to `{cid}`")
        }
    }

    async fn execute_control_call_for(
        &self,
        slug: &str,
        target_id: &str,
        video: bool,
    ) -> anyhow::Result<Value> {
        let Some(sess) = self.session_by_slug(slug) else {
            anyhow::bail!("account profile `{slug}` is not open");
        };
        let tid = target_id.trim();
        if tid.is_empty() {
            anyhow::bail!("target_id must not be empty");
        }
        let tid_json = serde_json::to_string(tid)?;
        let js = format!(
            "const tid = {tid_json};\n\
             const video = {video};\n\
             if (window.__zaloCall && typeof window.__zaloCall.start === 'function') {{\n\
               return await window.__zaloCall.start(tid, video);\n\
             }}\n\
             if (window.__olazControl && typeof window.__olazControl.startCall === 'function') {{\n\
               return await window.__olazControl.startCall(tid, video);\n\
             }}\n\
             return 'fallback-native';"
        );
        if let Ok(out) = sess.host.evaluate(&js).await {
            let s = out.as_str().unwrap_or("");
            if out.as_bool() == Some(true) || s == "started" || s == "sent" {
                return Ok(json!({
                    "ok": true,
                    "target_id": tid,
                    "video": video,
                    "via": "renderer",
                }));
            }
        }

        // Fallback directly to the pure-Rust VoIP engine (`makeCall`).
        let payload = json!({
            "type": "request",
            "command": "makeCall",
            "data": {
                "calleeId": tid,
                "partner": [{ "id": tid, "name": tid }],
                "callType": if video { 2 } else { 1 },
            }
        });
        sess.main_process
            .call_engine()
            .handle_send_to_native(payload);
        Ok(json!({
            "ok": true,
            "target_id": tid,
            "video": video,
            "via": "call_engine",
        }))
    }

    pub fn handle_deep_link(&self, url: &str) {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return;
        }
        let active = self.active_session();
        active
            .host
            .bridge()
            .send("open-deep-link", vec![json!(trimmed)]);
        if let Ok(url_json) = serde_json::to_string(trimmed) {
            let js = format!(
                "if (window.__olazControl && typeof window.__olazControl.openDeepLink === 'function') {{ window.__olazControl.openDeepLink({url_json}); }}"
            );
            active.host.webview().evaluate_javascript(
                &js,
                None,
                None,
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }

    fn shutdown_and_quit(&self) {
        self.persist_window_geometry();
        if let Some(srv) = self._api_server.borrow_mut().take() {
            srv.stop();
        }
        let tray_opt = self.tray_handle.borrow_mut().take();
        if let Some(tray) = tray_opt {
            self.tokio_handle.spawn(async move {
                tray.shutdown().await;
            });
        }
        let sessions: Vec<Rc<AccountSession>> = self.sessions.borrow().clone();
        for sess in sessions {
            sess.shutdown();
        }
        gtk::main_quit();
    }

    fn full_ui_snapshot(&self) -> Value {
        let active = self.active_session();
        let state = active.engine_state.borrow();
        let rt_prog = self.runtime_progress.borrow().clone();
        let settings = self.settings.borrow().clone();
        let incoming = active
            .main_process
            .call_coordinator()
            .borrow()
            .active_incoming
            .clone();

        engine_state_to_ui_snapshot(
            &state,
            rt_prog.as_ref(),
            &settings,
            incoming.as_ref(),
            active.signing_in.get(),
        )
    }

    fn push_ui_state(&self, patch: &Value) {
        if !self.ui_ready.get() {
            return;
        }
        let Ok(serialized) = serde_json::to_string(patch) else {
            return;
        };
        let js = format!("window.__olazPushState && window.__olazPushState({serialized});");
        self.ui_webview
            .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
    }

    fn apply_desktop_theme(&self, theme: &DesktopTheme) {
        if !self.ui_ready.get() {
            return;
        }
        let Ok(serialized) = serde_json::to_string(theme) else {
            return;
        };
        let js = format!(
            "window.__olazApplyDesktopTheme && window.__olazApplyDesktopTheme({serialized});"
        );
        self.ui_webview
            .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
    }

    fn send_ui_reply(&self, id: &str, result: Result<Value, String>) {
        let id_json = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".to_string());
        let (ok, val_json) = match result {
            Ok(v) => (
                true,
                serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string()),
            ),
            Err(err) => (
                false,
                serde_json::to_string(&err).unwrap_or_else(|_| "\"Lỗi\"".to_string()),
            ),
        };
        let js = format!("window.__olazReply && window.__olazReply({id_json}, {ok}, {val_json});");
        self.ui_webview
            .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
    }

    async fn dispatch_ui_command(self: &Rc<Self>, cmd: &str, args: Value) -> Result<Value, String> {
        let session = self.active_session();
        // Refresh 60-second hidden-chat inactivity timer on user actions.
        session
            .engine_state
            .borrow_mut()
            .conv_actions
            .note_activity(Instant::now());

        match cmd {
            "__ui_log_error" => {
                log_ui_js_error(&args);
                Ok(json!({ "ok": true }))
            }

            "get_initial_state" => {
                if self.demo_mode {
                    Ok(Value::Null)
                } else {
                    Ok(self.full_ui_snapshot())
                }
            }

            "retry_runtime_install" => {
                self.start_runtime_installation();
                Ok(json!({ "ok": true }))
            }

            "add_account" => {
                let proxy = opt_str(&args, "proxyUrl").filter(|s| !s.trim().is_empty());
                self.add_new_account_tab(proxy);
                Ok(json!({ "ok": true, "activeSlug": self.active_slug() }))
            }

            "switch_account" => {
                let slug = req_str(&args, "slug")?;
                self.switch_to_account(&slug);
                Ok(json!({ "ok": true, "activeSlug": self.active_slug() }))
            }

            "remove_account" => {
                let slug = req_str(&args, "slug")?;
                self.remove_account_session(&slug);
                Ok(json!({ "ok": true, "activeSlug": self.active_slug() }))
            }

            "refresh_qr" => {
                session.signing_in.set(false);
                *session.last_qr.borrow_mut() = None;
                session.qr_seq.set(session.qr_seq.get().wrapping_add(1));
                {
                    let mut state = session.engine_state.borrow_mut();
                    let kickout_notice = match &state.phase {
                        EnginePhase::Login { kickout_notice, .. } => kickout_notice.clone(),
                        _ => None,
                    };
                    state.phase = EnginePhase::Login {
                        qr: None,
                        qr_expired: false,
                        waiting_confirm: None,
                        kickout_notice,
                        error_notice: None,
                    };
                }
                self.push_login_phase_patch(&session);
                session
                    .host
                    .load(HostPage::Login, &session.main_process.page_query(false));
                Ok(json!({ "ok": true }))
            }

            "logout" => {
                session.main_process.logout();
                Ok(json!({ "ok": true }))
            }

            "toggle_debug_webview" => {
                let visible = session.host.toggle_debug_window();
                Ok(json!({ "ok": true, "visible": visible }))
            }

            "update_settings" => {
                let next = {
                    let mut s = self.settings.borrow_mut();
                    s.apply_patch(&args);
                    s.save(&self.settings_path);
                    s.clone()
                };
                if next.omarchy_sync {
                    let theme = self.desktop_theme.borrow().clone();
                    self.apply_desktop_theme(&theme);
                }
                Ok(json!({ "ok": true, "settings": next }))
            }

            "transfer_start_manual" => {
                let _ = session
                    .host
                    .evaluate("return window.__zaloTransfer && await window.__zaloTransfer.start();")
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "transfer_accept" => {
                let phase = session
                    .engine_state
                    .borrow()
                    .transfer
                    .phase
                    .clone()
                    .unwrap_or_else(|| "offer".to_string());
                let phase_json = serde_json::to_string(&phase).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloTransfer && await window.__zaloTransfer.respond(true, {phase_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "transfer_reject" => {
                let phase = session
                    .engine_state
                    .borrow()
                    .transfer
                    .phase
                    .clone()
                    .unwrap_or_else(|| "offer".to_string());
                let phase_json = serde_json::to_string(&phase).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloTransfer && await window.__zaloTransfer.respond(false, {phase_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "transfer_cancel" => {
                let phase = session
                    .engine_state
                    .borrow()
                    .transfer
                    .phase
                    .clone()
                    .unwrap_or_else(|| "transferring".to_string());
                let phase_json = serde_json::to_string(&phase).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloTransfer && await window.__zaloTransfer.dismiss({phase_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "select_conversation" => {
                let conv_id = req_str(&args, "convId")?;
                let prev_id = {
                    let mut st = session.engine_state.borrow_mut();
                    let prev = st.open_conversation_id.clone();
                    st.open_conversation_id = Some(conv_id.clone());
                    prev
                };

                // Clear any desktop notifications for the newly opened conversation.
                {
                    let notifier = session.main_process.notifier().clone();
                    let cid_for_notif = conv_id.clone();
                    self.tokio_handle.spawn(async move {
                        notifier.clear_conversation(&cid_for_notif).await;
                    });
                }

                // Restore any stashed failed draft for this conversation.
                let restored_draft = session
                    .engine_state
                    .borrow_mut()
                    .failed_drafts
                    .take_with_notice(&conv_id);

                let prev_json = serde_json::to_string(&prev_id).unwrap();
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let js = format!(
                    "const prev = {prev_json};\n\
                     const cid = {cid_json};\n\
                     if (prev && prev !== cid && window.__zaloChat) {{ try {{ window.__zaloChat.close(prev); }} catch {{}} }}\n\
                     if (window.__zaloChat) {{\n\
                       await window.__zaloChat.open(cid);\n\
                       try {{ await window.__zaloChat.markRead(cid); }} catch {{}}\n\
                     }}\n\
                     if (window.__zaloMsgActions) {{ try {{ window.__zaloMsgActions.watchPins(cid); }} catch {{}} }}\n\
                     return true;"
                );
                let _ = session.host.evaluate(&js).await;

                let mut patch = json!({ "activeConvId": conv_id });
                if let Some((draft, show_notice)) = restored_draft {
                    if let Some(obj) = patch.as_object_mut() {
                        obj.insert(
                            "failedDraft".to_string(),
                            json!({
                                "convId": conv_id,
                                "text": draft.text,
                                "mentions": draft.mentions.iter().map(|m| json!({
                                    "uid": m.uid,
                                    "pos": m.pos,
                                    "len": m.len,
                                    "isAll": m.is_all,
                                })).collect::<Vec<_>>(),
                            }),
                        );
                        if show_notice {
                            obj.insert(
                                "toast".to_string(),
                                json!({ "text": "Đã khôi phục bản nháp chưa gửi thành công." }),
                            );
                        }
                    }
                }
                self.push_ui_state(&patch);
                Ok(json!({ "ok": true }))
            }

            "load_older" => {
                let conv_id = req_str(&args, "convId")?;
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let res = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloChat ? await window.__zaloChat.loadMore({cid_json}) : null;"
                    ))
                    .await?;
                Ok(json!({ "ok": true, "result": res }))
            }

            "conv_action" => {
                let conv_id = req_str(&args, "convId")?;
                let action = req_str(&args, "action")?;
                let pin = opt_str(&args, "pin");
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let pin_json = serde_json::to_string(&pin).unwrap();

                let js = match action.as_str() {
                    "pin" => format!("return await window.__zaloConvActions.pin({cid_json});"),
                    "unpin" => format!("return await window.__zaloConvActions.unpin({cid_json});"),
                    "archive" => {
                        format!("return await window.__zaloConvActions.archive({cid_json});")
                    }
                    "unarchive" => {
                        format!("return await window.__zaloConvActions.unarchive({cid_json});")
                    }
                    "mark_read" => {
                        format!("return await window.__zaloConvActions.markRead({cid_json});")
                    }
                    "mark_unread" => {
                        format!("return await window.__zaloConvActions.markUnread({cid_json});")
                    }
                    "delete" => format!(
                        "return await window.__zaloConvActions.deleteConversation({cid_json});"
                    ),
                    "hide" => {
                        let has_pin = session.engine_state.borrow().conv_actions.pin_set;
                        if has_pin {
                            format!(
                                "return await window.__zaloConvActions.hide({cid_json}, {pin_json} || '');"
                            )
                        } else {
                            format!(
                                "return await window.__zaloConvActions.hideWithNewPin({cid_json}, {pin_json} || '');"
                            )
                        }
                    }
                    "unhide" => format!(
                        "return await window.__zaloConvActions.unhide({cid_json}, {pin_json});"
                    ),
                    "call" | "video_call" => {
                        let is_video = action == "video_call";
                        format!(
                            "return window.__zaloCall && typeof window.__zaloCall.start === 'function' ? await window.__zaloCall.start({cid_json}, {is_video}) : false;"
                        )
                    }
                    _ => return Ok(json!({ "ok": true })),
                };

                let out = session.host.evaluate(&js).await?;
                if out.as_bool() == Some(false)
                    || matches!(out.as_str(), Some("error" | "missing"))
                {
                    return Err("Thao tác hội thoại không thành công.".to_string());
                }
                Ok(json!({ "ok": true, "result": out }))
            }

            "set_conv_label" => {
                let conv_id = req_str(&args, "convId")?;
                let label_id = opt_str(&args, "labelId").filter(|s| !s.is_empty());
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let lid_json = serde_json::to_string(&label_id).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloConversations && await window.__zaloConversations.setLabel({cid_json}, {lid_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "unlock_hidden_pin" => {
                let pin = req_str(&args, "pin")?;
                let pin_json = serde_json::to_string(&pin).unwrap();
                let res = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloConvActions && await window.__zaloConvActions.checkPin({pin_json});"
                    ))
                    .await?;
                let ok = res.as_bool().unwrap_or_else(|| {
                    res.get("ok").and_then(Value::as_bool).unwrap_or(false)
                });
                if !ok {
                    return Err("Mã PIN trò chuyện ẩn không chính xác.".to_string());
                }

                let now = Instant::now();
                {
                    let mut st = session.engine_state.borrow_mut();
                    let hidden_ids: Vec<String> = st.conv_actions.hidden_ids.iter().cloned().collect();
                    for cid in hidden_ids {
                        st.conv_actions.unlock(cid, now);
                    }
                }
                session.sync_engine_state_mirror();
                let patch = {
                    let st = session.engine_state.borrow();
                    build_ui_patch_for_slices(
                        &st,
                        &[
                            "conversations".to_string(),
                            "conv_actions".to_string(),
                        ],
                        None,
                        session.signing_in.get(),
                    )
                };
                self.push_ui_state(&patch);
                Ok(json!({ "ok": true }))
            }

            "manage_pin" => {
                let op = req_str(&args, "op")?;
                match op.as_str() {
                    "set" => {
                        let new_pin = opt_str(&args, "newPin")
                            .or_else(|| opt_str(&args, "pin"))
                            .unwrap_or_default();
                        let np_json = serde_json::to_string(&new_pin).unwrap();
                        let _ = session
                            .host
                            .evaluate(&format!(
                                "return await window.__zaloConvActions.setPin({np_json}, false);"
                            ))
                            .await?;
                    }
                    "change" => {
                        let old_pin = req_str(&args, "pin")?;
                        let new_pin = req_str(&args, "newPin")?;
                        let op_json = serde_json::to_string(&old_pin).unwrap();
                        let np_json = serde_json::to_string(&new_pin).unwrap();
                        let ok_check = session
                            .host
                            .evaluate(&format!(
                                "return await window.__zaloConvActions.checkPin({op_json});"
                            ))
                            .await?;
                        if ok_check.as_bool() == Some(false) {
                            return Err("Mã PIN hiện tại không đúng.".to_string());
                        }
                        let _ = session
                            .host
                            .evaluate(&format!(
                                "return await window.__zaloConvActions.setPin({np_json}, true);"
                            ))
                            .await?;
                    }
                    "reset" => {
                        let _ = session
                            .host
                            .evaluate("return await window.__zaloConvActions.resetPin();")
                            .await?;
                        session.engine_state.borrow_mut().conv_actions.relock();
                        session.sync_engine_state_mirror();
                    }
                    _ => {}
                }
                Ok(json!({ "ok": true }))
            }

            "send_text" => {
                let conv_id = req_str(&args, "convId")?;
                let text = req_str(&args, "text")?;
                let trimmed = text.trim().to_string();
                if trimmed.is_empty() {
                    return Ok(json!({ "ok": true }));
                }

                let quote_id = args
                    .get("replyTo")
                    .and_then(|r| r.get("id").or_else(|| r.get("msgId")))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);

                let parsed_mentions: Vec<Mention> = args
                    .get("mentions")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                let uid = m.get("uid")?.as_str()?.to_string();
                                let pos = m.get("pos")?.as_u64()? as usize;
                                let len = m.get("len")?.as_u64()? as usize;
                                let is_all =
                                    m.get("isAll").and_then(Value::as_bool).unwrap_or(false);
                                Some(Mention {
                                    uid,
                                    pos,
                                    len,
                                    is_all,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let wire_mentions: Vec<Value> = parsed_mentions
                    .iter()
                    .map(|m| {
                        json!({
                            "pos": m.pos,
                            "len": m.len,
                            "uid": m.uid,
                            "type": if m.is_all { 1 } else { 0 },
                        })
                    })
                    .collect();

                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let txt_json = serde_json::to_string(&trimmed).unwrap();
                let qid_json = serde_json::to_string(&quote_id).unwrap();
                let men_json = serde_json::to_string(&wire_mentions).unwrap();

                let send_res = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloChat && await window.__zaloChat.sendText({cid_json}, {txt_json}, {qid_json}, {men_json});"
                    ))
                    .await;

                let sent_ok = matches!(&send_res, Ok(v) if v.as_str() == Some("sent") || v.as_bool() == Some(true));
                if !sent_ok {
                    // Guard 2: Stash unsent draft in `FailedDraftStore` and push back to UI without clobbering IME!
                    let draft = ComposerDraft {
                        text: trimmed.clone(),
                        mentions: parsed_mentions.clone(),
                        reply_to: None,
                    };
                    session
                        .engine_state
                        .borrow_mut()
                        .failed_drafts
                        .stash(&conv_id, draft);
                    if let Some((restored, _)) = session
                        .engine_state
                        .borrow_mut()
                        .failed_drafts
                        .take_with_notice(&conv_id)
                    {
                        self.push_ui_state(&json!({
                            "failedDraft": {
                                "convId": conv_id,
                                "text": restored.text,
                                "mentions": restored.mentions.iter().map(|m| json!({
                                    "uid": m.uid,
                                    "pos": m.pos,
                                    "len": m.len,
                                    "isAll": m.is_all,
                                })).collect::<Vec<_>>(),
                            }
                        }));
                    }
                    let err_detail = send_res
                        .err()
                        .unwrap_or_else(|| "Không thể gửi tin nhắn lúc này.".to_string());
                    return Err(err_detail);
                }

                Ok(json!({ "ok": true }))
            }

            "send_typing" => Ok(json!({ "ok": true })),

            "paste_clipboard_image" => {
                let conv_id = req_str(&args, "convId")?;
                let b64 = req_str(&args, "base64Png")?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .map_err(|e| format!("Dữ liệu ảnh dán không hợp lệ: {e}"))?;

                let paste_dir = session.platform.temp_dir().join("pasted");
                std::fs::create_dir_all(&paste_dir)
                    .map_err(|e| format!("Không tạo được thư mục tạm: {e}"))?;
                let file_path = paste_dir.join(format!("clipboard-{}.png", uuid::Uuid::new_v4()));
                std::fs::write(&file_path, bytes)
                    .map_err(|e| format!("Không ghi được ảnh tạm: {e}"))?;

                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let _ = session
                    .host
                    .evaluate_providing_files(
                        &format!(
                            "return window.__zaloCompose && await window.__zaloCompose.sendMedia({cid_json});"
                        ),
                        &[file_path],
                    )
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "pick_and_send_media" | "pick_and_send_files" => {
                let conv_id = req_str(&args, "convId")?;
                let media_only = cmd == "pick_and_send_media";
                let title = if media_only {
                    "Chọn ảnh hoặc video để gửi"
                } else {
                    "Chọn tệp đính kèm để gửi"
                };

                let (tx, rx) = oneshot::channel();
                self.tokio_handle.spawn(async move {
                    let _ = tx.send(pick_open_files(title, true, media_only).await);
                });
                let picked = rx
                    .await
                    .map_err(|_| "Hộp thoại chọn tệp bị gián đoạn.".to_string())?
                    .map_err(|e| format!("Không mở được XDG FileChooser Portal: {e:#}"))?;
                if picked.is_empty() {
                    return Ok(json!({ "ok": true, "cancelled": true }));
                }

                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let fn_name = if media_only { "sendMedia" } else { "sendFiles" };
                let _ = session
                    .host
                    .evaluate_providing_files(
                        &format!(
                            "return window.__zaloCompose && await window.__zaloCompose.{fn_name}({cid_json});"
                        ),
                        &picked,
                    )
                    .await?;
                Ok(json!({ "ok": true, "count": picked.len() }))
            }

            "send_dropped_paths" => {
                let conv_id = req_str(&args, "convId")?;
                let paths: Vec<PathBuf> = args
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(PathBuf::from)
                            .filter(|p| p.is_file())
                            .collect()
                    })
                    .unwrap_or_default();
                if paths.is_empty() {
                    return Ok(json!({ "ok": true }));
                }
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let _ = session
                    .host
                    .evaluate_providing_files(
                        &format!(
                            "return window.__zaloCompose && await window.__zaloCompose.sendFiles({cid_json});"
                        ),
                        &paths,
                    )
                    .await?;
                Ok(json!({ "ok": true, "count": paths.len() }))
            }

            "capture_screenshot" => {
                let to_my_cloud = args
                    .get("toMyCloud")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let conv_id = opt_str(&args, "convId")
                    .filter(|s| !s.is_empty())
                    .or_else(|| session.engine_state.borrow().self_conversation_id.clone())
                    .unwrap_or_default();

                let (tx, rx) = oneshot::channel();
                self.tokio_handle.spawn(async move {
                    let _ = tx.send(capture_interactive_screenshot().await);
                });
                let Some(staged_path) = rx
                    .await
                    .map_err(|_| "Chụp màn hình bị gián đoạn.".to_string())?
                    .map_err(|e| format!("Không thể chụp màn hình qua XDG Portal: {e:#}"))?
                else {
                    return Ok(json!({ "ok": true, "cancelled": true }));
                };

                // Ensure the staged file sits in `SCREENSHOT_STAGING_DIR` (`0700` dir / `0600` file).
                let final_shot = if staged_path.starts_with(SCREENSHOT_STAGING_DIR) {
                    staged_path
                } else {
                    stage_screenshot_file(&staged_path, Path::new(SCREENSHOT_STAGING_DIR))
                        .map_err(|e| e.to_string())?
                };

                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let _ = session
                    .host
                    .evaluate_providing_files(
                        &format!(
                            "return window.__zaloCompose && await window.__zaloCompose.sendShot({cid_json}, {to_my_cloud});"
                        ),
                        &[final_shot],
                    )
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "send_sticker" => {
                let conv_id = req_str(&args, "convId")?;
                let sticker_val = args
                    .get("sticker")
                    .cloned()
                    .unwrap_or_else(|| json!({ "id": args.get("stickerId").cloned().unwrap_or(Value::Null) }));
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let stk_json = serde_json::to_string(&sticker_val).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloSticker && await window.__zaloSticker.send({cid_json}, {stk_json}, 'category');"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "query_sticker_suggestions" => {
                let conv_id = opt_str(&args, "convId").unwrap_or_default();
                let keyword = opt_str(&args, "keyword").unwrap_or_default();
                if keyword.trim().is_empty() || !self.settings.borrow().sticker_suggest {
                    return Ok(json!({ "ok": true, "items": [] }));
                }
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let kw_json = serde_json::to_string(&keyword).unwrap();
                let items = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloSticker ? await window.__zaloSticker.suggest({cid_json}, {kw_json}) : [];"
                    ))
                    .await
                    .unwrap_or_else(|_| json!([]));
                self.push_ui_state(&json!({
                    "stickers": { "suggestions": items }
                }));
                Ok(json!({ "ok": true, "items": items }))
            }

            "react_message" => {
                let conv_id = req_str(&args, "convId")?;
                let msg_id = req_str(&args, "msgId")?;
                let reaction = req_str(&args, "reaction")?;
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let mid_json = serde_json::to_string(&msg_id).unwrap();
                let rxn_json = serde_json::to_string(&reaction).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloChat && await window.__zaloChat.react({cid_json}, {mid_json}, {rxn_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "recall_message" => {
                let conv_id = req_str(&args, "convId")?;
                let msg_id = req_str(&args, "msgId")?;
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let mid_json = serde_json::to_string(&msg_id).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloMsgActions && await window.__zaloMsgActions.recall({cid_json}, {mid_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "delete_message_for_me" => {
                let conv_id = req_str(&args, "convId")?;
                let msg_id = req_str(&args, "msgId")?;
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let mid_json = serde_json::to_string(&msg_id).unwrap();
                let res = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloMsgActions && await window.__zaloMsgActions.deleteForMe({cid_json}, {mid_json}, true);"
                    ))
                    .await?;
                if let Some(uid) = res
                    .get("undoId")
                    .or_else(|| res.get("id"))
                    .and_then(Value::as_str)
                {
                    *self.last_delete_undo_id.borrow_mut() = Some(uid.to_string());
                }
                self.push_ui_state(&json!({
                    "toast": {
                        "text": "Đã xóa tin nhắn ở phía bạn.",
                        "undo": { "convId": conv_id, "msgId": msg_id }
                    }
                }));
                Ok(json!({ "ok": true }))
            }

            "undo_delete" => {
                let undo_id = opt_str(&args, "undoId")
                    .or_else(|| self.last_delete_undo_id.borrow_mut().take())
                    .unwrap_or_default();
                if !undo_id.is_empty() {
                    let uid_json = serde_json::to_string(&undo_id).unwrap();
                    let _ = session
                        .host
                        .evaluate(&format!(
                            "return window.__zaloMsgActions && await window.__zaloMsgActions.undoDelete({uid_json});"
                        ))
                        .await?;
                }
                Ok(json!({ "ok": true }))
            }

            "forward_messages" => {
                let from_conv = req_str(&args, "fromConvId")?;
                let msg_ids: Vec<String> = args
                    .get("msgIds")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let targets: Vec<String> = args
                    .get("targetConvIds")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();

                let cid_json = serde_json::to_string(&from_conv).unwrap();
                let targets_json = serde_json::to_string(&targets).unwrap();
                for mid in msg_ids {
                    let mid_json = serde_json::to_string(&mid).unwrap();
                    let _ = session
                        .host
                        .evaluate(&format!(
                            "return window.__zaloMsgActions && await window.__zaloMsgActions.forward({cid_json}, {mid_json}, {targets_json});"
                        ))
                        .await?;
                }
                Ok(json!({ "ok": true }))
            }

            "pin_message" => {
                let conv_id = req_str(&args, "convId")?;
                let msg_id = req_str(&args, "msgId")?;
                let replacing = opt_str(&args, "replacingRef");
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let mid_json = serde_json::to_string(&msg_id).unwrap();
                let rep_json = serde_json::to_string(&replacing).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloMsgActions && await window.__zaloMsgActions.pin({cid_json}, {mid_json}, {rep_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "unpin_message" => {
                let conv_id = req_str(&args, "convId")?;
                let pin_ref = opt_str(&args, "ref")
                    .or_else(|| opt_str(&args, "msgId"))
                    .unwrap_or_default();
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let ref_json = serde_json::to_string(&pin_ref).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloMsgActions && await window.__zaloMsgActions.unpin({cid_json}, {ref_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "star_message" => {
                let conv_id = req_str(&args, "convId")?;
                let msg_id = req_str(&args, "msgId")?;
                let starred = args
                    .get("starred")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let mid_json = serde_json::to_string(&msg_id).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloMsgActions && await window.__zaloMsgActions.star({cid_json}, {mid_json}, {starred});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "resolve_media" => {
                let conv_id = req_str(&args, "convId")?;
                let msg_id = req_str(&args, "msgId")?;
                let kind = opt_str(&args, "kind").unwrap_or_else(|| "file".to_string());
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let mid_json = serde_json::to_string(&msg_id).unwrap();
                let kind_json = serde_json::to_string(&kind).unwrap();
                let out = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloChat && await window.__zaloChat.resolveMedia({cid_json}, {mid_json}, {kind_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true, "result": out }))
            }

            "open_downloaded_file" => {
                let raw_path = req_str(&args, "path")?;
                let display_name = opt_str(&args, "name").unwrap_or_default();
                let reveal_requested = args
                    .get("revealOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                let revealed = open_local_path_safely(
                    Path::new(&raw_path),
                    &display_name,
                    reveal_requested,
                );
                Ok(json!({ "ok": true, "revealed": revealed }))
            }

            "open_media_external" => {
                let raw_url = opt_str(&args, "url")
                    .or_else(|| opt_str(&args, "path"))
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "Thiếu tham số bắt buộc `url`".to_string())?;
                let display_name = opt_str(&args, "name").unwrap_or_default();
                let allowed_roots = ui_media_allowed_roots(&session.platform);

                if raw_url.starts_with('/') {
                    let confined =
                        validate_confined_media_file(Path::new(&raw_url), &allowed_roots)?;
                    let revealed = open_local_path_safely(&confined, &display_name, false);
                    return Ok(json!({ "ok": true, "revealed": revealed }));
                }

                let parsed = Url::parse(&raw_url)
                    .map_err(|_| "Liên kết phương tiện không hợp lệ.".to_string())?;
                match parsed.scheme() {
                    "file" => {
                        let file_path = parsed
                            .to_file_path()
                            .map_err(|()| "Đường dẫn tệp không hợp lệ.".to_string())?;
                        let confined = validate_confined_media_file(&file_path, &allowed_roots)?;
                        let revealed = open_local_path_safely(&confined, &display_name, false);
                        Ok(json!({ "ok": true, "revealed": revealed }))
                    }
                    OLAZ_MEDIA_SCHEME => {
                        let host = parsed.host_str().unwrap_or("");
                        let path_seg = parsed.path().trim_matches('/');
                        let mode = if !host.is_empty() { host } else { path_seg };
                        match mode {
                            "file" => {
                                let file_path = parsed
                                    .query_pairs()
                                    .find(|(k, _)| k == "path")
                                    .map(|(_, v)| v.into_owned())
                                    .ok_or_else(|| "Thiếu tham số `path`.".to_string())?;
                                let confined = validate_confined_media_file(
                                    Path::new(&file_path),
                                    &allowed_roots,
                                )?;
                                let revealed =
                                    open_local_path_safely(&confined, &display_name, false);
                                Ok(json!({ "ok": true, "revealed": revealed }))
                            }
                            "cdn" => {
                                let remote_url = parsed
                                    .query_pairs()
                                    .find(|(k, _)| k == "url")
                                    .map(|(_, v)| v.into_owned())
                                    .ok_or_else(|| "Thiếu tham số `url`.".to_string())?;
                                self.open_remote_or_cached_media(
                                    &session,
                                    &remote_url,
                                    &display_name,
                                    &allowed_roots,
                                    true,
                                )
                                .await
                            }
                            _ => Err("Nguồn phương tiện không được hỗ trợ.".to_string()),
                        }
                    }
                    "https" | "http" => {
                        let is_zalo_cdn = parsed.host_str().is_some_and(|h| {
                            let lower = h.to_ascii_lowercase();
                            lower.ends_with(".zdn.vn") || lower.ends_with(".zadn.vn")
                        });
                        self.open_remote_or_cached_media(
                            &session,
                            &raw_url,
                            &display_name,
                            &allowed_roots,
                            is_zalo_cdn,
                        )
                        .await
                    }
                    _ => Err("Giao thức liên kết không được hỗ trợ.".to_string()),
                }
            }

            "load_contacts" => {
                let _ = session
                    .host
                    .evaluate("return window.__zaloContacts && await window.__zaloContacts.refresh();")
                    .await;
                Ok(json!({ "ok": true }))
            }

            "friend_action" => {
                let action = req_str(&args, "action")?;
                let uid = req_str(&args, "userId")?;
                let uid_json = serde_json::to_string(&uid).unwrap();
                let js = match action.as_str() {
                    "accept" => format!("return await window.__zaloContacts.accept({uid_json});"),
                    "decline" => format!("return await window.__zaloContacts.decline({uid_json});"),
                    "cancel" => format!("return await window.__zaloContacts.cancel({uid_json});"),
                    "send" => {
                        let msg = opt_str(&args, "message").unwrap_or_default();
                        let msg_json = serde_json::to_string(&msg).unwrap();
                        format!(
                            "return await window.__zaloContacts.sendRequest({uid_json}, {msg_json});"
                        )
                    }
                    _ => return Ok(json!({ "ok": true })),
                };
                let _ = session.host.evaluate(&js).await?;
                Ok(json!({ "ok": true }))
            }

            "search_phone" => {
                let phone = opt_str(&args, "phone").unwrap_or_default();
                let trimmed = phone.trim();
                if trimmed.is_empty() {
                    let _ = session
                        .host
                        .evaluate("return window.__zaloContacts && window.__zaloContacts.resetSearch();")
                        .await;
                } else {
                    let ph_json = serde_json::to_string(trimmed).unwrap();
                    let _ = session
                        .host
                        .evaluate(&format!(
                            "return window.__zaloContacts && await window.__zaloContacts.search({ph_json});"
                        ))
                        .await?;
                }
                Ok(json!({ "ok": true }))
            }

            "create_group" => {
                let name = opt_str(&args, "name").unwrap_or_default();
                let ids: Vec<String> = args
                    .get("memberIds")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let spec = json!({ "name": name, "ids": ids });
                let spec_json = serde_json::to_string(&spec).unwrap();
                let out = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloGroupCreate && await window.__zaloGroupCreate.create({spec_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true, "result": out }))
            }

            "create_poll" => {
                let conv_id = req_str(&args, "convId")?;
                let spec = json!({
                    "question": opt_str(&args, "question").unwrap_or_default(),
                    "options": args.get("options").cloned().unwrap_or_else(|| json!([])),
                    "multi": args.get("multi").and_then(Value::as_bool).unwrap_or(true),
                    "allowAdd": args.get("allowAdd").and_then(Value::as_bool).unwrap_or(true),
                    "anonymous": args.get("anonymous").and_then(Value::as_bool).unwrap_or(false),
                });
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let spec_json = serde_json::to_string(&spec).unwrap();
                let out = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloPoll && await window.__zaloPoll.create({cid_json}, {spec_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true, "result": out }))
            }

            "vote_poll" => {
                let conv_id = opt_str(&args, "convId")
                    .or_else(|| session.engine_state.borrow().open_conversation_id.clone())
                    .unwrap_or_default();
                let poll_id = req_str(&args, "pollId")?;
                let voted_ids = args
                    .get("votedOptionIds")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                let new_options = args
                    .get("newOptions")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let pid_json = serde_json::to_string(&poll_id).unwrap();
                let new_json = serde_json::to_string(&new_options).unwrap();
                let voted_json = serde_json::to_string(&voted_ids).unwrap();
                let _ = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloPoll && await window.__zaloPoll.vote({cid_json}, {pid_json}, {new_json}, {voted_json});"
                    ))
                    .await?;
                Ok(json!({ "ok": true }))
            }

            "load_zcloud" => {
                let _ = session
                    .host
                    .evaluate("return window.__zaloCloud && await window.__zaloCloud.refresh();")
                    .await;
                Ok(json!({ "ok": true }))
            }

            "load_shared_media" => {
                let conv_id = opt_str(&args, "convId")
                    .or_else(|| session.engine_state.borrow().open_conversation_id.clone())
                    .unwrap_or_default();
                let kind = opt_str(&args, "kind").unwrap_or_else(|| "images".to_string());
                let cursor = args.get("cursor").cloned().unwrap_or(Value::Null);
                if conv_id.is_empty() {
                    return Ok(json!({ "ok": true }));
                }
                let cid_json = serde_json::to_string(&conv_id).unwrap();
                let kind_json = serde_json::to_string(&kind).unwrap();
                let cur_json = serde_json::to_string(&cursor).unwrap();
                let page = session
                    .host
                    .evaluate(&format!(
                        "return window.__zaloMediaStore ? await window.__zaloMediaStore.page({cid_json}, {kind_json}, {cur_json}, 40) : null;"
                    ))
                    .await
                    .unwrap_or(Value::Null);
                if let Some(items) = page.get("items").or_else(|| page.as_array().map(|_| &page)) {
                    let mut bucket = Map::new();
                    bucket.insert(kind, items.clone());
                    let mut by_conv = Map::new();
                    by_conv.insert(conv_id, Value::Object(bucket));
                    self.push_ui_state(&json!({ "sharedMediaByConv": by_conv }));
                }
                Ok(json!({ "ok": true, "page": page }))
            }

            _ => Ok(json!({ "ok": true })),
        }
    }

    async fn open_remote_or_cached_media(
        &self,
        session: &AccountSession,
        remote_url: &str,
        display_name: &str,
        allowed_roots: &[PathBuf],
        prefer_local_cache: bool,
    ) -> Result<Value, String> {
        if prefer_local_cache {
            let loader = session.main_process.media_loader().clone();
            let remote_owned = remote_url.to_string();
            let (tx, rx) = oneshot::channel();
            self.tokio_handle.spawn(async move {
                let _ = tx.send(loader.local_file(&remote_owned).await);
            });
            if let Ok(Ok(local_path)) = rx.await {
                if let Ok(confined) = validate_confined_media_file(&local_path, allowed_roots) {
                    let revealed = open_local_path_safely(&confined, display_name, false);
                    return Ok(json!({ "ok": true, "revealed": revealed }));
                }
            }
        }

        if remote_url.starts_with("https://") || remote_url.starts_with("http://") {
            gio::AppInfo::launch_default_for_uri(remote_url, gio::AppLaunchContext::NONE)
                .map_err(|e| format!("Không mở được liên kết ngoài: {e}"))?;
            Ok(json!({ "ok": true }))
        } else {
            Err("Không mở được phương tiện từ xa.".to_string())
        }
    }
}

// ============================================================================
// UI WebContext (`olaz-ui://` + `olaz-media://`)
// ============================================================================

fn ui_media_allowed_roots(platform: &ZaloPlatform) -> Vec<PathBuf> {
    vec![
        platform.user_data().to_path_buf(),
        platform.cache_dir().to_path_buf(),
        platform.temp_dir().to_path_buf(),
        platform.electron_path("downloads"),
        PathBuf::from(SCREENSHOT_STAGING_DIR),
    ]
}

fn build_ui_web_context(
    platform: &ZaloPlatform,
    media_loader: MediaLoader,
    tokio_handle: tokio::runtime::Handle,
) -> WebContext {
    let ui_data = platform.webkit_dir().join("ui-data");
    let ui_cache = platform.webkit_dir().join("ui-cache");
    let _ = std::fs::create_dir_all(&ui_data);
    let _ = std::fs::create_dir_all(&ui_cache);

    let allowed_roots = ui_media_allowed_roots(platform);
    for root in &allowed_roots {
        let _ = std::fs::create_dir_all(root);
    }

    let data_manager = WebsiteDataManager::builder()
        .base_data_directory(ui_data.to_string_lossy().as_ref())
        .base_cache_directory(ui_cache.to_string_lossy().as_ref())
        .build();
    if let Some(proxy_url) = platform.proxy_url() {
        let mut proxy_settings =
            NetworkProxySettings::new(Some(proxy_url), &["localhost", "127.0.0.1", "::1"]);
        data_manager
            .set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut proxy_settings));
    }

    let context = WebContext::with_website_data_manager(&data_manager);
    if let Some(sec) = context.security_manager() {
        for scheme in [OLAZ_UI_SCHEME, OLAZ_MEDIA_SCHEME] {
            sec.register_uri_scheme_as_secure(scheme);
            sec.register_uri_scheme_as_cors_enabled(scheme);
        }
    }

    context.register_uri_scheme(OLAZ_UI_SCHEME, |request| {
        handle_olaz_ui_scheme(request);
    });

    let roots_arc = Arc::new(allowed_roots);
    context.register_uri_scheme(OLAZ_MEDIA_SCHEME, move |request| {
        handle_olaz_media_scheme(request, &media_loader, &roots_arc, &tokio_handle);
    });

    context
}

fn handle_olaz_ui_scheme(request: &URISchemeRequest) {
    let raw_uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
    let path = Url::parse(&raw_uri)
        .ok()
        .map(|u| u.path().to_string())
        .or_else(|| request.path().map(|p| p.to_string()))
        .unwrap_or_default();

    let clean = path.trim_start_matches('/');
    let asset: Option<(&'static str, &'static [u8])> = match clean {
        "" | "index.html" => Some(("text/html; charset=utf-8", ASSET_INDEX_HTML)),
        "styles.css" => Some(("text/css; charset=utf-8", ASSET_STYLES_CSS)),
        "app.js" => Some(("text/javascript; charset=utf-8", ASSET_APP_JS)),
        "icons.js" => Some(("text/javascript; charset=utf-8", ASSET_ICONS_JS)),
        "components.js" => Some(("text/javascript; charset=utf-8", ASSET_COMPONENTS_JS)),
        "demo.js" => Some(("text/javascript; charset=utf-8", ASSET_DEMO_JS)),
        _ => None,
    };

    match asset {
        Some((mime, bytes)) => {
            finish_static_response(request, 200, "OK", mime, bytes.to_vec());
        }
        None => {
            finish_static_response(
                request,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            );
        }
    }
}

fn handle_olaz_media_scheme(
    request: &URISchemeRequest,
    media_loader: &MediaLoader,
    allowed_roots: &Arc<Vec<PathBuf>>,
    tokio_handle: &tokio::runtime::Handle,
) {
    let raw_uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
    let Ok(parsed) = Url::parse(&raw_uri) else {
        finish_static_response(
            request,
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"invalid olaz-media URI".to_vec(),
        );
        return;
    };

    let range_header = request
        .http_headers()
        .and_then(|h| h.one("Range"))
        .map(|g| g.to_string());

    let host = parsed.host_str().unwrap_or("");
    let path_seg = parsed.path().trim_matches('/');
    let mode = if !host.is_empty() { host } else { path_seg };

    match mode {
        "file" => {
            let Some(file_path) = parsed
                .query_pairs()
                .find(|(k, _)| k == "path")
                .map(|(_, v)| v.into_owned())
            else {
                finish_static_response(
                    request,
                    400,
                    "Bad Request",
                    "text/plain; charset=utf-8",
                    b"missing path query parameter".to_vec(),
                );
                return;
            };
            match serve_local_media_range(
                Path::new(&file_path),
                range_header.as_deref(),
                allowed_roots,
            ) {
                Ok(resp) => finish_media_range_response(request, resp),
                Err(err) => {
                    tracing::debug!(%err, path = %file_path, "denied or missing olaz-media://file");
                    finish_static_response(
                        request,
                        404,
                        "Not Found",
                        "text/plain; charset=utf-8",
                        b"media file unavailable".to_vec(),
                    );
                }
            }
        }

        "cdn" => {
            let Some(remote_url) = parsed
                .query_pairs()
                .find(|(k, _)| k == "url")
                .map(|(_, v)| v.into_owned())
            else {
                finish_static_response(
                    request,
                    400,
                    "Bad Request",
                    "text/plain; charset=utf-8",
                    b"missing url query parameter".to_vec(),
                );
                return;
            };

            let req_owned = request.clone();
            let loader = media_loader.clone();
            let roots = Arc::clone(allowed_roots);
            let (tx, rx) = oneshot::channel();
            tokio_handle.spawn(async move {
                let _ = tx.send(loader.local_file(&remote_url).await);
            });

            glib::MainContext::default().spawn_local(async move {
                match rx.await {
                    Ok(Ok(local_path)) => {
                        match serve_local_media_range(&local_path, range_header.as_deref(), &roots)
                        {
                            Ok(resp) => finish_media_range_response(&req_owned, resp),
                            Err(err) => {
                                tracing::warn!(%err, "failed serving cached CDN media");
                                finish_static_response(
                                    &req_owned,
                                    500,
                                    "Internal Server Error",
                                    "text/plain; charset=utf-8",
                                    b"media read error".to_vec(),
                                );
                            }
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::debug!(%err, "CDN media download failed");
                        finish_static_response(
                            &req_owned,
                            502,
                            "Bad Gateway",
                            "text/plain; charset=utf-8",
                            err.to_string().into_bytes(),
                        );
                    }
                    Err(_) => {
                        finish_static_response(
                            &req_owned,
                            504,
                            "Gateway Timeout",
                            "text/plain; charset=utf-8",
                            b"media request cancelled".to_vec(),
                        );
                    }
                }
            });
        }

        _ => {
            finish_static_response(
                request,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"unknown olaz-media endpoint".to_vec(),
            );
        }
    }
}

fn finish_static_response(
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

fn finish_media_range_response(request: &URISchemeRequest, media: MediaRangeResponse) {
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

fn configure_ui_navigation_policy(webview: &WebView) {
    webview.connect_decide_policy(|_, decision, decision_type| {
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

        if matches!(
            scheme.as_str(),
            OLAZ_UI_SCHEME | OLAZ_MEDIA_SCHEME | "about" | "blob" | "data"
        ) {
            decision.use_();
            return true;
        }

        if matches!(scheme.as_str(), "http" | "https") {
            let _ = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE);
        }
        decision.ignore();
        true
    });
}

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("Thiếu tham số bắt buộc `{key}`"))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn log_ui_js_error(args: &Value) {
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown UI JS error");
    let source = args.get("source").and_then(Value::as_str).unwrap_or("");
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("");
    tracing::error!(
        target: "olaz::ui_js",
        %message,
        %source,
        %stack,
        "uncaught JavaScript error in Olaz UI webview"
    );
}

fn validate_confined_media_file(
    candidate: &Path,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(candidate)
        .map_err(|_| "Tệp phương tiện không tồn tại.".to_string())?;
    if !canonical.is_file() {
        return Err("Đường dẫn không phải tệp hợp lệ.".to_string());
    }
    let allowed = allowed_roots.iter().any(|root| {
        std::fs::canonicalize(root)
            .is_ok_and(|canon_root| canonical != canon_root && canonical.starts_with(&canon_root))
    });
    if !allowed {
        return Err("Đường dẫn tệp nằm ngoài vùng cho phép.".to_string());
    }
    Ok(canonical)
}

fn open_local_path_safely(target_path: &Path, display_name: &str, reveal_requested: bool) -> bool {
    // Security rule (`FileCard::may_execute_either`): NEVER launch executable/launcher extensions directly;
    // always open the parent directory instead!
    let raw_path = target_path.to_string_lossy();
    let must_reveal_only =
        reveal_requested || FileCard::may_execute_either(raw_path.as_ref(), display_name);
    let open_target = if must_reveal_only {
        target_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| target_path.to_path_buf())
    } else {
        target_path.to_path_buf()
    };
    if let Ok(uri) = Url::from_file_path(&open_target) {
        let _ = gio::AppInfo::launch_default_for_uri(uri.as_str(), gio::AppLaunchContext::NONE);
    }
    must_reveal_only
}

// ============================================================================
// Pure `ZaloEngineState` -> UI Store Serialization (Unit-Tested Without GTK)
// ============================================================================

/// Serializes the full [`ZaloEngineState`] into the exact JSON store contract consumed by
/// `crates/olaz-app/ui/{app.js, components.js}`.
pub fn engine_state_to_ui_snapshot(
    state: &ZaloEngineState,
    runtime_progress: Option<&UiRuntimeProgress>,
    settings: &UiSettings,
    incoming_call: Option<&IncomingVoipCall>,
    signing_in: bool,
) -> Value {
    let (phase_str, login_val) = serialize_phase_and_login(&state.phase, signing_in);

    let mut messages_by_conv = Map::new();
    for (cid, msgs) in &state.messages {
        if state.conv_actions.is_locked(cid) {
            continue;
        }
        let has_more = state.has_more_history.get(cid).copied().unwrap_or(false);
        let items: Vec<Value> = msgs.iter().map(serialize_chat_message).collect();
        messages_by_conv.insert(cid.clone(), json!({ "items": items, "hasMore": has_more }));
    }

    json!({
        "phase": phase_str,
        "runtimeProgress": runtime_progress,
        "login": login_val,
        "capabilities": {
            "gstAudioSink": has_gstreamer_audio_sink(),
        },
        "user": state.user.as_ref().map(|u| json!({
            "id": u.user_id,
            "name": u.display_name,
            "phone": u.phone.clone().unwrap_or_default(),
            "avatar": u.avatar.clone().unwrap_or_default(),
            "online": true,
        })),
        "conversations": serialize_visible_conversations(state),
        "convExtras": serialize_conv_extras(state),
        "convActions": serialize_conv_actions(state),
        "activeConvId": state.open_conversation_id,
        "messagesByConv": messages_by_conv,
        "typingByConv": state.typing,
        "pinsByConv": serialize_pins_map(&state.pins),
        "contacts": state.contacts,
        "cloud": state.cloud,
        "transfer": serialize_transfer(state),
        "polls": state.polls,
        "stickers": state.stickers,
        "settings": settings,
        "incomingCall": incoming_call.map(incoming_call_to_ui_json),
    })
}

/// Serializes only the slices modified by [`ZaloEngineState::apply_zstate_patch`] for incremental
/// `window.__olazPushState(patch)` updates.
pub fn build_ui_patch_for_slices(
    state: &ZaloEngineState,
    changed: &[String],
    runtime_progress: Option<&UiRuntimeProgress>,
    signing_in: bool,
) -> Value {
    let mut out = Map::new();
    let mut convs_inserted = false;

    for slice in changed {
        if let Some(conv_id) = slice.strip_prefix("chat:") {
            if !state.conv_actions.is_locked(conv_id) {
                let items: Vec<Value> = state
                    .messages
                    .get(conv_id)
                    .map(|v| v.iter().map(serialize_chat_message).collect())
                    .unwrap_or_default();
                let has_more = state.has_more_history.get(conv_id).copied().unwrap_or(false);
                let entry = out
                    .entry("messagesByConv".to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(m) = entry.as_object_mut() {
                    m.insert(
                        conv_id.to_string(),
                        json!({ "items": items, "hasMore": has_more }),
                    );
                }
            }
            continue;
        }

        match slice.as_str() {
            "phase" => {
                let (phase_str, login_val) = serialize_phase_and_login(&state.phase, signing_in);
                out.insert("phase".to_string(), json!(phase_str));
                out.insert("login".to_string(), login_val);
                if let Some(rp) = runtime_progress {
                    out.insert("runtimeProgress".to_string(), json!(rp));
                }
            }
            "user" => {
                out.insert(
                    "user".to_string(),
                    state
                        .user
                        .as_ref()
                        .map(|u| {
                            json!({
                                "id": u.user_id,
                                "name": u.display_name,
                                "phone": u.phone.clone().unwrap_or_default(),
                                "avatar": u.avatar.clone().unwrap_or_default(),
                                "online": true,
                            })
                        })
                        .unwrap_or(Value::Null),
                );
            }
            "conversations" | "conv_extras" | "conv_actions" => {
                if !convs_inserted {
                    convs_inserted = true;
                    out.insert(
                        "conversations".to_string(),
                        Value::Array(serialize_visible_conversations(state)),
                    );
                }
                if slice == "conv_extras" {
                    out.insert("convExtras".to_string(), serialize_conv_extras(state));
                }
                if slice == "conv_actions" {
                    out.insert("convActions".to_string(), serialize_conv_actions(state));
                }
            }
            "typing" => {
                out.insert("typingByConv".to_string(), json!(state.typing));
            }
            "pins" => {
                out.insert("pinsByConv".to_string(), serialize_pins_map(&state.pins));
            }
            "contacts" => {
                out.insert("contacts".to_string(), state.contacts.clone());
            }
            "cloud" => {
                out.insert("cloud".to_string(), state.cloud.clone());
            }
            "transfer" => {
                out.insert("transfer".to_string(), serialize_transfer(state));
            }
            "polls" => {
                out.insert("polls".to_string(), json!(state.polls));
            }
            "stickers" => {
                out.insert("stickers".to_string(), state.stickers.clone());
            }
            _ => {}
        }
    }

    Value::Object(out)
}

fn serialize_phase_and_login(phase: &EnginePhase, signing_in: bool) -> (&'static str, Value) {
    match phase {
        EnginePhase::Booting => {
            if signing_in {
                ("signingIn", Value::Null)
            } else {
                ("starting", Value::Null)
            }
        }
        EnginePhase::InstallingRuntime { .. } => ("preparing", Value::Null),
        EnginePhase::Login {
            qr,
            qr_expired,
            waiting_confirm,
            kickout_notice,
            error_notice,
        } => {
            let subtitle_text = if let Some(err) = error_notice.as_ref().filter(|s| !s.is_empty()) {
                err.clone()
            } else if let Some(notice) = kickout_notice.as_ref().filter(|s| !s.is_empty()) {
                notice.clone()
            } else if let Some(w) = waiting_confirm.as_ref() {
                format!(
                    "Đã quét bằng tài khoản {} — vui lòng bấm Đăng nhập trên điện thoại.",
                    w.name
                )
            } else if *qr_expired {
                "Mã QR đã hết hạn — nhấn Làm mới để lấy mã mới.".to_string()
            } else if qr.is_none() {
                "Đang tạo mã QR…".to_string()
            } else {
                "Mở ứng dụng Zalo trên điện thoại và quét mã QR bên dưới để đồng bộ tài khoản."
                    .to_string()
            };
            let avatar_opt = waiting_confirm
                .as_ref()
                .and_then(|w| w.avatar.clone())
                .filter(|s| !s.is_empty());
            let waiting_confirm_opt = waiting_confirm.as_ref().map(|w| {
                json!({
                    "name": w.name,
                    "avatar": w.avatar.clone().filter(|s| !s.is_empty()),
                })
            });
            (
                "login",
                json!({
                    "qr": qr,
                    "invalid": *qr_expired,
                    "text": subtitle_text,
                    "avatar": avatar_opt,
                    "waitingConfirm": waiting_confirm_opt,
                    "kickoutNotice": kickout_notice,
                    "error": error_notice,
                }),
            )
        }
        EnginePhase::Ready => ("ready", Value::Null),
    }
}

fn serialize_visible_conversations(state: &ZaloEngineState) -> Vec<Value> {
    let self_id = state
        .self_conversation_id
        .as_deref()
        .or(state.conv_extras.self_conversation_id.as_deref());

    state
        .visible_conversations()
        .into_iter()
        .map(|conv| serialize_conversation(conv, state, self_id))
        .collect()
}

fn serialize_conversation(
    conv: &Conversation,
    state: &ZaloEngineState,
    self_id: Option<&str>,
) -> Value {
    let is_self_cloud = self_id.is_some_and(|sid| sid == conv.id);
    let label_id = state
        .conv_extras
        .label_by_conv
        .get(&conv.id)
        .cloned()
        .unwrap_or_default();
    let is_muted = conv
        .raw
        .get("muted")
        .or_else(|| conv.raw.get("isMuted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_e2ee = conv
        .raw
        .get("e2ee")
        .or_else(|| conv.raw.get("isE2EE"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let online = conv
        .raw
        .get("online")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let preview = conv.preview.as_ref().map(|p| {
        json!({
            "text": p.text,
            "ts": p.timestamp,
            "fromMe": p.from_me,
        })
    });

    json!({
        "id": conv.id,
        "name": conv.display_name(),
        "avatar": conv.avatar.clone().unwrap_or_default(),
        "isGroup": conv.is_group,
        "isSelfCloud": is_self_cloud,
        "members": conv.members,
        "pinned": conv.pinned,
        "isMuted": is_muted,
        "isE2EE": is_e2ee,
        "online": online,
        "unread": conv.unread,
        "labelId": label_id,
        "preview": preview,
    })
}

fn serialize_conv_extras(state: &ZaloEngineState) -> Value {
    let ex = &state.conv_extras;
    let labels: Vec<Value> = ex
        .labels
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "name": l.name,
                "color": l.color_hex,
            })
        })
        .collect();
    let mut stranger_ids: Vec<String> = ex.stranger_ids.iter().cloned().collect();
    stranger_ids.sort();

    json!({
        "inboxTitle": "Tin nhắn",
        "archivedEnabled": ex.archived_enabled,
        "sendToMeId": state
            .self_conversation_id
            .clone()
            .or_else(|| ex.self_conversation_id.clone())
            .unwrap_or_default(),
        "strangerIds": stranger_ids,
        "otherIds": ex.stranger_folder_ids,
        "labels": labels,
    })
}

fn serialize_conv_actions(state: &ZaloEngineState) -> Value {
    let ca = &state.conv_actions;
    let mut hidden_ids: Vec<String> = ca.hidden_ids.iter().cloned().collect();
    hidden_ids.sort();
    json!({
        "hidden": {
            "enabled": ca.hidden_enabled,
            "hasPin": ca.pin_set,
            "ids": hidden_ids,
        },
        "flags": ca.flags,
    })
}

fn serialize_pins_map(pins: &HashMap<String, Value>) -> Value {
    let mut out = Map::new();
    for (cid, val) in pins {
        let arr = if val.is_array() {
            val.clone()
        } else if let Some(items) = val.get("items").filter(|v| v.is_array()) {
            items.clone()
        } else {
            json!([])
        };
        out.insert(cid.clone(), arr);
    }
    Value::Object(out)
}

fn serialize_transfer(state: &ZaloEngineState) -> Value {
    if state.transfer.phase.is_some() {
        if state.transfer.raw.is_object() {
            state.transfer.raw.clone()
        } else {
            json!({
                "phase": state.transfer.phase,
                "progress": state.transfer.progress,
                "syncedConversations": state.transfer.synced_conversations,
                "totalConversations": state.transfer.total_conversations,
                "message": state.transfer.message,
                "errorCode": state.transfer.error_code,
                "errorName": state.transfer.error_name,
                "canRetry": state.transfer.can_retry,
            })
        }
    } else {
        Value::Null
    }
}

fn incoming_call_to_ui_json(call: &IncomingVoipCall) -> Value {
    json!({
        "callId": call.call_id,
        "convId": call.conv_id,
        "name": call.name,
        "caller": call.caller,
        "avatar": call.avatar,
        "isGroup": call.is_group,
        "isVideo": call.is_video,
    })
}

/// Converts a strongly-typed [`olaz_linux::ChatMessage`] into the UI timeline message shape
/// expected by `components.js::normalizeMessageKind` and `renderMessageBubbleContent`.
pub fn serialize_chat_message(msg: &olaz_linux::ChatMessage) -> Value {
    let kind_obj = if msg.recalled || msg.kind == "recalled" {
        json!({ "type": "recalled" })
    } else {
        match msg.kind.as_str() {
            "photo" | "gif" | "video" | "voice" => {
                let m = msg.media.as_ref();
                let url = m.and_then(|x| x.url.clone()).unwrap_or_default();
                let local_path = if url.starts_with('/') {
                    Some(url.clone())
                } else {
                    None
                };
                json!({
                    "type": msg.kind,
                    "url": url,
                    "thumbnail": m.and_then(|x| x.thumbnail.clone()).unwrap_or_default(),
                    "width": m.map(|x| x.width).unwrap_or(0),
                    "height": m.map(|x| x.height).unwrap_or(0),
                    "duration": m.map(|x| x.duration).unwrap_or(0),
                    "path": local_path,
                })
            }
            "file" => {
                let f = msg.file.as_ref();
                let url = f.and_then(|x| x.url.clone()).unwrap_or_default();
                let path = msg
                    .raw
                    .get("localPath")
                    .or_else(|| msg.raw.get("path"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| url.starts_with('/').then(|| url.clone()));
                json!({
                    "type": "file",
                    "name": f.map(|x| x.name.clone()).unwrap_or_else(|| msg.text.clone()),
                    "size": f.map(|x| x.size).unwrap_or(0),
                    "url": url,
                    "path": path,
                })
            }
            "sticker" => {
                let s = msg.sticker.as_ref();
                json!({
                    "type": "sticker",
                    "url": s.and_then(|x| x.url.clone()).unwrap_or_default(),
                    "cateId": s.and_then(|x| x.cate_id),
                    "stickerId": s.and_then(|x| x.sticker_id),
                })
            }
            "poll" => {
                let p = msg.poll.as_ref();
                json!({
                    "type": "poll",
                    "pollId": p.map(|x| x.poll_id.clone()).unwrap_or_default(),
                    "question": p.map(|x| x.question.clone()).unwrap_or_else(|| msg.text.clone()),
                })
            }
            "link" | "location" | "call" | "card" => {
                if let Some(ref c) = msg.card {
                    let mapped_type = match c.card_type.as_str() {
                        "location" => "location",
                        "call" => "call",
                        _ => "link",
                    };
                    json!({
                        "type": mapped_type,
                        "cardType": c.card_type,
                        "title": c.title,
                        "description": c.description,
                        "url": c.url,
                        "thumbnail": c.thumbnail,
                        "detail": c.detail,
                    })
                } else {
                    json!({ "type": msg.kind })
                }
            }
            other => json!({ "type": other }),
        }
    };

    let mentions: Vec<Value> = msg
        .mentions
        .iter()
        .map(|m| {
            json!({
                "uid": m.uid,
                "pos": m.pos,
                "len": m.len,
                "isAll": m.is_all,
            })
        })
        .collect();

    let quote = msg.quote.as_ref().map(|q| {
        json!({
            "senderName": q.sender_name,
            "text": q.text,
            "msgId": q.msg_id,
        })
    });

    let reactions: Vec<Value> = msg
        .reactions
        .iter()
        .map(|r| {
            json!({
                "emoji": r.emoji,
                "count": r.count,
                "includesMe": r.includes_me,
                "names": r.names,
                "people": r.people,
            })
        })
        .collect();

    json!({
        "id": msg.id,
        "conversationId": msg.conversation_id,
        "senderId": msg.sender_id,
        "senderName": msg.sender_name,
        "senderAvatar": msg.sender_avatar,
        "isMine": msg.is_mine,
        "date": msg.timestamp,
        "status": msg.status,
        "isRecalled": msg.recalled,
        "starred": msg.starred,
        "text": msg.text,
        "mentions": mentions,
        "quote": quote,
        "reactions": reactions,
        "kind": kind_obj,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_session::{link_shared_runtime_for_profile, load_legacy_tray_privacy};
    use olaz_linux::TrayPrivacyState;

    #[test]
    fn snapshot_maps_phases_and_enforces_fail_closed_hidden_conversations() {
        let mut state = ZaloEngineState::new();
        let settings = UiSettings::default();

        // 1. Booting -> "starting" (and "signingIn" when signing_in=true) + capabilities
        let s0 = engine_state_to_ui_snapshot(&state, None, &settings, None, false);
        assert_eq!(s0["phase"], "starting");
        assert!(s0["capabilities"]["gstAudioSink"].is_boolean());
        let s_sign = engine_state_to_ui_snapshot(&state, None, &settings, None, true);
        assert_eq!(s_sign["phase"], "signingIn");

        // 2. InstallingRuntime -> "preparing" (matches components.js:172)
        state.phase = EnginePhase::InstallingRuntime {
            step: "downloading".to_string(),
            fraction: 0.5,
            detail: None,
            error: None,
        };
        let prog = UiRuntimeProgress {
            step: "downloading".to_string(),
            received: 50,
            total: 100,
            failure: None,
        };
        let s_prep = engine_state_to_ui_snapshot(&state, Some(&prog), &settings, None, false);
        assert_eq!(s_prep["phase"], "preparing");
        assert_eq!(s_prep["runtimeProgress"]["received"], 50);

        // 3. Login serializes all 7 shared JSON contract fields cleanly
        state.phase = EnginePhase::Login {
            qr: Some("data:image/png;base64,iVBORw0KGgo=".to_string()),
            qr_expired: false,
            waiting_confirm: Some(WaitingConfirm {
                name: "Minh Nguyễn".to_string(),
                avatar: Some("https://ava.zdn.vn/u1.jpg".to_string()),
            }),
            kickout_notice: Some("Phiên trước đã đăng xuất.".to_string()),
            error_notice: None,
        };
        let s_login = engine_state_to_ui_snapshot(&state, None, &settings, None, false);
        assert_eq!(s_login["phase"], "login");
        assert_eq!(s_login["login"]["qr"], "data:image/png;base64,iVBORw0KGgo=");
        assert_eq!(s_login["login"]["invalid"], false);
        assert!(s_login["login"]["text"].is_string());
        assert_eq!(s_login["login"]["avatar"], "https://ava.zdn.vn/u1.jpg");
        assert_eq!(s_login["login"]["waitingConfirm"]["name"], "Minh Nguyễn");
        assert_eq!(
            s_login["login"]["waitingConfirm"]["avatar"],
            "https://ava.zdn.vn/u1.jpg"
        );
        assert_eq!(
            s_login["login"]["kickoutNotice"],
            "Phiên trước đã đăng xuất."
        );
        assert!(s_login["login"]["error"].is_null());

        // 4. Conversations stay hidden until `convActions.hidden` arrives (fail-closed)
        state.apply_zstate_patch(&json!({
            "phase": "ready",
            "conversations": [
                { "id": "c_pub", "name": "Nhóm Công Khai", "isGroup": true, "unread": 2 },
                { "id": "c_sec", "name": "Bí Mật", "isGroup": false, "unread": 5 }
            ]
        }));
        let s_before_hidden = engine_state_to_ui_snapshot(&state, None, &settings, None, false);
        assert_eq!(s_before_hidden["phase"], "ready");
        assert_eq!(s_before_hidden["conversations"].as_array().unwrap().len(), 0);

        // 5. Once `convActions.hidden` arrives with `c_sec` hidden, only `c_pub` is exposed
        state.apply_zstate_patch(&json!({
            "convActions": {
                "hidden": { "enabled": true, "hasPin": true, "ids": ["c_sec"] }
            }
        }));
        let s_locked = engine_state_to_ui_snapshot(&state, None, &settings, None, false);
        let visible = s_locked["conversations"].as_array().unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["id"], "c_pub");

        // 6. Unlocking `c_sec` exposes both conversations until auto-relock
        state.conv_actions.unlock("c_sec", Instant::now());
        let s_unlocked = engine_state_to_ui_snapshot(&state, None, &settings, None, false);
        assert_eq!(s_unlocked["conversations"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn serialize_chat_message_formats_rich_attachment_kinds_for_ui() {
        let mut state = ZaloEngineState::new();
        state.apply_zstate_patch(&json!({
            "convActions": { "hidden": { "enabled": false, "hasPin": false, "ids": [] } },
            "chat": {
                "convId": "conv_1",
                "mode": "snapshot",
                "hasMore": true,
                "messages": [
                    {
                        "id": "m_file",
                        "senderId": "u1",
                        "senderName": "Minh",
                        "ts": 1700000000000_i64,
                        "kind": "file",
                        "text": "report.pdf",
                        "file": { "name": "report.pdf", "size": 4096, "url": "/tmp/olaz/report.pdf" }
                    },
                    {
                        "id": "m_poll",
                        "senderId": "u1",
                        "senderName": "Minh",
                        "ts": 1700000001000_i64,
                        "kind": "poll",
                        "poll": { "pollId": "p_42", "question": "Họp lúc mấy giờ?" }
                    }
                ]
            }
        }));

        let snap = engine_state_to_ui_snapshot(&state, None, &UiSettings::default(), None, false);
        let items = snap["messagesByConv"]["conv_1"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["kind"]["type"], "file");
        assert_eq!(items[0]["kind"]["name"], "report.pdf");
        assert_eq!(items[0]["kind"]["path"], "/tmp/olaz/report.pdf");
        assert_eq!(items[1]["kind"]["type"], "poll");
        assert_eq!(items[1]["kind"]["pollId"], "p_42");
    }

    #[test]
    fn validate_confined_media_file_enforces_allowed_roots() {
        let dir = std::env::temp_dir().join(format!("olaz-media-test-{}", uuid::Uuid::new_v4()));
        let allowed = dir.join("cache");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let ok_file = allowed.join("photo.png");
        let bad_file = outside.join("secret.txt");
        std::fs::write(&ok_file, b"png").unwrap();
        std::fs::write(&bad_file, b"nope").unwrap();

        let roots = vec![allowed.clone()];
        assert!(validate_confined_media_file(&ok_file, &roots).is_ok());
        assert!(validate_confined_media_file(&bad_file, &roots).is_err());
        assert!(validate_confined_media_file(&allowed, &roots).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_window_state_persists_tiled_width_and_x11_position_with_legacy_compat() {
        let dir = std::env::temp_dir().join(format!("olaz-winstate-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state_file = dir.join("olaz-window-state.json");

        // Legacy JSON without pos_x / pos_y deserializes cleanly and allows tiled widths >= 420.
        std::fs::write(
            &state_file,
            r#"{"width": 520, "height": 480, "maximized": false}"#,
        )
        .unwrap();
        let loaded_legacy = SavedWindowState::load(&state_file);
        assert_eq!(loaded_legacy.width, 520);
        assert_eq!(loaded_legacy.height, 480);
        assert_eq!(loaded_legacy.pos_x, None);
        assert_eq!(loaded_legacy.pos_y, None);

        // Sub-minimum dimensions clamp up to MIN_MAIN_WINDOW_DIM (420).
        std::fs::write(
            &state_file,
            r#"{"width": 200, "height": 180, "maximized": true, "pos_x": 140, "pos_y": 90}"#,
        )
        .unwrap();
        let loaded_clamped = SavedWindowState::load(&state_file);
        assert_eq!(loaded_clamped.width, 420);
        assert_eq!(loaded_clamped.height, 420);
        assert!(loaded_clamped.maximized);
        assert_eq!(loaded_clamped.pos_x, Some(140));
        assert_eq!(loaded_clamped.pos_y, Some(90));

        // Round-trip save + load preserves X11 position and privacy state.
        let saved = SavedWindowState {
            width: 460,
            height: 920,
            maximized: false,
            pos_x: Some(960),
            pos_y: Some(32),
        };
        saved.save(&state_file);
        assert_eq!(SavedWindowState::load(&state_file), saved);

        let priv_file = dir.join("olaz-privacy.json");
        let priv_state = TrayPrivacyState {
            block_typing: true,
            block_seen: true,
            block_delivered: false,
            anti_recall: true,
        };
        save_legacy_tray_privacy(&priv_state, &priv_file);
        assert_eq!(load_legacy_tray_privacy(&priv_file), priv_state);

        // Multi-account shared runtime helper symlinks ZaloRuntime/current into secondary profiles cleanly.
        let root = dir.join("olaz");
        let rt_current = root.join("ZaloRuntime/current");
        std::fs::create_dir_all(rt_current.join("Contents/Resources")).unwrap();
        std::fs::write(rt_current.join("Contents/Info.plist"), b"<plist/>").unwrap();
        std::fs::write(rt_current.join("Contents/Resources/app.asar"), b"asar").unwrap();

        let p2 = ZaloPlatform::from_options(Some("account-2"), Some(root.clone()), None).unwrap();
        assert_eq!(resolve_instance_root_dir(&p2), root);
        link_shared_runtime_for_profile(&p2, &root);
        assert!(p2
            .runtime_root()
            .join("current/Contents/Resources/app.asar")
            .is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
