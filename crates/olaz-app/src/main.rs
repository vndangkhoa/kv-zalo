//! Olaz — Native Linux Desktop Client for Zalo (Omarchy & Ubuntu).
//!
//! Entry point initializing Wayland/NVIDIA WebKit2GTK flags, Tokio async runtime,
//! GTK3 + `webkit2gtk-4.1`, `ZaloRuntimeManager`, `OfficialClientHost`, `ZaloMainProcess`,
//! `OlazAppWindow`, and the `--engine-probe` diagnostic mode (`EngineProbe.swift` port).

mod account_session;
pub mod call_ui;
mod client_host;
mod main_process;
mod ui_window;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use account_session::{link_shared_runtime_for_profile, resolve_instance_root_dir};
use anyhow::{bail, Context, Result};
use base64::Engine as _;
use client_host::{
    init_gst_media_proxy_listener, HostPage, OfficialClientHost, PageEvent, ZaloWebRuntime,
};
use main_process::ZaloMainProcess;
use olaz_core::{ZaloPlatform, ZaloRuntimeManager};
use olaz_linux::{
    acquire_or_notify_in_dir, check_for_updates, configure_linux_webview_env,
    download_and_verify_artifact, make_instance_rpc_handler, prune_old_screenshots, run_mcp_stdio,
    AcquireProfileInstanceOutcome, DownloadLimits, OlazControlBackend, SocketRpcBackend,
    DEFAULT_API_PORT, SCREENSHOT_RETENTION, SCREENSHOT_STAGING_DIR,
};
use olaz_net::{
    parse_upstream_proxy, MediaLoader, NetworkProxy, ZaloCookieJar, ZaloDnsResolver,
};
use tracing_subscriber::EnvFilter;
use ui_window::OlazAppWindow;

/// Parsed CLI invocation mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Help,
    Version,
    Mcp,
    CheckUpdate { download: bool },
    Gui { start_minimized: bool },
    Demo,
    EngineProbe { page: ProbePage, seconds: u64 },
}

/// Complete parsed command-line configuration (mode + profile/proxy/API/deep-link options).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliConfig {
    pub action: CliAction,
    pub profile: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub proxy_url: Option<String>,
    pub api_port: Option<u16>,
    pub deep_link: Option<String>,
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
    /// Parses command-line arguments (excluding `argv[0]`) and returns the primary [`CliAction`].
    pub fn parse<I, S>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Ok(CliConfig::parse(args)?.action)
    }
}

impl CliConfig {
    /// Parses command-line arguments (excluding `argv[0]`) into a full [`CliConfig`].
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
            return Ok(Self {
                action: CliAction::Help,
                profile: None,
                data_dir: None,
                proxy_url: None,
                api_port: None,
                deep_link: None,
            });
        }
        if collected
            .iter()
            .any(|a| matches!(a.as_str(), "-V" | "--version"))
        {
            return Ok(Self {
                action: CliAction::Version,
                profile: None,
                data_dir: None,
                proxy_url: None,
                api_port: None,
                deep_link: None,
            });
        }

        let mut profile: Option<String> = None;
        let mut data_dir: Option<PathBuf> = None;
        let mut proxy_url: Option<String> = None;
        let mut api_port: Option<u16> = None;
        let mut deep_link: Option<String> = None;
        let mut start_minimized = false;
        let mut is_demo = false;
        let mut is_mcp = false;
        let mut check_update: Option<bool> = None;
        let mut engine_probe: Option<(ProbePage, u64)> = None;

        let mut idx = 0usize;
        while idx < collected.len() {
            let arg = &collected[idx];
            match arg.as_str() {
                "--demo" => {
                    is_demo = true;
                }
                "--mcp" => {
                    is_mcp = true;
                }
                "--check-update" => {
                    check_update = Some(check_update.unwrap_or(false));
                }
                "--update" => {
                    check_update = Some(true);
                }
                "--minimized" | "--tray" | "--hidden" => {
                    start_minimized = true;
                }
                "--profile" => {
                    idx += 1;
                    let val = collected
                        .get(idx)
                        .context("`--profile` requires a profile slug (e.g. `--profile work`)")?;
                    profile = Some(val.clone());
                }
                _ if arg.starts_with("--profile=") => {
                    profile = Some(arg["--profile=".len()..].to_string());
                }
                "--data-dir" => {
                    idx += 1;
                    let val = collected
                        .get(idx)
                        .context("`--data-dir` requires a directory path")?;
                    data_dir = Some(PathBuf::from(val));
                }
                _ if arg.starts_with("--data-dir=") => {
                    data_dir = Some(PathBuf::from(&arg["--data-dir=".len()..]));
                }
                "--proxy" => {
                    idx += 1;
                    let val = collected
                        .get(idx)
                        .context("`--proxy` requires a proxy URL (e.g. `socks5://127.0.0.1:1080`)")?;
                    proxy_url = Some(val.clone());
                }
                _ if arg.starts_with("--proxy=") => {
                    proxy_url = Some(arg["--proxy=".len()..].to_string());
                }
                "--api" => {
                    api_port = Some(DEFAULT_API_PORT);
                }
                "--api-port" => {
                    idx += 1;
                    let val = collected
                        .get(idx)
                        .context("`--api-port` requires a TCP port number (e.g. `17890`)")?;
                    let parsed: u16 = val
                        .parse()
                        .with_context(|| format!("invalid `--api-port` value `{val}`"))?;
                    api_port = Some(parsed);
                }
                _ if arg.starts_with("--api-port=") => {
                    let val = &arg["--api-port=".len()..];
                    let parsed: u16 = val
                        .parse()
                        .with_context(|| format!("invalid `--api-port` value `{val}`"))?;
                    api_port = Some(parsed);
                }
                "--engine-probe" => {
                    let mut page = ProbePage::Main;
                    let mut seconds = 20_u64;
                    if let Some(arg1) = collected.get(idx + 1) {
                        match arg1.as_str() {
                            "login" => {
                                page = ProbePage::Login;
                                idx += 1;
                            }
                            "main" => {
                                page = ProbePage::Main;
                                idx += 1;
                            }
                            _ => {}
                        }
                    }
                    if let Some(arg2) = collected.get(idx + 1) {
                        if let Ok(parsed_secs) = arg2.parse::<u64>() {
                            seconds = parsed_secs.max(1);
                            idx += 1;
                        }
                    }
                    engine_probe = Some((page, seconds));
                }
                _ if arg.starts_with("--gapplication-") => {}
                _ if arg.starts_with("--") => {
                    bail!(
                        "Unknown option `{arg}`.\nRun `olaz --help` to see supported options."
                    );
                }
                _ => {
                    let trimmed = arg.trim();
                    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("zalo://") {
                        deep_link = Some(trimmed.to_string());
                    }
                }
            }
            idx += 1;
        }

        let action = if is_mcp {
            CliAction::Mcp
        } else if let Some(download) = check_update {
            CliAction::CheckUpdate { download }
        } else if is_demo {
            CliAction::Demo
        } else if let Some((page, seconds)) = engine_probe {
            CliAction::EngineProbe { page, seconds }
        } else {
            CliAction::Gui { start_minimized }
        };

        Ok(Self {
            action,
            profile,
            data_dir,
            proxy_url,
            api_port,
            deep_link,
        })
    }
}

fn print_help() {
    println!(
        "\
Olaz {version} — Native Linux client for Zalo (Omarchy & Ubuntu)

USAGE:
    olaz [OPTIONS] [zalo://...]

OPTIONS:
    -h, --help                         Print this help information and exit
    -V, --version                      Print version and exit
        --minimized, --tray, --hidden  Launch hidden in the system tray (used by XDG autostart)
        --profile <SLUG>               Launch an isolated Zalo account profile (`[A-Za-z0-9_-]+`, default: `default`)
        --data-dir <PATH>              Override Olaz root data directory (default: `~/.local/share/olaz`)
        --proxy <URL>                  Route HTTP/WebSocket/DoH/WebView traffic via `socks5://`, `socks5h://`, `http://`, or `https://`
        --api, --api-port <PORT>       Start local loopback HTTP REST & MCP server on `127.0.0.1:<PORT>` (default: `{default_api_port}`)
        --mcp                          Run Model Context Protocol (MCP) JSON-RPC 2.0 server over `stdio`, bridging to the active GUI instance
        --check-update                 Check GitHub / Forgejo Releases for a newer Olaz version and exit
        --update                       Check for updates and download & SHA-256-verify the latest release package
        --demo                         Launch Olaz UI in offline mock/demo mode (`?demo=1`)
        --engine-probe [PAGE] [SECS]   Boot headless Zalo renderer + debug window (`login` or `main`, default: `main` 20s)

ENVIRONMENT:
    OLAZ_PROFILE                       Default profile slug when `--profile` is omitted
    OLAZ_DATA_DIR                      Default root data directory when `--data-dir` is omitted
    OLAZ_PROXY                         Upstream SOCKS5/HTTP(S) proxy URL when `--proxy` is omitted
    OLAZ_API_PORT                      Loopback REST API port when `--api-port` is omitted
    OLAZ_API_TOKEN                     Optional Bearer token required by the loopback REST API server
    RUST_LOG                           Tracing filter (e.g. `olaz=debug,olaz_app=debug,olaz_net=debug`)
    OLAZ_TRACE_WORKERS=1               Trace DedicatedWorker / SharedWorker IPC frames
    OLAZ_NO_SHARED_WORKER=1            Disable hidden `shared-worker.html` host
    OLAZ_ENABLE_WEBKIT_HWACCEL=1       Re-enable WebKit hardware acceleration on UI webview
    OLAZ_QR_OUT=/path/to/qr.png        Save login QR code PNG in `--engine-probe login` mode",
        version = env!("CARGO_PKG_VERSION"),
        default_api_port = DEFAULT_API_PORT,
    );
}

fn init_tracing(stderr_writer: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("olaz=info,olaz_app=info,olaz_core=info,olaz_net=info,olaz_linux=info,warn")
    });
    if stderr_writer {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .try_init();
    }
}

async fn run_cli_update_check(platform: &ZaloPlatform, download: bool) -> Result<()> {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(60));
    if let Some(raw_proxy) = platform.proxy_url() {
        if let Some((_, proxy)) = parse_upstream_proxy(raw_proxy)? {
            builder = builder.proxy(proxy);
        }
    }
    let client = builder.build().context("failed to build HTTP client")?;
    let info = check_for_updates(&client).await?;

    println!("Current version : v{}", info.current_version.trim_start_matches('v'));
    println!("Latest version  : v{} ({:?})", info.latest_version.trim_start_matches('v'), info.source);
    println!("Release URL     : {}", info.html_url);

    if !info.has_update {
        println!("Olaz is up to date.");
        return Ok(());
    }

    println!("Update available: {}", info.release_title);
    if let Some(ref asset) = info.asset_name {
        println!("Recommended pkg : {asset} ({} bytes)", info.asset_size);
    }

    if download {
        let dl_url = info
            .download_url
            .as_deref()
            .context("release does not expose a compatible Linux package asset")?;
        let asset_name = info
            .asset_name
            .as_deref()
            .context("release asset name is missing")?;
        let expected_sha = info
            .expected_sha256
            .as_deref()
            .context("release does not provide a SHA-256 digest (`SHA256SUMS.txt`); refusing unverified download")?;

        std::fs::create_dir_all(platform.downloads_dir())
            .context("failed to create downloads directory")?;
        let dest = platform.downloads_dir().join(asset_name);
        println!("Downloading & verifying SHA-256 ({expected_sha})...");
        let bytes = download_and_verify_artifact(
            &client,
            dl_url,
            asset_name,
            expected_sha,
            DownloadLimits::default(),
            Some(&dest),
        )
        .await?;
        println!(
            "Verified {} bytes -> {}",
            bytes.len(),
            dest.display()
        );
    }

    Ok(())
}

fn main() -> Result<()> {
    // Handle `--help` and `--version` BEFORE initializing GTK so headless CI / terminals succeed.
    let cli = CliConfig::parse(std::env::args().skip(1))?;
    match cli.action {
        CliAction::Help => {
            print_help();
            return Ok(());
        }
        CliAction::Version => {
            println!("olaz {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliAction::Mcp
        | CliAction::CheckUpdate { .. }
        | CliAction::Gui { .. }
        | CliAction::Demo
        | CliAction::EngineProbe { .. } => {}
    }

    init_tracing(matches!(cli.action, CliAction::Mcp));

    let effective_profile = cli
        .profile
        .clone()
        .or_else(|| std::env::var("OLAZ_PROFILE").ok().filter(|s| !s.trim().is_empty()));
    let effective_data_dir = cli.data_dir.clone().or_else(|| {
        std::env::var_os("OLAZ_DATA_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    });
    let effective_proxy = cli
        .proxy_url
        .clone()
        .or_else(|| std::env::var("OLAZ_PROXY").ok().filter(|s| !s.trim().is_empty()));
    let effective_api_port = match cli.api_port {
        Some(p) => Some(p),
        None => match std::env::var("OLAZ_API_PORT") {
            Ok(raw) if !raw.trim().is_empty() => Some(
                raw.trim()
                    .parse::<u16>()
                    .with_context(|| format!("invalid OLAZ_API_PORT `{raw}`"))?,
            ),
            _ => None,
        },
    };

    let platform_raw = ZaloPlatform::from_options(
        effective_profile.as_deref(),
        effective_data_dir,
        effective_proxy,
    )?;
    platform_raw.set_app_version(env!("CARGO_PKG_VERSION"));
    let instance_root_dir = resolve_instance_root_dir(&platform_raw);

    // Headless `--mcp` and `--check-update` / `--update` modes run on Tokio BEFORE `gtk::init()`
    // so they work cleanly in SSH sessions, AI agent hosts, and headless terminals.
    if matches!(cli.action, CliAction::Mcp | CliAction::CheckUpdate { .. }) {
        let headless_rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("olaz-cli")
            .build()
            .context("failed to build Tokio runtime")?;
        return match cli.action {
            CliAction::Mcp => {
                let backend: Arc<dyn OlazControlBackend> = Arc::new(
                    SocketRpcBackend::for_profile_in_dir(
                        &instance_root_dir,
                        platform_raw.profile_slug(),
                    ),
                );
                headless_rt.block_on(run_mcp_stdio(backend))
            }
            CliAction::CheckUpdate { download } => {
                headless_rt.block_on(run_cli_update_check(&platform_raw, download))
            }
            _ => unreachable!(),
        };
    }

    // Prepare platform directories and enforce profile-scoped single-instance re-activation BEFORE
    // starting GTK so a launcher click or `zalo://` link wakes the running instance immediately.
    link_shared_runtime_for_profile(&platform_raw, &instance_root_dir);
    let mut instance_guard = None;
    let mut instance_rx = None;
    if let CliAction::Gui { start_minimized } = cli.action {
        match acquire_or_notify_in_dir(
            &instance_root_dir,
            platform_raw.profile_slug(),
            start_minimized,
            cli.deep_link.as_deref(),
        ) {
            Ok(AcquireProfileInstanceOutcome::Primary(guard, rx)) => {
                instance_guard = Some(guard);
                instance_rx = Some(rx);
            }
            Ok(AcquireProfileInstanceOutcome::ForwardedToPrimary) => {
                tracing::info!(
                    profile = %platform_raw.profile_slug(),
                    start_minimized,
                    deep_link = ?cli.deep_link,
                    "olaz is already running for this profile; notified primary instance and exiting"
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

    // Configure `WEBKIT_DISABLE_DMABUF_RENDERER=1` (Wayland), `GDK_BACKEND=wayland,x11`, and
    // `OLAZ_GST_MEDIA_PROXY_BASE` BEFORE building the multi-threaded Tokio runtime or calling `gtk::init()`.
    configure_linux_webview_env();
    init_gst_media_proxy_listener();

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

    let direct_ui = matches!(cli.action, CliAction::Gui { .. });
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

    match cli.action {
        CliAction::Gui { start_minimized } => {
            let window = OlazAppWindow::launch_with_profile_options(
                platform,
                runtime_mgr,
                host,
                main_process,
                tokio_handle.clone(),
                false,
                start_minimized,
                instance_rx,
                effective_api_port,
            );
            if let Some(ref guard) = instance_guard {
                guard.set_tokio_handle(tokio_handle);
                guard.set_rpc_handler(make_instance_rpc_handler(window.control_backend()));
            }
            if let Some(ref dl) = cli.deep_link {
                window.handle_deep_link(dl);
            }
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
        CliAction::Help
        | CliAction::Version
        | CliAction::Mcp
        | CliAction::CheckUpdate { .. } => unreachable!(),
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

    #[test]
    fn cli_config_parses_profile_proxy_api_mcp_updater_and_deep_link() {
        let cfg = CliConfig::parse([
            "--profile",
            "work",
            "--data-dir=/tmp/olaz-custom",
            "--proxy",
            "socks5h://127.0.0.1:1080",
            "--api-port=18890",
            "--minimized",
            "zalo://conversation/123456",
        ])
        .unwrap();
        assert_eq!(
            cfg.action,
            CliAction::Gui {
                start_minimized: true
            }
        );
        assert_eq!(cfg.profile.as_deref(), Some("work"));
        assert_eq!(cfg.data_dir, Some(PathBuf::from("/tmp/olaz-custom")));
        assert_eq!(cfg.proxy_url.as_deref(), Some("socks5h://127.0.0.1:1080"));
        assert_eq!(cfg.api_port, Some(18890));
        assert_eq!(cfg.deep_link.as_deref(), Some("zalo://conversation/123456"));

        let api_default = CliConfig::parse(["--api", "--profile=personal"]).unwrap();
        assert_eq!(api_default.api_port, Some(DEFAULT_API_PORT));
        assert_eq!(api_default.profile.as_deref(), Some("personal"));

        let mcp_cfg = CliConfig::parse(["--mcp", "--profile", "work"]).unwrap();
        assert_eq!(mcp_cfg.action, CliAction::Mcp);
        assert_eq!(mcp_cfg.profile.as_deref(), Some("work"));

        assert_eq!(
            CliAction::parse(["--check-update"]).unwrap(),
            CliAction::CheckUpdate { download: false }
        );
        assert_eq!(
            CliAction::parse(["--update"]).unwrap(),
            CliAction::CheckUpdate { download: true }
        );
        assert!(CliConfig::parse(["--profile"]).is_err());
        assert!(CliConfig::parse(["--api-port", "not-a-port"]).is_err());
    }

    #[test]
    fn resolve_instance_root_dir_matches_across_default_and_named_profiles() {
        let dir = std::env::temp_dir().join(format!("olaz-main-root-{}", uuid::Uuid::new_v4()));
        let default_plat = ZaloPlatform::from_options(Some("default"), Some(dir.clone()), None).unwrap();
        let work_plat = ZaloPlatform::from_options(Some("work"), Some(dir.clone()), None).unwrap();
        assert_eq!(resolve_instance_root_dir(&default_plat), dir);
        assert_eq!(resolve_instance_root_dir(&work_plat), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
