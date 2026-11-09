//! Olaz — Native Linux Desktop Client for Zalo (Omarchy & Ubuntu).
//!
//! Entry point initializing Wayland/NVIDIA WebKit2GTK flags, Tokio async runtime,
//! GTK3 + `webkit2gtk-4.1`, `ZaloRuntimeManager`, `OfficialClientHost`, `ZaloMainProcess`,
//! `OlazAppWindow`, and the `--engine-probe` diagnostic mode (`EngineProbe.swift` port).

mod client_host;
mod main_process;
mod ui_window;

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use client_host::{HostPage, OfficialClientHost, PageEvent, ZaloWebRuntime};
use main_process::ZaloMainProcess;
use olaz_core::{ZaloPlatform, ZaloRuntimeManager};
use olaz_linux::{
    acquire_or_notify_primary, configure_linux_webview_env, prune_old_screenshots,
    AcquireInstanceOutcome, SCREENSHOT_RETENTION, SCREENSHOT_STAGING_DIR,
};
use olaz_net::{MediaLoader, NetworkProxy, ZaloCookieJar, ZaloDnsResolver};
use tracing_subscriber::EnvFilter;
use ui_window::OlazAppWindow;

///Parsed CLI invocation mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Help,
    Version,
    Gui { start_minimized: bool },
    Demo,
    EngineProbe { page: ProbePage, seconds: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbePage {
    Login,
    Main,
}

impl ProbePage {
    fn to_host_page(self) -> HostPage {
        match self {
            Self::Login => HostPage::Login,
            Self::Main => HostPage::Main,
        }
    }
}

impl CliAction {
    /// Parses command-line arguments (excluding `argv[0]`).
    pub fn parse<I, S>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let collected: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
        if collected
            .iter()
            .any(|a| matches!(a.as_str(), "-h" | "--help"))
        {
            return Ok(Self::Help);
        }
        if collected
            .iter()
            .any(|a| matches!(a.as_str(), "-V" | "--version"))
        {
            return Ok(Self::Version);
        }
        if collected.iter().any(|a| a == "--demo") {
            return Ok(Self::Demo);
        }

        if let Some(pos) = collected.iter().position(|a| a == "--engine-probe") {
            let mut page = ProbePage::Main;
            let mut seconds = 20_u64;
            let mut next_idx = pos + 1;

            if let Some(arg1) = collected.get(next_idx) {
                match arg1.as_str() {
                    "login" => {
                        page = ProbePage::Login;
                        next_idx += 1;
                    }
                    "main" => {
                        page = ProbePage::Main;
                        next_idx += 1;
                    }
                    _ => {}
                }
            }
            if let Some(arg2) = collected.get(next_idx) {
                if let Ok(parsed_secs) = arg2.parse::<u64>() {
                    seconds = parsed_secs.max(1);
                }
            }
            return Ok(Self::EngineProbe { page, seconds });
        }

        let mut start_minimized = false;
        for arg in &collected {
            match arg.as_str() {
                "--minimized" | "--tray" | "--hidden" => {
                    start_minimized = true;
                }
                _ if arg.starts_with("--") && !arg.starts_with("--gapplication-") => {
                    bail!(
                        "Unknown option `{arg}`.\nRun `olaz --help` to see supported options."
                    );
                }
                _ => {}
            }
        }

        Ok(Self::Gui { start_minimized })
    }
}

fn print_help() {
    println!(
        "\
Olaz {version} — Native Linux client for Zalo (Omarchy & Ubuntu)

USAGE:
    olaz [OPTIONS]

OPTIONS:
    -h, --help                         Print this help information and exit
    -V, --version                      Print version and exit
        --minimized, --tray, --hidden  Launch hidden in the system tray (used by XDG autostart)
        --demo                         Launch Olaz UI in offline mock/demo mode (`?demo=1`)
        --engine-probe [PAGE] [SECS]   Boot headless Zalo renderer + debug window (`login` or `main`, default: `main` 20s)

ENVIRONMENT:
    RUST_LOG                           Tracing filter (e.g. `olaz=debug,olaz_app=debug,olaz_net=debug`)
    OLAZ_TRACE_WORKERS=1               Trace DedicatedWorker / SharedWorker IPC frames
    OLAZ_NO_SHARED_WORKER=1            Disable hidden `shared-worker.html` host
    OLAZ_ENABLE_WEBKIT_HWACCEL=1       Re-enable WebKit hardware acceleration on UI webview
    OLAZ_QR_OUT=/path/to/qr.png        Save login QR code PNG in `--engine-probe login` mode",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("olaz=info,olaz_app=info,olaz_core=info,olaz_net=info,olaz_linux=info,warn")
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

fn main() -> Result<()> {
    // Handle `--help` and `--version` BEFORE initializing GTK so headless CI / terminals succeed.
    let action = CliAction::parse(std::env::args().skip(1))?;
    match action {
        CliAction::Help => {
            print_help();
            return Ok(());
        }
        CliAction::Version => {
            println!("olaz {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliAction::Gui { .. } | CliAction::Demo | CliAction::EngineProbe { .. } => {}
    }

    init_tracing();

    // Prepare platform directories and enforce single-instance re-activation BEFORE starting
    // Tokio / GTK so a launcher click while hidden in tray immediately wakes the primary window.
    let platform_raw = ZaloPlatform::new(env!("CARGO_PKG_VERSION"));
    let mut _instance_guard = None;
    let mut instance_rx = None;
    if let CliAction::Gui { start_minimized } = action {
        match acquire_or_notify_primary(platform_raw.base_dir(), !start_minimized) {
            Ok(AcquireInstanceOutcome::Primary(guard, rx)) => {
                _instance_guard = Some(guard);
                instance_rx = Some(rx);
            }
            Ok(AcquireInstanceOutcome::ForwardedToPrimary) => {
                tracing::info!(
                    start_minimized,
                    "olaz is already running; notified primary instance and exiting"
                );
                return Ok(());
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    "single-instance socket check failed; continuing startup without lock"
                );
            }
        }
    }

    // Configure `WEBKIT_DISABLE_DMABUF_RENDERER=1` (Wayland) & `GDK_BACKEND=wayland,x11` BEFORE `gtk::init()`.
    configure_linux_webview_env();

    glib::set_prgname(Some("olaz"));
    glib::set_application_name("Olaz");

    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("olaz-tokio")
        .build()
        .context("failed to build Tokio multi-threaded runtime")?;
    let _tokio_guard = tokio_rt.enter();
    let tokio_handle = tokio_rt.handle().clone();

    gtk::init().context(
        "failed to initialize GTK3 (ensure WAYLAND_DISPLAY or DISPLAY is set in your desktop session)",
    )?;

    prune_old_screenshots(Path::new(SCREENSHOT_STAGING_DIR), SCREENSHOT_RETENTION);

    // Promote any staged `pending/` Zalo runtime upgrade.
    let runtime_mgr = ZaloRuntimeManager::from_platform(&platform_raw);
    runtime_mgr.prepare_for_launch();
    if let Some(zalo_ver) = ZaloRuntimeManager::installed_version(&runtime_mgr.current_dir()) {
        platform_raw.set_app_version(zalo_ver);
    }
    let platform = Arc::new(platform_raw);

    let cookie_jar = ZaloCookieJar::open(platform.user_data().join("cookies.json"))
        .context("failed to open Zalo cookie jar")?;
    let dns_resolver = Arc::new(ZaloDnsResolver::new());
    let network = NetworkProxy::with_config(
        cookie_jar.clone(),
        Arc::clone(&dns_resolver),
        platform.identity.user_agent(),
    )
    .context("failed to initialize NetworkProxy")?;
    let media_loader =
        MediaLoader::new(platform.cache_dir(), dns_resolver).context("failed to init MediaLoader")?;

    let web_runtime = ZaloWebRuntime::new(&platform, None, &tokio_handle);
    let main_process = ZaloMainProcess::new(
        Arc::clone(&platform),
        web_runtime.clone(),
        cookie_jar,
        network.clone(),
        media_loader,
        tokio_handle.clone(),
    );

    let direct_ui = matches!(action, CliAction::Gui { .. });
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

    match action {
        CliAction::Gui { start_minimized } => {
            let _window = OlazAppWindow::launch_with_options(
                platform,
                runtime_mgr,
                host,
                main_process,
                tokio_handle,
                false,
                start_minimized,
                instance_rx,
            );
            gtk::main();
        }
        CliAction::Demo => {
            let _window = OlazAppWindow::launch(
                platform,
                runtime_mgr,
                host,
                main_process,
                tokio_handle,
                true,
            );
            gtk::main();
        }
        CliAction::EngineProbe { page, seconds } => {
            run_engine_probe(
                &tokio_rt,
                &runtime_mgr,
                host,
                main_process,
                page,
                seconds,
            )?;
        }
        CliAction::Help | CliAction::Version => unreachable!(),
    }

    Ok(())
}

/// Port of `Sources/Olaz/ZaloRuntime/EngineProbe.swift`: boots the real Zalo renderer against
/// `OfficialClientHost` + `ZaloMainProcess`, displays the debug renderer window, logs IPC/state/worker
/// activity, optionally dumps `OLAZ_QR_OUT`, and inspects React fibers after `seconds`.
fn run_engine_probe(
    tokio_rt: &tokio::runtime::Runtime,
    runtime_mgr: &ZaloRuntimeManager,
    host: Rc<OfficialClientHost>,
    main_process: Rc<ZaloMainProcess>,
    page: ProbePage,
    seconds: u64,
) -> Result<()> {
    if !ZaloRuntimeManager::is_complete(&runtime_mgr.current_dir()) {
        println!("[probe] Zalo runtime not yet installed; fetching & verifying official DMG...");
        let ver = tokio_rt.block_on(runtime_mgr.install(|prog| {
            println!("[probe:install] {prog:?}");
        }))?;
        main_process.platform().set_app_version(&ver);
        host.reload_user_scripts(main_process.platform());
    } else if let Some(ver) = ZaloRuntimeManager::installed_version(&runtime_mgr.current_dir()) {
        main_process.platform().set_app_version(&ver);
        host.reload_user_scripts(main_process.platform());
    }
    let asar = runtime_mgr
        .open_asar()
        .context("failed to open installed app.asar for --engine-probe")?;
    main_process.runtime().set_asar(Arc::new(asar));

    println!(
        "[probe] runtime: {} (v{})",
        runtime_mgr.current_dir().display(),
        ZaloRuntimeManager::installed_version(&runtime_mgr.current_dir()).unwrap_or_default()
    );
    println!("[probe] userData: {}", main_process.platform().user_data().display());
    println!("[probe] session present: {}", main_process.has_session());

    if std::env::var_os("OLAZ_TRACE_IPC").is_some() {
        host.bridge().set_trace(|key, args| {
            println!("[ipc] {key} ({} args)", args.len());
        });
    }
    main_process.shared_worker().set_trace(|line| {
        println!("[sw] {line}");
    });
    host.bridge().set_on_handler_error(|key, err| {
        println!("[bridge] {key}: {err}");
    });
    host.set_on_console(|line| {
        let truncated: String = line.chars().take(400).collect();
        println!("[console] {truncated}");
    });
    host.set_on_state(|payload| {
        if let Ok(qr_out) = std::env::var("OLAZ_QR_OUT") {
            if let Some(qr_str) = payload.get("qr").and_then(|v| v.as_str()) {
                if let Some((_, b64)) = qr_str.split_once("base64,") {
                    if let Ok(png_bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                        if std::fs::write(&qr_out, &png_bytes).is_ok() {
                            println!(
                                "[probe] wrote QR PNG -> {qr_out} ({} bytes)",
                                png_bytes.len()
                            );
                        }
                    }
                }
            }
        }
        let s = serde_json::to_string(&payload).unwrap_or_default();
        let truncated: String = s.chars().take(300).collect();
        println!("[state] {truncated}");
    });

    {
        let mp_weak = Rc::downgrade(&main_process);
        let host_weak = Rc::downgrade(&host);
        host.set_on_page_event(move |ev| {
            println!("[page] {ev:?}");
            if let Some(mp) = mp_weak.upgrade() {
                match ev {
                    PageEvent::StartedLoading => mp.page_did_start_loading(),
                    PageEvent::FinishedLoading(p) => mp.page_did_finish_loading(p),
                    PageEvent::Failed(_) => {}
                    PageEvent::Terminated => {
                        if let Some(h) = host_weak.upgrade() {
                            if h.allow_crash_recovery() {
                                h.load(page.to_host_page(), &mp.page_query(false));
                            }
                        }
                    }
                }
            }
        });
    }

    host.show_debug_window();

    let host_page = page.to_host_page();
    if host_page == HostPage::Main && main_process.has_session() {
        main_process.start_shared_worker();
    }
    host.load(host_page, &main_process.page_query(false));

    let host_for_done = Rc::clone(&host);
    let mp_for_done = Rc::clone(&main_process);
    glib::MainContext::default().spawn_local(async move {
        glib::timeout_future(Duration::from_secs(seconds)).await;
        let inspect_js = r#"
          const root = document.querySelector('#app, #root, body > div');
          const fiberKey = root && Object.keys(root).find(k => k.startsWith('__reactFiber') || k.startsWith('__reactContainer'));
          return {
            href: location.href,
            title: document.title,
            bodyChildCount: document.body ? document.body.children.length : 0,
            rootId: root && root.id,
            hasReactFiber: Boolean(fiberKey),
            hasWebpackHook: typeof window.__zaloWebpackRequire === 'function',
            imgs: Array.from(document.images).map(i => ({ w: i.naturalWidth, srcHead: (i.src || '').slice(0, 48) })),
            textSample: (document.body && document.body.innerText || '').slice(0, 300),
          };
        "#;
        match host_for_done.evaluate(inspect_js).await {
            Ok(dom) => println!("[dom] {dom}"),
            Err(err) => println!("[dom] error: {err}"),
        }
        println!(
            "[probe] done — bridge stats: {}",
            host_for_done.bridge().stats_summary()
        );
        let unhandled = host_for_done.bridge().unhandled_channels_snapshot();
        if !unhandled.is_empty() {
            println!("[probe] unhandled IPC channels: {unhandled:?}");
        }
        mp_for_done.stop_shared_worker();
        host_for_done.shutdown();
        gtk::main_quit();
    });

    gtk::main();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parser_handles_help_version_minimized_probe_and_default_gui() {
        assert_eq!(CliAction::parse(["--help"]).unwrap(), CliAction::Help);
        assert_eq!(CliAction::parse(["-V"]).unwrap(), CliAction::Version);
        assert_eq!(
            CliAction::parse(std::iter::empty::<&str>()).unwrap(),
            CliAction::Gui {
                start_minimized: false
            }
        );
        assert_eq!(
            CliAction::parse(["--minimized"]).unwrap(),
            CliAction::Gui {
                start_minimized: true
            }
        );
        assert_eq!(
            CliAction::parse(["--tray"]).unwrap(),
            CliAction::Gui {
                start_minimized: true
            }
        );
        assert_eq!(
            CliAction::parse(["--hidden"]).unwrap(),
            CliAction::Gui {
                start_minimized: true
            }
        );
        assert_eq!(
            CliAction::parse(["zalo://open"]).unwrap(),
            CliAction::Gui {
                start_minimized: false
            }
        );
        assert_eq!(CliAction::parse(["--demo"]).unwrap(), CliAction::Demo);
        assert_eq!(
            CliAction::parse(["--engine-probe"]).unwrap(),
            CliAction::EngineProbe {
                page: ProbePage::Main,
                seconds: 20
            }
        );
        assert_eq!(
            CliAction::parse(["--engine-probe", "login", "12"]).unwrap(),
            CliAction::EngineProbe {
                page: ProbePage::Login,
                seconds: 12
            }
        );
        assert!(CliAction::parse(["--nonexistent-flag"]).is_err());
    }
}
