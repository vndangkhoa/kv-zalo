//! CLI entrypoint for `olaz-web` stateless self-hosted relay.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use olaz_core::{InstallProgress, ZaloRuntimeManager};
use olaz_web::{start_web_server, AllowlistConfig, BundleProvider, WebServerConfig};
use tracing::info;

const DEFAULT_LISTEN: &str = "127.0.0.1:8080";

const HELP_TEXT: &str = "\
olaz-web — Zero-Server-Persistence Self-Hosted Zalo Web Relay

USAGE:
    olaz-web [OPTIONS]

OPTIONS:
    --listen <ADDR:PORT>     Bind address & port [env: OLAZ_WEB_LISTEN] [default: 127.0.0.1:8080]
    --asar <PATH>            Path to read-only app.asar (or unpacked bundle dir) [env: OLAZ_ASAR_PATH]
                             Auto-discovers ~/.local/share/olaz/ZaloRuntime/current/Contents/Resources/app.asar
    --fetch-asar <DEST_DIR>  Download & SHA-256 verify official VNG app.asar into <DEST_DIR> and exit
    --token <SECRET>         Optional constant-time edge auth token [env: OLAZ_WEB_TOKEN]
    -h, --help               Print this help message and exit

ZERO-PERSISTENCE GUARANTEE:
    olaz-web never stores cookies (zpw_sek), credentials, keys, or chat messages on the server.
    All user state lives strictly in the user's browser (IndexedDB + localStorage + OPFS).
";

#[derive(Debug)]
enum CliAction {
    Help,
    FetchAsar {
        dest_dir: PathBuf,
    },
    Serve {
        listen: SocketAddr,
        asar_path: PathBuf,
        token: Option<String>,
    },
}

fn parse_cli<I>(mut args: I) -> Result<CliAction>
where
    I: Iterator<Item = String>,
{
    let mut listen_raw: Option<String> = std::env::var("OLAZ_WEB_LISTEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let mut asar_raw: Option<PathBuf> = std::env::var_os("OLAZ_ASAR_PATH")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let mut token: Option<String> = std::env::var("OLAZ_WEB_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let mut fetch_dest: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(CliAction::Help),
            "--listen" => {
                let val = args.next().context("--listen requires <ADDR:PORT>")?;
                listen_raw = Some(val);
            }
            a if a.starts_with("--listen=") => {
                listen_raw = Some(a["--listen=".len()..].to_string());
            }
            "--asar" => {
                let val = args.next().context("--asar requires <PATH>")?;
                asar_raw = Some(PathBuf::from(val));
            }
            a if a.starts_with("--asar=") => {
                asar_raw = Some(PathBuf::from(&a["--asar=".len()..]));
            }
            "--fetch-asar" => {
                let val = args.next().context("--fetch-asar requires <DEST_DIR>")?;
                fetch_dest = Some(PathBuf::from(val));
            }
            a if a.starts_with("--fetch-asar=") => {
                fetch_dest = Some(PathBuf::from(&a["--fetch-asar=".len()..]));
            }
            "--token" => {
                let val = args.next().context("--token requires <SECRET>")?;
                token = Some(val);
            }
            a if a.starts_with("--token=") => {
                token = Some(a["--token=".len()..].to_string());
            }
            unknown => bail!("unknown argument '{unknown}' (pass --help for usage)"),
        }
    }

    if let Some(dest_dir) = fetch_dest {
        return Ok(CliAction::FetchAsar { dest_dir });
    }

    let listen_str = listen_raw.unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let listen: SocketAddr = listen_str
        .parse()
        .with_context(|| format!("invalid --listen socket address '{listen_str}'"))?;

    let asar_path = match asar_raw {
        Some(p) => p,
        None => discover_default_asar_path()?,
    };

    Ok(CliAction::Serve {
        listen,
        asar_path,
        token,
    })
}

fn discover_default_asar_path() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        candidates.push(
            PathBuf::from(xdg).join("olaz/ZaloRuntime/current/Contents/Resources/app.asar"),
        );
    }
    if let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        candidates.push(
            PathBuf::from(home)
                .join(".local/share/olaz/ZaloRuntime/current/Contents/Resources/app.asar"),
        );
    }
    candidates.push(PathBuf::from("/opt/olaz/app.asar"));

    for cand in &candidates {
        if cand.exists() {
            return Ok(cand.clone());
        }
    }

    bail!(
        "could not locate app.asar automatically (checked {}). Pass --asar <PATH>, set OLAZ_ASAR_PATH, or run `olaz-web --fetch-asar <DEST_DIR>` first.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

async fn run_fetch_asar(dest_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("failed to create destination directory {}", dest_dir.display()))?;
    let mgr = ZaloRuntimeManager::new(dest_dir);
    info!(dest = %dest_dir.display(), "fetching & SHA-256 verifying official VNG Zalo runtime...");
    let version = mgr
        .install(|progress| match progress {
            InstallProgress::Looking => info!("resolving VNG release metadata..."),
            InstallProgress::Downloading { received, total } => {
                if let Some(tot) = total {
                    info!(received_mb = received / (1024 * 1024), total_mb = tot / (1024 * 1024), "downloading DMG...");
                } else {
                    info!(received_mb = received / (1024 * 1024), "downloading DMG...");
                }
            }
            InstallProgress::Verifying => info!("verifying SHA-256 digest..."),
            InstallProgress::Extracting => info!("extracting app.asar..."),
            InstallProgress::Installing => info!("promoting runtime atomically..."),
            InstallProgress::Complete => info!("runtime installation complete"),
        })
        .await?;

    let installed_asar = mgr.current_dir().join("Contents/Resources/app.asar");
    println!(
        "Installed Zalo runtime v{version} -> {}",
        installed_asar.display()
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    match parse_cli(std::env::args().skip(1))? {
        CliAction::Help => {
            print!("{HELP_TEXT}");
            Ok(())
        }
        CliAction::FetchAsar { dest_dir } => run_fetch_asar(&dest_dir).await,
        CliAction::Serve {
            listen,
            asar_path,
            token,
        } => {
            let bundle = BundleProvider::open_auto(&asar_path)?;
            let config = WebServerConfig {
                listen_addr: listen,
                bundle,
                token,
                // Strictly false in production CLI: loopback & RFC1918 IPs are always rejected!
                allowlist: AllowlistConfig {
                    allow_loopback_upstream_for_tests: false,
                    dns_overrides: Default::default(),
                },
            };

            let handle = start_web_server(config).await?;
            tokio::signal::ctrl_c()
                .await
                .context("failed waiting for SIGINT / Ctrl-C")?;
            info!("shutting down olaz-web...");
            handle.shutdown().await;
            Ok(())
        }
    }
}
