//! Persistent Message Scheduler (`ScheduledMessage` / `SchedulerEngine`) and
//! Keyword / Away Auto-Reply Engine (`AutoReplyRule` / `AutoReplyEngine`).
//!
//! Adapted from `ctdozlite/src/scheduler.rs` and `ctdozlite/src/api_mcp.rs` with:
//! - Atomic JSON persistence (`scheduled_messages.json` and `autoreply_rules.json`).
//! - Quick relative & local civil-time schedule parser (`"+15m"`, `"+1h"`, `"18:00"`, `"YYYY-MM-DD HH:MM"`).
//! - Startup overdue catch-up (`due_messages(now_ms)` includes any `Pending` message with `send_at_ms <= now_ms`).
//! - Accent-insensitive Vietnamese auto-reply matching via [`crate::fold_vietnamese`].
//! - Self-message ignore guard (`is_mine` / `sender_id == self_user_id`) and per-`(rule_id, conv_id)` cooldown map.
//! - Background Tokio runner [`spawn_automation_loop`] driven by `tokio::time::interval`.

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::task::AbortHandle;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::engine::ChatMessage;
use crate::fold_vietnamese;
use crate::privacy::write_atomic_private;

/// Filename of persisted scheduled messages inside `<data_dir>` / `<config_dir>`.
pub const SCHEDULED_MESSAGES_FILENAME: &str = "scheduled_messages.json";

/// Filename of persisted auto-reply rules inside `<data_dir>` / `<config_dir>`.
pub const AUTOREPLY_RULES_FILENAME: &str = "autoreply_rules.json";

/// Default tick cadence for [`spawn_automation_loop`] (`1s`).
pub const DEFAULT_AUTOMATION_TICK: Duration = Duration::from_secs(1);

/// Returns current wall-clock epoch time in milliseconds.
pub fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Lifecycle state of a [`ScheduledMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStatus {
    Pending,
    Sent,
    Failed,
    Cancelled,
}

/// Persisted scheduled outbound message record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledMessage {
    pub id: String,
    pub conv_id: String,
    #[serde(default)]
    pub target_name: Option<String>,
    pub text: String,
    pub send_at_ms: i64,
    pub status: ScheduleStatus,
    pub created_at_ms: i64,
    #[serde(default)]
    pub sent_at_ms: Option<i64>,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Parses a schedule expression (`"+15m"`, `"+1h"`, `"+30s"`, `"18:00"`, `"YYYY-MM-DD HH:MM"`,
/// `"DD/MM/YYYY HH:MM"`, or raw future epoch timestamp) into target epoch milliseconds.
pub fn parse_schedule_time_ms(input: &str, now_ms: i64) -> Result<i64> {
    let s = input.trim();
    if s.is_empty() {
        bail!("schedule time expression must not be empty (examples: +15m, +1h, 18:00)");
    }

    // 1. Direct numeric epoch ms or epoch seconds
    if s.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(raw) = s.parse::<i64>() {
            let ts_ms = if raw >= 1_000_000_000_000 {
                raw
            } else {
                raw.saturating_mul(1000)
            };
            if ts_ms <= now_ms {
                bail!("scheduled timestamp must be in the future");
            }
            return Ok(ts_ms);
        }
    }

    // 2. Relative offsets (`+15m`, `+1h`, `+30s`, `+2d`, or unprefixed `15m`, `2h`)
    if let Some(rel) = s.strip_prefix('+') {
        return parse_relative_offset_ms(rel.trim(), now_ms);
    }
    if s.starts_with('-') {
        bail!("scheduled relative offset must be positive");
    }
    if let Ok(rel_ms) = parse_relative_offset_ms(s, now_ms) {
        return Ok(rel_ms);
    }

    // 3. Full local date-time (`YYYY-MM-DD HH:MM[:SS]` or `DD/MM/YYYY HH:MM[:SS]`)
    if let Some((date_part, time_part)) = s.split_once(' ') {
        let (hour, min, sec) = parse_hms(time_part.trim())?;
        if let Some((year, month, day)) = parse_date_ymd_or_dmy(date_part.trim())? {
            let ts_ms = local_ymd_hms_to_ms(year, month, day, hour, min, sec)?;
            if ts_ms <= now_ms {
                bail!("scheduled date-time must be in the future");
            }
            return Ok(ts_ms);
        }
    }

    // 4. Local clock time `HH:MM` or `HH:MM:SS` -> today if still in future, otherwise tomorrow
    if let Ok((hour, min, sec)) = parse_hms(s) {
        let (year, month, day) = local_civil_date(now_ms)?;
        let today_ms = local_ymd_hms_to_ms(year, month, day, hour, min, sec)?;
        if today_ms > now_ms {
            return Ok(today_ms);
        }
        let tomorrow_ms = local_ymd_hms_to_ms(year, month, day + 1, hour, min, sec)?;
        return Ok(tomorrow_ms);
    }

    bail!(
        "invalid schedule format `{s}` (supported: +15m, +1h, HH:MM, YYYY-MM-DD HH:MM, DD/MM/YYYY HH:MM)"
    )
}

fn parse_relative_offset_ms(rel: &str, now_ms: i64) -> Result<i64> {
    let r = rel.trim();
    let (num_str, unit_ms): (&str, i64) = if let Some(v) = r.strip_suffix("ms") {
        (v, 1)
    } else if let Some(v) = r.strip_suffix('s') {
        (v, 1_000)
    } else if let Some(v) = r
        .strip_suffix("ph")
        .or_else(|| r.strip_suffix('m'))
        .or_else(|| r.strip_suffix('p'))
    {
        (v, 60_000)
    } else if let Some(v) = r.strip_suffix('h').or_else(|| r.strip_suffix('g')) {
        (v, 3_600_000)
    } else if let Some(v) = r.strip_suffix('d') {
        (v, 86_400_000)
    } else {
        bail!("not a relative duration")
    };

    let n: i64 = num_str
        .trim()
        .parse()
        .context("invalid numeric offset in relative schedule")?;
    if n <= 0 {
        bail!("relative schedule offset must be > 0");
    }
    let delta = n
        .checked_mul(unit_ms)
        .context("relative schedule offset overflow")?;
    now_ms
        .checked_add(delta)
        .context("scheduled timestamp overflow")
}

fn parse_hms(raw: &str) -> Result<(i32, i32, i32)> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        bail!("invalid time `{raw}`");
    }
    let h: i32 = parts[0].trim().parse().context("invalid hour")?;
    let m: i32 = parts[1].trim().parse().context("invalid minute")?;
    let s: i32 = if parts.len() == 3 {
        parts[2].trim().parse().context("invalid second")?
    } else {
        0
    };
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) || !(0..=59).contains(&s) {
        bail!("clock time `{raw}` out of range");
    }
    Ok((h, m, s))
}

fn parse_date_ymd_or_dmy(raw: &str) -> Result<Option<(i32, i32, i32)>> {
    if raw.contains('-') {
        let parts: Vec<&str> = raw.split('-').collect();
        if parts.len() == 3 {
            let p0: i32 = parts[0].trim().parse().context("invalid date year/day")?;
            let p1: i32 = parts[1].trim().parse().context("invalid date month")?;
            let p2: i32 = parts[2].trim().parse().context("invalid date day/year")?;
            let (y, m, d) = if parts[0].trim().len() == 4 {
                (p0, p1, p2)
            } else {
                (p2, p1, p0)
            };
            validate_ymd(y, m, d)?;
            return Ok(Some((y, m, d)));
        }
    }
    if raw.contains('/') {
        let parts: Vec<&str> = raw.split('/').collect();
        if parts.len() == 3 {
            let d: i32 = parts[0].trim().parse().context("invalid day")?;
            let m: i32 = parts[1].trim().parse().context("invalid month")?;
            let y: i32 = parts[2].trim().parse().context("invalid year")?;
            validate_ymd(y, m, d)?;
            return Ok(Some((y, m, d)));
        }
    }
    Ok(None)
}

fn validate_ymd(year: i32, month: i32, day: i32) -> Result<()> {
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        bail!("calendar date {year:04}-{month:02}-{day:02} out of range");
    }
    Ok(())
}

fn local_civil_date(now_ms: i64) -> Result<(i32, i32, i32)> {
    let sec = (now_ms.div_euclid(1000)) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ptr = unsafe { libc::localtime_r(&sec, &mut tm) };
    if ptr.is_null() {
        bail!("localtime_r failed for timestamp {now_ms}");
    }
    Ok((tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday))
}

fn local_ymd_hms_to_ms(
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    min: i32,
    sec: i32,
) -> Result<i64> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = min;
    tm.tm_sec = sec;
    tm.tm_isdst = -1;
    let epoch_sec = unsafe { libc::mktime(&mut tm) };
    if epoch_sec == -1 {
        bail!("mktime rejected local date-time {year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}");
    }
    Ok(i64::from(epoch_sec).saturating_mul(1000))
}

/// Persistent thread-safe scheduled message queue backed by `<dir>/scheduled_messages.json`.
#[derive(Debug)]
pub struct SchedulerEngine {
    file_path: PathBuf,
    items: Mutex<Vec<ScheduledMessage>>,
}

impl SchedulerEngine {
    /// Returns `<dir>/scheduled_messages.json`.
    pub fn file_path_in(dir: &Path) -> PathBuf {
        dir.join(SCHEDULED_MESSAGES_FILENAME)
    }

    /// Loads existing scheduled messages from `<dir>/scheduled_messages.json`.
    ///
    /// Any `Pending` messages whose `send_at_ms` passed while Olaz was closed remain `Pending`
    /// so [`Self::due_messages`] immediately surfaces them for overdue catch-up on startup.
    pub fn load_from(dir: &Path) -> Self {
        let file_path = Self::file_path_in(dir);
        let loaded = fs::read(&file_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<ScheduledMessage>>(&bytes).ok())
            .unwrap_or_default();
        Self {
            file_path,
            items: Mutex::new(loaded),
        }
    }

    /// Atomically persists the current schedule list to `scheduled_messages.json`.
    pub fn save(&self) -> Result<()> {
        let snapshot = self.items.lock().clone();
        let mut bytes =
            serde_json::to_vec_pretty(&snapshot).context("failed to serialize scheduled messages")?;
        bytes.push(b'\n');
        write_atomic_private(&self.file_path, &bytes)
    }

    /// Schedules a new message at `send_at_ms` (must be `> now_ms`).
    pub fn schedule(
        &self,
        conv_id: impl Into<String>,
        target_name: Option<String>,
        text: impl Into<String>,
        send_at_ms: i64,
        now_ms: i64,
    ) -> Result<ScheduledMessage> {
        let conv_id = conv_id.into();
        if conv_id.trim().is_empty() {
            bail!("conv_id must not be empty");
        }
        let text_trimmed = text.into().trim().to_string();
        if text_trimmed.is_empty() {
            bail!("scheduled message text must not be empty");
        }
        if send_at_ms <= now_ms {
            bail!("send_at_ms ({send_at_ms}) must be greater than now_ms ({now_ms})");
        }

        let msg = ScheduledMessage {
            id: Uuid::new_v4().to_string(),
            conv_id,
            target_name: target_name.filter(|s| !s.trim().is_empty()),
            text: text_trimmed,
            send_at_ms,
            status: ScheduleStatus::Pending,
            created_at_ms: now_ms,
            sent_at_ms: None,
            last_error: None,
        };

        {
            let mut guard = self.items.lock();
            guard.push(msg.clone());
            guard.sort_by(|a, b| {
                a.send_at_ms
                    .cmp(&b.send_at_ms)
                    .then_with(|| a.created_at_ms.cmp(&b.created_at_ms))
            });
        }
        self.save()?;
        Ok(msg)
    }

    /// Schedules a new message using a preset/time expression (`"+15m"`, `"+1h"`, `"18:00"`, etc.).
    pub fn schedule_from_expr(
        &self,
        conv_id: impl Into<String>,
        target_name: Option<String>,
        text: impl Into<String>,
        when_expr: &str,
        now_ms: i64,
    ) -> Result<ScheduledMessage> {
        let send_at_ms = parse_schedule_time_ms(when_expr, now_ms)?;
        self.schedule(conv_id, target_name, text, send_at_ms, now_ms)
    }

    /// Inserts a pre-built [`ScheduledMessage`] record (e.g. for migration or overdue startup fixtures)
    /// and persists atomically.
    pub fn insert_record(&self, record: ScheduledMessage) -> Result<()> {
        {
            let mut guard = self.items.lock();
            guard.retain(|m| m.id != record.id);
            guard.push(record);
            guard.sort_by(|a, b| {
                a.send_at_ms
                    .cmp(&b.send_at_ms)
                    .then_with(|| a.created_at_ms.cmp(&b.created_at_ms))
            });
        }
        self.save()
    }

    /// Returns all scheduled messages (across all statuses), optionally filtered by `conv_id`.
    pub fn list(&self, conv_id: Option<&str>) -> Vec<ScheduledMessage> {
        let guard = self.items.lock();
        guard
            .iter()
            .filter(|m| conv_id.map_or(true, |cid| m.conv_id == cid))
            .cloned()
            .collect()
    }

    /// Returns all `Pending` scheduled messages, optionally filtered by `conv_id`.
    pub fn list_pending(&self, conv_id: Option<&str>) -> Vec<ScheduledMessage> {
        let guard = self.items.lock();
        guard
            .iter()
            .filter(|m| {
                m.status == ScheduleStatus::Pending
                    && conv_id.map_or(true, |cid| m.conv_id == cid)
            })
            .cloned()
            .collect()
    }

    /// Returns all `Pending` messages due at or before `now_ms` (including overdue startup messages),
    /// ordered by `(send_at_ms, created_at_ms)`.
    pub fn due_messages(&self, now_ms: i64) -> Vec<ScheduledMessage> {
        let mut due: Vec<ScheduledMessage> = self
            .items
            .lock()
            .iter()
            .filter(|m| m.status == ScheduleStatus::Pending && m.send_at_ms <= now_ms)
            .cloned()
            .collect();
        due.sort_by(|a, b| {
            a.send_at_ms
                .cmp(&b.send_at_ms)
                .then_with(|| a.created_at_ms.cmp(&b.created_at_ms))
        });
        due
    }

    /// Marks `id` as [`ScheduleStatus::Sent`] with `sent_at_ms` and persists atomically.
    pub fn mark_sent(&self, id: &str, sent_at_ms: i64) -> Result<bool> {
        let mut updated = false;
        {
            let mut guard = self.items.lock();
            if let Some(item) = guard.iter_mut().find(|m| m.id == id) {
                if item.status == ScheduleStatus::Pending || item.status == ScheduleStatus::Failed {
                    item.status = ScheduleStatus::Sent;
                    item.sent_at_ms = Some(sent_at_ms);
                    item.last_error = None;
                    updated = true;
                }
            }
        }
        if updated {
            self.save()?;
        }
        Ok(updated)
    }

    /// Marks `id` as [`ScheduleStatus::Failed`] with `error` and persists atomically.
    pub fn mark_failed(&self, id: &str, error: impl Into<String>) -> Result<bool> {
        let mut updated = false;
        let err_str = error.into();
        {
            let mut guard = self.items.lock();
            if let Some(item) = guard.iter_mut().find(|m| m.id == id) {
                if item.status == ScheduleStatus::Pending {
                    item.status = ScheduleStatus::Failed;
                    item.last_error = Some(err_str);
                    updated = true;
                }
            }
        }
        if updated {
            self.save()?;
        }
        Ok(updated)
    }

    /// Cancels a `Pending` or `Failed` scheduled message (`status -> Cancelled`) and persists atomically.
    pub fn cancel(&self, id: &str) -> Result<bool> {
        let mut cancelled = false;
        {
            let mut guard = self.items.lock();
            if let Some(item) = guard.iter_mut().find(|m| m.id == id) {
                if matches!(item.status, ScheduleStatus::Pending | ScheduleStatus::Failed) {
                    item.status = ScheduleStatus::Cancelled;
                    cancelled = true;
                }
            }
        }
        if cancelled {
            self.save()?;
        }
        Ok(cancelled)
    }
}

/// Keyword matching strategy for an [`AutoReplyRule`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    #[default]
    #[serde(alias = "Contains", alias = "CONTAINS")]
    Contains,
    #[serde(alias = "Exact", alias = "EXACT")]
    Exact,
    #[serde(alias = "Prefix", alias = "PREFIX")]
    Prefix,
    #[serde(alias = "Any", alias = "ANY", alias = "away", alias = "*")]
    Any,
}

impl MatchMode {
    pub fn from_str_lossy(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "exact" => Self::Exact,
            "prefix" => Self::Prefix,
            "any" | "away" | "all" | "*" => Self::Any,
            _ => Self::Contains,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_cooldown_secs() -> u64 {
    60
}

/// Persistent keyword / away auto-reply rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoReplyRule {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub match_mode: MatchMode,
    #[serde(default)]
    pub keyword: String,
    pub reply_text: String,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default)]
    pub scope_conv_id: Option<String>,
    #[serde(default)]
    pub allow_groups: bool,
}

/// Thread-safe keyword / away auto-reply evaluator with Vietnamese accent folding,
/// self-message ignore guard, and per-`(rule_id, conv_id)` cooldown tracking.
#[derive(Debug)]
pub struct AutoReplyEngine {
    file_path: PathBuf,
    rules: Mutex<Vec<AutoReplyRule>>,
    /// Maps `(rule_id, conv_id)` -> `last_triggered_ms`.
    cooldowns: Mutex<HashMap<(String, String), i64>>,
}

impl AutoReplyEngine {
    /// Returns `<dir>/autoreply_rules.json`.
    pub fn file_path_in(dir: &Path) -> PathBuf {
        dir.join(AUTOREPLY_RULES_FILENAME)
    }

    /// Loads existing rules from `<dir>/autoreply_rules.json`.
    pub fn load_from(dir: &Path) -> Self {
        let file_path = Self::file_path_in(dir);
        let rules = fs::read(&file_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<AutoReplyRule>>(&bytes).ok())
            .unwrap_or_default();
        Self {
            file_path,
            rules: Mutex::new(rules),
            cooldowns: Mutex::new(HashMap::new()),
        }
    }

    /// Atomically persists rules to `autoreply_rules.json`.
    pub fn save(&self) -> Result<()> {
        let snapshot = self.rules.lock().clone();
        let mut bytes =
            serde_json::to_vec_pretty(&snapshot).context("failed to serialize autoreply rules")?;
        bytes.push(b'\n');
        write_atomic_private(&self.file_path, &bytes)
    }

    /// Returns a snapshot of configured rules.
    pub fn rules(&self) -> Vec<AutoReplyRule> {
        self.rules.lock().clone()
    }

    /// Replaces the entire rule list and persists atomically.
    pub fn set_rules(&self, rules: Vec<AutoReplyRule>) -> Result<()> {
        *self.rules.lock() = rules;
        self.save()
    }

    /// Inserts or updates a rule by `id` (generating a UUID if `rule.id` is empty) and persists.
    pub fn upsert_rule(&self, mut rule: AutoReplyRule) -> Result<AutoReplyRule> {
        if rule.id.trim().is_empty() {
            rule.id = Uuid::new_v4().to_string();
        }
        if rule.reply_text.trim().is_empty() {
            bail!("auto-reply reply_text must not be empty");
        }
        if rule.match_mode != MatchMode::Any && rule.keyword.trim().is_empty() {
            bail!("auto-reply keyword must not be empty unless match_mode is `any`");
        }
        {
            let mut guard = self.rules.lock();
            if let Some(existing) = guard.iter_mut().find(|r| r.id == rule.id) {
                *existing = rule.clone();
            } else {
                guard.push(rule.clone());
            }
        }
        self.save()?;
        Ok(rule)
    }

    /// Removes a rule by `id` and persists atomically.
    pub fn remove_rule(&self, id: &str) -> Result<bool> {
        let removed = {
            let mut guard = self.rules.lock();
            let before = guard.len();
            guard.retain(|r| r.id != id);
            guard.len() < before
        };
        if removed {
            self.cooldowns.lock().retain(|(rid, _), _| rid != id);
            self.save()?;
        }
        Ok(removed)
    }

    /// Clears all configured rules and cooldowns and persists atomically.
    pub fn clear_rules(&self) -> Result<()> {
        self.rules.lock().clear();
        self.cooldowns.lock().clear();
        self.save()
    }

    /// Resets the in-memory cooldown map.
    pub fn reset_cooldowns(&self) {
        self.cooldowns.lock().clear();
    }

    /// Evaluates an incoming message against enabled rules.
    ///
    /// Enforces:
    /// 1. Self-message ignore guard (`is_mine` or `sender_id == self_user_id` -> `None`).
    /// 2. Group chat filter (`is_group && !rule.allow_groups` -> skipped).
    /// 3. Conversation scope filter (`rule.scope_conv_id` mismatch -> skipped).
    /// 4. Vietnamese accent-insensitive matching via [`fold_vietnamese`].
    /// 5. Per-`(rule_id, conv_id)` cooldown window (`cooldown_secs`).
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_incoming(
        &self,
        conv_id: &str,
        text: &str,
        is_mine: bool,
        is_group: bool,
        sender_id: Option<&str>,
        self_user_id: Option<&str>,
        now_ms: i64,
    ) -> Option<AutoReplyRule> {
        if is_mine {
            return None;
        }
        if let (Some(sender), Some(me)) = (
            sender_id.filter(|s| !s.is_empty()),
            self_user_id.filter(|s| !s.is_empty()),
        ) {
            if sender == me {
                return None;
            }
        }
        let conv_trimmed = conv_id.trim();
        let text_trimmed = text.trim();
        if conv_trimmed.is_empty() || text_trimmed.is_empty() {
            return None;
        }

        let folded_msg = fold_vietnamese(text_trimmed);
        let rules_snapshot = self.rules.lock().clone();
        let mut cooldowns = self.cooldowns.lock();

        for rule in rules_snapshot {
            if !rule.enabled || rule.reply_text.trim().is_empty() {
                continue;
            }
            if is_group && !rule.allow_groups {
                continue;
            }
            if let Some(ref scoped) = rule.scope_conv_id {
                let s = scoped.trim();
                if !s.is_empty() && s != conv_trimmed {
                    continue;
                }
            }

            let matched = match rule.match_mode {
                MatchMode::Any => true,
                MatchMode::Contains => {
                    let kw = fold_vietnamese(rule.keyword.trim());
                    !kw.is_empty() && folded_msg.contains(&kw)
                }
                MatchMode::Exact => {
                    let kw = fold_vietnamese(rule.keyword.trim());
                    !kw.is_empty() && folded_msg == kw
                }
                MatchMode::Prefix => {
                    let kw = fold_vietnamese(rule.keyword.trim());
                    !kw.is_empty() && folded_msg.starts_with(&kw)
                }
            };

            if !matched {
                continue;
            }

            let cd_key = (rule.id.clone(), conv_trimmed.to_string());
            if rule.cooldown_secs > 0 {
                let cooldown_ms = i64::try_from(rule.cooldown_secs)
                    .unwrap_or(i64::MAX / 2000)
                    .saturating_mul(1000);
                if let Some(&last_ms) = cooldowns.get(&cd_key) {
                    if now_ms.saturating_sub(last_ms) < cooldown_ms {
                        continue;
                    }
                }
            }

            cooldowns.insert(cd_key, now_ms);
            return Some(rule);
        }

        None
    }

    /// Convenience wrapper evaluating a [`ChatMessage`] directly (also ignoring recalled messages).
    pub fn evaluate_chat_message(
        &self,
        msg: &ChatMessage,
        is_group: bool,
        self_user_id: Option<&str>,
        now_ms: i64,
    ) -> Option<AutoReplyRule> {
        if msg.recalled {
            return None;
        }
        self.evaluate_incoming(
            &msg.conversation_id,
            &msg.text,
            msg.is_mine,
            is_group,
            Some(msg.sender_id.as_str()),
            self_user_id,
            now_ms,
        )
    }
}

/// Callback trait invoked by [`spawn_automation_loop`] (and control backends) to deliver outbound messages.
pub trait SendMessageCallback: Send + Sync + 'static {
    fn send_message<'a>(
        &'a self,
        conv_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

impl<F, Fut> SendMessageCallback for F
where
    F: Fn(String, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    fn send_message<'a>(
        &'a self,
        conv_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin((self)(conv_id.to_string(), text.to_string()))
    }
}

/// Handle controlling a running [`spawn_automation_loop`] background task.
///
/// Follows [`crate::OmarchyWatcherHandle`] semantics: stays alive until [`Self::stop`] is called
/// (or the Tokio runtime shuts down).
pub struct AutomationLoopHandle {
    stop_flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
    abort_handle: AbortHandle,
}

impl AutomationLoopHandle {
    /// Signals the automation loop to exit cleanly and aborts any idle sleep.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        self.abort_handle.abort();
    }
}

/// Dispatches all due scheduled messages once at `now_ms`, marking each `Sent` or `Failed`.
pub async fn dispatch_due_scheduled_once<C: SendMessageCallback + ?Sized>(
    scheduler: &SchedulerEngine,
    sender: &C,
    now_ms: i64,
) -> usize {
    let due = scheduler.due_messages(now_ms);
    let mut sent_count = 0usize;
    for item in due {
        match sender.send_message(&item.conv_id, &item.text).await {
            Ok(()) => {
                debug!(id = %item.id, conv_id = %item.conv_id, "delivered scheduled message");
                let _ = scheduler.mark_sent(&item.id, unix_now_ms().max(now_ms));
                sent_count += 1;
            }
            Err(err) => {
                warn!(id = %item.id, conv_id = %item.conv_id, %err, "scheduled message delivery failed");
                let _ = scheduler.mark_failed(&item.id, err.to_string());
            }
        }
    }
    sent_count
}

/// Spawns the background scheduler runner on the current Tokio runtime using `tokio::time::interval`
/// (performing immediate startup overdue catch-up on the very first tick).
pub fn spawn_automation_loop<C>(
    scheduler: Arc<SchedulerEngine>,
    tick_interval: Duration,
    sender: C,
) -> AutomationLoopHandle
where
    C: SendMessageCallback,
{
    let period = if tick_interval.is_zero() {
        Duration::from_millis(50)
    } else {
        tick_interval
    };
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_task = Arc::clone(&stop_flag);
    let notify = Arc::new(Notify::new());
    let notify_task = Arc::clone(&notify);
    let sender = Arc::new(sender);

    let join = tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = notify_task.notified() => {
                    if stop_task.load(Ordering::SeqCst) {
                        break;
                    }
                }
                _ = interval.tick() => {
                    if stop_task.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = dispatch_due_scheduled_once(&scheduler, sender.as_ref(), unix_now_ms()).await;
                }
            }
        }
    });

    AutomationLoopHandle {
        stop_flag,
        notify,
        abort_handle: join.abort_handle(),
    }
}
