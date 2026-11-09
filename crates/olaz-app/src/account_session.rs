//! Per-account runtime session (`AccountSession`) encapsulating an isolated `ZaloPlatform`,
//! `ZaloCookieJar`, `NetworkProxy`, WebKit2GTK `ZaloWebRuntime` (`WebsiteDataManager` + `WebContext`),
//! `OfficialClientHost`, `ZaloMainProcess` (`shared-worker.html`, `OlazCallEngine`, profile-scoped
//! `KeychainCipher` & `AutostartManager`), `CallUiController`, `ZaloEngineState`, `SchedulerEngine`,
//! `AutoReplyEngine`, `PrivacyConfig`, and `gtk::Overlay` workspace page container.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result};
use gtk::prelude::*;
use olaz_core::{AsarArchive, ZaloPlatform, ZaloRuntimeManager};
use olaz_linux::{
    spawn_automation_loop, unix_now_ms, AutoReplyEngine, AutomationLoopHandle,
    CallRecordingSettings, EngineControlBackend, PrivacyConfig, SchedulerEngine, StartCallCallback,
    TrayPrivacyState, ZaloEngineState, DEFAULT_AUTOMATION_TICK, PRIVACY_CONFIG_FILENAME,
};
use olaz_net::{MediaLoader, NetworkProxy, ZaloCookieJar, ZaloDnsResolver};
use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::oneshot;
use webkit2gtk::WebViewExt;

use crate::call_ui::CallUiController;
use crate::client_host::{OfficialClientHost, ZaloWebRuntime};
use crate::main_process::ZaloMainProcess;

/// Active viewport layout mode of an [`AccountSession`] inside the multi-account `gtk::Stack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowPageMode {
    /// Compact `400x580` QR / login card view (`login.html`).
    Login,
    /// Full-bleed desktop Zalo chat workspace (`index.html`).
    Main,
}

/// Outbound text send request routed to a specific [`AccountSession`] on the GTK main thread.
pub struct AccountOutboundSend {
    pub slug: String,
    pub conv_id: String,
    pub text: String,
    pub reply_tx: Option<oneshot::Sender<anyhow::Result<()>>>,
}

/// Outbound VoIP call request routed to a specific [`AccountSession`] on the GTK main thread.
pub struct AccountOutboundCall {
    pub slug: String,
    pub target_id: String,
    pub video: bool,
    pub reply_tx: oneshot::Sender<anyhow::Result<Value>>,
}

/// Privacy configuration update notification emitted by [`EngineControlBackend`] for a session.
pub struct AccountPrivacyEvent {
    pub slug: String,
    pub config: PrivacyConfig,
}

/// Computes the shared root directory holding `profiles.json`, `olaz-instance*.sock`, and the
/// primary `ZaloRuntime` across both `"default"` and named `--profile <slug>` accounts.
pub fn resolve_instance_root_dir(platform: &ZaloPlatform) -> PathBuf {
    if platform.profile_slug() == "default" {
        platform.base_dir().to_path_buf()
    } else {
        platform
            .base_dir()
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| platform.base_dir())
            .to_path_buf()
    }
}

/// Shares an already-installed `ZaloRuntime/current` from `instance_root_dir` into a secondary
/// profile's `runtime_root` via symlink so secondary accounts boot instantaneously without
/// re-downloading `app.asar`.
pub fn link_shared_runtime_for_profile(platform: &ZaloPlatform, instance_root_dir: &Path) {
    if platform.profile_slug() == "default" {
        return;
    }
    let dst_current = platform.runtime_root().join("current");
    if dst_current.exists() {
        return;
    }
    let src_current = instance_root_dir.join("ZaloRuntime").join("current");
    if ZaloRuntimeManager::is_complete(&src_current)
        && std::fs::create_dir_all(platform.runtime_root()).is_ok()
    {
        let _ = std::os::unix::fs::symlink(&src_current, &dst_current);
    }
}

pub fn tray_privacy_from_config(cfg: PrivacyConfig) -> TrayPrivacyState {
    TrayPrivacyState {
        block_typing: cfg.block_typing,
        block_seen: cfg.block_seen,
        block_delivered: cfg.block_delivered,
        anti_recall: cfg.anti_recall,
    }
}

pub fn privacy_config_from_tray(state: TrayPrivacyState) -> PrivacyConfig {
    PrivacyConfig {
        block_typing: state.block_typing,
        block_seen: state.block_seen,
        block_delivered: state.block_delivered,
        anti_recall: state.anti_recall,
    }
}

pub fn load_legacy_tray_privacy(path: &Path) -> TrayPrivacyState {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TrayPrivacyState>(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_legacy_tray_privacy(state: &TrayPrivacyState, path: &Path) {
    if let Ok(bytes) = serde_json::to_vec_pretty(state) {
        let _ = std::fs::write(path, bytes);
    }
}

/// Complete isolated runtime stack for a single Zalo account inside [`crate::ui_window::OlazAppWindow`].
pub struct AccountSession {
    pub slug: String,
    pub platform: Arc<ZaloPlatform>,
    #[allow(dead_code)]
    pub cookie_jar: Arc<ZaloCookieJar>,
    #[allow(dead_code)]
    pub proxy: Arc<NetworkProxy>,
    #[allow(dead_code)]
    pub web_runtime: Rc<ZaloWebRuntime>,
    pub host: Rc<OfficialClientHost>,
    pub main_process: Rc<ZaloMainProcess>,
    #[allow(dead_code)]
    pub call_ui: Rc<CallUiController>,
    pub engine_state: RefCell<ZaloEngineState>,
    pub engine_state_mirror: Arc<RwLock<ZaloEngineState>>,
    #[allow(dead_code)]
    pub scheduler: Arc<SchedulerEngine>,
    pub autoreply: Arc<AutoReplyEngine>,
    pub autoreply_seen_ids: RefCell<HashSet<(String, String)>>,
    pub privacy: RefCell<PrivacyConfig>,
    pub privacy_path: PathBuf,
    pub unread_count: Cell<u64>,
    pub page_mode: Cell<WindowPageMode>,
    pub page_container: gtk::Overlay,
    cancel_bar: gtk::Box,
    close_tab_btn: gtk::Button,
    webview_slot: gtk::Box,
    pub signing_in: Cell<bool>,
    pub qr_seq: Cell<u64>,
    pub last_qr: RefCell<Option<String>>,
    pub boot_time_ms: i64,
    pub control_backend: Arc<EngineControlBackend>,
    pub outbound_send_tx: tokio::sync::mpsc::UnboundedSender<AccountOutboundSend>,
    _automation_loop: Option<AutomationLoopHandle>,
}

impl AccountSession {
    /// Wraps an already-constructed primary `(platform, host, main_process)` into an [`AccountSession`].
    #[allow(clippy::too_many_arguments)]
    pub fn from_existing(
        platform: Arc<ZaloPlatform>,
        host: Rc<OfficialClientHost>,
        main_process: Rc<ZaloMainProcess>,
        tokio_handle: &tokio::runtime::Handle,
        demo_mode: bool,
        outbound_send_tx: tokio::sync::mpsc::UnboundedSender<AccountOutboundSend>,
        outbound_call_tx: tokio::sync::mpsc::UnboundedSender<AccountOutboundCall>,
        privacy_obs_tx: tokio::sync::mpsc::UnboundedSender<AccountPrivacyEvent>,
    ) -> Rc<Self> {
        let slug = platform.profile_slug().to_string();
        let privacy_path = platform.user_data().join("olaz-privacy.json");
        let config_privacy_file = platform.config_dir().join(PRIVACY_CONFIG_FILENAME);
        let initial_privacy_cfg = if config_privacy_file.exists() {
            let cfg = PrivacyConfig::load_from(platform.config_dir());
            save_legacy_tray_privacy(&tray_privacy_from_config(cfg), &privacy_path);
            cfg
        } else {
            let legacy = load_legacy_tray_privacy(&privacy_path);
            let cfg = privacy_config_from_tray(legacy);
            let _ = cfg.save_to(platform.config_dir());
            cfg
        };

        let engine_state_mirror = Arc::new(RwLock::new(ZaloEngineState::new()));
        let scheduler = Arc::new(SchedulerEngine::load_from(platform.config_dir()));
        let autoreply = Arc::new(AutoReplyEngine::load_from(platform.config_dir()));

        let slug_for_send = slug.clone();
        let send_tx_for_backend = outbound_send_tx.clone();
        let slug_for_sched = slug.clone();
        let send_tx_for_scheduler = outbound_send_tx.clone();
        let slug_for_call = slug.clone();
        let call_cb: StartCallCallback = Arc::new(move |target_id: String, video: bool| {
            let tx = outbound_call_tx.clone();
            let slug_c = slug_for_call.clone();
            Box::pin(async move {
                let (reply_tx, reply_rx) = oneshot::channel();
                tx.send(AccountOutboundCall {
                    slug: slug_c,
                    target_id,
                    video,
                    reply_tx,
                })
                .map_err(|_| anyhow::anyhow!("Olaz UI call channel closed"))?;
                reply_rx
                    .await
                    .map_err(|_| anyhow::anyhow!("Olaz UI call handler dropped"))?
            })
        });

        let slug_for_priv = slug.clone();
        let control_backend = Arc::new(
            EngineControlBackend::new(
                Arc::clone(&engine_state_mirror),
                Arc::clone(&scheduler),
                Arc::clone(&autoreply),
                platform.config_dir(),
            )
            .with_profile_slug(slug.clone())
            .with_send_callback(move |conv_id: String, text: String| {
                let tx = send_tx_for_backend.clone();
                let slug_s = slug_for_send.clone();
                async move {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    tx.send(AccountOutboundSend {
                        slug: slug_s,
                        conv_id,
                        text,
                        reply_tx: Some(reply_tx),
                    })
                    .map_err(|_| anyhow::anyhow!("Olaz UI send channel closed"))?;
                    reply_rx
                        .await
                        .map_err(|_| anyhow::anyhow!("Olaz UI send handler dropped"))?
                }
            })
            .with_call_callback(call_cb)
            .with_privacy_observer(move |config| {
                let _ = privacy_obs_tx.send(AccountPrivacyEvent {
                    slug: slug_for_priv.clone(),
                    config,
                });
            }),
        );

        let automation_loop = if demo_mode {
            None
        } else {
            let _enter = tokio_handle.enter();
            Some(spawn_automation_loop(
                Arc::clone(&scheduler),
                DEFAULT_AUTOMATION_TICK,
                move |conv_id: String, text: String| {
                    let tx = send_tx_for_scheduler.clone();
                    let slug_s = slug_for_sched.clone();
                    async move {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        tx.send(AccountOutboundSend {
                            slug: slug_s,
                            conv_id,
                            text,
                            reply_tx: Some(reply_tx),
                        })
                        .map_err(|_| anyhow::anyhow!("Olaz UI send channel closed"))?;
                        reply_rx
                            .await
                            .map_err(|_| anyhow::anyhow!("Olaz UI send handler dropped"))?
                    }
                },
            ))
        };

        let page_container = gtk::Overlay::new();
        page_container.set_hexpand(true);
        page_container.set_vexpand(true);
        page_container.style_context().add_class("olaz-account-page");

        let outer_vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer_vbox.set_hexpand(true);
        outer_vbox.set_vexpand(true);

        let cancel_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        cancel_bar.set_margin_top(12);
        cancel_bar.set_margin_start(16);
        cancel_bar.set_margin_end(16);
        cancel_bar.set_no_show_all(true);
        cancel_bar.hide();

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let close_tab_btn = gtk::Button::with_label("✕ Đóng tab này");
        close_tab_btn
            .style_context()
            .add_class("olaz-close-login-tab-btn");
        cancel_bar.pack_start(&spacer, true, true, 0);
        cancel_bar.pack_end(&close_tab_btn, false, false, 0);
        close_tab_btn.show();
        spacer.show();

        let webview_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        webview_slot.set_hexpand(true);
        webview_slot.set_vexpand(true);
        webview_slot
            .style_context()
            .add_class("olaz-account-webview-slot");

        outer_vbox.pack_start(&cancel_bar, false, false, 0);
        outer_vbox.pack_start(&webview_slot, true, true, 0);
        page_container.add(&outer_vbox);

        let initial_mode = if main_process.has_session() {
            WindowPageMode::Main
        } else {
            WindowPageMode::Login
        };

        let cookie_jar = Arc::new(main_process.cookie_jar().clone());
        let proxy = Arc::new(main_process.network().clone());
        let web_runtime = Rc::new(main_process.runtime().clone());
        let call_ui = Rc::clone(main_process.call_ui());

        Rc::new(Self {
            slug,
            platform,
            cookie_jar,
            proxy,
            web_runtime,
            host,
            main_process,
            call_ui,
            engine_state: RefCell::new(ZaloEngineState::new()),
            engine_state_mirror,
            scheduler,
            autoreply,
            autoreply_seen_ids: RefCell::new(HashSet::new()),
            privacy: RefCell::new(initial_privacy_cfg),
            privacy_path,
            unread_count: Cell::new(0),
            page_mode: Cell::new(initial_mode),
            page_container,
            cancel_bar,
            close_tab_btn,
            webview_slot,
            signing_in: Cell::new(false),
            qr_seq: Cell::new(0),
            last_qr: RefCell::new(None),
            boot_time_ms: unix_now_ms(),
            control_backend,
            outbound_send_tx,
            _automation_loop: automation_loop,
        })
    }

    /// Creates a brand-new isolated secondary [`AccountSession`] for `slug`, symlinking the shared
    /// `ZaloRuntime/current` before booting its isolated `WebsiteDataManager`, `ZaloCookieJar`,
    /// `NetworkProxy`, `KeychainCipher`, `AutostartManager`, and `OfficialClientHost`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_secondary(
        slug: &str,
        proxy_url: Option<String>,
        primary_platform: &ZaloPlatform,
        instance_root_dir: &Path,
        shared_asar: Option<Arc<AsarArchive>>,
        tokio_handle: &tokio::runtime::Handle,
        direct_ui: bool,
        outbound_send_tx: tokio::sync::mpsc::UnboundedSender<AccountOutboundSend>,
        outbound_call_tx: tokio::sync::mpsc::UnboundedSender<AccountOutboundCall>,
        privacy_obs_tx: tokio::sync::mpsc::UnboundedSender<AccountPrivacyEvent>,
    ) -> Result<Rc<Self>> {
        let data_dir_override = if primary_platform.is_isolated_root() {
            Some(instance_root_dir.to_path_buf())
        } else {
            std::env::var_os("OLAZ_DATA_DIR")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        };
        let effective_proxy =
            proxy_url.or_else(|| primary_platform.proxy_url().map(str::to_owned));

        let platform_raw =
            ZaloPlatform::from_options(Some(slug), data_dir_override, effective_proxy)?;
        platform_raw.set_app_version(primary_platform.app_version());
        link_shared_runtime_for_profile(&platform_raw, instance_root_dir);
        let platform = Arc::new(platform_raw);

        let cookie_jar = ZaloCookieJar::open(platform.user_data().join("cookies.json"))
            .with_context(|| format!("failed to open Zalo cookie jar for `{slug}`"))?;
        let dns_resolver = Arc::new(
            ZaloDnsResolver::new()
                .with_proxy_url(platform.proxy_url())
                .context("failed to configure upstream proxy on ZaloDnsResolver")?,
        );
        let network = NetworkProxy::with_config(
            cookie_jar.clone(),
            Arc::clone(&dns_resolver),
            platform.identity.user_agent(),
        )
        .context("failed to initialize NetworkProxy")?
        .with_proxy_url(platform.proxy_url())
        .context("failed to configure upstream proxy on NetworkProxy")?;
        let media_loader = MediaLoader::new(platform.cache_dir(), dns_resolver)
            .context("failed to init MediaLoader")?;

        let web_runtime = ZaloWebRuntime::new(&platform, shared_asar, tokio_handle);
        let main_process = ZaloMainProcess::new(
            Arc::clone(&platform),
            web_runtime.clone(),
            cookie_jar,
            network.clone(),
            media_loader,
            tokio_handle.clone(),
        );

        let mp_for_bridge = Rc::clone(&main_process);
        let host = OfficialClientHost::new_with_mode(
            &platform,
            web_runtime,
            network,
            tokio_handle.clone(),
            true,
            direct_ui,
            move |bridge| {
                mp_for_bridge.install_bridge_on(bridge);
            },
        );
        main_process.attach_host(&host);

        Ok(Self::from_existing(
            platform,
            host,
            main_process,
            tokio_handle,
            false,
            outbound_send_tx,
            outbound_call_tx,
            privacy_obs_tx,
        ))
    }

    /// Connects a callback invoked when the user clicks `"✕ Đóng tab này"` on an unauthenticated
    /// secondary login tab.
    pub fn connect_close_login_tab(&self, on_close: impl Fn() + 'static) {
        self.close_tab_btn.connect_clicked(move |_| {
            on_close();
        });
    }

    /// Mounts `self.host`'s WebView into `self.webview_slot` inside `self.page_container`.
    pub fn mount_into_page(&self) {
        self.host.mount_into(&self.webview_slot);
        self.host.webview().show();
        self.webview_slot.show_all();
        self.page_container.show_all();
    }

    /// Unmounts `self.host`'s WebView back to its `gtk::OffscreenWindow` while hidden in the tray.
    pub fn unmount_to_offscreen(&self) {
        self.host.unmount_to_offscreen();
    }

    /// Updates `self.webview_slot` and `self.cancel_bar` alignment/sizing for single-account vs
    /// multi-account mode:
    /// - When `multi_account` is `true` and `self.page_mode` is [`WindowPageMode::Login`], centers
    ///   the `WebView` at `400x580` inside its stack page and shows `"✕ Đóng tab này"` if not yet
    ///   authenticated.
    /// - Otherwise expands `self.webview_slot` to full-bleed `gtk::Align::Fill`.
    pub fn apply_layout_mode(&self, multi_account: bool) {
        if multi_account && self.page_mode.get() == WindowPageMode::Login {
            self.webview_slot.set_halign(gtk::Align::Center);
            self.webview_slot.set_valign(gtk::Align::Center);
            self.webview_slot.set_size_request(400, 580);
            if !self.main_process.has_session() {
                self.cancel_bar.show();
            } else {
                self.cancel_bar.hide();
            }
        } else {
            self.cancel_bar.hide();
            self.webview_slot.set_halign(gtk::Align::Fill);
            self.webview_slot.set_valign(gtk::Align::Fill);
            self.webview_slot.set_size_request(-1, -1);
        }
    }

    /// Pushes `self.privacy` (`block_typing`, `block_seen`, `block_delivered`, `anti_recall`) into
    /// this session's `WebView` (`window.__olazSetPrivacy(...)`).
    pub fn sync_privacy_to_webview(&self) {
        let cfg = *self.privacy.borrow();
        let js = cfg.to_js_init_script();
        self.host
            .webview()
            .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
    }

    /// Pushes the active call recording directory (`window.__olazSetCallRecording(...)`) into this
    /// session's `WebView` so Zalo's `Cài đặt` -> `Cài đặt cuộc gọi` displays the live save path.
    pub fn sync_call_recording_settings_to_webview(&self) {
        if !self.host.is_direct_ui() {
            return;
        }
        let active_dir = self.main_process.call_engine().recordings_dir();
        let default_dir = self.platform.base_dir().join("recordings");
        let js = CallRecordingSettings::to_js_init_script(&active_dir, &default_dir);
        self.host
            .webview()
            .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
    }

    /// Pushes the live multi-account workspace snapshot (`{ selfSlug, activeSlug, accounts }`) into
    /// this session's `WebView` (`window.__olazSetAccounts(...)`) so Zalo's `#main-tab` sidebar
    /// renders aligned account switcher avatars and the Add-Account button.
    pub fn sync_accounts_to_webview(&self, payload: &Value) {
        if !self.host.is_direct_ui() {
            return;
        }
        let js = format!(
            "window.__olazAccountsState = {payload}; window.__olazSetAccounts && window.__olazSetAccounts({payload});"
        );
        self.host
            .webview()
            .evaluate_javascript(&js, None, None, gio::Cancellable::NONE, |_| {});
    }

    /// Copies the latest `self.engine_state` snapshot into `self.engine_state_mirror` for REST/MCP readers.
    pub fn sync_engine_state_mirror(&self) {
        let snap = self.engine_state.borrow().clone();
        *self.engine_state_mirror.write() = snap;
    }

    /// Stops automation timers, shared worker, VoIP engine, and WebKit host for clean tab removal or app exit.
    pub fn shutdown(&self) {
        if let Some(ref loop_h) = self._automation_loop {
            loop_h.stop();
        }
        self.main_process.call_engine().stop();
        self.main_process.stop_shared_worker();
        self.host.shutdown();
    }
}
