//! Local loopback HTTP REST API (`127.0.0.1:<port>`, optional `OLAZ_API_TOKEN` Bearer auth)
//! and Model Context Protocol (MCP) JSON-RPC 2.0 server (`--mcp` over `stdio` or `POST /api/v1/mcp`).
//!
//! Adapted from `ctdozlite/src/api_mcp.rs` using pure `tokio::net::TcpListener` and
//! `tokio::io::BufReader` with zero external HTTP framework dependencies.
//!
//! Includes two [`OlazControlBackend`] implementations:
//! 1. [`EngineControlBackend`]: in-process backend attached to live `ZaloEngineState`,
//!    `SchedulerEngine`, `AutoReplyEngine`, `PrivacyConfig`, and send/call callbacks.
//! 2. [`SocketRpcBackend`]: proxy backend forwarding requests over [`crate::instance::send_instance_rpc_to_socket_async`]
//!    so `olaz --mcp` seamlessly queries and commands an already-running `olaz` GUI instance.

use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use parking_lot::RwLock;
use percent_encoding::percent_decode_str;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::automation::{
    parse_schedule_time_ms, unix_now_ms, AutoReplyEngine, AutoReplyRule, MatchMode,
    SchedulerEngine, SendMessageCallback,
};
use crate::engine::{EnginePhase, ZaloEngineState};
use crate::instance::{
    instance_socket_path_for_profile, instance_socket_path_in_dir,
    send_instance_rpc_to_socket_async, RpcHandler,
};
use crate::privacy::PrivacyConfig;

/// Default loopback TCP port for the Olaz REST & HTTP-MCP server.
pub const DEFAULT_API_PORT: u16 = 17890;

/// MCP protocol version advertised during `initialize`.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Maximum HTTP request payload accepted on the local loopback server (`1 MiB`).
const MAX_HTTP_REQUEST_BYTES: usize = 1024 * 1024;

/// Async callback invoked to initiate a 1:1 audio or video call (`POST /api/v1/call` / `zalo_start_call`).
pub type StartCallCallback = Arc<
    dyn Fn(String, bool) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>> + Send + Sync,
>;

/// Unified control plane interface shared by the HTTP REST server, MCP stdio/HTTP server,
/// and Unix domain socket RPC bridge.
pub trait OlazControlBackend: Send + Sync + 'static {
    fn status<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn list_threads<'a>(
        &'a self,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn get_messages<'a>(
        &'a self,
        conv_id: &'a str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn send_message<'a>(
        &'a self,
        conv_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn list_scheduled<'a>(
        &'a self,
        conv_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn schedule_message<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn cancel_scheduled<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn get_autoreply<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn set_autoreply<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn get_privacy<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn set_privacy<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    fn start_call<'a>(
        &'a self,
        target_id: &'a str,
        video: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

    /// Lists all active/registered account profiles managed by this Olaz instance.
    fn list_profiles<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let mut st = self.status().await?;
            let slug = st
                .get("profile")
                .and_then(Value::as_str)
                .unwrap_or("default")
                .to_string();
            if let Some(obj) = st.as_object_mut() {
                obj.insert("is_active".to_string(), json!(true));
            }
            Ok(json!({
                "ok": true,
                "active_profile": slug,
                "profiles": [st],
            }))
        })
    }

    /// Resolves a profile-specific child backend when running in multi-account workspace mode.
    fn resolve_profile_backend(&self, _profile: Option<&str>) -> Option<Arc<dyn OlazControlBackend>> {
        None
    }
}

/// Entry registered inside [`MultiAccountControlBackend`].
#[derive(Clone)]
struct MultiAccountEntry {
    slug: String,
    label: Option<String>,
    backend: Arc<dyn OlazControlBackend>,
}

struct MultiAccountState {
    active_slug: String,
    entries: Vec<MultiAccountEntry>,
}

/// Workspace-wide [`OlazControlBackend`] multiplexing multiple in-window account sessions,
/// routing calls to the active (or explicitly requested) account and exposing [`Self::list_profiles`].
pub struct MultiAccountControlBackend {
    state: RwLock<MultiAccountState>,
}

impl MultiAccountControlBackend {
    pub fn new(active_slug: impl Into<String>) -> Self {
        Self {
            state: RwLock::new(MultiAccountState {
                active_slug: active_slug.into(),
                entries: Vec::new(),
            }),
        }
    }

    pub fn register_account(
        &self,
        slug: impl Into<String>,
        label: Option<String>,
        backend: Arc<dyn OlazControlBackend>,
    ) {
        let slug = slug.into();
        let mut st = self.state.write();
        if let Some(existing) = st.entries.iter_mut().find(|e| e.slug == slug) {
            existing.label = label;
            existing.backend = backend;
        } else {
            st.entries.push(MultiAccountEntry {
                slug: slug.clone(),
                label,
                backend,
            });
        }
        if st.entries.len() == 1 || st.active_slug.is_empty() {
            st.active_slug = slug;
        }
    }

    pub fn unregister_account(&self, slug: &str) {
        let mut st = self.state.write();
        st.entries.retain(|e| e.slug != slug);
        if st.active_slug == slug {
            st.active_slug = st
                .entries
                .first()
                .map(|e| e.slug.clone())
                .unwrap_or_else(|| "default".to_string());
        }
    }

    pub fn set_active_slug(&self, slug: impl Into<String>) {
        let slug = slug.into();
        let mut st = self.state.write();
        if st.entries.iter().any(|e| e.slug == slug) || st.entries.is_empty() {
            st.active_slug = slug;
        }
    }

    pub fn active_slug(&self) -> String {
        self.state.read().active_slug.clone()
    }

    pub fn set_account_label(&self, slug: &str, label: Option<String>) {
        let mut st = self.state.write();
        if let Some(entry) = st.entries.iter_mut().find(|e| e.slug == slug) {
            entry.label = label;
        }
    }

    pub fn account_backend(&self, slug: Option<&str>) -> Result<Arc<dyn OlazControlBackend>> {
        let st = self.state.read();
        let target = slug
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(st.active_slug.as_str());
        if let Some(found) = st.entries.iter().find(|e| e.slug == target) {
            return Ok(Arc::clone(&found.backend));
        }
        if slug.is_none() {
            if let Some(first) = st.entries.first() {
                return Ok(Arc::clone(&first.backend));
            }
        }
        bail!("account profile `{target}` is not open in this workspace")
    }
}

impl OlazControlBackend for MultiAccountControlBackend {
    fn status<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let b = self.account_backend(None)?;
            let mut val = b.status().await?;
            let count = self.state.read().entries.len();
            if let Some(obj) = val.as_object_mut() {
                obj.insert("account_count".to_string(), json!(count));
            }
            Ok(val)
        })
    }

    fn list_threads<'a>(
        &'a self,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.list_threads(limit).await })
    }

    fn get_messages<'a>(
        &'a self,
        conv_id: &'a str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.get_messages(conv_id, limit).await })
    }

    fn send_message<'a>(
        &'a self,
        conv_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.send_message(conv_id, text).await })
    }

    fn list_scheduled<'a>(
        &'a self,
        conv_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.list_scheduled(conv_id).await })
    }

    fn schedule_message<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let prof = req.get("profile").and_then(Value::as_str);
            self.account_backend(prof)?.schedule_message(req).await
        })
    }

    fn cancel_scheduled<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.cancel_scheduled(id).await })
    }

    fn get_autoreply<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.get_autoreply().await })
    }

    fn set_autoreply<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let prof = req.get("profile").and_then(Value::as_str);
            self.account_backend(prof)?.set_autoreply(req).await
        })
    }

    fn get_privacy<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.get_privacy().await })
    }

    fn set_privacy<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let prof = req.get("profile").and_then(Value::as_str);
            self.account_backend(prof)?.set_privacy(req).await
        })
    }

    fn start_call<'a>(
        &'a self,
        target_id: &'a str,
        video: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { self.account_backend(None)?.start_call(target_id, video).await })
    }

    fn list_profiles<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let (active_slug, snapshot) = {
                let st = self.state.read();
                (st.active_slug.clone(), st.entries.clone())
            };
            let mut profiles = Vec::with_capacity(snapshot.len());
            for entry in snapshot {
                match entry.backend.status().await {
                    Ok(mut st_val) => {
                        if let Some(obj) = st_val.as_object_mut() {
                            obj.insert("slug".to_string(), json!(entry.slug));
                            obj.insert("profile".to_string(), json!(entry.slug));
                            obj.insert("label".to_string(), json!(entry.label));
                            obj.insert("is_active".to_string(), json!(entry.slug == active_slug));
                        }
                        profiles.push(st_val);
                    }
                    Err(err) => {
                        profiles.push(json!({
                            "ok": false,
                            "slug": entry.slug,
                            "profile": entry.slug,
                            "label": entry.label,
                            "is_active": entry.slug == active_slug,
                            "error": err.to_string(),
                        }));
                    }
                }
            }
            Ok(json!({
                "ok": true,
                "active_profile": active_slug,
                "profiles": profiles,
            }))
        })
    }

    fn resolve_profile_backend(&self, profile: Option<&str>) -> Option<Arc<dyn OlazControlBackend>> {
        let slug = profile.map(str::trim).filter(|s| !s.is_empty())?;
        let st = self.state.read();
        st.entries
            .iter()
            .find(|e| e.slug == slug)
            .map(|e| Arc::clone(&e.backend))
    }
}

/// Live in-process implementation of [`OlazControlBackend`] backed by [`ZaloEngineState`],
/// [`SchedulerEngine`], [`AutoReplyEngine`], [`PrivacyConfig`], and live send/call hooks.
pub struct EngineControlBackend {
    engine_state: Arc<RwLock<ZaloEngineState>>,
    scheduler: Arc<SchedulerEngine>,
    autoreply: Arc<AutoReplyEngine>,
    privacy: Arc<RwLock<PrivacyConfig>>,
    config_dir: PathBuf,
    profile_slug: String,
    send_cb: Option<Arc<dyn SendMessageCallback>>,
    call_cb: Option<StartCallCallback>,
    on_privacy_change: Option<Arc<dyn Fn(PrivacyConfig) + Send + Sync>>,
}

impl EngineControlBackend {
    /// Creates a live [`EngineControlBackend`] loading initial [`PrivacyConfig`] from `config_dir`.
    pub fn new(
        engine_state: Arc<RwLock<ZaloEngineState>>,
        scheduler: Arc<SchedulerEngine>,
        autoreply: Arc<AutoReplyEngine>,
        config_dir: impl Into<PathBuf>,
    ) -> Self {
        let config_dir = config_dir.into();
        let privacy = PrivacyConfig::load_from(&config_dir);
        Self {
            engine_state,
            scheduler,
            autoreply,
            privacy: Arc::new(RwLock::new(privacy)),
            config_dir,
            profile_slug: "default".to_string(),
            send_cb: None,
            call_cb: None,
            on_privacy_change: None,
        }
    }

    pub fn with_profile_slug(mut self, slug: impl Into<String>) -> Self {
        self.profile_slug = slug.into();
        self
    }

    pub fn with_send_callback<C: SendMessageCallback>(mut self, cb: C) -> Self {
        self.send_cb = Some(Arc::new(cb));
        self
    }

    pub fn with_call_callback(mut self, cb: StartCallCallback) -> Self {
        self.call_cb = Some(cb);
        self
    }

    pub fn with_privacy_observer<F>(mut self, cb: F) -> Self
    where
        F: Fn(PrivacyConfig) + Send + Sync + 'static,
    {
        self.on_privacy_change = Some(Arc::new(cb));
        self
    }

    pub fn privacy_config(&self) -> PrivacyConfig {
        *self.privacy.read()
    }
}

fn is_conv_accessible(state: &ZaloEngineState, conv_id: &str) -> bool {
    if state.conv_actions.loaded {
        !state.conv_actions.is_locked(conv_id)
    } else {
        !state.conv_actions.locked_ids().contains(conv_id)
    }
}

impl OlazControlBackend for EngineControlBackend {
    fn status<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let (logged_in, phase_name, user_id, display_name, conv_count, unread_total) = {
                let st = self.engine_state.read();
                let logged_in = matches!(st.phase, EnginePhase::Ready) || st.user.is_some();
                let phase_name = match &st.phase {
                    EnginePhase::Booting => "booting",
                    EnginePhase::InstallingRuntime { .. } => "installing_runtime",
                    EnginePhase::Login { .. } => "login",
                    EnginePhase::Ready => "ready",
                };
                let uid = st
                    .user
                    .as_ref()
                    .map(|u| u.user_id.clone())
                    .unwrap_or_default();
                let dname = st
                    .user
                    .as_ref()
                    .map(|u| u.display_name.clone())
                    .unwrap_or_default();
                let accessible: Vec<_> = st
                    .conversations
                    .iter()
                    .filter(|c| is_conv_accessible(&st, &c.id))
                    .collect();
                let unread: u64 = accessible.iter().map(|c| u64::from(c.unread)).sum();
                (logged_in, phase_name, uid, dname, accessible.len(), unread)
            };
            let pending_scheduled = self.scheduler.list_pending(None).len();
            let autoreply_count = self.autoreply.rules().len();
            let privacy = *self.privacy.read();
            Ok(json!({
                "ok": true,
                "app": "olaz",
                "version": env!("CARGO_PKG_VERSION"),
                "profile": self.profile_slug,
                "phase": phase_name,
                "logged_in": logged_in,
                "user_id": user_id,
                "display_name": display_name,
                "conversation_count": conv_count,
                "unread_total": unread_total,
                "pending_scheduled": pending_scheduled,
                "autoreply_rules": autoreply_count,
                "privacy": privacy,
            }))
        })
    }

    fn list_threads<'a>(
        &'a self,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let cap = limit.clamp(1, 500);
            let threads: Vec<Value> = {
                let st = self.engine_state.read();
                st.conversations
                    .iter()
                    .filter(|c| is_conv_accessible(&st, &c.id))
                    .take(cap)
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "name": c.display_name(),
                            "is_group": c.is_group,
                            "pinned": c.pinned,
                            "unread": c.unread,
                            "members": c.members,
                            "preview": c.preview,
                        })
                    })
                    .collect()
            };
            Ok(json!({
                "ok": true,
                "threads": threads,
            }))
        })
    }

    fn get_messages<'a>(
        &'a self,
        conv_id: &'a str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let cid = conv_id.trim();
            if cid.is_empty() {
                bail!("conv_id is required");
            }
            let cap = limit.clamp(1, 500);
            let messages: Vec<Value> = {
                let st = self.engine_state.read();
                if !is_conv_accessible(&st, cid) {
                    bail!("conversation `{cid}` is PIN-locked or hidden");
                }
                st.messages
                    .get(cid)
                    .map(|list| {
                        let skip = list.len().saturating_sub(cap);
                        list.iter()
                            .skip(skip)
                            .map(|m| {
                                json!({
                                    "id": m.id,
                                    "conversation_id": m.conversation_id,
                                    "sender_id": m.sender_id,
                                    "sender_name": m.sender_name,
                                    "timestamp": m.timestamp,
                                    "is_mine": m.is_mine,
                                    "kind": m.kind,
                                    "text": m.text,
                                    "recalled": m.recalled,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Ok(json!({
                "ok": true,
                "conv_id": cid,
                "messages": messages,
            }))
        })
    }

    fn send_message<'a>(
        &'a self,
        conv_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let cid = conv_id.trim();
            let body = text.trim();
            if cid.is_empty() {
                bail!("conv_id is required");
            }
            if body.is_empty() {
                bail!("text must not be empty");
            }
            let Some(ref cb) = self.send_cb else {
                bail!("message sending callback is not attached");
            };
            cb.send_message(cid, body).await?;
            Ok(json!({
                "ok": true,
                "conv_id": cid,
                "text": body,
            }))
        })
    }

    fn list_scheduled<'a>(
        &'a self,
        conv_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let filter = conv_id.map(str::trim).filter(|s| !s.is_empty());
            let items = self.scheduler.list(filter);
            Ok(json!({
                "ok": true,
                "scheduled": items,
            }))
        })
    }

    fn schedule_message<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let conv_id = req
                .get("conv_id")
                .or_else(|| req.get("to_id"))
                .or_else(|| req.get("thread_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if conv_id.is_empty() {
                bail!("conv_id is required");
            }
            let text = req
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if text.is_empty() {
                bail!("text is required");
            }
            let target_name = req
                .get("target_name")
                .or_else(|| req.get("thread_name"))
                .and_then(Value::as_str)
                .map(str::to_string);

            let now_ms = unix_now_ms();
            let send_at_ms = if let Some(explicit_ms) = req.get("send_at_ms").and_then(Value::as_i64)
            {
                explicit_ms
            } else if let Some(when_str) = req.get("when").and_then(Value::as_str) {
                parse_schedule_time_ms(when_str, now_ms)?
            } else {
                bail!("either `send_at_ms` or `when` (e.g. \"+15m\", \"18:00\") is required");
            };

            let item =
                self.scheduler
                    .schedule(conv_id, target_name, text, send_at_ms, now_ms)?;
            Ok(json!({
                "ok": true,
                "scheduled": item,
            }))
        })
    }

    fn cancel_scheduled<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let trimmed = id.trim();
            if trimmed.is_empty() {
                bail!("scheduled message `id` is required");
            }
            let cancelled = self.scheduler.cancel(trimmed)?;
            Ok(json!({
                "ok": true,
                "id": trimmed,
                "cancelled": cancelled,
            }))
        })
    }

    fn get_autoreply<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            Ok(json!({
                "ok": true,
                "rules": self.autoreply.rules(),
            }))
        })
    }

    fn set_autoreply<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(arr) = req.get("rules").and_then(Value::as_array) {
                let mut parsed = Vec::with_capacity(arr.len());
                for item in arr {
                    let rule: AutoReplyRule = serde_json::from_value(item.clone())
                        .context("invalid AutoReplyRule in `rules` array")?;
                    parsed.push(rule);
                }
                self.autoreply.set_rules(parsed)?;
                return Ok(json!({
                    "ok": true,
                    "rules": self.autoreply.rules(),
                }));
            }

            let action = req
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("upsert")
                .trim()
                .to_ascii_lowercase();

            match action.as_str() {
                "list" => Ok(json!({
                    "ok": true,
                    "rules": self.autoreply.rules(),
                })),
                "clear" => {
                    self.autoreply.clear_rules()?;
                    Ok(json!({
                        "ok": true,
                        "rules": [],
                    }))
                }
                "remove" | "delete" => {
                    let id = req
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    if id.is_empty() {
                        bail!("`id` is required when removing an auto-reply rule");
                    }
                    let removed = self.autoreply.remove_rule(id)?;
                    Ok(json!({
                        "ok": true,
                        "removed": removed,
                        "rules": self.autoreply.rules(),
                    }))
                }
                "upsert" | "add" | "set" => {
                    let rule_val = req.get("rule").unwrap_or(req);
                    let id = rule_val
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let enabled = rule_val
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    let match_mode = match rule_val.get("match_mode") {
                        Some(v) => serde_json::from_value::<MatchMode>(v.clone())
                            .unwrap_or_else(|_| {
                                MatchMode::from_str_lossy(v.as_str().unwrap_or("contains"))
                            }),
                        None => MatchMode::Contains,
                    };
                    let keyword = rule_val
                        .get("keyword")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let reply_text = rule_val
                        .get("reply_text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let cooldown_secs = rule_val
                        .get("cooldown_secs")
                        .and_then(Value::as_u64)
                        .unwrap_or(60);
                    let scope_conv_id = rule_val
                        .get("scope_conv_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string);
                    let allow_groups = rule_val
                        .get("allow_groups")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                    let saved = self.autoreply.upsert_rule(AutoReplyRule {
                        id,
                        enabled,
                        match_mode,
                        keyword,
                        reply_text,
                        cooldown_secs,
                        scope_conv_id,
                        allow_groups,
                    })?;
                    Ok(json!({
                        "ok": true,
                        "rule": saved,
                        "rules": self.autoreply.rules(),
                    }))
                }
                other => bail!("unsupported autoreply action `{other}`"),
            }
        })
    }

    fn get_privacy<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let cfg = *self.privacy.read();
            Ok(json!({
                "ok": true,
                "privacy": cfg,
            }))
        })
    }

    fn set_privacy<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let next = {
                let mut guard = self.privacy.write();
                guard.apply_patch(req);
                *guard
            };
            next.save_to(&self.config_dir)?;
            if let Some(ref obs) = self.on_privacy_change {
                obs(next);
            }
            Ok(json!({
                "ok": true,
                "privacy": next,
                "js_init_script": next.to_js_init_script(),
            }))
        })
    }

    fn start_call<'a>(
        &'a self,
        target_id: &'a str,
        video: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let tid = target_id.trim();
            if tid.is_empty() {
                bail!("target_id is required");
            }
            let Some(ref cb) = self.call_cb else {
                bail!("VoIP call callback is not attached");
            };
            let res = cb(tid.to_string(), video).await?;
            Ok(json!({
                "ok": true,
                "target_id": tid,
                "video": video,
                "call": res,
            }))
        })
    }
}

/// Dispatches an internal socket RPC envelope `{"op": "...", "params": {...}}` against `backend`.
pub async fn dispatch_control_rpc(backend: &Arc<dyn OlazControlBackend>, req: Value) -> Value {
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
    let target_backend = backend
        .resolve_profile_backend(params.get("profile").and_then(Value::as_str))
        .unwrap_or_else(|| Arc::clone(backend));

    let result: Result<Value> = match op {
        "status" => target_backend.status().await,
        "list_profiles" => backend.list_profiles().await,
        "list_threads" => {
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            target_backend.list_threads(limit).await
        }
        "get_messages" => {
            let cid = params.get("conv_id").and_then(Value::as_str).unwrap_or("");
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            target_backend.get_messages(cid, limit).await
        }
        "send_message" => {
            let cid = params.get("conv_id").and_then(Value::as_str).unwrap_or("");
            let text = params.get("text").and_then(Value::as_str).unwrap_or("");
            target_backend.send_message(cid, text).await
        }
        "list_scheduled" => {
            let cid = params.get("conv_id").and_then(Value::as_str);
            target_backend.list_scheduled(cid).await
        }
        "schedule_message" => target_backend.schedule_message(&params).await,
        "cancel_scheduled" => {
            let id = params.get("id").and_then(Value::as_str).unwrap_or("");
            target_backend.cancel_scheduled(id).await
        }
        "get_autoreply" => target_backend.get_autoreply().await,
        "set_autoreply" => target_backend.set_autoreply(&params).await,
        "get_privacy" => target_backend.get_privacy().await,
        "set_privacy" => target_backend.set_privacy(&params).await,
        "start_call" => {
            let target_id = params
                .get("target_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let video = params.get("video").and_then(Value::as_bool).unwrap_or(false);
            target_backend.start_call(target_id, video).await
        }
        other => Err(anyhow::anyhow!("unknown control rpc op `{other}`")),
    };

    match result {
        Ok(data) => json!({ "ok": true, "data": data }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    }
}

/// Adapts any [`OlazControlBackend`] into an [`RpcHandler`] suitable for
/// [`crate::instance::SingleInstanceGuard::set_rpc_handler`].
pub fn make_instance_rpc_handler(backend: Arc<dyn OlazControlBackend>) -> RpcHandler {
    Arc::new(move |req| {
        let backend = Arc::clone(&backend);
        Box::pin(async move { dispatch_control_rpc(&backend, req).await })
    })
}

/// Proxy [`OlazControlBackend`] forwarding all operations over the profile's Unix domain socket
/// (`"rpc <json>\n"`) to a running primary `olaz` desktop instance.
#[derive(Debug, Clone)]
pub struct SocketRpcBackend {
    sock_path: PathBuf,
}

impl SocketRpcBackend {
    /// Creates a proxy backend targeting `profile` under [`crate::instance::default_instance_base_dir`].
    pub fn for_profile(profile: &str) -> Self {
        Self {
            sock_path: instance_socket_path_for_profile(profile),
        }
    }

    /// Creates a proxy backend targeting `(base_dir, profile)`.
    pub fn for_profile_in_dir(base_dir: &std::path::Path, profile: &str) -> Self {
        Self {
            sock_path: instance_socket_path_in_dir(base_dir, profile),
        }
    }

    /// Creates a proxy backend targeting an explicit Unix socket path.
    pub fn for_socket_path(sock_path: impl Into<PathBuf>) -> Self {
        Self {
            sock_path: sock_path.into(),
        }
    }

    async fn call_op(&self, op: &str, params: Value) -> Result<Value> {
        let envelope = json!({
            "op": op,
            "params": params,
        });
        let reply = send_instance_rpc_to_socket_async(&self.sock_path, &envelope).await?;
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(reply.get("data").cloned().unwrap_or_else(|| json!({})))
        } else {
            let msg = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("remote primary instance RPC failed");
            bail!("{msg}")
        }
    }
}

impl OlazControlBackend for SocketRpcBackend {
    fn status<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("status", json!({})))
    }

    fn list_threads<'a>(
        &'a self,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("list_threads", json!({ "limit": limit })))
    }

    fn get_messages<'a>(
        &'a self,
        conv_id: &'a str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("get_messages", json!({ "conv_id": conv_id, "limit": limit })))
    }

    fn send_message<'a>(
        &'a self,
        conv_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("send_message", json!({ "conv_id": conv_id, "text": text })))
    }

    fn list_scheduled<'a>(
        &'a self,
        conv_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("list_scheduled", json!({ "conv_id": conv_id })))
    }

    fn schedule_message<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        let cloned = req.clone();
        Box::pin(self.call_op("schedule_message", cloned))
    }

    fn cancel_scheduled<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("cancel_scheduled", json!({ "id": id })))
    }

    fn get_autoreply<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("get_autoreply", json!({})))
    }

    fn set_autoreply<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        let cloned = req.clone();
        Box::pin(self.call_op("set_autoreply", cloned))
    }

    fn get_privacy<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("get_privacy", json!({})))
    }

    fn set_privacy<'a>(
        &'a self,
        req: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        let cloned = req.clone();
        Box::pin(self.call_op("set_privacy", cloned))
    }

    fn start_call<'a>(
        &'a self,
        target_id: &'a str,
        video: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op(
            "start_call",
            json!({ "target_id": target_id, "video": video }),
        ))
    }

    fn list_profiles<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.call_op("list_profiles", json!({})))
    }
}

/// Returns the JSON Schema descriptor list for all MCP tools exposed by Olaz.
pub fn mcp_tools_catalog() -> Value {
    json!([
        {
            "name": "zalo_status",
            "description": "Check Olaz session status, active profile, unread counts, and privacy/automation summary",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "Optional account profile slug" }
                }
            }
        },
        {
            "name": "zalo_list_profiles",
            "description": "List all Zalo account profiles open in the Olaz workspace with login and unread status",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "zalo_list_threads",
            "description": "List accessible Zalo conversations (direct, group, Cloud of Mine)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Maximum conversations to return (default 50)" }
                }
            }
        },
        {
            "name": "zalo_get_messages",
            "description": "Read recent messages for a Zalo conversation",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": { "type": "string", "description": "Conversation ID" },
                    "limit": { "type": "integer", "description": "Maximum messages to return (default 50)" }
                },
                "required": ["conv_id"]
            }
        },
        {
            "name": "zalo_send_message",
            "description": "Send a text message to a Zalo conversation",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": { "type": "string", "description": "Target conversation ID" },
                    "text": { "type": "string", "description": "Message text" }
                },
                "required": ["conv_id", "text"]
            }
        },
        {
            "name": "zalo_schedule_message",
            "description": "Schedule a message to be sent in the future (+15m, +1h, 18:00, or epoch ms)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": { "type": "string", "description": "Target conversation ID" },
                    "target_name": { "type": "string", "description": "Optional conversation display name" },
                    "text": { "type": "string", "description": "Message text" },
                    "when": { "type": "string", "description": "Schedule expression (+15m, +1h, 18:00, YYYY-MM-DD HH:MM)" },
                    "send_at_ms": { "type": "integer", "description": "Alternative explicit future epoch timestamp in milliseconds" }
                },
                "required": ["conv_id", "text"]
            }
        },
        {
            "name": "zalo_list_scheduled",
            "description": "List scheduled messages, optionally filtered by conversation ID",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": { "type": "string", "description": "Optional conversation ID filter" }
                }
            }
        },
        {
            "name": "zalo_cancel_scheduled",
            "description": "Cancel a pending or failed scheduled message by ID",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Scheduled message ID" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "zalo_set_autoreply",
            "description": "Configure keyword or away auto-reply rules (upsert, remove, clear, list)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "'upsert' (default), 'remove', 'clear', or 'list'" },
                    "id": { "type": "string", "description": "Rule ID (optional on upsert, required on remove)" },
                    "enabled": { "type": "boolean", "description": "Whether rule is active" },
                    "match_mode": { "type": "string", "description": "'contains', 'exact', 'prefix', or 'any'" },
                    "keyword": { "type": "string", "description": "Trigger keyword (accent-insensitive Vietnamese)" },
                    "reply_text": { "type": "string", "description": "Automated reply message" },
                    "cooldown_secs": { "type": "integer", "description": "Per-(rule, conversation) cooldown in seconds" },
                    "scope_conv_id": { "type": "string", "description": "Optional single conversation scope" },
                    "allow_groups": { "type": "boolean", "description": "Allow triggering inside group chats" }
                }
            }
        },
        {
            "name": "zalo_set_privacy",
            "description": "Update outbound typing, seen, delivered privacy suppression and anti-recall (Xem tin thu hồi) flags",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "block_typing": { "type": "boolean" },
                    "block_seen": { "type": "boolean" },
                    "block_delivered": { "type": "boolean" },
                    "anti_recall": { "type": "boolean" }
                }
            }
        },
        {
            "name": "zalo_start_call",
            "description": "Start an outbound Zalo audio or video call",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target_id": { "type": "string", "description": "Callee user ID" },
                    "video": { "type": "boolean", "description": "true for video call, false for voice call" }
                },
                "required": ["target_id"]
            }
        }
    ])
}

async fn execute_mcp_tool(
    backend: &Arc<dyn OlazControlBackend>,
    name: &str,
    args: &Value,
) -> (bool, Value) {
    let target_backend = backend
        .resolve_profile_backend(args.get("profile").and_then(Value::as_str))
        .unwrap_or_else(|| Arc::clone(backend));
    let res: Result<Value> = match name {
        "zalo_status" => target_backend.status().await,
        "zalo_list_profiles" => backend.list_profiles().await,
        "zalo_list_threads" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            target_backend.list_threads(limit).await
        }
        "zalo_get_messages" => {
            let cid = args
                .get("conv_id")
                .or_else(|| args.get("thread_id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            target_backend.get_messages(cid, limit).await
        }
        "zalo_send_message" => {
            let cid = args
                .get("conv_id")
                .or_else(|| args.get("to_id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            target_backend.send_message(cid, text).await
        }
        "zalo_schedule_message" => target_backend.schedule_message(args).await,
        "zalo_list_scheduled" => {
            let cid = args.get("conv_id").and_then(Value::as_str);
            target_backend.list_scheduled(cid).await
        }
        "zalo_cancel_scheduled" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or("");
            target_backend.cancel_scheduled(id).await
        }
        "zalo_set_autoreply" => target_backend.set_autoreply(args).await,
        "zalo_set_privacy" => target_backend.set_privacy(args).await,
        "zalo_start_call" => {
            let tid = args.get("target_id").and_then(Value::as_str).unwrap_or("");
            let video = args.get("video").and_then(Value::as_bool).unwrap_or(false);
            target_backend.start_call(tid, video).await
        }
        unknown => Err(anyhow::anyhow!("unknown MCP tool `{unknown}`")),
    };

    match res {
        Ok(val) => (false, val),
        Err(err) => (
            true,
            json!({
                "ok": false,
                "error": err.to_string(),
            }),
        ),
    }
}

/// Processes a single MCP JSON-RPC 2.0 request object (`initialize`, `notifications/initialized`,
/// `ping`, `tools/list`, `tools/call`), returning `None` for JSON-RPC notifications.
pub async fn handle_mcp_jsonrpc(
    backend: &Arc<dyn OlazControlBackend>,
    req: &Value,
) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    if id.is_none() || method.starts_with("notifications/") {
        return None;
    }
    let req_id = id.unwrap_or(Value::Null);

    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "serverInfo": {
                    "name": "olaz-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "tools": { "listChanged": false }
                }
            }
        })),
        "ping" => Some(json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {}
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "tools": mcp_tools_catalog()
            }
        })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {
                        "code": -32602,
                        "message": "Missing required `name` in tools/call params"
                    }
                }));
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (is_err, payload) = execute_mcp_tool(backend, name, &args).await;
            Some(json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "isError": is_err,
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
                    }]
                }
            }))
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        })),
    }
}

/// Runs the newline-delimited JSON-RPC 2.0 MCP loop over arbitrary async `reader` / `writer` streams.
pub async fn run_mcp_io<R, W>(
    backend: Arc<dyn OlazControlBackend>,
    mut reader: R,
    mut writer: W,
) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .context("failed reading MCP JSON-RPC line")?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(trimmed) {
            Ok(req) => handle_mcp_jsonrpc(&backend, &req).await,
            Err(err) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {
                    "code": -32700,
                    "message": format!("Parse error: {err}")
                }
            })),
        };
        if let Some(reply) = resp {
            let mut encoded = serde_json::to_string(&reply)?;
            encoded.push('\n');
            writer.write_all(encoded.as_bytes()).await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

/// Runs the MCP JSON-RPC 2.0 stdio server (`olaz --mcp`) using `tokio::io::stdin` and `tokio::io::stdout`.
pub async fn run_mcp_stdio(backend: Arc<dyn OlazControlBackend>) -> Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    run_mcp_io(backend, stdin, stdout).await
}

fn url_decode_param(raw: &str) -> String {
    let plus_as_space = raw.replace('+', " ");
    percent_decode_str(&plus_as_space)
        .decode_utf8_lossy()
        .into_owned()
}

fn parse_query_map(query_str: &str) -> HashMap<String, String> {
    query_str
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            Some((url_decode_param(k), url_decode_param(v)))
        })
        .collect()
}

fn map_backend_result(res: Result<Value>) -> (u16, Value) {
    match res {
        Ok(v) => (200, v),
        Err(err) => (
            400,
            json!({
                "ok": false,
                "error": err.to_string(),
            }),
        ),
    }
}

/// Dispatches a single HTTP REST request against `backend`, returning `(status_code, json_body)`.
pub async fn handle_http_rest_request(
    backend: &Arc<dyn OlazControlBackend>,
    method: &str,
    path_and_query: &str,
    body: &str,
) -> (u16, Value) {
    let (path, query_str) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let query = parse_query_map(query_str);

    let parse_json_body = || -> std::result::Result<Value, (u16, Value)> {
        if body.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str::<Value>(body).map_err(|e| {
            (
                400,
                json!({
                    "ok": false,
                    "error": format!("Invalid JSON body: {e}"),
                }),
            )
        })
    };

    let query_backend = backend
        .resolve_profile_backend(query.get("profile").map(String::as_str))
        .unwrap_or_else(|| Arc::clone(backend));

    match (method, path) {
        ("GET", "/api/v1/status") | ("GET", "/health") => {
            map_backend_result(query_backend.status().await)
        }
        ("GET", "/api/v1/profiles") => map_backend_result(backend.list_profiles().await),
        ("GET", "/api/v1/threads") => {
            let limit = query
                .get("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(100);
            map_backend_result(query_backend.list_threads(limit).await)
        }
        ("GET", "/api/v1/messages") => {
            let conv_id = query
                .get("conv_id")
                .or_else(|| query.get("thread_id"))
                .cloned()
                .unwrap_or_default();
            let limit = query
                .get("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50);
            map_backend_result(query_backend.get_messages(&conv_id, limit).await)
        }
        ("GET", p) if p.starts_with("/api/v1/threads/") && p.ends_with("/messages") => {
            let conv_id = p
                .trim_start_matches("/api/v1/threads/")
                .trim_end_matches("/messages")
                .trim_matches('/');
            let decoded = url_decode_param(conv_id);
            let limit = query
                .get("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50);
            map_backend_result(query_backend.get_messages(&decoded, limit).await)
        }
        ("POST", "/api/v1/send") => {
            let payload = match parse_json_body() {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let target = backend
                .resolve_profile_backend(
                    payload
                        .get("profile")
                        .and_then(Value::as_str)
                        .or_else(|| query.get("profile").map(String::as_str)),
                )
                .unwrap_or_else(|| Arc::clone(&query_backend));
            let conv_id = payload
                .get("conv_id")
                .or_else(|| payload.get("to_id"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
            map_backend_result(target.send_message(conv_id, text).await)
        }
        ("GET", "/api/v1/schedule") => {
            let conv_id = query.get("conv_id").map(String::as_str);
            map_backend_result(query_backend.list_scheduled(conv_id).await)
        }
        ("POST", "/api/v1/schedule") => {
            let payload = match parse_json_body() {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            map_backend_result(query_backend.schedule_message(&payload).await)
        }
        ("DELETE", "/api/v1/schedule") => {
            let id = if let Some(q_id) = query.get("id").filter(|s| !s.is_empty()) {
                q_id.clone()
            } else {
                let payload = match parse_json_body() {
                    Ok(v) => v,
                    Err(resp) => return resp,
                };
                payload
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            map_backend_result(query_backend.cancel_scheduled(&id).await)
        }
        ("DELETE", p) if p.starts_with("/api/v1/schedule/") => {
            let id = url_decode_param(p.trim_start_matches("/api/v1/schedule/").trim_matches('/'));
            map_backend_result(query_backend.cancel_scheduled(&id).await)
        }
        ("GET", "/api/v1/autoreply") => map_backend_result(query_backend.get_autoreply().await),
        ("POST", "/api/v1/autoreply") => {
            let payload = match parse_json_body() {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            map_backend_result(query_backend.set_autoreply(&payload).await)
        }
        ("GET", "/api/v1/privacy") => map_backend_result(query_backend.get_privacy().await),
        ("POST", "/api/v1/privacy") => {
            let payload = match parse_json_body() {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let target = backend
                .resolve_profile_backend(
                    payload
                        .get("profile")
                        .and_then(Value::as_str)
                        .or_else(|| query.get("profile").map(String::as_str)),
                )
                .unwrap_or_else(|| Arc::clone(&query_backend));
            map_backend_result(target.set_privacy(&payload).await)
        }
        ("POST", "/api/v1/call") => {
            let payload = match parse_json_body() {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let target = backend
                .resolve_profile_backend(
                    payload
                        .get("profile")
                        .and_then(Value::as_str)
                        .or_else(|| query.get("profile").map(String::as_str)),
                )
                .unwrap_or_else(|| Arc::clone(&query_backend));
            let target_id = payload
                .get("target_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let video = payload
                .get("video")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            map_backend_result(target.start_call(target_id, video).await)
        }
        ("POST", "/api/v1/mcp") => match serde_json::from_str::<Value>(body) {
            Ok(req) => match handle_mcp_jsonrpc(backend, &req).await {
                Some(resp) => (200, resp),
                None => (202, json!({ "ok": true })),
            },
            Err(err) => (
                400,
                json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {err}")
                    }
                }),
            ),
        },
        _ => (
            404,
            json!({
                "ok": false,
                "error": format!("Endpoint not found: {method} {path}"),
                "available_endpoints": [
                    "GET /api/v1/status",
                    "GET /api/v1/profiles",
                    "GET /api/v1/threads",
                    "GET /api/v1/messages?conv_id=...&limit=...",
                    "POST /api/v1/send",
                    "GET /api/v1/schedule",
                    "POST /api/v1/schedule",
                    "DELETE /api/v1/schedule",
                    "GET /api/v1/autoreply",
                    "POST /api/v1/autoreply",
                    "GET /api/v1/privacy",
                    "POST /api/v1/privacy",
                    "POST /api/v1/call",
                    "POST /api/v1/mcp"
                ]
            }),
        ),
    }
}

fn check_bearer_auth(headers_raw: &str, expected_token: &str) -> bool {
    for line in headers_raw.lines().skip(1) {
        let Some((name, val)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("authorization") {
            continue;
        }
        let v = val.trim();
        let provided = if v.len() >= 7 && v[..7].eq_ignore_ascii_case("bearer ") {
            v[7..].trim()
        } else {
            continue;
        };
        if provided.len() == expected_token.len()
            && provided
                .as_bytes()
                .ct_eq(expected_token.as_bytes())
                .unwrap_u8()
                == 1
        {
            return true;
        }
    }
    false
}

async fn read_http_request(stream: &mut TcpStream) -> Result<Option<(String, String)>> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() >= MAX_HTTP_REQUEST_BYTES {
            bail!("HTTP request headers exceed maximum size");
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            bail!("incomplete HTTP request headers");
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let body_start = header_end + 4;
    let content_length = head_str
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .next()
        .unwrap_or(0);

    if content_length > MAX_HTTP_REQUEST_BYTES {
        bail!("HTTP request body exceeds 1 MiB limit");
    }

    while buf.len() < body_start + content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    let end = (body_start + content_length).min(buf.len());
    let body_str = String::from_utf8_lossy(&buf[body_start..end]).into_owned();
    Ok(Some((head_str, body_str)))
}

async fn handle_tcp_client(
    mut stream: TcpStream,
    backend: Arc<dyn OlazControlBackend>,
    expected_token: Option<String>,
) {
    let req_res = tokio::time::timeout(Duration::from_secs(10), read_http_request(&mut stream)).await;
    let (head, body) = match req_res {
        Ok(Ok(Some(pair))) => pair,
        Ok(Ok(None)) => return,
        Ok(Err(err)) => {
            let payload = json!({ "ok": false, "error": err.to_string() }).to_string();
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            return;
        }
        Err(_) => return,
    };

    let req_line = head.lines().next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");

    if method == "OPTIONS" {
        let cors = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(cors.as_bytes()).await;
        return;
    }

    if let Some(ref token) = expected_token {
        if !check_bearer_auth(&head, token) {
            let payload = json!({
                "ok": false,
                "error": "Unauthorized: invalid or missing Bearer token"
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json; charset=utf-8\r\nWWW-Authenticate: Bearer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            return;
        }
    }

    let (status, json_val) = handle_http_rest_request(&backend, method, path, &body).await;
    let status_text = match status {
        200 => "200 OK",
        202 => "202 Accepted",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    let body_str = serde_json::to_string(&json_val).unwrap_or_else(|_| "{}".into());
    let resp = format!(
        "HTTP/1.1 {status_text}\r\nContent-Type: application/json; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str
    );
    let _ = stream.write_all(resp.as_bytes()).await;
}

/// Handle to a running loopback HTTP REST & MCP server.
pub struct LocalApiServerHandle {
    local_addr: SocketAddr,
    stop_flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
    abort_handle: AbortHandle,
}

impl LocalApiServerHandle {
    /// Bound loopback socket address (`127.0.0.1:<port>`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops accepting new HTTP connections and aborts the listener loop.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        self.abort_handle.abort();
    }
}

/// Resolves the optional Bearer API token: explicit `override_token` if provided, otherwise `OLAZ_API_TOKEN`.
pub fn resolve_api_token(override_token: Option<String>) -> Option<String> {
    override_token
        .or_else(|| std::env::var("OLAZ_API_TOKEN").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Binds `127.0.0.1:<port>` (pass `port = 0` for an OS-assigned ephemeral loopback port) and spawns
/// the local HTTP REST & MCP server with an explicit `expected_token` (`None` = unauthenticated loopback).
pub async fn spawn_local_api_server_with_token(
    backend: Arc<dyn OlazControlBackend>,
    port: u16,
    expected_token: Option<String>,
) -> Result<LocalApiServerHandle> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .with_context(|| format!("failed to bind local API server on 127.0.0.1:{port}"))?;
    let local_addr = listener.local_addr()?;
    let token = expected_token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_task = Arc::clone(&stop_flag);
    let notify = Arc::new(Notify::new());
    let notify_task = Arc::clone(&notify);

    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = notify_task.notified() => {
                    if stop_task.load(Ordering::SeqCst) {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    let Ok((stream, peer_addr)) = accepted else {
                        if stop_task.load(Ordering::SeqCst) {
                            break;
                        }
                        continue;
                    };
                    if !peer_addr.ip().is_loopback() {
                        continue;
                    }
                    let b = Arc::clone(&backend);
                    let t = token.clone();
                    tokio::spawn(async move {
                        handle_tcp_client(stream, b, t).await;
                    });
                }
            }
        }
    });

    Ok(LocalApiServerHandle {
        local_addr,
        stop_flag,
        notify,
        abort_handle: join.abort_handle(),
    })
}

/// Binds `127.0.0.1:<port>` and spawns the local HTTP REST & MCP server, reading `OLAZ_API_TOKEN`
/// from the environment when set.
pub async fn spawn_local_api_server(
    backend: Arc<dyn OlazControlBackend>,
    port: u16,
) -> Result<LocalApiServerHandle> {
    let token = resolve_api_token(None);
    spawn_local_api_server_with_token(backend, port, token).await
}

/// Runs the local HTTP REST & MCP server on `127.0.0.1:<port>` until an unrecoverable accept error.
pub async fn run_local_api_server(backend: Arc<dyn OlazControlBackend>, port: u16) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .with_context(|| format!("failed to bind local API server on 127.0.0.1:{port}"))?;
    let token = resolve_api_token(None);
    loop {
        let (stream, peer_addr) = listener.accept().await?;
        if !peer_addr.ip().is_loopback() {
            continue;
        }
        let b = Arc::clone(&backend);
        let t = token.clone();
        tokio::spawn(async move {
            handle_tcp_client(stream, b, t).await;
        });
    }
}
