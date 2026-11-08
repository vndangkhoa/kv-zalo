//! Rich `serde` data models for `ZaloEngineState`, porting `ZaloEngine*.swift` and `ChatModels.swift`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Kickout message shown when Zalo logs out this desktop session due to another PC login.
pub const DEFAULT_KICKOUT_NOTICE: &str =
    "Tài khoản của bạn vừa đăng nhập trên một Zalo PC khác, nên phiên này đã bị đăng xuất. Zalo chỉ cho phép một phiên máy tính cho mỗi tài khoản.";

/// Account confirmation info shown after scanning the QR code on mobile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingConfirm {
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

/// Top-level lifecycle phase of the Olaz engine.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum EnginePhase {
    #[default]
    Booting,
    InstallingRuntime {
        step: String,
        fraction: f64,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    Login {
        #[serde(default)]
        qr: Option<String>,
        #[serde(default)]
        qr_expired: bool,
        #[serde(default)]
        waiting_confirm: Option<WaitingConfirm>,
        #[serde(default)]
        kickout_notice: Option<String>,
        #[serde(default)]
        error_notice: Option<String>,
    },
    Ready,
}

/// Signed-in Zalo user profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    pub user_id: String,
    pub display_name: String,
    #[serde(default)]
    pub zalo_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub raw: Value,
}

impl UserProfile {
    pub fn from_json(val: &Value) -> Option<Self> {
        let obj = val.as_object()?;
        let user_id = obj
            .get("userId")
            .or_else(|| obj.get("user_id"))
            .or_else(|| obj.get("id"))
            .and_then(|v| v.as_str())?
            .trim()
            .to_string();
        if user_id.is_empty() {
            return None;
        }
        let zalo_name = obj
            .get("zaloName")
            .or_else(|| obj.get("zalo_name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let display_name = obj
            .get("displayName")
            .or_else(|| obj.get("display_name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| zalo_name.clone())
            .unwrap_or_default();
        let avatar = obj
            .get("avatar")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let phone = obj
            .get("phoneNumber")
            .or_else(|| obj.get("phone"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);

        Some(Self {
            user_id,
            display_name,
            zalo_name,
            avatar,
            phone,
            raw: val.clone(),
        })
    }
}

/// Last-message preview shown in a conversation list row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationPreview {
    pub text: String,
    /// Normalized epoch timestamp in milliseconds, if present.
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default)]
    pub from_me: bool,
}

/// Conversation summary from `conv-info-manager` + profiles (`ZaloEngine.ConversationSummary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    #[serde(default)]
    pub is_group: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub members: u32,
    #[serde(default)]
    pub unread: u32,
    #[serde(default)]
    pub preview: Option<ConversationPreview>,
    #[serde(default)]
    pub raw: Value,
}

impl Conversation {
    /// Fallback display name matching `ZaloEngine.swift:65`.
    pub fn display_name(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else if self.is_group {
            "Nhóm"
        } else {
            "Người dùng Zalo"
        }
    }

    pub fn from_json(val: &Value) -> Option<Self> {
        let obj = val.as_object()?;
        let id = obj.get("id").and_then(|v| v.as_str())?.to_string();
        if id.is_empty() {
            return None;
        }
        let preview = obj
            .get("preview")
            .and_then(|v| v.as_object())
            .map(|p| {
                let raw_ts = p
                    .get("ts")
                    .or_else(|| p.get("timestamp"))
                    .and_then(json_f64)
                    .unwrap_or(0.0);
                let timestamp = if raw_ts > 0.0 {
                    let ms = if raw_ts > 1e12 {
                        raw_ts
                    } else {
                        raw_ts * 1000.0
                    };
                    Some(clamped_i64(ms))
                } else {
                    None
                };
                ConversationPreview {
                    text: p
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    timestamp,
                    from_me: p
                        .get("fromMe")
                        .or_else(|| p.get("from_me"))
                        .and_then(json_bool)
                        .unwrap_or(false),
                }
            });

        Some(Self {
            id,
            is_group: obj
                .get("isGroup")
                .or_else(|| obj.get("is_group"))
                .and_then(json_bool)
                .unwrap_or(false),
            name: obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            avatar: obj
                .get("avatar")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            pinned: obj.get("pinned").and_then(json_bool).unwrap_or(false),
            members: obj
                .get("members")
                .and_then(json_f64)
                .map(|n| clamped_u32(n))
                .unwrap_or(0),
            unread: obj
                .get("unread")
                .and_then(json_f64)
                .map(|n| clamped_u32(n))
                .unwrap_or(0),
            preview,
            raw: val.clone(),
        })
    }

    /// Sorts conversations pinned-first, then newest preview timestamp, then display name (`ZaloEngine.swift:487`).
    pub fn sort_list(list: &mut [Self]) {
        list.sort_by(|a, b| {
            if a.pinned != b.pinned {
                return b.pinned.cmp(&a.pinned);
            }
            let ta = a.preview.as_ref().and_then(|p| p.timestamp).unwrap_or(i64::MIN);
            let tb = b.preview.as_ref().and_then(|p| p.timestamp).unwrap_or(i64::MIN);
            if ta != tb {
                return tb.cmp(&ta);
            }
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
        });
    }
}

/// Conversation label ("Phân loại").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationLabel {
    pub id: String,
    pub name: String,
    pub color_hex: String,
    #[serde(default)]
    pub conv_ids: Vec<String>,
}

/// Pseudo-row metadata for unanswered stranger conversations (`stranger_box`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrangerBox {
    pub chats: u32,
    pub unread_chats: u32,
    pub unread_messages: u32,
    #[serde(default)]
    pub newest_id: Option<String>,
}

/// Conversation list ordering, labels ("Phân loại"), and stranger box state (`ZaloEngine+Conversations.swift`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationExtras {
    pub loaded: bool,
    #[serde(default)]
    pub self_conversation_id: Option<String>,
    #[serde(default)]
    pub list_ids: Vec<String>,
    #[serde(default)]
    pub labels: Vec<ConversationLabel>,
    #[serde(default)]
    pub label_by_conv: HashMap<String, String>,
    #[serde(default)]
    pub stranger_box: StrangerBox,
    #[serde(default)]
    pub stranger_ids: HashSet<String>,
    #[serde(default)]
    pub stranger_folder_ids: Vec<String>,
    #[serde(default)]
    pub archived_enabled: bool,
    #[serde(default)]
    pub type_filter: i32,
    #[serde(default)]
    pub label_filter_ids: Vec<String>,
    #[serde(default)]
    pub unbox_stranger: bool,
}

impl ConversationExtras {
    pub fn apply_patch(&mut self, json: &Value) {
        let Some(obj) = json.as_object() else {
            return;
        };
        self.loaded = true;

        if let Some(self_id) = obj
            .get("selfConversationId")
            .or_else(|| obj.get("self_conversation_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            self.self_conversation_id = Some(self_id.to_string());
        }

        if let Some(rows) = obj.get("labels").and_then(|v| v.as_array()) {
            let mut labels = Vec::new();
            let mut map = HashMap::new();
            for row in rows {
                let Some(id) = row.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let conv_ids: Vec<String> = row
                    .get("convIds")
                    .or_else(|| row.get("conv_ids"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(ToString::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                for cid in &conv_ids {
                    map.insert(cid.clone(), id.to_string());
                }
                labels.push(ConversationLabel {
                    id: id.to_string(),
                    name: row
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    color_hex: row
                        .get("color")
                        .or_else(|| row.get("color_hex"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    conv_ids,
                });
            }
            self.labels = labels;
            self.label_by_conv = map;
        }

        if let Some(map_obj) = obj.get("label_by_conv").and_then(|v| v.as_object()) {
            self.label_by_conv = map_obj
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();
        }

        if let Some(ids) = obj
            .get("listIds")
            .or_else(|| obj.get("list_ids"))
            .and_then(|v| v.as_array())
        {
            self.list_ids = ids
                .iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect();
        }

        if let Some(ids) = obj
            .get("strangerIds")
            .or_else(|| obj.get("stranger_ids"))
            .and_then(|v| v.as_array())
        {
            self.stranger_ids = ids
                .iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect();
        }

        if let Some(ids) = obj
            .get("strangerFolderIds")
            .or_else(|| obj.get("stranger_folder_ids"))
            .and_then(|v| v.as_array())
        {
            self.stranger_folder_ids = ids
                .iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect();
        }

        if let Some(box_obj) = obj
            .get("strangerBox")
            .or_else(|| obj.get("stranger_box"))
            .and_then(|v| v.as_object())
        {
            self.stranger_box = StrangerBox {
                chats: box_obj
                    .get("chats")
                    .and_then(json_f64)
                    .map(clamped_u32)
                    .unwrap_or(0),
                unread_chats: box_obj
                    .get("unreadChats")
                    .or_else(|| box_obj.get("unread_chats"))
                    .and_then(json_f64)
                    .map(clamped_u32)
                    .unwrap_or(0),
                unread_messages: box_obj
                    .get("unreadMessages")
                    .or_else(|| box_obj.get("unread_messages"))
                    .and_then(json_f64)
                    .map(clamped_u32)
                    .unwrap_or(0),
                newest_id: box_obj
                    .get("newestId")
                    .or_else(|| box_obj.get("newest_id"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
            };
        }

        if let Some(flag) = obj
            .get("archivedEnabled")
            .or_else(|| obj.get("archived_enabled"))
            .and_then(json_bool)
        {
            self.archived_enabled = flag;
        }

        if let Some(filter) = obj.get("filter").and_then(|v| v.as_object()) {
            if let Some(t) = filter.get("type").and_then(json_f64) {
                self.type_filter = clamped_i64(t) as i32;
            }
            if let Some(lids) = filter.get("labelIds").and_then(|v| v.as_array()) {
                self.label_filter_ids = lids
                    .iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect();
            }
        }

        if let Some(flag) = obj
            .get("unboxStranger")
            .or_else(|| obj.get("unbox_stranger"))
            .and_then(json_bool)
        {
            self.unbox_stranger = flag;
        }
    }
}

/// Bitmask constants matching `ConversationActionFlags` (`ZaloEngine+ConversationActions.swift`).
pub mod conv_action_flags {
    pub const PINNED: u32 = 1;
    pub const ARCHIVED: u32 = 2;
    pub const HIDDEN: u32 = 4;
    pub const CANNOT_PIN: u32 = 8;
    pub const CANNOT_ARCHIVE: u32 = 16;
    pub const CANNOT_MUTE: u32 = 32;
    pub const CANNOT_DELETE: u32 = 64;
    pub const NO_PIN_MENU: u32 = 128;
    pub const CANNOT_HIDE: u32 = 256;
    pub const UNREAD_MARK: u32 = 512;
}

/// Hidden-chat PIN state, per-conversation action flags, auto-delete TTLs,
/// and 60-second inactivity auto-relock (`HiddenStateGuard.relockAfterInactivity`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConvActionsState {
    pub loaded: bool,
    pub hidden_known: bool,
    pub hidden_enabled: bool,
    #[serde(default)]
    pub hidden_ids: HashSet<String>,
    pub pin_set: bool,
    #[serde(default)]
    pub unlocked_ids: HashSet<String>,
    /// Auto-delete TTL in milliseconds/seconds per conversation ID (`0` = off, omitted when unset).
    #[serde(default)]
    pub auto_delete: HashMap<String, u64>,
    #[serde(default)]
    pub flags: HashMap<String, u32>,
    #[serde(default)]
    pub left: HashMap<String, String>,
    pub mark_unread_enabled: bool,
    /// Last user activity timestamp used to enforce the 60s hidden-chat auto-relock rule.
    #[serde(skip)]
    pub last_activity: Option<Instant>,
}

impl PartialEq for ConvActionsState {
    fn eq(&self, other: &Self) -> bool {
        self.loaded == other.loaded
            && self.hidden_known == other.hidden_known
            && self.hidden_enabled == other.hidden_enabled
            && self.hidden_ids == other.hidden_ids
            && self.pin_set == other.pin_set
            && self.unlocked_ids == other.unlocked_ids
            && self.auto_delete == other.auto_delete
            && self.flags == other.flags
            && self.left == other.left
            && self.mark_unread_enabled == other.mark_unread_enabled
    }
}

impl Eq for ConvActionsState {}

impl ConvActionsState {
    /// Hidden conversations unlocked via PIN automatically relock after 60s of inactivity.
    pub const RELOCK_AFTER_INACTIVITY: Duration = Duration::from_secs(60);

    pub fn is_hidden(&self, conv_id: &str) -> bool {
        self.hidden_ids.contains(conv_id)
            || (self.flags.get(conv_id).copied().unwrap_or(0) & conv_action_flags::HIDDEN) != 0
    }

    pub fn is_locked(&self, conv_id: &str) -> bool {
        !self.hidden_known || (self.is_hidden(conv_id) && !self.unlocked_ids.contains(conv_id))
    }

    /// Returns all currently locked hidden conversation IDs (`hidden_ids \ unlocked_ids`).
    pub fn locked_ids(&self) -> HashSet<String> {
        let mut locked: HashSet<String> = self
            .hidden_ids
            .difference(&self.unlocked_ids)
            .cloned()
            .collect();
        for (id, &bits) in &self.flags {
            if (bits & conv_action_flags::HIDDEN) != 0 && !self.unlocked_ids.contains(id) {
                locked.insert(id.clone());
            }
        }
        locked
    }

    /// Records user activity, refreshing the 60s auto-relock timer.
    pub fn note_activity(&mut self, now: Instant) {
        self.last_activity = Some(now);
    }

    /// Unlocks a hidden conversation with the user's PIN and starts/refreshes the 60s activity timer.
    pub fn unlock(&mut self, conv_id: impl Into<String>, now: Instant) {
        self.unlocked_ids.insert(conv_id.into());
        self.last_activity = Some(now);
    }

    /// Immediately relocks all unlocked hidden conversations.
    pub fn relock(&mut self) {
        self.unlocked_ids.clear();
    }

    /// Checks whether 60s have elapsed since `last_activity` and clears `unlocked_ids` if so.
    /// Returns `true` if one or more unlocked chats were relocked.
    pub fn check_auto_relock(&mut self, now: Instant) -> bool {
        let Some(last) = self.last_activity else {
            if !self.unlocked_ids.is_empty() {
                self.unlocked_ids.clear();
                return true;
            }
            return false;
        };
        if now.saturating_duration_since(last) >= Self::RELOCK_AFTER_INACTIVITY {
            if !self.unlocked_ids.is_empty() {
                self.unlocked_ids.clear();
                return true;
            }
        }
        false
    }

    pub fn apply_patch(&mut self, json: &Value) {
        let Some(obj) = json.as_object() else {
            return;
        };

        if let Some(hidden) = obj.get("hidden").and_then(|v| v.as_object()) {
            self.hidden_known = true;
            self.hidden_enabled = hidden.get("enabled").and_then(json_bool).unwrap_or(false);
            self.pin_set = hidden
                .get("hasPin")
                .or_else(|| hidden.get("pin_set"))
                .and_then(json_bool)
                .unwrap_or(false);
            if let Some(ids) = hidden.get("ids").and_then(|v| v.as_array()) {
                self.hidden_ids = ids
                    .iter()
                    .filter_map(|x| x.as_str().map(ToString::to_string))
                    .collect();
            }
        }

        if let Some(hk) = obj
            .get("hiddenKnown")
            .or_else(|| obj.get("hidden_known"))
            .and_then(json_bool)
        {
            self.hidden_known = hk;
        }
        if let Some(ps) = obj
            .get("pinSet")
            .or_else(|| obj.get("pin_set"))
            .and_then(json_bool)
        {
            self.pin_set = ps;
        }
        if let Some(ids) = obj
            .get("hiddenIds")
            .or_else(|| obj.get("hidden_ids"))
            .and_then(|v| v.as_array())
        {
            self.hidden_known = true;
            self.hidden_ids = ids
                .iter()
                .filter_map(|x| x.as_str().map(ToString::to_string))
                .collect();
        }

        if let Some(ad) = obj
            .get("autoDelete")
            .or_else(|| obj.get("auto_delete"))
            .and_then(|v| v.as_object())
        {
            let mut next_ad = HashMap::new();
            for (cid, ttl_val) in ad {
                if let Some(ttl) = json_f64(ttl_val) {
                    if ttl > 0.0 {
                        next_ad.insert(cid.clone(), ttl as u64);
                    }
                }
            }
            self.auto_delete = next_ad;
        }

        if let Some(raw_flags) = obj.get("flags").and_then(|v| v.as_object()) {
            self.loaded = true;
            let mut next_flags = HashMap::new();
            for (cid, bits_val) in raw_flags {
                if let Some(bits) = json_f64(bits_val).map(clamped_u32) {
                    if bits != 0 {
                        next_flags.insert(cid.clone(), bits);
                    }
                }
            }
            self.flags = next_flags;
        }

        if let Some(raw_left) = obj.get("left").and_then(|v| v.as_object()) {
            self.left = raw_left
                .iter()
                .map(|(cid, v)| (cid.clone(), v.as_str().unwrap_or("kick").to_string()))
                .collect();
        }

        if let Some(mu) = obj
            .get("markUnread")
            .or_else(|| obj.get("mark_unread_enabled"))
            .and_then(json_bool)
        {
            self.mark_unread_enabled = mu;
        }
    }
}

/// An `@mention` inside a message or composer draft (`pos` and `len` are UTF-16 code-unit offsets!).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mention {
    pub uid: String,
    pub pos: usize,
    pub len: usize,
    #[serde(default)]
    pub is_all: bool,
}

/// Delivery state of a [`ChatMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Sending,
    #[default]
    Sent,
    Delivered,
    Seen,
    Failed,
}

impl MessageStatus {
    pub fn from_str_lossy(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sending" | "pending" => Self::Sending,
            "delivered" => Self::Delivered,
            "seen" | "read" => Self::Seen,
            "failed" | "error" => Self::Failed,
            _ => Self::Sent,
        }
    }
}

/// Quoted reply header inside a [`ChatMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageQuote {
    pub sender_name: String,
    pub text: String,
    #[serde(default)]
    pub msg_id: Option<String>,
}

/// Aggregated emoji reaction chip on a [`ChatMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReaction {
    pub r#type: String,
    #[serde(default)]
    pub key: String,
    pub emoji: String,
    #[serde(default)]
    pub image: String,
    pub count: u32,
    #[serde(default)]
    pub includes_me: bool,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub people: u32,
}

/// Photo / video / GIF / voice attachment metadata on a [`ChatMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAttachment {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub duration: u32,
}

/// File attachment metadata + executable safety check (`MessageBubble.swift:1007-1037`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCard {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: Option<String>,
}

impl FileCard {
    /// Extensions that could execute arbitrary code or install packages when opened directly
    /// (`MessageBubble.swift:1007-1037` + Linux `.desktop` / `.AppImage` / `.deb` / `.rpm` / `.run` / ELF / Windows executables).
    const BLOCKED_EXTENSIONS: &'static [&'static str] = &[
        // macOS / Swift original set (`MessageBubble.swift:1008-1009`)
        "command",
        "tool",
        "terminal",
        "fileloc",
        "webloc",
        "inetloc",
        "pkg",
        "mpkg",
        "app",
        "scpt",
        "scptd",
        "workflow",
        "action",
        "sh",
        "bash",
        "zsh",
        "csh",
        "ksh",
        "jar",
        "dylib",
        "bundle",
        // Linux desktop launchers, binaries, scripts, and packages
        "desktop",
        "appimage",
        "run",
        "bin",
        "elf",
        "out",
        "so",
        "deb",
        "rpm",
        "flatpak",
        "flatpakref",
        "snap",
        "apk",
        "fish",
        "py",
        "pyc",
        "pl",
        "pm",
        "rb",
        "lua",
        "php",
        "tcl",
        "awk",
        // Windows / Wine / cross-platform scripts & executables
        "exe",
        "msi",
        "msp",
        "bat",
        "cmd",
        "com",
        "scr",
        "pif",
        "vbs",
        "vbe",
        "js",
        "jse",
        "mjs",
        "wsf",
        "wsh",
        "ps1",
        "psm1",
        "hta",
        "reg",
        "url",
    ];

    /// Returns `true` when `filename` (untrusted sender-supplied name or path) carries an extension
    /// that may execute code or launch commands when opened directly, so the UI should reveal it in
    /// the file manager rather than `xdg-open`ing it.
    pub fn may_execute(filename: &str) -> bool {
        let trimmed = filename.trim().trim_end_matches([' ', '.']);
        if trimmed.is_empty() {
            return false;
        }
        // Strip query string / fragment if a URL was passed.
        let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
        let base = Path::new(without_query)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(without_query)
            .trim_end_matches([' ', '.']);

        let Some(ext) = Path::new(base).extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let lower = ext.to_ascii_lowercase();
        Self::BLOCKED_EXTENSIONS.contains(&lower.as_str())
    }

    /// Checks both a local/remote file path and its sender-provided display name (`MessageBubble.swift:1012`).
    pub fn may_execute_either(file_path_or_url: &str, display_name: &str) -> bool {
        Self::may_execute(file_path_or_url) || Self::may_execute(display_name)
    }
}

/// Sticker attachment metadata on a [`ChatMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerAttachment {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, alias = "cateId")]
    pub cate_id: Option<i64>,
    #[serde(default, alias = "stickerId")]
    pub sticker_id: Option<i64>,
}

/// Group poll reference inside a [`ChatMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollAttachment {
    #[serde(alias = "pollId")]
    pub poll_id: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub anonymous: bool,
    #[serde(default, alias = "conversationId")]
    pub conversation_id: String,
}

/// Rich card attachment (link preview, contact card, location, ecard, OA message, call log).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardAttachment {
    #[serde(alias = "cardType")]
    pub card_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub detail: Value,
}

/// Rich chat message preserving both strongly typed fields and the flexible raw `serde_json::Value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    #[serde(default)]
    pub msg_id: Option<String>,
    #[serde(default)]
    pub cli_msg_id: Option<String>,
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub sender_id: String,
    #[serde(default)]
    pub sender_name: String,
    #[serde(default)]
    pub sender_avatar: Option<String>,
    /// Epoch milliseconds.
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub is_mine: bool,
    #[serde(default)]
    pub status: MessageStatus,
    #[serde(default = "default_kind_text")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    #[serde(default)]
    pub quote: Option<MessageQuote>,
    #[serde(default)]
    pub reactions: Vec<MessageReaction>,
    #[serde(default)]
    pub media: Option<MediaAttachment>,
    #[serde(default)]
    pub file: Option<FileCard>,
    #[serde(default)]
    pub sticker: Option<StickerAttachment>,
    #[serde(default)]
    pub poll: Option<PollAttachment>,
    #[serde(default)]
    pub card: Option<CardAttachment>,
    #[serde(default)]
    pub encrypted: bool,
    #[serde(default)]
    pub recalled: bool,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub raw: Value,
}

fn default_kind_text() -> String {
    "text".to_string()
}

impl ChatMessage {
    /// Parses a `ChatMessageJSON` object from `chat-adapter.js` (or serialized `ChatMessage` JSON)
    /// while preserving the entire raw `serde_json::Value`.
    pub fn from_json(val: &Value, fallback_conv_id: &str) -> Option<Self> {
        let obj = val.as_object()?;

        let non_empty = |keys: &[&str]| -> Option<String> {
            for &k in keys {
                if let Some(v) = obj.get(k) {
                    match v {
                        Value::String(s) if !s.trim().is_empty() => return Some(s.clone()),
                        Value::Number(n) => return Some(n.to_string()),
                        _ => {}
                    }
                }
            }
            None
        };

        let msg_id = non_empty(&["msgId", "msg_id"]);
        let cli_msg_id = non_empty(&["cliMsgId", "cli_msg_id"]);
        let is_mine = obj
            .get("isMine")
            .or_else(|| obj.get("is_mine"))
            .and_then(json_bool)
            .unwrap_or(false);

        // Own messages prefer `cliMsgId` for stable identity across pending -> confirmed transitions
        // (`ChatModels.swift:14`), unless an explicit `id` is already supplied.
        let id = non_empty(&["id"])
            .or_else(|| {
                if is_mine {
                    cli_msg_id.clone().or_else(|| msg_id.clone())
                } else {
                    msg_id.clone().or_else(|| cli_msg_id.clone())
                }
            })?;

        let conversation_id =
            non_empty(&["conversationId", "conversation_id", "convId"]).unwrap_or_else(|| fallback_conv_id.to_string());
        let sender_id = non_empty(&["senderId", "sender_id"]).unwrap_or_default();
        let sender_name = non_empty(&["senderName", "sender_name"]).unwrap_or_default();
        let sender_avatar = non_empty(&["senderAvatar", "sender_avatar"]);

        let raw_ts = obj
            .get("timestamp")
            .or_else(|| obj.get("date"))
            .or_else(|| obj.get("ts"))
            .and_then(json_f64)
            .unwrap_or(0.0);
        let timestamp = clamped_i64(raw_ts);

        let status = obj
            .get("status")
            .and_then(|v| v.as_str())
            .map(MessageStatus::from_str_lossy)
            .unwrap_or(MessageStatus::Sent);

        // `kind` can be either `{ "type": "photo", ... }` (from `chat-adapter.js`) or a flat string `"photo"`.
        let kind_obj = obj.get("kind").and_then(|v| v.as_object());
        let kind = match obj.get("kind") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Object(ko)) => ko
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("text")
                .to_string(),
            _ => "text".to_string(),
        };

        let text = obj
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mentions = obj
            .get("mentions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let uid = m.get("uid")?.as_str()?.to_string();
                        if uid.is_empty() {
                            return None;
                        }
                        let pos = m.get("pos").and_then(json_f64).map(clamped_usize).unwrap_or(0);
                        let len = m.get("len").and_then(json_f64).map(clamped_usize).unwrap_or(0);
                        let is_all = m
                            .get("isAll")
                            .or_else(|| m.get("is_all"))
                            .and_then(json_bool)
                            .unwrap_or_else(|| {
                                uid == "-1" || m.get("type").and_then(json_f64) == Some(1.0)
                            });
                        Some(Mention {
                            uid,
                            pos,
                            len,
                            is_all,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let quote = obj
            .get("quote")
            .and_then(|v| v.as_object())
            .map(|q| MessageQuote {
                sender_name: q
                    .get("senderName")
                    .or_else(|| q.get("sender_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                text: q
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                msg_id: q
                    .get("msgId")
                    .or_else(|| q.get("msg_id"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
            });

        let reactions = obj
            .get("reactions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        let ro = r.as_object()?;
                        let emoji = ro.get("emoji").and_then(|v| v.as_str()).unwrap_or("");
                        let key = ro.get("key").and_then(|v| v.as_str()).unwrap_or("");
                        if emoji.is_empty() && key.is_empty() {
                            return None;
                        }
                        let rtype = ro
                            .get("type")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or(if key.is_empty() { emoji } else { key })
                            .to_string();
                        Some(MessageReaction {
                            r#type: rtype,
                            key: key.to_string(),
                            emoji: if emoji.is_empty() {
                                key.to_string()
                            } else {
                                emoji.to_string()
                            },
                            image: ro
                                .get("image")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            count: ro.get("count").and_then(json_f64).map(clamped_u32).unwrap_or(0),
                            includes_me: ro
                                .get("includesMe")
                                .or_else(|| ro.get("includes_me"))
                                .and_then(json_bool)
                                .unwrap_or(false),
                            names: ro
                                .get("names")
                                .and_then(|v| v.as_array())
                                .map(|ns| {
                                    ns.iter()
                                        .filter_map(|n| n.as_str().map(ToString::to_string))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            people: ro
                                .get("people")
                                .and_then(json_f64)
                                .map(clamped_u32)
                                .unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Parse media / file / sticker / poll / card from either explicit top-level fields or `kind` object.
        let mut media = obj
            .get("media")
            .and_then(|v| serde_json::from_value::<MediaAttachment>(v.clone()).ok());
        let mut file = obj
            .get("file")
            .and_then(|v| serde_json::from_value::<FileCard>(v.clone()).ok());
        let mut sticker = obj
            .get("sticker")
            .and_then(|v| serde_json::from_value::<StickerAttachment>(v.clone()).ok());
        let mut poll = obj
            .get("poll")
            .and_then(|v| serde_json::from_value::<PollAttachment>(v.clone()).ok());
        let mut card = obj
            .get("card")
            .and_then(|v| serde_json::from_value::<CardAttachment>(v.clone()).ok());

        if let Some(ko) = kind_obj {
            let ko_str = |k: &str| -> Option<String> {
                ko.get(k)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            };
            match kind.as_str() {
                "photo" | "video" | "gif" | "voice" if media.is_none() => {
                    media = Some(MediaAttachment {
                        url: ko_str("url"),
                        thumbnail: ko_str("thumbnail").or_else(|| ko_str("thumb")),
                        width: ko.get("width").and_then(json_f64).map(clamped_u32).unwrap_or(0),
                        height: ko.get("height").and_then(json_f64).map(clamped_u32).unwrap_or(0),
                        duration: ko
                            .get("duration")
                            .and_then(json_f64)
                            .map(clamped_u32)
                            .unwrap_or(0),
                    });
                }
                "file" if file.is_none() => {
                    file = Some(FileCard {
                        name: ko_str("name").unwrap_or_default(),
                        size: ko
                            .get("size")
                            .and_then(json_f64)
                            .map(|n| n.max(0.0) as u64)
                            .unwrap_or(0),
                        url: ko_str("url"),
                    });
                }
                "sticker" if sticker.is_none() => {
                    sticker = Some(StickerAttachment {
                        url: ko_str("url"),
                        cate_id: ko.get("cateId").and_then(json_f64).map(clamped_i64),
                        sticker_id: ko.get("stickerId").and_then(json_f64).map(clamped_i64),
                    });
                }
                "poll" if poll.is_none() => {
                    poll = Some(PollAttachment {
                        poll_id: ko_str("pollId").unwrap_or_default(),
                        question: ko_str("question").unwrap_or_default(),
                        header: ko_str("header").unwrap_or_default(),
                        anonymous: ko.get("anonymous").and_then(json_bool).unwrap_or(false),
                        conversation_id: ko_str("conversationId")
                            .unwrap_or_else(|| conversation_id.clone()),
                    });
                }
                "link" | "contactCard" | "location" | "call" | "ecard" | "oaMessage"
                | "stickerSet" | "zinstant"
                    if card.is_none() =>
                {
                    card = Some(CardAttachment {
                        card_type: kind.clone(),
                        title: ko_str("title").or_else(|| ko_str("name")).unwrap_or_default(),
                        description: ko_str("description")
                            .or_else(|| ko_str("address"))
                            .or_else(|| ko_str("detail"))
                            .unwrap_or_default(),
                        url: ko_str("url"),
                        thumbnail: ko_str("thumbnail").or_else(|| ko_str("thumb")),
                        detail: Value::Object(ko.clone()),
                    });
                }
                _ => {}
            }
        }

        let encrypted = obj
            .get("encrypted")
            .or_else(|| obj.get("isE2EE"))
            .and_then(json_bool)
            .unwrap_or(false);

        let recalled = obj
            .get("recalled")
            .and_then(json_bool)
            .unwrap_or_else(|| kind == "recalled");

        let starred = obj
            .get("starred")
            .and_then(json_bool)
            .or_else(|| {
                obj.get("menu")
                    .and_then(|m| m.get("starred"))
                    .and_then(json_bool)
            })
            .unwrap_or(false);

        Some(Self {
            id,
            msg_id,
            cli_msg_id,
            conversation_id,
            sender_id,
            sender_name,
            sender_avatar,
            timestamp,
            is_mine,
            status,
            kind,
            text,
            mentions,
            quote,
            reactions,
            media,
            file,
            sticker,
            poll,
            card,
            encrypted,
            recalled,
            starred,
            raw: val.clone(),
        })
    }
}

/// Phone -> PC message history sync state (`ZaloEngine+Transfer.swift`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TransferSlice {
    pub can_start: bool,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub synced_conversations: u32,
    #[serde(default)]
    pub total_conversations: Option<u32>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error_code: Option<i64>,
    #[serde(default)]
    pub error_name: Option<String>,
    #[serde(default)]
    pub can_retry: bool,
    #[serde(default)]
    pub raw: Value,
}

pub(crate) fn json_f64(val: &Value) -> Option<f64> {
    match val {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

pub(crate) fn json_bool(val: &Value) -> Option<bool> {
    match val {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|i| i != 0),
        _ => None,
    }
}

pub(crate) fn clamped_i64(val: f64) -> i64 {
    if !val.is_finite() {
        return 0;
    }
    val.clamp(-9e15, 9e15) as i64
}

pub(crate) fn clamped_u32(val: f64) -> u32 {
    if !val.is_finite() || val <= 0.0 {
        return 0;
    }
    val.min(f64::from(u32::MAX)) as u32
}

pub(crate) fn clamped_usize(val: f64) -> usize {
    if !val.is_finite() || val <= 0.0 {
        return 0;
    }
    val.min(9e15) as usize
}
