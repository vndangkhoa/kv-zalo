//! Chat history delta & reset merging (`ZaloEngine+Chat.swift:105-135`), preserving
//! `cliMsgId` stable identity across optimistic -> confirmed transitions and chronological order.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::models::{json_bool, ChatMessage};

/// Outcome of merging a `chat` state update for a single conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatMergeOutcome {
    pub messages_changed: bool,
    pub has_more_changed: bool,
    pub tick_incremented: bool,
}

impl ChatMergeOutcome {
    pub fn any_changed(self) -> bool {
        self.messages_changed || self.has_more_changed || self.tick_incremented
    }
}

/// Merges a `chat` payload (`reset`, `messages` / `changed`, `removed`, `hasMore`, `grew`)
/// into `messages_by_conv`, `has_more_by_conv`, and `history_ticks`.
pub fn merge_chat(
    conv_id: &str,
    chat_payload: &Value,
    messages_by_conv: &mut HashMap<String, Vec<ChatMessage>>,
    has_more_by_conv: &mut HashMap<String, bool>,
    history_ticks: &mut HashMap<String, u64>,
) -> ChatMergeOutcome {
    let Some(obj) = chat_payload.as_object() else {
        return ChatMergeOutcome {
            messages_changed: false,
            has_more_changed: false,
            tick_incremented: false,
        };
    };

    let reset = obj.get("reset").and_then(json_bool).unwrap_or(false);

    // Accept both `"messages"` (`chat-adapter.js`) and `"changed"` delta array keys.
    let mut incoming: Vec<ChatMessage> = Vec::new();
    for key in ["messages", "changed"] {
        if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
            incoming.extend(
                arr.iter()
                    .filter_map(|item| ChatMessage::from_json(item, conv_id)),
            );
        }
    }

    let removed: HashSet<String> = obj
        .get("removed")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| match v {
                    Value::String(s) if !s.is_empty() => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let prev_list = messages_by_conv.get(conv_id).cloned().unwrap_or_default();
    let mut list: Vec<ChatMessage> = if reset {
        Vec::with_capacity(incoming.len())
    } else {
        prev_list.clone()
    };

    if !removed.is_empty() {
        list.retain(|m| !matches_removed(m, &removed));
    }

    let mut needs_sort = reset;

    for mut msg in incoming {
        if let Some(at) = find_existing_message_index(&list, &msg) {
            let existing = &list[at];
            // Preserve stable `cliMsgId`-anchored `id` when an optimistic message is confirmed by server.
            if existing.cli_msg_id.is_some()
                && existing.cli_msg_id == msg.cli_msg_id
                && existing.id == *existing.cli_msg_id.as_ref().unwrap()
            {
                msg.id = existing.id.clone();
            }
            if msg.cli_msg_id.is_none() {
                msg.cli_msg_id = existing.cli_msg_id.clone();
            }
            if msg.msg_id.is_none() {
                msg.msg_id = existing.msg_id.clone();
            }
            if existing.timestamp != msg.timestamp {
                needs_sort = true;
            }
            list[at] = msg;
        } else {
            // Normalize own pending/confirmed message id to `cli_msg_id` when available for stable UI keys.
            if msg.is_mine {
                if let Some(cli) = &msg.cli_msg_id {
                    if !cli.is_empty() {
                        msg.id = cli.clone();
                    }
                }
            }
            if let Some(last) = list.last() {
                if msg.timestamp < last.timestamp {
                    needs_sort = true;
                }
            }
            list.push(msg);
        }
    }

    if needs_sort {
        // Rust's `sort_by_key` is guaranteed to be a stable merge/drift sort:
        // equal timestamps keep their exact relative arrival order.
        list.sort_by_key(|m| m.timestamp);
    }

    let messages_changed = if !messages_by_conv.contains_key(conv_id) {
        !list.is_empty() || reset
    } else {
        prev_list != list
    };
    if messages_changed || !messages_by_conv.contains_key(conv_id) {
        messages_by_conv.insert(conv_id.to_string(), list);
    }

    let mut has_more_changed = false;
    if let Some(has_more) = obj
        .get("hasMore")
        .or_else(|| obj.get("has_more"))
        .and_then(json_bool)
    {
        if has_more_by_conv.get(conv_id).copied() != Some(has_more) {
            has_more_by_conv.insert(conv_id.to_string(), has_more);
            has_more_changed = true;
        }
    }

    let mut tick_incremented = false;
    if obj.get("grew").and_then(json_bool) == Some(true) {
        *history_ticks.entry(conv_id.to_string()).or_insert(0) += 1;
        tick_incremented = true;
    }

    ChatMergeOutcome {
        messages_changed,
        has_more_changed,
        tick_incremented,
    }
}

fn matches_removed(m: &ChatMessage, removed: &HashSet<String>) -> bool {
    if removed.contains(&m.id) {
        return true;
    }
    if let Some(mid) = &m.msg_id {
        if removed.contains(mid) {
            return true;
        }
    }
    if let Some(cid) = &m.cli_msg_id {
        if removed.contains(cid) {
            return true;
        }
    }
    false
}

fn find_existing_message_index(list: &[ChatMessage], incoming: &ChatMessage) -> Option<usize> {
    list.iter().position(|existing| {
        if existing.id == incoming.id {
            return true;
        }
        if let Some(in_cli) = incoming.cli_msg_id.as_deref().filter(|s| !s.is_empty()) {
            if existing.id == in_cli || existing.cli_msg_id.as_deref() == Some(in_cli) {
                return true;
            }
        }
        if let Some(in_msg) = incoming.msg_id.as_deref().filter(|s| !s.is_empty()) {
            if existing.id == in_msg || existing.msg_id.as_deref() == Some(in_msg) {
                return true;
            }
        }
        false
    })
}
