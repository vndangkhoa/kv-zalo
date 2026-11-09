//! XDG Autostart (`$XDG_CONFIG_HOME/autostart/olaz.desktop`) and persistent start-in-tray state.
//!
//! Compatible across both `systemd-xdg-autostart-generator` (`xdg-desktop-autostart.target` on
//! Omarchy Arch/Hyprland) and traditional XDG desktop sessions (Ubuntu GNOME, KDE Plasma, XFCE).
//! Intentionally omits `OnlyShowIn=` so compositors such as `XDG_CURRENT_DESKTOP=Hyprland` never
//! skip the autostart unit.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use directories::BaseDirs;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Filename written inside `$XDG_CONFIG_HOME/autostart/`.
pub const AUTOSTART_DESKTOP_FILENAME: &str = "olaz.desktop";

/// Persistent JSON state filename stored inside `<platform.base_dir()>/`.
pub const AUTOSTART_STATE_FILENAME: &str = "olaz-autostart.json";

const fn default_true() -> bool {
    true
}

/// Persisted autostart & start-in-tray configuration exposed to Zalo's General Settings UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartState {
    /// Whether Olaz launches automatically when the desktop session starts.
    pub open_at_login: bool,
    /// Whether autostart passes `--minimized` so Olaz launches silently into the system tray.
    #[serde(default = "default_true")]
    pub start_in_tray: bool,
}

impl Default for AutostartState {
    fn default() -> Self {
        Self {
            open_at_login: false,
            start_in_tray: true,
        }
    }
}

/// Manages `<base_dir>/olaz-autostart.json` and `<autostart_dir>/olaz.desktop`.
#[derive(Debug)]
pub struct AutostartManager {
    state_file: PathBuf,
    autostart_dir: PathBuf,
    exe_path: PathBuf,
    notify_systemd: bool,
    lock: Mutex<()>,
}

impl AutostartManager {
    /// Creates an [`AutostartManager`] using the host's current `olaz` executable path.
    pub fn new(base_dir: &Path, autostart_dir: &Path, notify_systemd: bool) -> Self {
        Self::with_paths(
            base_dir.join(AUTOSTART_STATE_FILENAME),
            autostart_dir.to_path_buf(),
            resolve_olaz_executable_path(),
            notify_systemd,
        )
    }

    /// Creates an [`AutostartManager`] with explicit paths (used directly by isolated unit tests).
    pub fn with_paths(
        state_file: impl Into<PathBuf>,
        autostart_dir: impl Into<PathBuf>,
        exe_path: impl Into<PathBuf>,
        notify_systemd: bool,
    ) -> Self {
        Self {
            state_file: state_file.into(),
            autostart_dir: autostart_dir.into(),
            exe_path: exe_path.into(),
            notify_systemd,
            lock: Mutex::new(()),
        }
    }

    /// Full path to `<autostart_dir>/olaz.desktop`.
    pub fn desktop_file_path(&self) -> PathBuf {
        self.autostart_dir.join(AUTOSTART_DESKTOP_FILENAME)
    }

    /// Full path to `<base_dir>/olaz-autostart.json`.
    pub fn state_file_path(&self) -> &Path {
        &self.state_file
    }

    /// Returns `true` if `<base_dir>/olaz-autostart.json` has been explicitly written.
    pub fn has_persisted_state(&self) -> bool {
        self.state_file.exists()
    }

    /// Reads the current autostart state from disk (falling back to inspecting `olaz.desktop`
    /// if present, or [`AutostartState::default`]).
    pub fn state(&self) -> AutostartState {
        let _guard = self.lock.lock();
        self.read_state_unlocked(None)
    }

    /// Updates `open_at_login` ("Start Zalo on system startup").
    ///
    /// When transitioning from `false -> true`, `start_in_tray` automatically defaults to `true`
    /// so enabling system autostart launches minimized to the tray out of the box.
    pub fn set_open_at_login(&self, enabled: bool) -> Result<AutostartState> {
        let _guard = self.lock.lock();
        let prev = self.read_state_unlocked(None);
        let next_start_in_tray = if enabled && !prev.open_at_login {
            true
        } else {
            prev.start_in_tray
        };
        let next = AutostartState {
            open_at_login: enabled,
            start_in_tray: next_start_in_tray,
        };
        self.persist_and_apply_unlocked(next)?;
        Ok(next)
    }

    /// Updates `start_in_tray` ("Launch minimized to system tray") and immediately rewrites
    /// `olaz.desktop` when `open_at_login` is currently enabled.
    pub fn set_start_in_tray(&self, start_in_tray: bool) -> Result<AutostartState> {
        let _guard = self.lock.lock();
        let prev = self.read_state_unlocked(None);
        let next = AutostartState {
            open_at_login: prev.open_at_login,
            start_in_tray,
        };
        self.persist_and_apply_unlocked(next)?;
        Ok(next)
    }

    /// One-shot migration for users who previously toggled Zalo's broken "Start with system"
    /// switch ON (which saved `ud_launch = "true"` in `localStorage` without writing `olaz.desktop`).
    ///
    /// Does nothing if `olaz-autostart.json` or `olaz.desktop` already exists on disk.
    pub fn migrate_legacy_enabled(&self) -> Result<AutostartState> {
        let _guard = self.lock.lock();
        if self.state_file.exists() || self.desktop_file_path().exists() {
            return Ok(self.read_state_unlocked(None));
        }
        let migrated = AutostartState {
            open_at_login: true,
            start_in_tray: true,
        };
        self.persist_and_apply_unlocked(migrated)?;
        Ok(migrated)
    }

    /// Reconciles `<autostart_dir>/olaz.desktop` on application startup so an enabled autostart
    /// setting always repairs a missing `.desktop` file or refreshes a moved binary path.
    pub fn reconcile(&self, legacy_config_autolaunch: Option<bool>) -> Result<AutostartState> {
        let _guard = self.lock.lock();
        let state = self.read_state_unlocked(legacy_config_autolaunch);
        if state.open_at_login {
            self.write_state_file_unlocked(state)?;
            let changed = self.write_desktop_file_if_changed_unlocked(state.start_in_tray)?;
            if changed && self.notify_systemd {
                reload_user_systemd_nonblocking();
            }
        } else if self.state_file.exists() {
            let removed = self.remove_desktop_file_unlocked()?;
            if removed && self.notify_systemd {
                reload_user_systemd_nonblocking();
            }
        }
        Ok(state)
    }

    fn read_state_unlocked(&self, legacy_config_autolaunch: Option<bool>) -> AutostartState {
        if let Ok(bytes) = std::fs::read(&self.state_file) {
            if let Ok(parsed) = serde_json::from_slice::<AutostartState>(&bytes) {
                return parsed;
            }
        }
        if let Ok(desktop_txt) = std::fs::read_to_string(self.desktop_file_path()) {
            if let Some(inferred) = parse_existing_desktop_entry(&desktop_txt) {
                return inferred;
            }
        }
        if legacy_config_autolaunch == Some(true) {
            return AutostartState {
                open_at_login: true,
                start_in_tray: true,
            };
        }
        AutostartState::default()
    }

    fn persist_and_apply_unlocked(&self, state: AutostartState) -> Result<()> {
        self.write_state_file_unlocked(state)?;
        let desktop_changed = if state.open_at_login {
            self.write_desktop_file_if_changed_unlocked(state.start_in_tray)?
        } else {
            self.remove_desktop_file_unlocked()?
        };
        if desktop_changed && self.notify_systemd {
            reload_user_systemd_nonblocking();
        }
        Ok(())
    }

    fn write_state_file_unlocked(&self, state: AutostartState) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(&state)
            .context("failed to serialize AutostartState")?;
        bytes.push(b'\n');
        write_bytes_atomic(&self.state_file, &bytes)
    }

    fn write_desktop_file_if_changed_unlocked(&self, start_in_tray: bool) -> Result<bool> {
        let content = format_autostart_desktop_entry(&self.exe_path, start_in_tray);
        let desktop_path = self.desktop_file_path();
        if let Ok(existing) = std::fs::read_to_string(&desktop_path) {
            if existing == content {
                return Ok(false);
            }
        }
        write_bytes_atomic(&desktop_path, content.as_bytes())?;
        Ok(true)
    }

    fn remove_desktop_file_unlocked(&self) -> Result<bool> {
        let desktop_path = self.desktop_file_path();
        match std::fs::remove_file(&desktop_path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).with_context(|| {
                format!("failed to remove autostart desktop entry {}", desktop_path.display())
            }),
        }
    }
}

/// Resolves the installed `olaz` executable path for the `Exec=` line of `olaz.desktop`.
pub fn resolve_olaz_executable_path() -> PathBuf {
    if let Ok(cur) = std::env::current_exe() {
        let cleaned = strip_linux_deleted_suffix(&cur);
        if cleaned.exists() {
            return cleaned;
        }
        return cleaned;
    }

    if let Some(base) = BaseDirs::new() {
        let local_bin = base.home_dir().join(".local/bin/olaz");
        if local_bin.exists() {
            return local_bin;
        }
    }
    for candidate in ["/usr/local/bin/olaz", "/usr/bin/olaz"] {
        let p = Path::new(candidate);
        if p.exists() {
            return p.to_path_buf();
        }
    }
    PathBuf::from("olaz")
}

fn strip_linux_deleted_suffix(path: &Path) -> PathBuf {
    if let Some(s) = path.to_str() {
        if let Some(stripped) = s.strip_suffix(" (deleted)") {
            return PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

/// Formats an executable path as the first argument of a Desktop Entry `Exec=` key
/// per the XDG Desktop Entry Specification.
pub fn format_desktop_exec_arg(exe_path: &Path) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`',
    ];
    let raw = exe_path.to_string_lossy();
    let field_escaped = raw.replace('%', "%%");
    let needs_quoting = field_escaped.is_empty()
        || field_escaped.chars().any(|c| RESERVED.contains(&c));
    if !needs_quoting {
        return field_escaped;
    }

    let mut out = String::with_capacity(field_escaped.len() + 4);
    out.push('"');
    for ch in field_escaped.chars() {
        match ch {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Builds the exact contents of `$XDG_CONFIG_HOME/autostart/olaz.desktop`.
pub fn format_autostart_desktop_entry(exe_path: &Path, start_in_tray: bool) -> String {
    let exec_bin = format_desktop_exec_arg(exe_path);
    let exec_line = if start_in_tray {
        format!("{exec_bin} --minimized")
    } else {
        exec_bin
    };
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Olaz\n\
         Comment=Zalo Desktop Client for Linux\n\
         Exec={exec_line}\n\
         Icon=olaz\n\
         Terminal=false\n\
         Categories=Network;InstantMessaging;Chat;\n\
         X-GNOME-Autostart-enabled=true\n\
         StartupNotify=false\n"
    )
}

/// Parses an existing `olaz.desktop` file into [`AutostartState`].
pub fn parse_existing_desktop_entry(contents: &str) -> Option<AutostartState> {
    let mut has_exec = false;
    let mut start_in_tray = false;
    let mut disabled = false;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        match key {
            "Hidden" if val.eq_ignore_ascii_case("true") => {
                disabled = true;
            }
            "X-GNOME-Autostart-enabled" if val.eq_ignore_ascii_case("false") => {
                disabled = true;
            }
            "Exec" => {
                has_exec = true;
                start_in_tray = val
                    .split_whitespace()
                    .any(|tok| matches!(tok, "--minimized" | "--tray" | "--hidden"));
            }
            _ => {}
        }
    }

    if !has_exec {
        return None;
    }
    if disabled {
        return Some(AutostartState {
            open_at_login: false,
            start_in_tray: true,
        });
    }
    Some(AutostartState {
        open_at_login: true,
        start_in_tray,
    })
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("target path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    let tmp_path = parent.join(format!(".olaz-tmp-{}", Uuid::new_v4().simple()));
    {
        let mut f = std::fs::File::create(&tmp_path)
            .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("failed writing temp file {}", tmp_path.display()))?;
        let _ = f.sync_all();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644));
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err)
            .with_context(|| format!("failed to atomically replace {}", path.display()));
    }
    Ok(())
}

/// Runs `systemctl --user daemon-reload` asynchronously so `systemd-xdg-autostart-generator`
/// picks up changes to `~/.config/autostart/olaz.desktop` without blocking UI or leaving zombies.
pub fn reload_user_systemd_nonblocking() {
    let _ = std::thread::Builder::new()
        .name("olaz-systemd-reload".to_string())
        .spawn(|| {
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_test_manager(tmp: &TempDir, exe_path: &str) -> AutostartManager {
        AutostartManager::with_paths(
            tmp.path().join("olaz-autostart.json"),
            tmp.path().join("xdg-config/autostart"),
            PathBuf::from(exe_path),
            false,
        )
    }

    #[test]
    fn default_state_is_disabled_with_start_in_tray_default_true() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_test_manager(&tmp, "/home/tester/.local/bin/olaz");
        assert_eq!(
            mgr.state(),
            AutostartState {
                open_at_login: false,
                start_in_tray: true,
            }
        );
        assert!(!mgr.desktop_file_path().exists());
    }

    #[test]
    fn enabling_autostart_defaults_start_in_tray_true_and_writes_clean_desktop_entry() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_test_manager(&tmp, "/home/tester/.local/bin/olaz");

        let st = mgr.set_open_at_login(true).unwrap();
        assert_eq!(
            st,
            AutostartState {
                open_at_login: true,
                start_in_tray: true,
            }
        );

        let content = std::fs::read_to_string(mgr.desktop_file_path()).unwrap();
        assert!(content.contains("[Desktop Entry]\n"));
        assert!(content.contains("Type=Application\n"));
        assert!(content.contains("Name=Olaz\n"));
        assert!(content.contains("Exec=/home/tester/.local/bin/olaz --minimized\n"));
        assert!(content.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(content.contains("StartupNotify=false\n"));
        assert!(
            !content.contains("OnlyShowIn"),
            "OnlyShowIn must never be emitted so Hyprland systemd-xdg-autostart-generator runs it"
        );
    }

    #[test]
    fn toggling_start_in_tray_updates_exec_line_and_reenabling_autostart_resets_default_on() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_test_manager(&tmp, "/usr/bin/olaz");

        mgr.set_open_at_login(true).unwrap();
        let st_full = mgr.set_start_in_tray(false).unwrap();
        assert_eq!(
            st_full,
            AutostartState {
                open_at_login: true,
                start_in_tray: false,
            }
        );
        let content_full = std::fs::read_to_string(mgr.desktop_file_path()).unwrap();
        assert!(content_full.contains("Exec=/usr/bin/olaz\n"));
        assert!(!content_full.contains("--minimized"));

        // Disable autostart -> deletes .desktop file
        let st_off = mgr.set_open_at_login(false).unwrap();
        assert!(!st_off.open_at_login);
        assert!(!mgr.desktop_file_path().exists());

        // Toggle autostart OFF -> ON again -> start_in_tray defaults back to true!
        let st_re_on = mgr.set_open_at_login(true).unwrap();
        assert_eq!(
            st_re_on,
            AutostartState {
                open_at_login: true,
                start_in_tray: true,
            }
        );
        let content_tray = std::fs::read_to_string(mgr.desktop_file_path()).unwrap();
        assert!(content_tray.contains("Exec=/usr/bin/olaz --minimized\n"));
    }

    #[test]
    fn quotes_and_escapes_executable_paths_with_spaces_and_special_chars() {
        let quoted = format_desktop_exec_arg(Path::new("/home/test user/My $Apps/olaz%bin"));
        assert_eq!(quoted, r#""/home/test user/My \$Apps/olaz%%bin""#);

        let entry = format_autostart_desktop_entry(
            Path::new("/home/test user/Apps/olaz"),
            true,
        );
        assert!(entry.contains("Exec=\"/home/test user/Apps/olaz\" --minimized\n"));
    }

    #[test]
    fn infers_existing_desktop_file_and_reconciles_missing_or_moved_binary() {
        let tmp = TempDir::new().unwrap();
        let desktop_dir = tmp.path().join("xdg-config/autostart");
        std::fs::create_dir_all(&desktop_dir).unwrap();
        std::fs::write(
            desktop_dir.join("olaz.desktop"),
            "[Desktop Entry]\nType=Application\nExec=/old/path/olaz --minimized\n",
        )
        .unwrap();

        let mgr = make_test_manager(&tmp, "/new/path/olaz");
        assert_eq!(
            mgr.state(),
            AutostartState {
                open_at_login: true,
                start_in_tray: true,
            }
        );

        let reconciled = mgr.reconcile(None).unwrap();
        assert_eq!(
            reconciled,
            AutostartState {
                open_at_login: true,
                start_in_tray: true,
            }
        );
        let updated = std::fs::read_to_string(mgr.desktop_file_path()).unwrap();
        assert!(updated.contains("Exec=/new/path/olaz --minimized\n"));

        // If the user's .desktop was manually deleted while persisted state is enabled,
        // startup reconcile restores it automatically.
        std::fs::remove_file(mgr.desktop_file_path()).unwrap();
        mgr.reconcile(None).unwrap();
        assert!(mgr.desktop_file_path().exists());
    }

    #[test]
    fn legacy_localstorage_migration_repairs_once_without_overriding_explicit_disable() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_test_manager(&tmp, "/home/tester/.local/bin/olaz");

        let migrated = mgr.migrate_legacy_enabled().unwrap();
        assert_eq!(
            migrated,
            AutostartState {
                open_at_login: true,
                start_in_tray: true,
            }
        );
        assert!(mgr.desktop_file_path().exists());

        // User explicitly turns autostart OFF; subsequent legacy migration check must respect it.
        mgr.set_open_at_login(false).unwrap();
        let after = mgr.migrate_legacy_enabled().unwrap();
        assert!(!after.open_at_login);
        assert!(!mgr.desktop_file_path().exists());
    }

    #[test]
    fn strips_linux_proc_self_exe_deleted_suffix() {
        assert_eq!(
            strip_linux_deleted_suffix(Path::new("/home/user/.local/bin/olaz (deleted)")),
            PathBuf::from("/home/user/.local/bin/olaz")
        );
        assert_eq!(
            strip_linux_deleted_suffix(Path::new("/home/user/.local/bin/olaz")),
            PathBuf::from("/home/user/.local/bin/olaz")
        );
    }
}
