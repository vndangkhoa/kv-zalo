//! `ComposerDraft`, `FailedDraftStore`, and exact UTF-16 code-unit <-> UTF-8 byte index conversions
//! porting `ChatModels.swift` (`ComposerDraft`, `FailedDraftStore`) and `ZaloEngine+Chat.swift`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::models::{ChatMessage, Mention};

/// Returns the exact UTF-16 code-unit length of `s` (matching JavaScript `String.length` and `NSString.length`).
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Converts a UTF-16 code-unit offset into a valid UTF-8 byte index in `s`.
///
/// Returns `None` if `utf16_offset` exceeds `utf16_len(s)` or lands inside the middle of a surrogate pair.
pub fn utf16_offset_to_byte_idx(s: &str, utf16_offset: usize) -> Option<usize> {
    if utf16_offset == 0 {
        return Some(0);
    }
    let mut cur_utf16 = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if cur_utf16 == utf16_offset {
            return Some(byte_idx);
        }
        cur_utf16 += ch.len_utf16();
        if cur_utf16 > utf16_offset {
            // Landed on the low surrogate half of a supplementary-plane scalar.
            return None;
        }
    }
    if cur_utf16 == utf16_offset {
        Some(s.len())
    } else {
        None
    }
}

/// Converts a UTF-8 byte index into its UTF-16 code-unit offset in `s`.
///
/// Returns `None` if `byte_idx` is out of bounds or not on a UTF-8 character boundary.
pub fn byte_idx_to_utf16_offset(s: &str, byte_idx: usize) -> Option<usize> {
    if byte_idx > s.len() || !s.is_char_boundary(byte_idx) {
        return None;
    }
    Some(s[..byte_idx].encode_utf16().count())
}

/// Slices `s` using UTF-16 `(pos, len)` code-unit coordinates, returning `None` if out of bounds
/// or splitting a surrogate pair.
pub fn utf16_slice(s: &str, pos: usize, len: usize) -> Option<&str> {
    let start = utf16_offset_to_byte_idx(s, pos)?;
    let end_u16 = pos.checked_add(len)?;
    let end = utf16_offset_to_byte_idx(s, end_u16)?;
    Some(&s[start..end])
}

/// Composer draft holding unsent text, UTF-16 `@mention` spans, and an optional reply target.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ComposerDraft {
    pub text: String,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    #[serde(default)]
    pub reply_to: Option<Box<ChatMessage>>,
}

impl ComposerDraft {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mentions: Vec::new(),
            reply_to: None,
        }
    }

    pub fn with_mentions(text: impl Into<String>, mentions: Vec<Mention>) -> Self {
        Self {
            text: text.into(),
            mentions,
            reply_to: None,
        }
    }

    pub fn with_reply(mut self, reply_to: Option<ChatMessage>) -> Self {
        self.reply_to = reply_to.map(Box::new);
        self
    }

    /// Concatenates `other` one newline below `self` when both are non-empty, shifting `other.mentions`
    /// by the exact UTF-16 code-unit length of `self.text` + 1 (`'\n'`).
    /// Preserves `self.reply_to`, falling back to `other.reply_to` if `self` has none.
    pub fn append(&self, other: &ComposerDraft) -> ComposerDraft {
        if self.text.is_empty() {
            return ComposerDraft {
                text: other.text.clone(),
                mentions: other.mentions.clone(),
                reply_to: self.reply_to.clone().or_else(|| other.reply_to.clone()),
            };
        }
        if other.text.is_empty() {
            return self.clone();
        }

        let shift = utf16_len(&self.text) + 1;
        let mut merged_mentions = Vec::with_capacity(self.mentions.len() + other.mentions.len());
        merged_mentions.extend(self.mentions.iter().cloned());
        merged_mentions.extend(other.mentions.iter().map(|m| Mention {
            uid: m.uid.clone(),
            pos: m.pos + shift,
            len: m.len,
            is_all: m.is_all,
        }));

        ComposerDraft {
            text: format!("{}\n{}", self.text, other.text),
            mentions: merged_mentions,
            reply_to: self.reply_to.clone().or_else(|| other.reply_to.clone()),
        }
    }

    /// Alias for [`ComposerDraft::append`] matching Swift's `appending(_:)`.
    pub fn appending(&self, other: &ComposerDraft) -> ComposerDraft {
        self.append(other)
    }

    /// Trims leading and trailing whitespace like `ZaloEngine.sendText` (`ZaloEngine+Chat.swift:29-41`),
    /// rebasing valid `@mention` UTF-16 offsets and dropping stale/out-of-range mentions.
    pub fn trimmed_for_wire(&self) -> (String, Vec<Mention>) {
        let trimmed_start = self.text.trim_start();
        let leading_bytes = self.text.len() - trimmed_start.len();
        let leading_u16 = self.text[..leading_bytes].encode_utf16().count();

        let trimmed = trimmed_start.trim_end().to_string();
        let body_u16_len = utf16_len(&trimmed);

        let mut valid_mentions: Vec<Mention> = self
            .mentions
            .iter()
            .filter_map(|m| {
                let pos = m.pos.checked_sub(leading_u16)?;
                if m.len <= 1 || pos.checked_add(m.len)? > body_u16_len {
                    return None;
                }
                let span = utf16_slice(&trimmed, pos, m.len)?;
                if !span.starts_with('@') {
                    return None;
                }
                Some(Mention {
                    uid: m.uid.clone(),
                    pos,
                    len: m.len,
                    is_all: m.is_all,
                })
            })
            .collect();

        valid_mentions.sort_by_key(|m| m.pos);
        (trimmed, valid_mentions)
    }
}

/// Per-conversation store of drafts that failed to send (`ChatModels.swift:82-113`),
/// merging consecutive failures in order and tracking `silent` stashes + `revision`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FailedDraftStore {
    drafts: HashMap<String, ComposerDraft>,
    silent: HashSet<String>,
    revision: u64,
}

impl FailedDraftStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn has(&self, conv_id: &str) -> bool {
        self.drafts.contains_key(conv_id)
    }

    /// Stashes an unsent draft for `conv_id` (non-silent). Empty drafts are ignored.
    pub fn stash(&mut self, conv_id: &str, draft: ComposerDraft) {
        self.stash_with_silence(conv_id, draft, false);
    }

    /// Stashes an unsent draft for `conv_id` with explicit `is_silent` notice flag.
    /// Merging a non-silent failure onto an earlier silent stash upgrades it to non-silent.
    pub fn stash_with_silence(&mut self, conv_id: &str, draft: ComposerDraft, is_silent: bool) {
        if draft.text.is_empty() || conv_id.is_empty() {
            return;
        }
        if !self.drafts.contains_key(conv_id) {
            if is_silent {
                self.silent.insert(conv_id.to_string());
            }
        } else if !is_silent {
            self.silent.remove(conv_id);
        }

        let merged = match self.drafts.get(conv_id) {
            Some(existing) => existing.append(&draft),
            None => draft,
        };
        self.drafts.insert(conv_id.to_string(), merged);
        self.revision += 1;
    }

    /// Takes the held draft for `conv_id`, removing it from the store.
    pub fn take(&mut self, conv_id: &str) -> Option<ComposerDraft> {
        self.take_with_notice(conv_id).map(|(draft, _)| draft)
    }

    /// Takes the held draft and returns `(draft, show_not_sent_notice)`.
    pub fn take_with_notice(&mut self, conv_id: &str) -> Option<(ComposerDraft, bool)> {
        let was_silent = self.silent.remove(conv_id);
        self.drafts
            .remove(conv_id)
            .map(|draft| (draft, !was_silent))
    }

    /// Drains all held drafts across conversations, leaving the store empty.
    pub fn drain(&mut self) -> Vec<(String, ComposerDraft)> {
        self.silent.clear();
        self.drafts.drain().collect()
    }
}
