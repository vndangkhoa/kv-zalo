//! XDG Desktop Portal wrappers (`FileChooser` & `Screenshot`) via `ashpd` (`zbus` + `tokio`),
//! working identically on Hyprland (`xdg-desktop-portal-hyprland` / `-gtk`), GNOME (`-gnome`),
//! KDE (`-kde`), and inside Flatpak sandboxes.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use ashpd::desktop::screenshot::Screenshot;
use ashpd::desktop::ResponseError;
use percent_encoding::percent_decode_str;
use uuid::Uuid;

/// Default temporary staging root for interactive screenshots, matching `ScreenCapture.swift`.
pub const SCREENSHOT_STAGING_DIR: &str = "/tmp/olaz-screenshots";

/// Retention window for staged screenshots before background pruning (6 hours, matching `ScreenCapture.swift`).
pub const SCREENSHOT_RETENTION: Duration = Duration::from_secs(6 * 3600);

fn is_portal_cancelled(err: &ashpd::Error) -> bool {
    matches!(
        err,
        ashpd::Error::Response(ResponseError::Cancelled)
            | ashpd::Error::Portal(ashpd::PortalError::Cancelled(_))
    )
}

/// Converts a portal `file://` [`url::Url`] into a local [`PathBuf`].
pub fn uri_to_local_path(uri: &url::Url) -> Option<PathBuf> {
    if uri.scheme() != "file" {
        return None;
    }
    if let Ok(path) = uri.to_file_path() {
        return Some(path);
    }
    let decoded = percent_decode_str(uri.path()).decode_utf8().ok()?;
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(decoded.as_ref()))
}

/// Parses a `file://` URI string into a local [`PathBuf`].
pub fn str_uri_to_local_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed).ok()?;
    uri_to_local_path(&parsed)
}

/// Builds the media [`FileFilter`] used when picking photos & videos for Zalo chat.
pub fn build_media_file_filter() -> FileFilter {
    FileFilter::new("Ảnh & Video / Images & Videos")
        .mimetype("image/png")
        .mimetype("image/jpeg")
        .mimetype("image/gif")
        .mimetype("image/webp")
        .mimetype("image/heic")
        .mimetype("image/avif")
        .mimetype("video/mp4")
        .mimetype("video/quicktime")
        .mimetype("video/webm")
        .mimetype("video/x-matroska")
        .glob("*.png")
        .glob("*.jpg")
        .glob("*.jpeg")
        .glob("*.gif")
        .glob("*.webp")
        .glob("*.mp4")
        .glob("*.mov")
        .glob("*.webm")
}

/// Opens the native XDG `org.freedesktop.portal.FileChooser` OpenFile dialog.
///
/// Returns an empty `Vec` when the user cancels the dialog.
pub async fn pick_open_files(
    title: &str,
    multiple: bool,
    media_only: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut request = SelectedFiles::open_file()
        .title(title)
        .modal(true)
        .multiple(multiple);

    if media_only {
        let media_filter = build_media_file_filter();
        request = request
            .filter(media_filter.clone())
            .current_filter(media_filter);
    }

    let response = match request.send().await {
        Ok(req) => match req.response() {
            Ok(files) => files,
            Err(err) if is_portal_cancelled(&err) => return Ok(Vec::new()),
            Err(err) => return Err(err).context("FileChooser OpenFile response failed"),
        },
        Err(err) if is_portal_cancelled(&err) => return Ok(Vec::new()),
        Err(err) => return Err(err).context("FileChooser OpenFile request failed"),
    };

    Ok(response.uris().iter().filter_map(uri_to_local_path).collect())
}

/// Opens an XDG `FileChooser` dialog supporting files or directories, custom extension filters,
/// and `zenity` CLI fallback when no XDG portal backend is reachable.
pub async fn pick_open_dialog(
    title: &str,
    multiple: bool,
    directory: bool,
    default_dir: Option<&Path>,
    filters: &[(String, Vec<String>)],
) -> anyhow::Result<Vec<PathBuf>> {
    let mut request = SelectedFiles::open_file()
        .title(title)
        .modal(true)
        .multiple(multiple)
        .directory(directory);

    if let Some(dir) = default_dir.filter(|p| p.is_dir()) {
        if let Ok(req_with_dir) = request.current_folder(dir) {
            request = req_with_dir;
        } else {
            request = SelectedFiles::open_file()
                .title(title)
                .modal(true)
                .multiple(multiple)
                .directory(directory);
        }
    }

    if !directory {
        let mut first_filter: Option<FileFilter> = None;
        for (name, exts) in filters {
            if exts.is_empty() {
                continue;
            }
            let label = if name.is_empty() { "Files" } else { name.as_str() };
            let mut f = FileFilter::new(label);
            for ext in exts {
                let clean = ext.trim().trim_start_matches("*.").trim_start_matches('.');
                if clean == "*" || clean.is_empty() {
                    f = f.glob("*");
                } else {
                    f = f.glob(&format!("*.{clean}"));
                }
            }
            if first_filter.is_none() {
                first_filter = Some(f.clone());
            }
            request = request.filter(f);
        }
        if let Some(cf) = first_filter {
            request = request.current_filter(cf);
        }
    }

    match request.send().await {
        Ok(req) => match req.response() {
            Ok(files) => {
                return Ok(files.uris().iter().filter_map(uri_to_local_path).collect());
            }
            Err(err) if is_portal_cancelled(&err) => return Ok(Vec::new()),
            Err(err) => {
                tracing::debug!("FileChooser portal response failed ({err}); trying zenity fallback");
            }
        },
        Err(err) if is_portal_cancelled(&err) => return Ok(Vec::new()),
        Err(err) => {
            tracing::debug!("FileChooser portal request failed ({err}); trying zenity fallback");
        }
    }

    zenity_open_fallback(title, multiple, directory, default_dir).await
}

/// Opens the native XDG `FileChooser` directory picker (`openDirectory`).
pub async fn pick_open_directory(title: &str) -> anyhow::Result<Vec<PathBuf>> {
    pick_open_dialog(title, false, true, None, &[]).await
}

/// Opens the native XDG `org.freedesktop.portal.FileChooser` SaveFile dialog with optional folder.
pub async fn pick_save_file_with_dir(
    title: &str,
    current_name: &str,
    default_dir: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    let mut request = SelectedFiles::save_file().title(title).modal(true);
    if !current_name.is_empty() {
        request = request.current_name(current_name);
    }
    if let Some(dir) = default_dir.filter(|p| p.is_dir()) {
        if let Ok(req_with_dir) = request.current_folder(dir) {
            request = req_with_dir;
        } else {
            request = SelectedFiles::save_file().title(title).modal(true);
            if !current_name.is_empty() {
                request = request.current_name(current_name);
            }
        }
    }

    match request.send().await {
        Ok(req) => match req.response() {
            Ok(files) => return Ok(files.uris().first().and_then(uri_to_local_path)),
            Err(err) if is_portal_cancelled(&err) => return Ok(None),
            Err(err) => {
                tracing::debug!("SaveFile portal response failed ({err}); trying zenity fallback");
            }
        },
        Err(err) if is_portal_cancelled(&err) => return Ok(None),
        Err(err) => {
            tracing::debug!("SaveFile portal request failed ({err}); trying zenity fallback");
        }
    }

    zenity_save_fallback(title, current_name, default_dir).await
}

async fn zenity_open_fallback(
    title: &str,
    multiple: bool,
    directory: bool,
    default_dir: Option<&Path>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut cmd = tokio::process::Command::new("zenity");
    cmd.arg("--file-selection").arg("--title").arg(title);
    if directory {
        cmd.arg("--directory");
    }
    if multiple {
        cmd.arg("--multiple").arg("--separator=\n");
    }
    if let Some(dir) = default_dir {
        let mut s = dir.as_os_str().to_os_string();
        s.push("/");
        cmd.arg("--filename").arg(s);
    }
    let out = cmd
        .output()
        .await
        .context("neither XDG FileChooser portal nor zenity is available")?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

async fn zenity_save_fallback(
    title: &str,
    current_name: &str,
    default_dir: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    let mut cmd = tokio::process::Command::new("zenity");
    cmd.arg("--file-selection")
        .arg("--save")
        .arg("--confirm-overwrite")
        .arg("--title")
        .arg(title);
    let candidate = match (default_dir, current_name.is_empty()) {
        (Some(dir), false) => Some(dir.join(current_name)),
        (Some(dir), true) => Some(dir.to_path_buf()),
        (None, false) => Some(PathBuf::from(current_name)),
        (None, true) => None,
    };
    if let Some(c) = candidate {
        cmd.arg("--filename").arg(c);
    }
    let out = cmd
        .output()
        .await
        .context("neither XDG FileChooser portal nor zenity is available")?;
    if !out.status.success() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string();
    if line.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(line)))
    }
}

/// Opens the native XDG `org.freedesktop.portal.FileChooser` SaveFile dialog.
///
/// Returns `Ok(None)` when the user cancels the dialog.
pub async fn pick_save_file(title: &str, current_name: &str) -> anyhow::Result<Option<PathBuf>> {
    pick_save_file_with_dir(title, current_name, None).await
}

/// Prunes screenshot folders under `base_dir` older than `max_age`.
pub fn prune_old_screenshots(base_dir: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(base_dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if let Ok(age) = now.duration_since(modified) {
            if age >= max_age {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
}

/// Copies a captured screenshot image into `<base_dir>/<uuid>/screenshot.png` with private `0700`
/// directory permissions so it remains stable while the Zalo uploader reads it.
pub fn stage_screenshot_file(source: &Path, base_dir: &Path) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(base_dir)
        .with_context(|| format!("failed to create screenshot root {}", base_dir.display()))?;
    let _ = fs::set_permissions(base_dir, fs::Permissions::from_mode(0o700));

    prune_old_screenshots(base_dir, SCREENSHOT_RETENTION);

    let shot_dir = base_dir.join(Uuid::new_v4().to_string());
    fs::create_dir_all(&shot_dir)
        .with_context(|| format!("failed to create screenshot dir {}", shot_dir.display()))?;
    let _ = fs::set_permissions(&shot_dir, fs::Permissions::from_mode(0o700));

    let dest = shot_dir.join("screenshot.png");
    fs::copy(source, &dest).with_context(|| {
        format!(
            "failed to copy screenshot from {} to {}",
            source.display(),
            dest.display()
        )
    })?;
    let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o600));

    Ok(dest)
}

/// Triggers an interactive XDG Desktop Portal screenshot (`org.freedesktop.portal.Screenshot`)
/// and stages the resulting PNG into `/tmp/olaz-screenshots/<uuid>/screenshot.png`.
///
/// Returns `Ok(None)` if the user dismisses/cancels the interactive capture overlay.
pub async fn capture_interactive_screenshot() -> anyhow::Result<Option<PathBuf>> {
    let response = match Screenshot::request()
        .interactive(true)
        .modal(true)
        .send()
        .await
    {
        Ok(req) => match req.response() {
            Ok(shot) => shot,
            Err(err) if is_portal_cancelled(&err) => return Ok(None),
            Err(err) => return Err(err).context("Screenshot portal response failed"),
        },
        Err(err) if is_portal_cancelled(&err) => return Ok(None),
        Err(err) => return Err(err).context("Screenshot portal request failed"),
    };

    let source_path = uri_to_local_path(response.uri()).with_context(|| {
        format!(
            "Screenshot portal returned non-local URI: {}",
            response.uri()
        )
    })?;

    let staged = stage_screenshot_file(&source_path, Path::new(SCREENSHOT_STAGING_DIR))?;
    Ok(Some(staged))
}
