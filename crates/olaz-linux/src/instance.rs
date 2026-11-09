//! Single-instance IPC guard and window re-activation over a local Unix domain socket.
//!
//! Ensures that:
//! - Launching `olaz` from the desktop app launcher while `olaz --minimized` is running in the
//!   system tray immediately presents and focuses the existing primary window instead of opening
//!   a competing WebKit/IndexedDB instance.
//! - A duplicate `olaz --minimized` invocation exits quietly without disturbing a hidden tray window.
//! - Stale socket files left behind after a crash (`ECONNREFUSED`) are automatically reclaimed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

/// Maximum Unix domain socket path length before falling back to a short `/tmp` hash path
/// (`sockaddr_un.sun_path` is 108 bytes on Linux).
const MAX_DIRECT_SUN_PATH_BYTES: usize = 96;

/// Activation events forwarded from secondary `olaz` invocations to the primary instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceEvent {
    /// A secondary `olaz` invocation (without `--minimized`) asked the primary instance to show & focus.
    PresentWindow,
    /// A secondary `olaz --minimized` invocation checked in and exited without requesting focus.
    BackgroundPing,
}

/// Result of attempting to become the primary `olaz` instance for a given `base_dir`.
pub enum AcquireInstanceOutcome {
    /// Current process owns the single-instance lock and receives secondary activation events.
    Primary(SingleInstanceGuard, mpsc::UnboundedReceiver<InstanceEvent>),
    /// An active primary instance is already running and acknowledged our message; caller should exit `0`.
    ForwardedToPrimary,
}

/// RAII guard holding the primary instance's Unix domain socket listener.
pub struct SingleInstanceGuard {
    sock_path: PathBuf,
    bound_ino: Option<u64>,
    stop: Arc<AtomicBool>,
}

impl SingleInstanceGuard {
    /// Path to the bound Unix domain socket.
    pub fn socket_path(&self) -> &Path {
        &self.sock_path
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

/// Computes a deterministic `sun_path`-safe socket path scoped to `base_dir`.
pub fn instance_socket_path(base_dir: &Path) -> PathBuf {
    let direct = base_dir.join("olaz-instance.sock");
    if direct.as_os_str().as_bytes().len() <= MAX_DIRECT_SUN_PATH_BYTES {
        return direct;
    }
    let hash = fnv1a_64(base_dir.as_os_str().as_bytes());
    PathBuf::from(format!("/tmp/olaz-inst-{hash:016x}.sock"))
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn notify_existing_primary(sock_path: &Path, request_present: bool) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));
    let _ = stream.set_read_timeout(Some(Duration::from_millis(750)));
    let msg: &[u8] = if request_present {
        b"present\n"
    } else {
        b"ping\n"
    };
    if stream.write_all(msg).is_err() || stream.flush().is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    if reader.read_line(&mut reply).is_err() {
        return false;
    }
    reply.trim() == "ok"
}

/// Acquires the primary instance socket for `base_dir`, or forwards an activation request
/// (`request_present == true` -> `PresentWindow`, `false` -> `BackgroundPing`) to an already-running primary.
pub fn acquire_or_notify_primary(
    base_dir: &Path,
    request_present: bool,
) -> std::io::Result<AcquireInstanceOutcome> {
    let sock_path = instance_socket_path(base_dir);
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if notify_existing_primary(&sock_path, request_present) {
        return Ok(AcquireInstanceOutcome::ForwardedToPrimary);
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
            if notify_existing_primary(&sock_path, request_present) {
                return Ok(AcquireInstanceOutcome::ForwardedToPrimary);
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
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
                let Ok(mut writer) = stream.try_clone() else {
                    continue;
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let evt = match line.trim() {
                        "present" => Some(InstanceEvent::PresentWindow),
                        "ping" => Some(InstanceEvent::BackgroundPing),
                        _ => None,
                    };
                    if let Some(ev) = evt {
                        let _ = tx.send(ev);
                        let _ = writer.write_all(b"ok\n");
                        let _ = writer.flush();
                    }
                }
            }
        });

    Ok(AcquireInstanceOutcome::Primary(
        SingleInstanceGuard {
            sock_path,
            bound_ino,
            stop,
        },
        rx,
    ))
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
    }
}
