//! Single-instance IPC guard, profile-scoped socket lock, `zalo://` deep-link forwarding,
//! and local JSON-RPC bridge over a Unix domain socket.
//!
//! Ensures that:
//! - Launching `olaz` from the desktop app launcher while `olaz --minimized` is running in the
//!   system tray immediately presents and focuses the existing primary window instead of opening
//!   a competing WebKit/IndexedDB instance.
//! - Multi-account profiles (`OLAZ_PROFILE` / `--profile <slug>`) acquire isolated per-profile
//!   sockets (`olaz-instance-<slug>.sock`), while `"default"` keeps the exact `olaz-instance.sock` path.
//! - Secondary invocations passing `zalo://...` forward `"deeplink <url>\n"` to the running primary.
//! - Headless CLI / MCP invocations (`olaz --mcp`) can send `"rpc <single-line-json>\n"` over the
//!   Unix socket to query or command the live primary GUI instance.
//! - Stale socket files left behind after a crash (`ECONNREFUSED`) are automatically reclaimed.

use std::future::Future;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use directories::BaseDirs;
use parking_lot::RwLock;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::UnixStream as TokioUnixStream;
use tokio::sync::mpsc;

/// Maximum Unix domain socket path length before falling back to a short `/tmp` hash path
/// (`sockaddr_un.sun_path` is 108 bytes on Linux).
const MAX_DIRECT_SUN_PATH_BYTES: usize = 96;

/// Maximum single-line IPC frame accepted on the instance Unix socket (`1 MiB`).
const MAX_IPC_LINE_BYTES: u64 = 1024 * 1024;

/// Async RPC callback invoked when a client sends `"rpc <single-line-json>\n"` to the primary socket.
pub type RpcHandler = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync,
>;

/// Activation events forwarded from secondary `olaz` invocations to the primary instance
/// (preserved verbatim for backwards compatibility with existing callers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceEvent {
    /// A secondary `olaz` invocation (without `--minimized`) asked the primary instance to show & focus.
    PresentWindow,
    /// A secondary `olaz --minimized` invocation checked in and exited without requesting focus.
    BackgroundPing,
}

/// Profile-aware activation and deep-link events forwarded from secondary `olaz` invocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileInstanceEvent {
    /// A secondary `olaz` invocation (without `--minimized`) asked the primary instance to show & focus.
    PresentWindow,
    /// A secondary `olaz --minimized` invocation checked in and exited without requesting focus.
    BackgroundPing,
    /// A secondary `olaz zalo://...` invocation asked the primary instance to focus and open a deep link.
    OpenDeepLink(String),
}

/// Result of attempting to become the primary `olaz` instance for a given `base_dir`.
pub enum AcquireInstanceOutcome {
    /// Current process owns the single-instance lock and receives secondary activation events.
    Primary(SingleInstanceGuard, mpsc::UnboundedReceiver<InstanceEvent>),
    /// An active primary instance is already running and acknowledged our message; caller should exit `0`.
    ForwardedToPrimary,
}

/// Result of attempting to become the primary `olaz` instance for a profile.
pub enum AcquireProfileInstanceOutcome {
    /// Current process owns the profile's single-instance lock and receives activation/deep-link events.
    Primary(SingleInstanceGuard, mpsc::UnboundedReceiver<ProfileInstanceEvent>),
    /// An active primary instance for this profile acknowledged our message; caller should exit `0`.
    ForwardedToPrimary,
}

struct SharedRpcSlot {
    handler: RwLock<Option<RpcHandler>>,
    tokio_handle: RwLock<Option<tokio::runtime::Handle>>,
}

impl Default for SharedRpcSlot {
    fn default() -> Self {
        Self {
            handler: RwLock::new(None),
            tokio_handle: RwLock::new(None),
        }
    }
}

/// Cloneable handle allowing late registration of the primary instance's local JSON-RPC handler
/// after the Tokio runtime and `ZaloMainProcess` have been initialized.
#[derive(Clone)]
pub struct InstanceRpcHandle {
    slot: Arc<SharedRpcSlot>,
}

impl InstanceRpcHandle {
    /// Registers an async JSON-RPC handler for `"rpc <json>\n"` requests on this primary socket.
    pub fn set_rpc_handler(&self, handler: RpcHandler) {
        if let Ok(rt_handle) = tokio::runtime::Handle::try_current() {
            *self.slot.tokio_handle.write() = Some(rt_handle);
        }
        *self.slot.handler.write() = Some(handler);
    }

    /// Registers a synchronous JSON-RPC handler closure on this primary socket.
    pub fn set_sync_rpc_handler<F>(&self, handler: F)
    where
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        self.set_rpc_handler(Arc::new(move |req| {
            let h = Arc::clone(&handler);
            Box::pin(async move { h(req) })
        }));
    }

    /// Explicitly binds the Tokio runtime handle used to spawn async RPC tasks.
    pub fn set_tokio_handle(&self, handle: tokio::runtime::Handle) {
        *self.slot.tokio_handle.write() = Some(handle);
    }

    /// Clears the registered RPC handler.
    pub fn clear_rpc_handler(&self) {
        *self.slot.handler.write() = None;
    }
}

/// RAII guard holding the primary instance's Unix domain socket listener.
pub struct SingleInstanceGuard {
    sock_path: PathBuf,
    bound_ino: Option<u64>,
    stop: Arc<AtomicBool>,
    rpc_slot: Arc<SharedRpcSlot>,
    deep_link_rx: Option<mpsc::UnboundedReceiver<String>>,
}

impl SingleInstanceGuard {
    /// Path to the bound Unix domain socket.
    pub fn socket_path(&self) -> &Path {
        &self.sock_path
    }

    /// Returns a cloneable [`InstanceRpcHandle`] for registering or updating the socket RPC handler.
    pub fn rpc_handle(&self) -> InstanceRpcHandle {
        InstanceRpcHandle {
            slot: Arc::clone(&self.rpc_slot),
        }
    }

    /// Registers an async RPC handler on this primary instance's Unix domain socket.
    pub fn set_rpc_handler(&self, handler: RpcHandler) {
        self.rpc_handle().set_rpc_handler(handler);
    }

    /// Registers a synchronous RPC handler closure on this primary instance's Unix domain socket.
    pub fn set_sync_rpc_handler<F>(&self, handler: F)
    where
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        self.rpc_handle().set_sync_rpc_handler(handler);
    }

    /// Explicitly sets the Tokio runtime handle used to drive async [`RpcHandler`] futures.
    pub fn set_tokio_handle(&self, handle: tokio::runtime::Handle) {
        self.rpc_handle().set_tokio_handle(handle);
    }

    /// Takes the auxiliary deep-link receiver populated when legacy [`acquire_or_notify_primary`] is used.
    pub fn take_deep_link_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.deep_link_rx.take()
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.sock_path);
        let should_unlink = match (self.bound_ino, std::fs::symlink_metadata(&self.sock_path)) {
            (Some(expected_ino), Ok(meta)) => meta.ino() == expected_ino,
            (None, Ok(_)) => true,
            _ => false,
        };
        if should_unlink {
            let _ = std::fs::remove_file(&self.sock_path);
        }
    }
}

/// Sanitizes an account profile identifier to `[A-Za-z0-9_-]+`, normalizing empty and `"default"`
/// (case-insensitive) to `"default"`.
pub fn sanitize_instance_profile(profile: &str) -> String {
    let trimmed = profile.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        return "default".to_string();
    }
    let slug: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if slug.is_empty() || slug.eq_ignore_ascii_case("default") {
        "default".to_string()
    } else {
        slug
    }
}

/// Resolves the default Olaz root data directory (`OLAZ_DATA_DIR` if set, otherwise `~/.local/share/olaz`).
pub fn default_instance_base_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("OLAZ_DATA_DIR").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(custom);
        if p.is_absolute() {
            return p;
        }
        if let Ok(cwd) = std::env::current_dir() {
            return cwd.join(p);
        }
        return p;
    }
    let base_dirs = BaseDirs::new();
    let home_dir = base_dirs
        .as_ref()
        .map(|b| b.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let data_root = base_dirs
        .as_ref()
        .map(|b| b.data_local_dir().to_path_buf())
        .unwrap_or_else(|| home_dir.join(".local/share"));
    data_root.join("olaz")
}

/// Computes a deterministic `sun_path`-safe socket path scoped to `base_dir` (default profile).
pub fn instance_socket_path(base_dir: &Path) -> PathBuf {
    let direct = base_dir.join("olaz-instance.sock");
    if direct.as_os_str().as_bytes().len() <= MAX_DIRECT_SUN_PATH_BYTES {
        return direct;
    }
    let hash = fnv1a_64(base_dir.as_os_str().as_bytes());
    PathBuf::from(format!("/tmp/olaz-inst-{hash:016x}.sock"))
}

/// Computes a deterministic `sun_path`-safe socket path scoped to `(base_dir, profile)`.
///
/// When `profile == "default"` or `""`, preserves the exact [`instance_socket_path`] (`olaz-instance.sock`).
/// Otherwise uses `<base_dir>/olaz-instance-<slug>.sock` with a `< 96` byte `/tmp` hash fallback.
pub fn instance_socket_path_in_dir(base_dir: &Path, profile: &str) -> PathBuf {
    let slug = sanitize_instance_profile(profile);
    if slug == "default" {
        return instance_socket_path(base_dir);
    }
    let direct = base_dir.join(format!("olaz-instance-{slug}.sock"));
    if direct.as_os_str().as_bytes().len() <= MAX_DIRECT_SUN_PATH_BYTES {
        return direct;
    }
    let mut material = base_dir.as_os_str().as_bytes().to_vec();
    material.push(0);
    material.extend_from_slice(slug.as_bytes());
    let hash = fnv1a_64(&material);
    let short_slug: String = slug.chars().take(16).collect();
    PathBuf::from(format!("/tmp/olaz-inst-{short_slug}-{hash:016x}.sock"))
}

/// Computes the Unix domain socket path for `profile` under [`default_instance_base_dir`].
///
/// When `profile == "default"` or `""`, keeps the exact `olaz-instance.sock` path; otherwise
/// returns `olaz-instance-<slug>.sock` (with the `< 96` byte `/tmp` fallback).
pub fn instance_socket_path_for_profile(profile: &str) -> PathBuf {
    instance_socket_path_in_dir(&default_instance_base_dir(), profile)
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn sanitize_deep_link(deep_link: Option<&str>) -> Option<String> {
    let raw = deep_link?.trim();
    if raw.is_empty() {
        return None;
    }
    let clean: String = raw.chars().filter(|c| *c != '\r' && *c != '\n' && *c != '\0').collect();
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn notify_existing_primary_wire(
    sock_path: &Path,
    request_present: bool,
    deep_link: Option<&str>,
) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));
    let _ = stream.set_read_timeout(Some(Duration::from_millis(750)));
    let wire_line = if let Some(dl) = sanitize_deep_link(deep_link) {
        format!("deeplink {dl}\n")
    } else if request_present {
        "present\n".to_string()
    } else {
        "ping\n".to_string()
    };
    if stream.write_all(wire_line.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    if reader.read_line(&mut reply).is_err() {
        return false;
    }
    reply.trim() == "ok"
}

fn dispatch_rpc_on_connection(
    writer: UnixStream,
    rpc_slot: Arc<SharedRpcSlot>,
    raw_json: String,
) {
    let _ = writer.set_write_timeout(Some(Duration::from_secs(15)));

    let write_json_line = |mut w: UnixStream, val: &Value| {
        if let Ok(mut line) = serde_json::to_string(val) {
            line.push('\n');
            let _ = w.write_all(line.as_bytes());
            let _ = w.flush();
        }
    };

    let req_val = match serde_json::from_str::<Value>(&raw_json) {
        Ok(v) => v,
        Err(err) => {
            write_json_line(
                writer,
                &json!({
                    "ok": false,
                    "error": format!("invalid rpc json: {err}"),
                }),
            );
            return;
        }
    };

    let handler_opt = rpc_slot.handler.read().clone();
    let Some(handler) = handler_opt else {
        write_json_line(
            writer,
            &json!({
                "ok": false,
                "error": "no rpc handler registered on primary instance",
            }),
        );
        return;
    };

    let tokio_handle_opt = rpc_slot.tokio_handle.read().clone();
    if let Some(rt_handle) = tokio_handle_opt {
        rt_handle.spawn(async move {
            let resp = handler(req_val).await;
            let _ = tokio::task::spawn_blocking(move || {
                write_json_line(writer, &resp);
            })
            .await;
        });
    } else {
        let _ = std::thread::Builder::new()
            .name("olaz-rpc-oneshot".to_string())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => {
                        let resp = rt.block_on(handler(req_val));
                        write_json_line(writer, &resp);
                    }
                    Err(err) => {
                        write_json_line(
                            writer,
                            &json!({
                                "ok": false,
                                "error": format!("failed to build rpc runtime: {err}"),
                            }),
                        );
                    }
                }
            });
    }
}

fn bind_and_listen_socket(
    sock_path: PathBuf,
    request_present: bool,
    deep_link: Option<&str>,
) -> std::io::Result<Option<(SingleInstanceGuard, mpsc::UnboundedReceiver<ProfileInstanceEvent>)>> {
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if notify_existing_primary_wire(&sock_path, request_present, deep_link) {
        return Ok(None);
    }

    match std::fs::remove_file(&sock_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }

    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(bind_err) => {
            // Lost a simultaneous startup race; try forwarding once more.
            if notify_existing_primary_wire(&sock_path, request_present, deep_link) {
                return Ok(None);
            }
            return Err(bind_err);
        }
    };

    let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
    let bound_ino = std::fs::symlink_metadata(&sock_path)
        .ok()
        .map(|m| m.ino());

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let rpc_slot = Arc::new(SharedRpcSlot::default());
    if let Ok(rt_handle) = tokio::runtime::Handle::try_current() {
        *rpc_slot.tokio_handle.write() = Some(rt_handle);
    }
    let rpc_slot_thread = Arc::clone(&rpc_slot);
    let (tx, rx) = mpsc::unbounded_channel();

    let _ = std::thread::Builder::new()
        .name("olaz-instance-ipc".to_string())
        .spawn(move || {
            for incoming in listener.incoming() {
                if stop_thread.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = incoming else {
                    if stop_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));
                let Ok(mut writer) = stream.try_clone() else {
                    continue;
                };
                let mut reader = BufReader::new(stream.take(MAX_IPC_LINE_BYTES));
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if let Some(rpc_payload) = trimmed.strip_prefix("rpc ") {
                        dispatch_rpc_on_connection(
                            writer,
                            Arc::clone(&rpc_slot_thread),
                            rpc_payload.trim().to_string(),
                        );
                        continue;
                    }
                    let evt = if trimmed == "present" {
                        Some(ProfileInstanceEvent::PresentWindow)
                    } else if trimmed == "ping" {
                        Some(ProfileInstanceEvent::BackgroundPing)
                    } else if let Some(dl) = trimmed.strip_prefix("deeplink ") {
                        sanitize_deep_link(Some(dl)).map(ProfileInstanceEvent::OpenDeepLink)
                    } else {
                        None
                    };
                    if let Some(ev) = evt {
                        let _ = tx.send(ev);
                        let _ = writer.write_all(b"ok\n");
                        let _ = writer.flush();
                    }
                }
            }
        });

    Ok(Some((
        SingleInstanceGuard {
            sock_path,
            bound_ino,
            stop,
            rpc_slot,
            deep_link_rx: None,
        },
        rx,
    )))
}

/// Acquires the primary instance socket for `(base_dir, profile)` or forwards activation /
/// `zalo://...` deep link (`"deeplink <url>\n"`) to an already-running primary for that profile.
pub fn acquire_or_notify_in_dir(
    base_dir: &Path,
    profile: &str,
    start_hidden: bool,
    deep_link: Option<&str>,
) -> std::io::Result<AcquireProfileInstanceOutcome> {
    let sock_path = instance_socket_path_in_dir(base_dir, profile);
    match bind_and_listen_socket(sock_path, !start_hidden, deep_link)? {
        Some((guard, rx)) => Ok(AcquireProfileInstanceOutcome::Primary(guard, rx)),
        None => Ok(AcquireProfileInstanceOutcome::ForwardedToPrimary),
    }
}

/// Acquires the primary instance socket for `profile` under [`default_instance_base_dir`],
/// or forwards activation / `zalo://...` deep link (`"deeplink <url>\n"`) to the running primary.
pub fn acquire_or_notify_for_profile(
    profile: &str,
    start_hidden: bool,
    deep_link: Option<&str>,
) -> std::io::Result<AcquireProfileInstanceOutcome> {
    acquire_or_notify_in_dir(&default_instance_base_dir(), profile, start_hidden, deep_link)
}

/// Acquires the primary instance socket for `base_dir` (default profile), or forwards an activation
/// request (`request_present == true` -> `PresentWindow`, `false` -> `BackgroundPing`) to an
/// already-running primary.
pub fn acquire_or_notify_primary(
    base_dir: &Path,
    request_present: bool,
) -> std::io::Result<AcquireInstanceOutcome> {
    let sock_path = instance_socket_path(base_dir);
    let Some((mut guard, mut prof_rx)) = bind_and_listen_socket(sock_path, request_present, None)?
    else {
        return Ok(AcquireInstanceOutcome::ForwardedToPrimary);
    };

    let (legacy_tx, legacy_rx) = mpsc::unbounded_channel();
    let (dl_tx, dl_rx) = mpsc::unbounded_channel();
    guard.deep_link_rx = Some(dl_rx);

    let _ = std::thread::Builder::new()
        .name("olaz-instance-event-bridge".to_string())
        .spawn(move || {
            while let Some(ev) = prof_rx.blocking_recv() {
                match ev {
                    ProfileInstanceEvent::PresentWindow => {
                        if legacy_tx.send(InstanceEvent::PresentWindow).is_err() {
                            break;
                        }
                    }
                    ProfileInstanceEvent::BackgroundPing => {
                        if legacy_tx.send(InstanceEvent::BackgroundPing).is_err() {
                            break;
                        }
                    }
                    ProfileInstanceEvent::OpenDeepLink(url) => {
                        let _ = dl_tx.send(url);
                        if legacy_tx.send(InstanceEvent::PresentWindow).is_err() {
                            break;
                        }
                    }
                }
            }
        });

    Ok(AcquireInstanceOutcome::Primary(guard, legacy_rx))
}

/// Sends a single-line JSON-RPC request (`"rpc <json>\n"`) synchronously to `sock_path` and
/// parses the primary instance's JSON reply.
pub fn send_instance_rpc_to_socket(sock_path: &Path, req: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(sock_path).with_context(|| {
        format!(
            "failed to connect to running olaz instance socket at {}",
            sock_path.display()
        )
    })?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .ok();

    let serialized = serde_json::to_string(req).context("failed to serialize RPC request")?;
    let wire = format!("rpc {serialized}\n");
    stream
        .write_all(wire.as_bytes())
        .context("failed to write RPC request to instance socket")?;
    stream
        .flush()
        .context("failed to flush RPC request to instance socket")?;

    let mut reader = BufReader::new(stream.take(MAX_IPC_LINE_BYTES));
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .context("failed to read RPC response from instance socket")?;
    if n == 0 || line.trim().is_empty() {
        bail!("empty RPC response from primary instance socket");
    }
    serde_json::from_str::<Value>(line.trim())
        .context("failed to parse JSON RPC response from primary instance socket")
}

/// Sends a JSON-RPC request to `(base_dir, profile)`'s primary instance socket.
pub fn send_instance_rpc_in_dir(base_dir: &Path, profile: &str, req: &Value) -> Result<Value> {
    let sock_path = instance_socket_path_in_dir(base_dir, profile);
    send_instance_rpc_to_socket(&sock_path, req)
}

/// Sends a JSON-RPC request to `profile`'s primary instance socket under [`default_instance_base_dir`].
pub fn send_instance_rpc(profile: &str, req: &Value) -> Result<Value> {
    let sock_path = instance_socket_path_for_profile(profile);
    send_instance_rpc_to_socket(&sock_path, req)
}

/// Async counterpart of [`send_instance_rpc_to_socket`] using `tokio::net::UnixStream`.
pub async fn send_instance_rpc_to_socket_async(sock_path: &Path, req: &Value) -> Result<Value> {
    let stream = tokio::time::timeout(Duration::from_secs(5), TokioUnixStream::connect(sock_path))
        .await
        .with_context(|| format!("timed out connecting to {}", sock_path.display()))?
        .with_context(|| format!("failed to connect to {}", sock_path.display()))?;

    let serialized = serde_json::to_string(req).context("failed to serialize RPC request")?;
    let wire = format!("rpc {serialized}\n");

    let (read_half, mut write_half) = stream.into_split();
    tokio::time::timeout(Duration::from_secs(5), async {
        write_half.write_all(wire.as_bytes()).await?;
        write_half.flush().await
    })
    .await
    .context("timed out writing RPC request")?
    .context("failed to write RPC request")?;

    let mut reader = TokioBufReader::new(read_half);
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut line))
        .await
        .context("timed out waiting for RPC response")?
        .context("failed to read RPC response")?;
    if n == 0 || line.trim().is_empty() {
        bail!("empty RPC response from primary instance socket");
    }
    serde_json::from_str::<Value>(line.trim())
        .context("failed to parse JSON RPC response from primary instance socket")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn second_invocation_forwards_present_or_ping_to_primary_and_reclaims_stale_socket() {
        let tmp = TempDir::new().unwrap();
        let base_dir = tmp.path().join("olaz-data");

        let (guard, mut rx) = match acquire_or_notify_primary(&base_dir, true).unwrap() {
            AcquireInstanceOutcome::Primary(g, r) => (g, r),
            AcquireInstanceOutcome::ForwardedToPrimary => panic!("expected primary on clean dir"),
        };
        let sock_path = guard.socket_path().to_path_buf();
        assert!(sock_path.exists());

        // Secondary normal launch -> forwards PresentWindow
        assert!(matches!(
            acquire_or_notify_primary(&base_dir, true).unwrap(),
            AcquireInstanceOutcome::ForwardedToPrimary
        ));
        assert_eq!(rx.recv().await, Some(InstanceEvent::PresentWindow));

        // Secondary `--minimized` launch -> forwards BackgroundPing
        assert!(matches!(
            acquire_or_notify_primary(&base_dir, false).unwrap(),
            AcquireInstanceOutcome::ForwardedToPrimary
        ));
        assert_eq!(rx.recv().await, Some(InstanceEvent::BackgroundPing));

        // Dropping primary guard removes socket
        drop(guard);
        assert!(!sock_path.exists());

        // Simulate stale crash file at `sock_path`
        std::fs::write(&sock_path, b"stale").unwrap();
        let (guard2, _rx2) = match acquire_or_notify_primary(&base_dir, true).unwrap() {
            AcquireInstanceOutcome::Primary(g, r) => (g, r),
            AcquireInstanceOutcome::ForwardedToPrimary => {
                panic!("stale non-listening file must be reclaimed")
            }
        };
        drop(guard2);
    }

    #[test]
    fn long_base_dir_falls_back_to_short_sun_path() {
        let deep = PathBuf::from(format!("/tmp/{}", "very_long_segment_".repeat(10)));
        let sock = instance_socket_path(&deep);
        assert!(sock.as_os_str().as_bytes().len() <= MAX_DIRECT_SUN_PATH_BYTES);
        let prof_sock = instance_socket_path_in_dir(&deep, "work_account_secondary_profile");
        assert!(prof_sock.as_os_str().as_bytes().len() <= MAX_DIRECT_SUN_PATH_BYTES);
    }
}
