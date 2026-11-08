//! `ZaloEngineState`, `zstate` patch reducer, and Vietnamese diacritic folding.

pub mod chat_merge;
pub mod drafts;
pub mod models;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;

pub use chat_merge::{merge_chat, ChatMergeOutcome};
pub use drafts::{
    byte_idx_to_utf16_offset, utf16_len, utf16_offset_to_byte_idx, utf16_slice, ComposerDraft,
    FailedDraftStore,
};
pub use models::{
    conv_action_flags, CardAttachment, ChatMessage, ConvActionsState, Conversation,
    ConversationExtras, ConversationLabel, ConversationPreview, EnginePhase, FileCard,
    MediaAttachment, Mention, MessageQuote, MessageReaction, MessageStatus, PollAttachment,
    StickerAttachment, StrangerBox, TransferSlice, UserProfile, WaitingConfirm,
    DEFAULT_KICKOUT_NOTICE,
};

use models::{clamped_i64, clamped_u32, json_bool, json_f64};

/// Folds Vietnamese text for accent-insensitive QuickSwitcher & contact/conversation search:
/// converts `đ`/`Đ` -> `'d'`, NFD-decomposes, strips Unicode combining marks (`\u{0300}..=\u{036f}`),
/// and lowercases.
pub fn fold_vietnamese(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'đ' | 'Đ' => out.push('d'),
            other => {
                for decomposed in other.nfd() {
                    if !('\u{0300}'..='\u{036f}').contains(&decomposed) {
                        for lower in decomposed.to_lowercase() {
                            if lower == 'đ' {
                                out.push('d');
                            } else {
                                out.push(lower);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Complete engine state mirroring `ZaloEngine*.swift` across all `zstate` adapter slices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZaloEngineState {
    pub phase: EnginePhase,
    pub user: Option<UserProfile>,
    pub self_conversation_id: Option<String>,
    pub open_conversation_id: Option<String>,
    pub conversations: Vec<Conversation>,
    pub conv_extras: ConversationExtras,
    pub conv_actions: ConvActionsState,
    pub messages: HashMap<String, Vec<ChatMessage>>,
    pub has_more_history: HashMap<String, bool>,
    pub history_ticks: HashMap<String, u64>,
    pub typing: HashMap<String, String>,
    pub failed_drafts: FailedDraftStore,
    pub contacts: Value,
    pub cloud: Value,
    pub transfer: TransferSlice,
    pub polls: HashMap<String, Value>,
    pub stickers: Value,
    pub pins: HashMap<String, Value>,
    pub notifications: Value,
}

impl Default for ZaloEngineState {
    fn default() -> Self {
        Self::new()
    }
}

impl ZaloEngineState {
    pub fn new() -> Self {
        Self {
            phase: EnginePhase::Booting,
            user: None,
            self_conversation_id: None,
            open_conversation_id: None,
            conversations: Vec::new(),
            conv_extras: ConversationExtras::default(),
            conv_actions: ConvActionsState::default(),
            messages: HashMap::new(),
            has_more_history: HashMap::new(),
            history_ticks: HashMap::new(),
            typing: HashMap::new(),
            failed_drafts: FailedDraftStore::new(),
            contacts: Value::Object(Map::new()),
            cloud: Value::Object(Map::new()),
            transfer: TransferSlice::default(),
            polls: HashMap::new(),
            stickers: Value::Object(Map::new()),
            pins: HashMap::new(),
            notifications: Value::Object(Map::new()),
        }
    }

    /// Conversations safe to show in the sidebar: empty until `hidden_known` is true (fail-closed),
    /// excluding hidden conversations unless currently unlocked (`ZaloEngine+ConversationActions.swift:409`).
    pub fn visible_conversations(&self) -> Vec<&Conversation> {
        if !self.conv_actions.hidden_known {
            return Vec::new();
        }
        self.conversations
            .iter()
            .filter(|c| !self.conv_actions.is_locked(&c.id))
            .collect()
    }

    /// Total unread message count across visible (non-locked) conversations.
    pub fn total_unread(&self) -> u64 {
        self.visible_conversations()
            .into_iter()
            .map(|c| u64::from(c.unread))
            .sum()
    }

    /// Searches visible conversations using accent-insensitive Vietnamese folding.
    pub fn search_conversations(&self, query: &str) -> Vec<&Conversation> {
        let q = fold_vietnamese(query.trim());
        let visible = self.visible_conversations();
        if q.is_empty() {
            return visible;
        }
        visible
            .into_iter()
            .filter(|c| fold_vietnamese(c.display_name()).contains(&q))
            .collect()
    }

    /// Clears all account-scoped slices upon logout or server kickout.
    pub fn reset_account_state(&mut self, kickout_notice: Option<String>) {
        self.user = None;
        self.self_conversation_id = None;
        self.open_conversation_id = None;
        self.conversations.clear();
        self.conv_extras = ConversationExtras::default();
        self.conv_actions = ConvActionsState::default();
        self.messages.clear();
        self.has_more_history.clear();
        self.history_ticks.clear();
        self.typing.clear();
        self.failed_drafts = FailedDraftStore::new();
        self.contacts = Value::Object(Map::new());
        self.cloud = Value::Object(Map::new());
        self.transfer = TransferSlice::default();
        self.polls.clear();
        self.stickers = Value::Object(Map::new());
        self.pins.clear();
        self.notifications = Value::Object(Map::new());
        self.phase = EnginePhase::Login {
            qr: None,
            qr_expired: false,
            waiting_confirm: None,
            kickout_notice,
            error_notice: None,
        };
    }

    /// Applies an incoming `zstate` JSON patch from the renderer adapters or host process,
    /// returning the ordered list of slice keys that actually mutated:
    /// `"phase"`, `"user"`, `"conversations"`, `"conv_extras"`, `"conv_actions"`,
    /// `"chat:<id>"`, `"typing"`, `"contacts"`, `"cloud"`, `"transfer"`,
    /// `"polls"`, `"stickers"`, `"pins"`, `"notifications"`.
    pub fn apply_zstate_patch(&mut self, payload: &Value) -> Vec<String> {
        let Some(obj) = payload.as_object() else {
            return Vec::new();
        };

        let mut changed: Vec<String> = Vec::new();
        let mut push_slice = |name: String| {
            if !changed.contains(&name) {
                changed.push(name);
            }
        };

        // 1. Phase transitions (`page: "login"`, explicit `phase`, `runtimeSetup`, `kickout`)
        if self.apply_phase_slice(obj) {
            push_slice("phase".to_string());
        }

        // 2. Signed-in user profile (`user` or `profile`)
        if let Some(u_val) = obj.get("user").or_else(|| obj.get("profile")) {
            let next_user = if u_val.is_null() {
                None
            } else {
                UserProfile::from_json(u_val).or_else(|| self.user.clone())
            };
            if next_user != self.user {
                self.user = next_user;
                push_slice("user".to_string());
            }
        }

        // 3. Conversations list (`conversations`)
        if let Some(rows) = obj.get("conversations").and_then(|v| v.as_array()) {
            let mut next_convs: Vec<Conversation> =
                rows.iter().filter_map(Conversation::from_json).collect();
            Conversation::sort_list(&mut next_convs);
            if next_convs != self.conversations {
                self.conversations = next_convs;
                push_slice("conversations".to_string());
            }
        }

        // 4. Conversation extras (`conversationExtras` or `conv_extras`)
        if let Some(extras_val) = obj
            .get("conversationExtras")
            .or_else(|| obj.get("conv_extras"))
        {
            let mut next = self.conv_extras.clone();
            next.apply_patch(extras_val);
            if let Some(sid) = &next.self_conversation_id {
                self.self_conversation_id = Some(sid.clone());
            }
            if next != self.conv_extras {
                self.conv_extras = next;
                push_slice("conv_extras".to_string());
            }
        }

        // 5. Conversation actions & hidden-chat lock (`convActions` or `conv_actions`)
        if let Some(ca_val) = obj.get("convActions").or_else(|| obj.get("conv_actions")) {
            let mut next = self.conv_actions.clone();
            next.apply_patch(ca_val);
            if next != self.conv_actions {
                self.conv_actions = next;
                push_slice("conv_actions".to_string());
            }
        }

        // 6. Chat history & deltas (`chat` -> `"chat:<id>"`)
        if let Some(chat_val) = obj.get("chat") {
            if let Some(conv_id) = chat_val
                .get("convId")
                .or_else(|| chat_val.get("conv_id"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let outcome = merge_chat(
                    conv_id,
                    chat_val,
                    &mut self.messages,
                    &mut self.has_more_history,
                    &mut self.history_ticks,
                );
                if outcome.any_changed() {
                    push_slice(format!("chat:{conv_id}"));
                }
                // Clear typing indicator if a fresh message from the peer arrived.
                if self.typing.contains_key(conv_id) {
                    if let Some(last) = self.messages.get(conv_id).and_then(|m| m.last()) {
                        if !last.is_mine && outcome.messages_changed {
                            self.typing.remove(conv_id);
                            push_slice("typing".to_string());
                        }
                    }
                }
            }
        }

        // 7. Typing indicator (`typing`)
        if let Some(typing_val) = obj.get("typing") {
            if self.apply_typing_slice(typing_val) {
                push_slice("typing".to_string());
            }
        }

        // 8. Contacts (`contacts`)
        if let Some(contacts_val) = obj.get("contacts") {
            if merge_json_section(&mut self.contacts, contacts_val) {
                push_slice("contacts".to_string());
            }
        }

        // 9. zCloud (`zcloud` or `cloud`)
        if let Some(cloud_val) = obj.get("zcloud").or_else(|| obj.get("cloud")) {
            if let Some(sid) = cloud_val
                .get("selfId")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                self.self_conversation_id = Some(sid.to_string());
            }
            if merge_json_section(&mut self.cloud, cloud_val) {
                push_slice("cloud".to_string());
            }
        }

        // 10. Phone -> PC Transfer (`transfer` and/or `transferAvailable`)
        if obj.contains_key("transfer") || obj.contains_key("transferAvailable") {
            if self.apply_transfer_slice(obj) {
                push_slice("transfer".to_string());
            }
        }

        // 11. Group polls (`poll` or `polls`)
        if let Some(poll_val) = obj.get("poll").or_else(|| obj.get("polls")) {
            if self.apply_polls_slice(poll_val) {
                push_slice("polls".to_string());
            }
        }

        // 12. Stickers (`sticker` or `stickers`)
        if let Some(sticker_val) = obj.get("sticker").or_else(|| obj.get("stickers")) {
            if merge_json_section(&mut self.stickers, sticker_val) {
                push_slice("stickers".to_string());
            }
        }

        // 13. Pinned messages (`pins`)
        if let Some(pins_val) = obj.get("pins") {
            if self.apply_pins_slice(pins_val) {
                push_slice("pins".to_string());
            }
        }

        // 14. Notification settings & events (`notification` or `notifications`)
        if let Some(notif_val) = obj
            .get("notification")
            .or_else(|| obj.get("notifications"))
        {
            if merge_json_section(&mut self.notifications, notif_val) {
                push_slice("notifications".to_string());
            }
        }

        changed
    }

    fn apply_phase_slice(&mut self, obj: &Map<String, Value>) -> bool {
        let prev = self.phase.clone();

        if obj.get("kickout").and_then(json_bool) == Some(true) {
            let notice = obj
                .get("kickoutNotice")
                .or_else(|| obj.get("kickout_notice"))
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_KICKOUT_NOTICE)
                .to_string();
            self.phase = EnginePhase::Login {
                qr: None,
                qr_expired: false,
                waiting_confirm: None,
                kickout_notice: Some(notice),
                error_notice: None,
            };
            return self.phase != prev;
        }

        if let Some(phase_val) = obj.get("phase") {
            match phase_val {
                Value::String(s) => match s.as_str() {
                    "booting" | "starting" => self.phase = EnginePhase::Booting,
                    "ready" => self.phase = EnginePhase::Ready,
                    "login" => {
                        if !matches!(self.phase, EnginePhase::Login { .. }) {
                            self.phase = EnginePhase::Login {
                                qr: None,
                                qr_expired: false,
                                waiting_confirm: None,
                                kickout_notice: None,
                                error_notice: None,
                            };
                        }
                    }
                    "installing_runtime" => {
                        self.phase = EnginePhase::InstallingRuntime {
                            step: obj
                                .get("step")
                                .and_then(|v| v.as_str())
                                .unwrap_or("preparing")
                                .to_string(),
                            fraction: obj
                                .get("fraction")
                                .and_then(json_f64)
                                .unwrap_or(0.0)
                                .clamp(0.0, 1.0),
                            detail: obj
                                .get("detail")
                                .and_then(|v| v.as_str())
                                .map(ToString::to_string),
                            error: obj
                                .get("error")
                                .and_then(|v| v.as_str())
                                .map(ToString::to_string),
                        };
                    }
                    _ => {}
                },
                Value::Object(_) => {
                    if let Ok(parsed) = serde_json::from_value::<EnginePhase>(phase_val.clone()) {
                        self.phase = parsed;
                    }
                }
                _ => {}
            }
        }

        if let Some(rs) = obj
            .get("runtimeSetup")
            .or_else(|| obj.get("installingRuntime"))
            .and_then(|v| v.as_object())
        {
            self.phase = EnginePhase::InstallingRuntime {
                step: rs
                    .get("step")
                    .and_then(|v| v.as_str())
                    .unwrap_or("installing")
                    .to_string(),
                fraction: rs
                    .get("fraction")
                    .and_then(json_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                detail: rs
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                error: rs
                    .get("error")
                    .or_else(|| rs.get("failure"))
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
            };
        }

        if obj.get("page").and_then(|v| v.as_str()) == Some("login") {
            let (
                mut qr,
                mut qr_expired,
                mut waiting_confirm,
                mut kickout_notice,
                mut error_notice,
            ) = match &self.phase {
                EnginePhase::Login {
                    qr,
                    qr_expired,
                    waiting_confirm,
                    kickout_notice,
                    error_notice,
                } => (
                    qr.clone(),
                    *qr_expired,
                    waiting_confirm.clone(),
                    kickout_notice.clone(),
                    error_notice.clone(),
                ),
                _ => (None, false, None, None, None),
            };
            if let Some(new_qr) = obj
                .get("qr")
                .and_then(|v| v.as_str())
                .filter(|s| is_valid_login_qr_data(s))
            {
                if qr.as_deref() != Some(new_qr) {
                    qr = Some(new_qr.to_string());
                    qr_expired = false;
                    waiting_confirm = None;
                    error_notice = None;
                }
            }
            if let Some(inv) = obj
                .get("invalid")
                .or_else(|| obj.get("qrExpired"))
                .or_else(|| obj.get("qrInvalid"))
                .and_then(json_bool)
            {
                if inv {
                    qr_expired = true;
                }
            }
            if let Some(wc) = obj
                .get("waitingConfirm")
                .or_else(|| obj.get("waiting_confirm"))
                .and_then(|v| v.as_object())
            {
                if let Some(name) = wc
                    .get("name")
                    .or_else(|| wc.get("display_name"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    waiting_confirm = Some(WaitingConfirm {
                        name: name.to_string(),
                        avatar: wc
                            .get("avatar")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(ToString::to_string),
                    });
                }
            }
            if let Some(kn) = obj
                .get("kickoutNotice")
                .or_else(|| obj.get("kickout_notice"))
                .and_then(|v| v.as_str())
            {
                kickout_notice = if kn.is_empty() {
                    None
                } else {
                    Some(kn.to_string())
                };
            }
            if let Some(en) = obj
                .get("errorNotice")
                .or_else(|| obj.get("error_notice"))
                .or_else(|| obj.get("error"))
                .and_then(|v| v.as_str())
            {
                error_notice = if en.is_empty() {
                    None
                } else {
                    Some(en.to_string())
                };
            }
            self.phase = EnginePhase::Login {
                qr,
                qr_expired,
                waiting_confirm,
                kickout_notice,
                error_notice,
            };
        } else if obj.get("page").and_then(|v| v.as_str()) == Some("main")
            && matches!(self.phase, EnginePhase::Booting | EnginePhase::Login { .. })
            && (obj.contains_key("user") || obj.contains_key("conversations"))
        {
            self.phase = EnginePhase::Ready;
        }

        self.phase != prev
    }

    fn apply_typing_slice(&mut self, typing_val: &Value) -> bool {
        let Some(tobj) = typing_val.as_object() else {
            return false;
        };
        if let Some(conv_id) = tobj
            .get("convId")
            .or_else(|| tobj.get("conv_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let name = tobj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if name.is_empty() {
                return self.typing.remove(conv_id).is_some();
            }
            if self.typing.get(conv_id).map(String::as_str) != Some(name) {
                self.typing.insert(conv_id.to_string(), name.to_string());
                return true;
            }
            return false;
        }

        // Map shape `{"conv1": "An", "conv2": ""}`
        let mut mutated = false;
        for (cid, v) in tobj {
            let name = v.as_str().unwrap_or("").trim();
            if name.is_empty() {
                if self.typing.remove(cid).is_some() {
                    mutated = true;
                }
            } else if self.typing.get(cid).map(String::as_str) != Some(name) {
                self.typing.insert(cid.clone(), name.to_string());
                mutated = true;
            }
        }
        mutated
    }

    fn apply_transfer_slice(&mut self, obj: &Map<String, Value>) -> bool {
        let mut next = self.transfer.clone();

        if let Some(avail) = obj.get("transferAvailable").and_then(|v| v.as_object()) {
            next.can_start = avail.get("canStart").and_then(json_bool).unwrap_or(false);
            next.blocked_reason = avail
                .get("reason")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
        }

        if let Some(t_val) = obj.get("transfer") {
            if t_val.is_null() {
                next.phase = None;
                next.progress = None;
                next.synced_conversations = 0;
                next.total_conversations = None;
                next.message = None;
                next.error_code = None;
                next.error_name = None;
                next.can_retry = false;
                next.raw = Value::Null;
            } else if let Some(tobj) = t_val.as_object() {
                next.phase = tobj
                    .get("phase")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
                next.progress = tobj
                    .get("progress")
                    .and_then(json_f64)
                    .map(|p| p.clamp(0.0, 1.0));
                next.synced_conversations = tobj
                    .get("syncedConversations")
                    .or_else(|| tobj.get("synced_conversations"))
                    .and_then(json_f64)
                    .map(clamped_u32)
                    .unwrap_or(0);
                next.total_conversations = tobj
                    .get("totalConversations")
                    .or_else(|| tobj.get("total_conversations"))
                    .and_then(json_f64)
                    .map(clamped_u32);
                next.message = tobj
                    .get("message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
                next.error_code = tobj
                    .get("errorCode")
                    .or_else(|| tobj.get("error_code"))
                    .and_then(json_f64)
                    .map(clamped_i64);
                next.error_name = tobj
                    .get("errorName")
                    .or_else(|| tobj.get("error_name"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
                next.can_retry = tobj
                    .get("canRetry")
                    .or_else(|| tobj.get("can_retry"))
                    .and_then(json_bool)
                    .unwrap_or(false);
                next.raw = t_val.clone();
            }
        }

        if next != self.transfer {
            self.transfer = next;
            true
        } else {
            false
        }
    }

    fn apply_polls_slice(&mut self, poll_val: &Value) -> bool {
        let Some(pobj) = poll_val.as_object() else {
            return false;
        };
        if let Some(id) = pobj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let entry_val = pobj.get("detail").cloned().unwrap_or_else(|| poll_val.clone());
            if self.polls.get(id) != Some(&entry_val) {
                self.polls.insert(id.to_string(), entry_val);
                return true;
            }
            return false;
        }

        let mut mutated = false;
        for (pid, v) in pobj {
            if self.polls.get(pid) != Some(v) {
                self.polls.insert(pid.clone(), v.clone());
                mutated = true;
            }
        }
        mutated
    }

    fn apply_pins_slice(&mut self, pins_val: &Value) -> bool {
        let Some(pobj) = pins_val.as_object() else {
            return false;
        };
        if let Some(conv_id) = pobj
            .get("convId")
            .or_else(|| pobj.get("conv_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if self.pins.get(conv_id) != Some(pins_val) {
                self.pins.insert(conv_id.to_string(), pins_val.clone());
                return true;
            }
            return false;
        }

        let mut mutated = false;
        for (cid, v) in pobj {
            if self.pins.get(cid) != Some(v) {
                self.pins.insert(cid.clone(), v.clone());
                mutated = true;
            }
        }
        mutated
    }
}

fn is_valid_login_qr_data(new_qr: &str) -> bool {
    !new_qr.is_empty()
        && (new_qr.starts_with("data:image/png")
            || new_qr.starts_with("data:image/jpeg")
            || new_qr.starts_with("data:image/webp")
            || (!new_qr.starts_with("data:image/svg+xml") && new_qr.len() > 2048))
}

fn merge_json_section(target: &mut Value, incoming: &Value) -> bool {
    if let (Some(dst), Some(src)) = (target.as_object_mut(), incoming.as_object()) {
        let mut mutated = false;
        for (k, v) in src {
            if dst.get(k) != Some(v) {
                dst.insert(k.clone(), v.clone());
                mutated = true;
            }
        }
        mutated
    } else if target != incoming {
        *target = incoming.clone();
        true
    } else {
        false
    }
}
