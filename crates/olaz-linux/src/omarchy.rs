//! Omarchy & Linux desktop theme synchronization, Hyprland window rounding,
//! language tracking, and WebKit2GTK Wayland/NVIDIA startup configuration.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Default Zalo brand accent used when Omarchy theme files are absent (e.g. Ubuntu GNOME).
pub const ZALO_ACCENT: &str = "#0068FF";

/// Dark foreground used over high-luminance accents (`L > 0.179`).
pub const ACCENT_FG_DARK: &str = "#081B3A";

/// Light foreground used over low-luminance accents (`L <= 0.179`).
pub const ACCENT_FG_LIGHT: &str = "#FFFFFF";

/// Debounce interval for Omarchy theme and language file changes.
pub const WATCHER_DEBOUNCE: Duration = Duration::from_millis(80);

/// Timeout when querying `hyprctl -j getoption decoration:rounding`.
pub const HYPRCTL_TIMEOUT: Duration = Duration::from_millis(500);

/// Semantic desktop palette & geometry exposed to the Olaz shell and WebView CSS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopTheme {
    /// `"dark"` or `"light"`.
    pub mode: String,
    pub accent: String,
    /// Readable text color over `accent` via WCAG relative luminance (`L > 0.179 ? "#081B3A" : "#FFFFFF"`).
    pub accent_fg: String,
    pub background: String,
    pub dark_background: String,
    pub darker_background: String,
    pub lighter_background: String,
    pub foreground: String,
    pub muted: String,
    pub selection: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub radius_px: u32,
    pub is_omarchy: bool,
    pub language: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawColorsToml {
    mode: Option<String>,
    accent: Option<String>,
    selection: Option<String>,
    muted: Option<String>,
    background: Option<String>,
    dark_background: Option<String>,
    darker_background: Option<String>,
    lighter_background: Option<String>,
    foreground: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
}

impl DesktopTheme {
    /// Clean Zalo `#0068FF` fallback palette when Omarchy files are absent (e.g. on Ubuntu).
    pub fn zalo_fallback(radius_px: u32, language: String) -> Self {
        let accent = ZALO_ACCENT.to_string();
        let accent_fg = compute_accent_fg(&accent);
        Self {
            mode: "dark".to_string(),
            accent,
            accent_fg,
            background: "#141821".to_string(),
            dark_background: "#0F131A".to_string(),
            darker_background: "#0A0D12".to_string(),
            lighter_background: "#1E2533".to_string(),
            foreground: "#E8ECF4".to_string(),
            muted: "#7D889C".to_string(),
            selection: "#1D355E".to_string(),
            red: "#EF4444".to_string(),
            green: "#22C55E".to_string(),
            yellow: "#F59E0B".to_string(),
            blue: ZALO_ACCENT.to_string(),
            radius_px,
            is_omarchy: false,
            language,
        }
    }

    /// Exports the semantic palette as CSS custom properties (`:root { ... }`).
    pub fn to_css_vars(&self) -> String {
        format!(
            ":root {{\n\
             --olaz-mode: {};\n\
             --olaz-accent: {};\n\
             --olaz-accent-fg: {};\n\
             --olaz-bg: {};\n\
             --olaz-bg-dark: {};\n\
             --olaz-bg-darker: {};\n\
             --olaz-bg-lighter: {};\n\
             --olaz-fg: {};\n\
             --olaz-muted: {};\n\
             --olaz-selection: {};\n\
             --olaz-red: {};\n\
             --olaz-green: {};\n\
             --olaz-yellow: {};\n\
             --olaz-blue: {};\n\
             --olaz-radius: {}px;\n\
             }}",
            self.mode,
            self.accent,
            self.accent_fg,
            self.background,
            self.dark_background,
            self.darker_background,
            self.lighter_background,
            self.foreground,
            self.muted,
            self.selection,
            self.red,
            self.green,
            self.yellow,
            self.blue,
            self.radius_px,
        )
    }
}

/// Parses `#RRGGBB`, `RRGGBB`, or `#RGB` into `(r, g, b)`.
pub fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let trimmed = hex.trim().strip_prefix('#').unwrap_or_else(|| hex.trim());
    match trimmed.len() {
        6 => {
            let r = u8::from_str_radix(&trimmed[0..2], 16).ok()?;
            let g = u8::from_str_radix(&trimmed[2..4], 16).ok()?;
            let b = u8::from_str_radix(&trimmed[4..6], 16).ok()?;
            Some((r, g, b))
        }
        3 => {
            let r = u8::from_str_radix(&trimmed[0..1], 16).ok()?;
            let g = u8::from_str_radix(&trimmed[1..2], 16).ok()?;
            let b = u8::from_str_radix(&trimmed[2..3], 16).ok()?;
            Some((r * 17, g * 17, b * 17))
        }
        _ => None,
    }
}

/// Normalizes a hex color string to `#RRGGBB` (preserving case if 6-digit hex), or returns `fallback`.
fn normalize_color(candidate: Option<&str>, fallback: &str) -> String {
    if let Some(raw) = candidate {
        let trimmed = raw.trim();
        if let Some((r, g, b)) = parse_hex_rgb(trimmed) {
            if trimmed.starts_with('#') && trimmed.len() == 7 {
                return trimmed.to_string();
            }
            return format!("#{r:02X}{g:02X}{b:02X}");
        }
    }
    fallback.to_string()
}

/// Computes WCAG 2.1 relative luminance in `0.0..=1.0` for a hex color.
pub fn relative_luminance(hex: &str) -> Option<f64> {
    let (r, g, b) = parse_hex_rgb(hex)?;
    fn channel_linear(c: u8) -> f64 {
        let v = f64::from(c) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    Some(0.2126 * channel_linear(r) + 0.7152 * channel_linear(g) + 0.0722 * channel_linear(b))
}

/// Picks readable foreground text on `accent` using WCAG relative luminance (`L > 0.179 ? "#081B3A" : "#FFFFFF"`).
pub fn compute_accent_fg(accent_hex: &str) -> String {
    match relative_luminance(accent_hex) {
        Some(l) if l > 0.179 => ACCENT_FG_DARK.to_string(),
        _ => ACCENT_FG_LIGHT.to_string(),
    }
}

/// Resolves the UI language from `~/.config/omarchy/language` (first line matching `^[a-z]{2,3}(-[A-Za-z]{2})?$`),
/// falling back to `$LANG` (`"vi"` when Vietnamese locale, otherwise `"en"`).
pub fn resolve_language(language_file: Option<&Path>, lang_env: Option<&str>) -> String {
    let valid_re = Regex::new(r"^[a-z]{2,3}(-[A-Za-z]{2})?$").expect("valid language regex");
    if let Some(path) = language_file {
        if let Ok(contents) = fs::read_to_string(path) {
            let first_line = contents.lines().next().unwrap_or("").trim();
            if valid_re.is_match(first_line) {
                return first_line.to_string();
            }
        }
    }
    fallback_language_from_env(lang_env)
}

fn fallback_language_from_env(lang_env: Option<&str>) -> String {
    if let Some(raw) = lang_env {
        let trimmed = raw.trim();
        let code = trimmed
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if code == "vi" || code.starts_with("vi_") || code.starts_with("vi-") {
            return "vi".to_string();
        }
    }
    "en".to_string()
}

/// Queries Hyprland's `decoration:rounding` when `HYPRLAND_INSTANCE_SIGNATURE` is set,
/// enforcing a strict timeout (`HYPRCTL_TIMEOUT`, 500ms).
///
/// Defaults:
/// - `0` when running on Hyprland (`hyprland_sig` is non-empty) and `hyprctl` fails or times out.
/// - `12` when not running on Hyprland (e.g. Ubuntu GNOME).
pub fn query_hyprland_rounding_with(
    hyprland_sig: Option<&str>,
    hyprctl_bin: &Path,
    timeout: Duration,
) -> u32 {
    let on_hyprland = hyprland_sig.is_some_and(|s| !s.trim().is_empty());
    if !on_hyprland {
        return 12;
    }

    let mut child = match Command::new(hyprctl_bin)
        .args(["-j", "getoption", "decoration:rounding"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            tracing::debug!("hyprctl spawn failed ({err}); defaulting Hyprland radius to 0");
            return 0;
        }
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return 0;
                }
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    if let Some(n) = json.get("int").and_then(|v| v.as_i64()) {
                        if let Ok(r) = u32::try_from(n) {
                            return r;
                        }
                    }
                }
                return 0;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    tracing::warn!("hyprctl timed out after {:?}; killing child", timeout);
                    let _ = child.kill();
                    let _ = child.wait();
                    return 0;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return 0;
            }
        }
    }
}

/// Loads the current `DesktopTheme` from explicit directories and environment parameters (test-friendly).
pub fn load_omarchy_theme_with(
    state_current_dir: &Path,
    config_omarchy_dir: &Path,
    hyprland_sig: Option<&str>,
    hyprctl_bin: &Path,
    lang_env: Option<&str>,
) -> DesktopTheme {
    let radius_px = query_hyprland_rounding_with(hyprland_sig, hyprctl_bin, HYPRCTL_TIMEOUT);
    let language_file = config_omarchy_dir.join("language");
    let language = resolve_language(Some(&language_file), lang_env);

    let colors_path = state_current_dir.join("theme").join("colors.toml");
    let Ok(toml_content) = fs::read_to_string(&colors_path) else {
        return DesktopTheme::zalo_fallback(radius_px, language);
    };

    let parsed: RawColorsToml = match toml::from_str(&toml_content) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(
                "failed to parse Omarchy colors.toml at {}: {err}",
                colors_path.display()
            );
            return DesktopTheme::zalo_fallback(radius_px, language);
        }
    };

    let background_candidate = parsed.background.as_deref();
    let inferred_light = background_candidate
        .and_then(relative_luminance)
        .is_some_and(|l| l > 0.179);

    let mode = match parsed
        .mode
        .as_deref()
        .map(|m| m.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("light") => "light".to_string(),
        Some("dark") => "dark".to_string(),
        _ => {
            if inferred_light {
                "light".to_string()
            } else {
                "dark".to_string()
            }
        }
    };

    let is_light = mode == "light";
    let default_bg = if is_light { "#F5F7FA" } else { "#1E1E2E" };
    let default_dark_bg = if is_light { "#EAECEF" } else { "#161622" };
    let default_darker_bg = if is_light { "#DFE3E8" } else { "#101019" };
    let default_lighter_bg = if is_light { "#FFFFFF" } else { "#313244" };
    let default_fg = if is_light { "#111827" } else { "#CDD6F4" };
    let default_muted = if is_light { "#6B7280" } else { "#585B70" };
    let default_selection = if is_light { "#DBEAFE" } else { "#45475A" };

    let accent = normalize_color(parsed.accent.as_deref(), ZALO_ACCENT);
    let accent_fg = compute_accent_fg(&accent);

    DesktopTheme {
        mode,
        accent,
        accent_fg,
        background: normalize_color(parsed.background.as_deref(), default_bg),
        dark_background: normalize_color(parsed.dark_background.as_deref(), default_dark_bg),
        darker_background: normalize_color(parsed.darker_background.as_deref(), default_darker_bg),
        lighter_background: normalize_color(
            parsed.lighter_background.as_deref(),
            default_lighter_bg,
        ),
        foreground: normalize_color(parsed.foreground.as_deref(), default_fg),
        muted: normalize_color(parsed.muted.as_deref(), default_muted),
        selection: normalize_color(parsed.selection.as_deref(), default_selection),
        red: normalize_color(parsed.red.as_deref(), "#F38BA8"),
        green: normalize_color(parsed.green.as_deref(), "#A6E3A1"),
        yellow: normalize_color(parsed.yellow.as_deref(), "#F9E2AF"),
        blue: normalize_color(parsed.blue.as_deref(), ZALO_ACCENT),
        radius_px,
        is_omarchy: true,
        language,
    }
}

fn home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(home);
    }
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn default_omarchy_state_current_dir() -> PathBuf {
    home_dir().join(".local/state/omarchy/current")
}

pub fn default_omarchy_config_dir() -> PathBuf {
    home_dir().join(".config/omarchy")
}

/// Loads the live Omarchy desktop theme (or Zalo `#0068FF` fallback palette on non-Omarchy Linux).
pub fn load_omarchy_theme() -> DesktopTheme {
    let state_dir = default_omarchy_state_current_dir();
    let config_dir = default_omarchy_config_dir();
    let hyprland_sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
    let lang_env = std::env::var("LANG")
        .ok()
        .or_else(|| std::env::var("LC_ALL").ok())
        .or_else(|| std::env::var("LC_MESSAGES").ok());
    load_omarchy_theme_with(
        &state_dir,
        &config_dir,
        hyprland_sig.as_deref(),
        Path::new("hyprctl"),
        lang_env.as_deref(),
    )
}

/// Handle to a running Omarchy theme/language file watcher.
///
/// The background watcher stays alive until [`OmarchyWatcherHandle::stop`] is called (or the process exits),
/// so callers may either hold the handle or ignore the return value at app startup.
pub struct OmarchyWatcherHandle {
    stop_flag: Arc<AtomicBool>,
    wake_tx: mpsc::Sender<()>,
    join_handle: Option<JoinHandle<()>>,
}

impl OmarchyWatcherHandle {
    /// Signals the watcher thread to stop and waits for it to exit cleanly.
    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = self.wake_tx.send(());
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.exists() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

/// Computes all paths (ancestors, symlink parents, and canonical targets) that must be watched
/// so atomic `ln -sfn` / `mv` symlink and file replacements never drop future notifications.
fn collect_watch_targets(state_current_dir: &Path, config_omarchy_dir: &Path) -> Vec<PathBuf> {
    let mut set = HashSet::new();
    let mut out = Vec::new();
    let mut push = |p: PathBuf| {
        if p.exists() && set.insert(p.clone()) {
            out.push(p);
        }
    };

    for root in [state_current_dir, config_omarchy_dir] {
        if let Some(ancestor) = nearest_existing_ancestor(root) {
            if let Some(parent) = ancestor.parent() {
                push(parent.to_path_buf());
            }
            push(ancestor);
        }
        if let Some(parent) = root.parent() {
            push(parent.to_path_buf());
        }
        push(root.to_path_buf());
        if let Ok(canon) = fs::canonicalize(root) {
            push(canon);
        }
    }

    let theme_dir = state_current_dir.join("theme");
    let colors_file = theme_dir.join("colors.toml");
    for p in [&theme_dir, &colors_file] {
        push(p.clone());
        if let Ok(canon) = fs::canonicalize(p) {
            if let Some(parent) = canon.parent() {
                push(parent.to_path_buf());
            }
            push(canon);
        }
    }

    let lang_file = config_omarchy_dir.join("language");
    push(lang_file.clone());
    if let Ok(canon) = fs::canonicalize(&lang_file) {
        push(canon);
    }

    out
}

fn rearm_watches(
    watcher: &mut RecommendedWatcher,
    armed: &mut Vec<PathBuf>,
    state_current_dir: &Path,
    config_omarchy_dir: &Path,
) {
    for old in armed.drain(..) {
        let _ = watcher.unwatch(&old);
    }
    for path in collect_watch_targets(state_current_dir, config_omarchy_dir) {
        if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
            armed.push(path);
        }
    }
}

/// Spawns an Omarchy watcher for `~/.local/state/omarchy/current` and `~/.config/omarchy`
/// with 80ms debouncing and automatic watch re-arming across symlink replacements.
pub fn spawn_omarchy_watcher(
    on_change: impl Fn(DesktopTheme) + Send + 'static,
) -> OmarchyWatcherHandle {
    let state_dir = default_omarchy_state_current_dir();
    let config_dir = default_omarchy_config_dir();
    let hyprland_sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
    let lang_env = std::env::var("LANG")
        .ok()
        .or_else(|| std::env::var("LC_ALL").ok())
        .or_else(|| std::env::var("LC_MESSAGES").ok());
    spawn_omarchy_watcher_with(
        state_dir,
        config_dir,
        hyprland_sig,
        PathBuf::from("hyprctl"),
        lang_env,
        WATCHER_DEBOUNCE,
        on_change,
    )
}

/// Testable variant of [`spawn_omarchy_watcher`] accepting custom state/config directories and `hyprctl` binary.
pub fn spawn_omarchy_watcher_with(
    state_current_dir: PathBuf,
    config_omarchy_dir: PathBuf,
    hyprland_sig: Option<String>,
    hyprctl_bin: PathBuf,
    lang_env: Option<String>,
    debounce: Duration,
    on_change: impl Fn(DesktopTheme) + Send + 'static,
) -> OmarchyWatcherHandle {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop_flag);
    let (event_tx, event_rx) = mpsc::channel::<()>();
    let wake_tx = event_tx.clone();

    let join_handle = thread::Builder::new()
        .name("olaz-omarchy-watcher".to_string())
        .spawn(move || {
            let tx_for_notify = event_tx.clone();
            let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
                if res.is_ok() {
                    let _ = tx_for_notify.send(());
                }
            }) {
                Ok(w) => w,
                Err(err) => {
                    tracing::warn!("failed to create notify watcher for Omarchy: {err}");
                    return;
                }
            };

            let mut armed = Vec::new();
            rearm_watches(
                &mut watcher,
                &mut armed,
                &state_current_dir,
                &config_omarchy_dir,
            );

            let mut last_theme = load_omarchy_theme_with(
                &state_current_dir,
                &config_omarchy_dir,
                hyprland_sig.as_deref(),
                &hyprctl_bin,
                lang_env.as_deref(),
            );

            while !stop_clone.load(Ordering::SeqCst) {
                if event_rx.recv().is_err() || stop_clone.load(Ordering::SeqCst) {
                    break;
                }

                // Debounce bursts of filesystem operations (e.g. symlink unlink + create + colors.toml write)
                // over `debounce` (80ms), bounded so continuous writes still flush within 400ms.
                let batch_start = Instant::now();
                let max_batch = debounce.saturating_mul(5);
                loop {
                    if stop_clone.load(Ordering::SeqCst) {
                        return;
                    }
                    if batch_start.elapsed() >= max_batch {
                        break;
                    }
                    match event_rx.recv_timeout(debounce) {
                        Ok(()) => continue,
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }

                // Always re-arm watches because `omarchy-theme-set` replaces symlinks/files atomically.
                rearm_watches(
                    &mut watcher,
                    &mut armed,
                    &state_current_dir,
                    &config_omarchy_dir,
                );

                let next_theme = load_omarchy_theme_with(
                    &state_current_dir,
                    &config_omarchy_dir,
                    hyprland_sig.as_deref(),
                    &hyprctl_bin,
                    lang_env.as_deref(),
                );

                if next_theme != last_theme {
                    last_theme = next_theme.clone();
                    on_change(next_theme);
                }
            }
        })
        .ok();

    OmarchyWatcherHandle {
        stop_flag,
        wake_tx,
        join_handle,
    }
}

/// Pure decision helper computing environment overrides needed for WebKit2GTK on Linux:
/// - On ANY Wayland session (`WAYLAND_DISPLAY` non-empty or `XDG_SESSION_TYPE == "wayland"`),
///   sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` unless already set by the user.
/// - On Wayland + NVIDIA, also sets `WEBKIT_DISABLE_COMPOSITING_MODE=1` unless already set.
/// - Sets `GDK_BACKEND=wayland,x11` unless `GDK_BACKEND` is already set.
pub fn compute_linux_webview_env_overrides(
    webkit_disable_dmabuf: Option<&OsStr>,
    webkit_disable_compositing: Option<&OsStr>,
    wayland_display: Option<&OsStr>,
    xdg_session_type: Option<&OsStr>,
    gdk_backend: Option<&OsStr>,
    has_nvidia_driver: bool,
) -> Vec<(&'static str, &'static str)> {
    let mut overrides = Vec::new();

    let on_wayland = wayland_display.is_some_and(|v| !v.is_empty())
        || xdg_session_type
            .is_some_and(|v| v.to_string_lossy().trim().eq_ignore_ascii_case("wayland"));

    if on_wayland {
        if webkit_disable_dmabuf.is_none() {
            overrides.push(("WEBKIT_DISABLE_DMABUF_RENDERER", "1"));
        }
        if has_nvidia_driver && webkit_disable_compositing.is_none() {
            overrides.push(("WEBKIT_DISABLE_COMPOSITING_MODE", "1"));
        }
    }

    if gdk_backend.is_none() {
        overrides.push(("GDK_BACKEND", "wayland,x11"));
    }

    overrides
}

/// Detects whether the proprietary or open NVIDIA kernel driver is loaded.
pub fn system_has_nvidia_driver() -> bool {
    Path::new("/sys/module/nvidia_drm").exists()
        || Path::new("/proc/driver/nvidia/version").exists()
}

#[cfg(has_embedded_gst_plugin)]
const EMBEDDED_GST_OLAZAUTO_SO: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/libgstolazauto.so"));

/// Returns the user-writable GStreamer plugin staging directory (`<XDG_CACHE_HOME>/olaz/gst-plugins`).
pub fn olaz_gst_plugin_dir() -> PathBuf {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(cache).join("olaz/gst-plugins");
    }
    home_dir().join(".cache/olaz/gst-plugins")
}

/// Materializes the embedded `libgstolazauto.so` GStreamer 1.0 plugin (`autoaudiosink`,
/// `autovideosink`, `olazwavparse`, `olazavdemux`, `olazmediasrc`) into [`olaz_gst_plugin_dir`].
pub fn ensure_olaz_gst_plugin_staged() -> Option<PathBuf> {
    let dir = olaz_gst_plugin_dir();
    let so_path = dir.join("libgstolazauto.so");

    #[cfg(has_embedded_gst_plugin)]
    {
        if let Err(err) = fs::create_dir_all(&dir) {
            tracing::warn!("failed to create GStreamer plugin cache dir {}: {err}", dir.display());
            return so_path.is_file().then_some(dir);
        }
        let up_to_date = fs::read(&so_path)
            .map(|existing| existing == EMBEDDED_GST_OLAZAUTO_SO)
            .unwrap_or(false);
        if !up_to_date {
            let tmp_path = dir.join(format!(".libgstolazauto.so.tmp.{}", std::process::id()));
            if let Err(err) = fs::write(&tmp_path, EMBEDDED_GST_OLAZAUTO_SO)
                .and_then(|()| fs::rename(&tmp_path, &so_path))
            {
                let _ = fs::remove_file(&tmp_path);
                tracing::warn!("failed to stage {}: {err}", so_path.display());
            }
        }
    }

    so_path.is_file().then_some(dir)
}

/// Checks whether a working GStreamer `autoaudiosink` provider (`libgstautodetect.so` from
/// `gst-plugins-good` OR Olaz's staged `libgstolazauto.so`) is available.
///
/// Without `autoaudiosink`, WebKit2GTK 2.54.1 triggers `RELEASE_ASSERT(audioSink)` (`SIGABRT`)
/// inside `WebCore::MediaPlayerPrivateGStreamer::createAudioSink()`.
pub fn has_gstreamer_audio_sink() -> bool {
    const PLUGIN_FILENAMES: &[&str] = &["libgstautodetect.so", "libgstolazauto.so"];
    const STANDARD_PATHS: &[&str] = &[
        "/usr/lib/gstreamer-1.0/libgstautodetect.so",
        "/usr/lib64/gstreamer-1.0/libgstautodetect.so",
        "/usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstautodetect.so",
        "/usr/lib/aarch64-linux-gnu/gstreamer-1.0/libgstautodetect.so",
        "/usr/local/lib/gstreamer-1.0/libgstautodetect.so",
        "/usr/local/lib64/gstreamer-1.0/libgstautodetect.so",
    ];

    if STANDARD_PATHS.iter().any(|p| Path::new(p).is_file()) {
        return true;
    }

    let staged = olaz_gst_plugin_dir().join("libgstolazauto.so");
    if staged.is_file() {
        return true;
    }

    for env_key in [
        "GST_PLUGIN_PATH_1_0",
        "GST_PLUGIN_PATH",
        "GST_PLUGIN_SYSTEM_PATH_1_0",
        "GST_PLUGIN_SYSTEM_PATH",
    ] {
        if let Some(raw) = std::env::var_os(env_key) {
            for dir in std::env::split_paths(&raw) {
                if !dir.as_os_str().is_empty()
                    && PLUGIN_FILENAMES.iter().any(|f| dir.join(f).is_file())
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Pure decision helper computing GStreamer video-sink overrides when WebKit2GTK runs
/// with DMABUF or accelerated compositing disabled (`WEBKIT_DISABLE_DMABUF_RENDERER=1` /
/// `WEBKIT_DISABLE_COMPOSITING_MODE=1`), forcing software `webkitvideosink` instead of
/// EGL/GBM `webkitglvideosink` so `<video>` never renders black GL textures.
pub fn compute_linux_gst_video_sink_overrides(
    effective_disable_dmabuf: Option<&OsStr>,
    effective_disable_compositing: Option<&OsStr>,
    gst_disable_gl_sink: Option<&OsStr>,
    gst_dmabuf_sink_disabled: Option<&OsStr>,
) -> Vec<(&'static str, &'static str)> {
    let is_truthy = |v: Option<&OsStr>| {
        v.is_some_and(|s| {
            let t = s.to_string_lossy();
            let trimmed = t.trim();
            !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
        })
    };
    let mut out = Vec::new();
    if is_truthy(effective_disable_dmabuf) || is_truthy(effective_disable_compositing) {
        if gst_dmabuf_sink_disabled.is_none() {
            out.push(("WEBKIT_GST_DMABUF_SINK_DISABLED", "1"));
        }
        if gst_disable_gl_sink.is_none() {
            out.push(("WEBKIT_GST_DISABLE_GL_SINK", "1"));
        }
    }
    out
}

/// Safely configures WebKit2GTK / GDK / GStreamer environment variables at process startup (before GTK init):
/// - Stages `libgstolazauto.so` into `<XDG_CACHE_HOME>/olaz/gst-plugins` and prepends it to `GST_PLUGIN_PATH_1_0`.
/// - Sets `WEBKIT_GST_ALLOWED_URI_PROTOCOLS=olaz-media,zfile,zalo-app` if unset so WebKit routes custom media schemes.
/// - Sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` if unset when running under Wayland.
/// - Sets `WEBKIT_DISABLE_COMPOSITING_MODE=1` if unset when running under Wayland with NVIDIA drivers
///   (`/sys/module/nvidia_drm` or `/proc/driver/nvidia/version`).
/// - Sets `WEBKIT_GST_DMABUF_SINK_DISABLED=1` and `WEBKIT_GST_DISABLE_GL_SINK=1` if unset when DMABUF/compositing is disabled.
/// - Sets `GDK_BACKEND=wayland,x11` if `GDK_BACKEND` is unset.
pub fn configure_linux_webview_env() {
    if let Some(gst_dir) = ensure_olaz_gst_plugin_staged() {
        let mut paths = vec![gst_dir.clone()];
        if let Some(existing) = std::env::var_os("GST_PLUGIN_PATH_1_0") {
            for p in std::env::split_paths(&existing) {
                if !p.as_os_str().is_empty() && p != gst_dir {
                    paths.push(p);
                }
            }
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            tracing::info!("staging Olaz GStreamer plugin path: {}", gst_dir.display());
            std::env::set_var("GST_PLUGIN_PATH_1_0", joined);
        }
    }

    if std::env::var_os("WEBKIT_GST_ALLOWED_URI_PROTOCOLS").is_none() {
        std::env::set_var(
            "WEBKIT_GST_ALLOWED_URI_PROTOCOLS",
            "olaz-media,zfile,zalo-app",
        );
    }

    let dmabuf = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER");
    let compositing = std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE");
    let wayland = std::env::var_os("WAYLAND_DISPLAY");
    let xdg_session = std::env::var_os("XDG_SESSION_TYPE");
    let gdk = std::env::var_os("GDK_BACKEND");
    let has_nvidia = system_has_nvidia_driver();

    for (key, value) in compute_linux_webview_env_overrides(
        dmabuf.as_deref(),
        compositing.as_deref(),
        wayland.as_deref(),
        xdg_session.as_deref(),
        gdk.as_deref(),
        has_nvidia,
    ) {
        tracing::info!("setting {key}={value} for Linux WebKit2GTK compatibility");
        // Called once at early single-threaded startup prior to GTK/WebKit initialization.
        std::env::set_var(key, value);
    }

    let eff_dmabuf = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER");
    let eff_compositing = std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE");
    let gst_gl = std::env::var_os("WEBKIT_GST_DISABLE_GL_SINK");
    let gst_dmabuf = std::env::var_os("WEBKIT_GST_DMABUF_SINK_DISABLED");
    for (key, value) in compute_linux_gst_video_sink_overrides(
        eff_dmabuf.as_deref(),
        eff_compositing.as_deref(),
        gst_gl.as_deref(),
        gst_dmabuf.as_deref(),
    ) {
        tracing::info!("setting {key}={value} for Linux WebKit2GTK GStreamer software sink");
        std::env::set_var(key, value);
    }
}
