//! Desktop notifications (`org.freedesktop.Notifications` via `notify-rust`),
//! `NativeNotifier.swift` privacy & suppression policies, `rodio` MP3 notification chime
//! playback, and Ubuntu Dock unread badge updates (`com.canonical.Unity.LauncherEntry`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use base64::Engine as _;
use notify_rust::{Hint, Notification, Timeout};
use parking_lot::Mutex;
use rodio::{Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zbus::zvariant::Value;

/// Generic notification text used when message preview is off or a chat is PIN-locked / hidden-unknown.
/// Matches `STR_PASS_CODE_NOTIFY_MESSAGE` in `NativeNotifier.swift`.
pub const HIDDEN_PREVIEW_TEXT: &str = "Bạn có tin nhắn mới";

/// Default user-idle threshold (in seconds) below which notifications for the actively viewed
/// conversation are suppressed (`NativeNotifier.swift`: 180s).
pub const DEFAULT_IDLE_THRESHOLD_SECS: f64 = 180.0;

/// Ubuntu Dock / Plank / KDE task-manager LauncherEntry D-Bus constants.
pub const UNITY_LAUNCHER_OBJECT_PATH: &str = "/com/canonical/unity/launcherentry/olaz";
pub const UNITY_LAUNCHER_INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
pub const UNITY_LAUNCHER_MEMBER: &str = "Update";
pub const UNITY_DESKTOP_URI: &str = "application://olaz.desktop";

/// Parsed icon reference from `$znoti.createNotification(title, options)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationIcon {
    /// Remote `http(s)://` avatar URL.
    Remote(String),
    /// Decoded `data:image/...;base64,...` mosaic bytes.
    Inline(Vec<u8>),
    /// Bundled asset relative to `pc-dist/` (query string stripped).
    Bundled(String),
}

/// Incoming notification request from the Zalo renderer (`_noti_create`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationRequest {
    pub title: String,
    pub body: String,
    /// Conversation ID (`fromUid` for 1-1, `g...` for groups).
    pub tag: String,
    pub silent: bool,
    pub icon: Option<NotificationIcon>,
}

impl NotificationRequest {
    /// Parses Electron `Notification(title, options)` IPC arguments (`[title, {body, tag, silent, icon}]`).
    pub fn parse_ipc_args(args: &serde_json::Value) -> Option<Self> {
        let arr = args.as_array()?;
        let title = arr.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let opts = arr.get(1).and_then(|v| v.as_object());

        let body = opts
            .and_then(|o| o.get("body"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if title.is_empty() && body.is_empty() {
            return None;
        }

        let tag = match opts.and_then(|o| o.get("tag")) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };

        let silent = opts
            .and_then(|o| o.get("silent"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let icon = opts
            .and_then(|o| o.get("icon"))
            .and_then(|v| v.as_str())
            .and_then(Self::parse_icon);

        Some(Self {
            title,
            body,
            tag,
            silent,
            icon,
        })
    }

    /// Parses an `icon` string from the renderer notification options (`NativeNotifier.swift:49-64`).
    pub fn parse_icon(raw: &str) -> Option<NotificationIcon> {
        let text = raw.trim();
        if text.is_empty() {
            return None;
        }
        if text.starts_with("data:") {
            let (header, encoded) = text.split_once(',')?;
            if !header.contains(";base64") {
                return None;
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .ok()?;
            if bytes.is_empty() {
                return None;
            }
            return Some(NotificationIcon::Inline(bytes));
        }
        if let Ok(parsed) = url::Url::parse(text) {
            if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() {
                return Some(NotificationIcon::Remote(text.to_string()));
            }
        }
        if text.contains("://") {
            return None;
        }
        let path = text.split('?').next().unwrap_or(text);
        if path.is_empty() {
            None
        } else {
            Some(NotificationIcon::Bundled(path.to_string()))
        }
    }
}

/// Effective notification fields after applying privacy and PIN-lock redaction policies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedNotification {
    pub title: String,
    pub body: String,
    pub allow_inline_reply: bool,
    pub redacted_by_lock: bool,
}

/// Pure notification suppression, privacy, and sound allow-list policy
/// ported from `NativeNotifier.swift:67-102`.
pub struct NotificationPolicy;

impl NotificationPolicy {
    /// Suppresses desktop banner when main window is focused, visible, `open_conversation == Some(tag)`,
    /// and `idle_seconds < 180.0`.
    pub fn should_suppress(
        window_focused: bool,
        window_visible: bool,
        open_conversation: Option<&str>,
        tag: &str,
        idle_seconds: f64,
    ) -> bool {
        Self::should_suppress_with_threshold(
            window_focused,
            window_visible,
            open_conversation,
            tag,
            idle_seconds,
            DEFAULT_IDLE_THRESHOLD_SECS,
        )
    }

    pub fn should_suppress_with_threshold(
        window_focused: bool,
        window_visible: bool,
        open_conversation: Option<&str>,
        tag: &str,
        idle_seconds: f64,
        idle_threshold_secs: f64,
    ) -> bool {
        if !window_focused || !window_visible || tag.is_empty() {
            return false;
        }
        if open_conversation != Some(tag) {
            return false;
        }
        idle_seconds < idle_threshold_secs
    }

    /// Inline reply must be refused when hidden-chat state has not loaded yet or the conversation is PIN-locked.
    pub fn refuses_reply(
        hidden_known: bool,
        locked_ids: &HashSet<String>,
        conv_id: &str,
    ) -> bool {
        !hidden_known || (!conv_id.is_empty() && locked_ids.contains(conv_id))
    }

    /// Redacts notification content and inline-reply capability according to PIN-lock state and preview preferences:
    /// - If `!hidden_known || locked_ids.contains(conv_id)`: both `title` and `body` become generic
    ///   `"Bạn có tin nhắn mới"` and inline reply is stripped.
    /// - Otherwise if `!shows_preview`: `title` is preserved while `body` becomes `"Bạn có tin nhắn mới"`.
    pub fn redact(
        title: &str,
        body: &str,
        conv_id: &str,
        shows_preview: bool,
        hidden_known: bool,
        locked_ids: &HashSet<String>,
    ) -> RedactedNotification {
        if Self::refuses_reply(hidden_known, locked_ids, conv_id) {
            return RedactedNotification {
                title: HIDDEN_PREVIEW_TEXT.to_string(),
                body: HIDDEN_PREVIEW_TEXT.to_string(),
                allow_inline_reply: false,
                redacted_by_lock: true,
            };
        }

        RedactedNotification {
            title: if title.is_empty() {
                "Olaz".to_string()
            } else {
                title.to_string()
            },
            body: if shows_preview {
                body.to_string()
            } else {
                HIDDEN_PREVIEW_TEXT.to_string()
            },
            allow_inline_reply: true,
            redacted_by_lock: false,
        }
    }

    /// Conversations that became hidden between two snapshots (`new - old`), whose banners must be cleared.
    pub fn newly_hidden(old: &HashSet<String>, new: &HashSet<String>) -> HashSet<String> {
        new.difference(old).cloned().collect()
    }

    /// Sniffs supported attachment image MIME type and extension from magic bytes (`PNG`, `JPEG`, `GIF`, `WEBP`).
    pub fn attachment_type(data: &[u8]) -> Option<(&'static str, &'static str)> {
        if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            return Some(("image/png", "png"));
        }
        if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(("image/jpeg", "jpg"));
        }
        if data.starts_with(b"GIF8") {
            return Some(("image/gif", "gif"));
        }
        if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            return Some(("image/webp", "webp"));
        }
        None
    }

    /// Checks whether `host` belongs to an official Zalo CDN domain.
    pub fn is_zalo_cdn_host(host: &str) -> bool {
        let lower = host.trim().to_ascii_lowercase();
        const SUFFIXES: &[&str] = &[".zdn.vn", ".zadn.vn", ".zalo.me", ".zaloapp.com"];
        const EXACT: &[&str] = &["zdn.vn", "zadn.vn", "zalo.me", "zaloapp.com"];
        EXACT.contains(&lower.as_str()) || SUFFIXES.iter().any(|s| lower.ends_with(s))
    }

    /// Only official Zalo CDN HTTPS `/sound/noti.mp3` URLs may be fetched and played (`NativeNotifier.swift:98-101`).
    pub fn is_sound_url_allowed(raw_url: &str) -> bool {
        let Ok(url) = url::Url::parse(raw_url.trim()) else {
            return false;
        };
        if url.scheme() != "https" {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        Self::is_zalo_cdn_host(host) && url.path().ends_with("/sound/noti.mp3")
    }
}

/// Builds the D-Bus `com.canonical.Unity.LauncherEntry` `Update` signal message for `unread_count`.
pub fn build_unity_launcher_signal(unread_count: u64) -> zbus::Result<zbus::Message> {
    let mut props: HashMap<&str, Value<'_>> = HashMap::with_capacity(2);
    let clamped = i64::try_from(unread_count).unwrap_or(i64::MAX);
    props.insert("count", Value::I64(clamped));
    props.insert("count-visible", Value::Bool(unread_count > 0));

    zbus::Message::signal(
        UNITY_LAUNCHER_OBJECT_PATH,
        UNITY_LAUNCHER_INTERFACE,
        UNITY_LAUNCHER_MEMBER,
    )?
    .build(&(UNITY_DESKTOP_URI, props))
}

/// Event emitted when the user interacts with a desktop notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationEvent {
    Clicked { id: String, conv_id: String },
    Closed { id: String, conv_id: String },
}

#[derive(Debug, Clone)]
struct LiveEntry {
    xdg_id: u32,
    conv_id: String,
}

/// Linux desktop notification dispatcher, chime player, and Ubuntu Dock badge emitter.
#[derive(Clone)]
pub struct LinuxNotifier {
    live: Arc<Mutex<HashMap<String, LiveEntry>>>,
    on_event: Arc<Mutex<Option<Arc<dyn Fn(NotificationEvent) + Send + Sync + 'static>>>>,
}

impl Default for LinuxNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxNotifier {
    pub fn new() -> Self {
        Self {
            live: Arc::new(Mutex::new(HashMap::new())),
            on_event: Arc::new(Mutex::new(None)),
        }
    }

    /// Registers a callback invoked on notification click or close.
    pub fn set_on_event(&self, callback: impl Fn(NotificationEvent) + Send + Sync + 'static) {
        *self.on_event.lock() = Some(Arc::new(callback));
    }

    /// Sends a desktop notification via `org.freedesktop.Notifications` after applying
    /// suppression and PIN-lock privacy policies.
    ///
    /// Returns `Ok(Some(notification_uuid))` when displayed, or `Ok(None)` when suppressed.
    #[allow(clippy::too_many_arguments)]
    pub fn show_notification(
        &self,
        request: &NotificationRequest,
        window_focused: bool,
        window_visible: bool,
        open_conversation: Option<&str>,
        idle_seconds: f64,
        shows_preview: bool,
        hidden_known: bool,
        locked_ids: &HashSet<String>,
        avatar_path: Option<&Path>,
    ) -> anyhow::Result<Option<String>> {
        if NotificationPolicy::should_suppress(
            window_focused,
            window_visible,
            open_conversation,
            &request.tag,
            idle_seconds,
        ) {
            tracing::debug!(tag = %request.tag, "notification suppressed (conversation actively open)");
            return Ok(None);
        }

        let redacted = NotificationPolicy::redact(
            &request.title,
            &request.body,
            &request.tag,
            shows_preview,
            hidden_known,
            locked_ids,
        );

        let id = Uuid::new_v4().to_string();
        let mut notif = Notification::new();
        notif
            .appname("Olaz")
            .summary(&redacted.title)
            .body(&redacted.body)
            .icon("olaz")
            .hint(Hint::DesktopEntry("olaz".to_string()))
            .hint(Hint::Category("im.received".to_string()))
            .timeout(Timeout::Default)
            .action("default", "Mở");

        if request.silent {
            notif.hint(Hint::SuppressSound(true));
        }

        if !redacted.redacted_by_lock {
            if let Some(path) = avatar_path {
                if path.exists() {
                    if let Some(path_str) = path.to_str() {
                        notif.image_path(path_str);
                    }
                }
            }
        }

        let handle = notif.show()?;
        let xdg_id = handle.id();
        let conv_id = request.tag.clone();

        self.live.lock().insert(
            id.clone(),
            LiveEntry {
                xdg_id,
                conv_id: conv_id.clone(),
            },
        );

        let live_map = Arc::clone(&self.live);
        let cb_slot = Arc::clone(&self.on_event);
        let id_for_thread = id.clone();

        let _ = thread::Builder::new()
            .name("olaz-notif-wait".to_string())
            .spawn(move || {
                handle.wait_for_action(|action| {
                    live_map.lock().remove(&id_for_thread);
                    let cb = cb_slot.lock().clone();
                    if let Some(cb) = cb {
                        match action {
                            "__closed" => cb(NotificationEvent::Closed {
                                id: id_for_thread.clone(),
                                conv_id: conv_id.clone(),
                            }),
                            _ => {
                                cb(NotificationEvent::Clicked {
                                    id: id_for_thread.clone(),
                                    conv_id: conv_id.clone(),
                                });
                                cb(NotificationEvent::Closed {
                                    id: id_for_thread.clone(),
                                    conv_id: conv_id.clone(),
                                });
                            }
                        }
                    }
                });
            });

        Ok(Some(id))
    }

    /// Closes a specific notification by its Olaz UUID over `org.freedesktop.Notifications`.
    pub async fn close_notification(&self, id: &str) {
        let entry = self.live.lock().remove(id);
        if let Some(entry) = entry {
            let _ = close_xdg_notification_id(entry.xdg_id).await;
        }
    }

    /// Closes all delivered notifications belonging to `conv_id` (e.g. when opened or newly hidden).
    pub async fn clear_conversation(&self, conv_id: &str) {
        if conv_id.is_empty() {
            return;
        }
        let xdg_ids: Vec<u32> = {
            let mut guard = self.live.lock();
            let keys_to_remove: Vec<String> = guard
                .iter()
                .filter(|(_, v)| v.conv_id == conv_id)
                .map(|(k, _)| k.clone())
                .collect();
            keys_to_remove
                .into_iter()
                .filter_map(|k| guard.remove(&k).map(|e| e.xdg_id))
                .collect()
        };
        for xdg_id in xdg_ids {
            let _ = close_xdg_notification_id(xdg_id).await;
        }
    }

    /// Plays MP3 chime bytes asynchronously on a background thread via `rodio`.
    /// Gracefully no-ops (`Ok(false)`) when no audio output device exists or decoding fails.
    pub fn play_chime_bytes(&self, mp3_bytes: Vec<u8>) {
        if mp3_bytes.is_empty() {
            return;
        }
        let _ = thread::Builder::new()
            .name("olaz-chime".to_string())
            .spawn(move || {
                let _ = Self::play_chime_bytes_sync(&mp3_bytes);
            });
    }

    /// Plays a cached/bundled MP3 file asynchronously via `rodio`, gracefully no-oping if unavailable.
    pub fn play_chime_file(&self, path: &Path) {
        if let Ok(bytes) = fs::read(path) {
            self.play_chime_bytes(bytes);
        }
    }

    /// Synchronous helper that attempts to play MP3 bytes on the default output device.
    /// Returns `false` (never panicking or failing) if no audio output device is present or data is invalid.
    pub fn play_chime_bytes_sync(mp3_bytes: &[u8]) -> bool {
        if mp3_bytes.is_empty() {
            return false;
        }
        let Ok((_stream, stream_handle)) = OutputStream::try_default() else {
            tracing::debug!("no audio output device available; skipping notification chime");
            return false;
        };
        let Ok(sink) = Sink::try_new(&stream_handle) else {
            tracing::debug!("failed to create rodio Sink; skipping notification chime");
            return false;
        };
        let cursor = Cursor::new(mp3_bytes.to_vec());
        let Ok(decoder) = Decoder::new(cursor) else {
            tracing::debug!("failed to decode MP3 notification chime");
            return false;
        };
        sink.append(decoder);
        sink.sleep_until_end();
        true
    }

    /// Emits the `com.canonical.Unity.LauncherEntry` `Update` D-Bus signal on the session bus
    /// (`application://olaz.desktop`, `count`, `count-visible`) for Ubuntu Dock unread badges.
    pub async fn emit_unity_badge(&self, unread_count: u64) -> anyhow::Result<()> {
        let conn = zbus::Connection::session().await?;
        let mut props: HashMap<&str, Value<'_>> = HashMap::with_capacity(2);
        let clamped = i64::try_from(unread_count).unwrap_or(i64::MAX);
        props.insert("count", Value::I64(clamped));
        props.insert("count-visible", Value::Bool(unread_count > 0));

        conn.emit_signal(
            None::<&str>,
            UNITY_LAUNCHER_OBJECT_PATH,
            UNITY_LAUNCHER_INTERFACE,
            UNITY_LAUNCHER_MEMBER,
            &(UNITY_DESKTOP_URI, props),
        )
        .await?;

        Ok(())
    }
}

async fn close_xdg_notification_id(xdg_id: u32) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "CloseNotification",
        &(xdg_id,),
    )
    .await?;
    Ok(())
}
