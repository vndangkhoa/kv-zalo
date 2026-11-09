//! Native HTTP(S) and WebSocket proxy matching `NetworkProxy.swift` and `HostScripts/network-shim.js`.
//!
//! Owns the shared `reqwest::Client` (rustls TLS, persistent `ZaloCookieJar`, DoH-capable
//! `ZaloDnsResolver`, official `ZaloPC-darwin-23v694` user agent, no browser CORS) and
//! multiplexes WebSocket connections (`ws.open`, `ws.send`, `ws.close`).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine as _;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, WebSocketConfig};
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async_tls_with_config, Connector,
};
use tracing::{debug, trace, warn};
use url::Url;

use crate::cookies::ZaloCookieJar;
use crate::doh::{is_zalo_cdn, parse_upstream_proxy, ZaloDnsResolver};

/// Default Zalo PC macOS User-Agent (`OfficialClientHost.userAgent(clientType: "23", clientVersion: "695")`).
pub const DEFAULT_USER_AGENT: &str = "ZaloPC-darwin-23v695";

/// Origin header sent on WebSocket upgrades (`Electron` renderer loaded from `file://`).
pub const WEBSOCKET_ORIGIN: &str = "file://";

/// Default HTTP request timeout (`60s`).
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum WebSocket frame & message payload size (`64 MiB`, matching `NetworkProxy.swift:221`).
pub const MAX_WS_MESSAGE_BYTES: usize = 64 << 20;

/// Callback invoked with `(url, status_code, body_bytes)` for completed HTTP responses
/// (used to observe `/api/session/kickout-info` and `qr-id.zaloapp.com/polling/qr/*`).
pub type ResponseObserver = Arc<dyn Fn(&str, u16, &[u8]) + Send + Sync>;

/// Event pushed back to `window.__zaloNet.socketEvent(id, type, payload)` in the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsEvent {
    pub id: u64,
    #[serde(rename = "type", alias = "kind")]
    pub kind: String,
    pub payload: Value,
}

/// Thread-safe sink receiving [`WsEvent`] notifications from active WebSockets.
#[derive(Clone)]
pub struct WsEventSink(Arc<dyn Fn(WsEvent) + Send + Sync>);

impl std::fmt::Debug for WsEventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsEventSink(..)")
    }
}

impl WsEventSink {
    /// Wraps a closure as a [`WsEventSink`].
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(WsEvent) + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Creates a [`WsEventSink`] forwarding events into an unbounded Tokio channel.
    pub fn from_sender(tx: mpsc::UnboundedSender<WsEvent>) -> Self {
        Self::new(move |ev| {
            let _ = tx.send(ev);
        })
    }

    /// Creates a no-op sink (useful when invoking pure HTTP requests in tests).
    pub fn noop() -> Self {
        Self::new(|_| {})
    }

    /// Delivers an event to the sink.
    pub fn emit(&self, event: WsEvent) {
        (self.0)(event);
    }
}

impl<F> From<F> for WsEventSink
where
    F: Fn(WsEvent) + Send + Sync + 'static,
{
    fn from(f: F) -> Self {
        Self::new(f)
    }
}

impl From<mpsc::UnboundedSender<WsEvent>> for WsEventSink {
    fn from(tx: mpsc::UnboundedSender<WsEvent>) -> Self {
        Self::from_sender(tx)
    }
}

enum WsCommand {
    Send(Message),
    Close { code: u16, reason: String },
}

struct ActiveSocket {
    tx: mpsc::UnboundedSender<WsCommand>,
    abort_handle: AbortHandle,
}

/// Native transport proxy handling HTTP(S) requests and realtime WebSocket streams.
#[derive(Clone)]
pub struct NetworkProxy {
    client: reqwest::Client,
    jar: ZaloCookieJar,
    dns_resolver: Arc<ZaloDnsResolver>,
    user_agent: String,
    proxy_url: Option<String>,
    rustls_config: Arc<rustls::ClientConfig>,
    sockets: Arc<DashMap<u64, ActiveSocket>>,
    response_observer: Arc<RwLock<Option<ResponseObserver>>>,
}

impl std::fmt::Debug for NetworkProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkProxy")
            .field("user_agent", &self.user_agent)
            .field("proxy_url", &self.proxy_url)
            .field("open_sockets", &self.sockets.len())
            .finish()
    }
}

fn build_proxy_http_client(
    jar: &ZaloCookieJar,
    dns_resolver: &Arc<ZaloDnsResolver>,
    user_agent: &str,
    proxy_url: Option<&str>,
) -> Result<(reqwest::Client, Option<String>)> {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .cookie_provider(jar.inner())
        .dns_resolver(Arc::clone(dns_resolver))
        .user_agent(user_agent)
        .timeout(DEFAULT_HTTP_TIMEOUT);

    let normalized_proxy = match proxy_url {
        Some(raw) => match parse_upstream_proxy(raw)? {
            Some((u, proxy)) => {
                builder = builder.proxy(proxy);
                Some(u)
            }
            None => {
                builder = builder.no_proxy();
                None
            }
        },
        None => None,
    };

    let client = builder
        .build()
        .context("failed to build NetworkProxy reqwest::Client")?;
    Ok((client, normalized_proxy))
}

impl NetworkProxy {
    /// Constructs a new `NetworkProxy` with default user agent (`ZaloPC-darwin-23v694`) and DoH resolver.
    pub fn new(jar: ZaloCookieJar) -> Result<Self> {
        Self::with_config(jar, Arc::new(ZaloDnsResolver::new()), DEFAULT_USER_AGENT)
    }

    /// Constructs a `NetworkProxy` with custom [`ZaloDnsResolver`] and `user_agent`,
    /// honoring `OLAZ_PROXY` (or `dns_resolver.proxy_url()`) when configured.
    pub fn with_config(
        jar: ZaloCookieJar,
        dns_resolver: Arc<ZaloDnsResolver>,
        user_agent: impl Into<String>,
    ) -> Result<Self> {
        let user_agent = user_agent.into();
        let rustls_config = build_rustls_client_config()?;
        let env_proxy = std::env::var("OLAZ_PROXY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| dns_resolver.proxy_url().map(str::to_owned));

        let (client, proxy_url) =
            build_proxy_http_client(&jar, &dns_resolver, &user_agent, env_proxy.as_deref())?;

        Ok(Self {
            client,
            jar,
            dns_resolver,
            user_agent,
            proxy_url,
            rustls_config,
            sockets: Arc::new(DashMap::new()),
            response_observer: Arc::new(RwLock::new(None)),
        })
    }

    /// Builder-style helper to attach (or clear when `None`) an upstream SOCKS5/HTTP(S) proxy URL.
    pub fn with_proxy_url(mut self, proxy_url: Option<&str>) -> Result<Self> {
        let explicit = proxy_url.unwrap_or("");
        let (client, normalized) =
            build_proxy_http_client(&self.jar, &self.dns_resolver, &self.user_agent, Some(explicit))?;
        self.client = client;
        self.proxy_url = normalized;
        Ok(self)
    }

    /// Returns the active upstream proxy URL configured on this `NetworkProxy`, if any.
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    /// Attaches or clears the HTTP response observer callback.
    pub fn set_response_observer(&self, observer: Option<ResponseObserver>) {
        *self.response_observer.write() = observer;
    }

    /// Builder-style helper to set the HTTP response observer.
    pub fn with_response_observer(self, observer: ResponseObserver) -> Self {
        self.set_response_observer(Some(observer));
        self
    }

    /// Returns a reference to the shared [`ZaloCookieJar`].
    pub fn cookie_jar(&self) -> &ZaloCookieJar {
        &self.jar
    }

    /// Returns a reference to the shared [`ZaloDnsResolver`].
    pub fn dns_resolver(&self) -> &Arc<ZaloDnsResolver> {
        &self.dns_resolver
    }

    /// Returns a reference to the underlying `reqwest::Client`.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Returns the number of currently tracked WebSockets.
    pub fn active_socket_count(&self) -> usize {
        self.sockets.len()
    }

    /// Cancels and closes all open WebSockets (e.g. on logout or window shutdown).
    pub fn close_all_sockets(&self) {
        let ids: Vec<u64> = self.sockets.iter().map(|entry| *entry.key()).collect();
        for id in ids {
            if let Some((_, sock)) = self.sockets.remove(&id) {
                let _ = sock.tx.send(WsCommand::Close {
                    code: 1001,
                    reason: String::new(),
                });
                sock.abort_handle.abort();
            }
        }
    }

    /// Dispatches a `znet` IPC request from `HostScripts/network-shim.js`.
    ///
    /// Supports:
    /// - `"http"` (or omitted `op` when `url` is present)
    /// - `"ws.open"`
    /// - `"ws.send"`
    /// - `"ws.close"`
    pub async fn handle_request(
        &self,
        payload: Value,
        ws_sink: WsEventSink,
    ) -> std::result::Result<Value, String> {
        let Some(obj) = payload.as_object() else {
            return Err("bad message".to_owned());
        };

        let op = match obj.get("op").and_then(|v| v.as_str()) {
            Some(op) => op,
            None if obj.contains_key("url") => "http",
            None => return Err("bad message".to_owned()),
        };

        match op {
            "http" => Ok(self.perform_http(obj).await),
            "ws.open" => {
                self.open_socket(obj, ws_sink);
                Ok(Value::Null)
            }
            "ws.send" => {
                self.send_socket(obj);
                Ok(Value::Null)
            }
            "ws.close" => {
                self.close_socket(obj, &ws_sink);
                Ok(Value::Null)
            }
            other => Err(format!("unknown op {other}")),
        }
    }

    async fn perform_http(&self, body: &Map<String, Value>) -> Value {
        let Some(url_str) = body.get("url").and_then(|v| v.as_str()) else {
            return json!({ "error": "bad url" });
        };
        let Ok(parsed_url) = Url::parse(url_str) else {
            return json!({ "error": "bad url" });
        };
        match parsed_url.scheme() {
            "http" | "https" => {}
            _ => return json!({ "error": "bad url" }),
        }

        let method_str = body
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET");
        let Ok(method) = reqwest::Method::from_bytes(method_str.as_bytes()) else {
            return json!({ "error": format!("bad method {method_str}") });
        };

        let encoded_body = body.get("body").and_then(|v| v.as_object());
        let is_multipart = encoded_body
            .and_then(|b| b.get("kind"))
            .and_then(|v| v.as_str())
            == Some("formdata");

        let mut header_map = HeaderMap::new();
        let mut has_content_type = false;
        if let Some(hdrs) = body.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in hdrs {
                let Some(val_str) = v.as_str() else {
                    continue;
                };
                if is_multipart && k.eq_ignore_ascii_case("content-type") {
                    // Multipart form-data always overrides caller-guessed Content-Type with its boundary.
                    continue;
                }
                if let (Ok(hname), Ok(hval)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(val_str),
                ) {
                    if hname == CONTENT_TYPE {
                        has_content_type = true;
                    }
                    header_map.insert(hname, hval);
                }
            }
        }

        let mut req_builder = self.client.request(method.clone(), parsed_url.clone());

        if let Some(timeout_ms) = body.get("timeout").and_then(|v| v.as_f64()) {
            if timeout_ms > 0.0 {
                req_builder = req_builder.timeout(Duration::from_secs_f64(timeout_ms / 1000.0));
            }
        }

        if let Some(encoded) = encoded_body {
            if !is_multipart && !has_content_type {
                if let Some(ct) = encoded
                    .get("contentType")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    if let Ok(hval) = HeaderValue::from_str(ct) {
                        header_map.insert(CONTENT_TYPE, hval);
                        has_content_type = true;
                    }
                }
            }

            match encoded.get("kind").and_then(|v| v.as_str()) {
                Some("text") => {
                    let text = encoded
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !has_content_type {
                        header_map.insert(
                            CONTENT_TYPE,
                            HeaderValue::from_static("text/plain;charset=UTF-8"),
                        );
                    }
                    req_builder = req_builder.body(text.as_bytes().to_vec());
                }
                Some("base64") => {
                    let b64 = encoded
                        .get("data")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match decode_b64(b64) {
                        Ok(bytes) => {
                            req_builder = req_builder.body(bytes);
                        }
                        Err(err) => {
                            return json!({ "error": format!("invalid base64 request body: {err}") });
                        }
                    }
                }
                Some("formdata") => {
                    let mut form = Form::new();
                    if let Some(parts) = encoded.get("parts").and_then(|v| v.as_array()) {
                        for part_val in parts {
                            let Some(part_obj) = part_val.as_object() else {
                                continue;
                            };
                            let name = part_obj
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned();
                            if let Some(filename) =
                                part_obj.get("filename").and_then(|v| v.as_str())
                            {
                                let b64 = part_obj
                                    .get("data")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let bytes = decode_b64(b64).unwrap_or_default();
                                let mime = part_obj
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("application/octet-stream");
                                let part = Part::bytes(bytes).file_name(filename.to_owned());
                                let part = match part.mime_str(mime) {
                                    Ok(p) => p,
                                    Err(_) => Part::bytes(
                                        decode_b64(b64).unwrap_or_default(),
                                    )
                                    .file_name(filename.to_owned())
                                    .mime_str("application/octet-stream")
                                    .expect("static valid mime"),
                                };
                                form = form.part(name, part);
                            } else {
                                let value = part_obj
                                    .get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_owned();
                                form = form.text(name, value);
                            }
                        }
                    }
                    req_builder = req_builder.multipart(form);
                }
                _ => {}
            }
        }

        req_builder = req_builder.headers(header_map);

        match req_builder.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let status_text = resp
                    .status()
                    .canonical_reason()
                    .unwrap_or("")
                    .to_owned();
                let final_url = resp.url().to_string();

                let mut resp_headers = Map::new();
                for (name, val) in resp.headers() {
                    let key = name.as_str().to_ascii_lowercase();
                    // Do not leak raw Set-Cookie into XHR headers (already handled by cookie jar).
                    if key == "set-cookie" {
                        continue;
                    }
                    let val_str = String::from_utf8_lossy(val.as_bytes()).into_owned();
                    match resp_headers.get_mut(&key) {
                        Some(Value::String(existing)) => {
                            existing.push_str(", ");
                            existing.push_str(&val_str);
                        }
                        _ => {
                            resp_headers.insert(key, Value::String(val_str));
                        }
                    }
                }

                match resp.bytes().await {
                    Ok(bytes) => {
                        if let Err(err) = self.jar.save() {
                            warn!("failed to persist cookies after HTTP {method} {final_url}: {err}");
                        }

                        let observer = self.response_observer.read().clone();
                        if let Some(obs) = observer {
                            obs(&final_url, status, &bytes);
                        }

                        json!({
                            "status": status,
                            "statusText": status_text,
                            "headers": resp_headers,
                            "url": final_url,
                            "body": STANDARD.encode(&bytes),
                        })
                    }
                    Err(err) => {
                        json!({
                            "error": err.to_string(),
                            "timedOut": err.is_timeout(),
                        })
                    }
                }
            }
            Err(err) => {
                debug!(
                    "HTTP {} {} failed (timeout={}): {}",
                    method,
                    parsed_url,
                    err.is_timeout(),
                    err
                );
                json!({
                    "error": err.to_string(),
                    "timedOut": err.is_timeout(),
                })
            }
        }
    }

    fn open_socket(&self, body: &Map<String, Value>, ws_sink: WsEventSink) {
        let Some(id) = parse_socket_id(body.get("id")) else {
            return;
        };
        let Some(url_str) = body.get("url").and_then(|v| v.as_str()) else {
            return;
        };
        let Ok(parsed_url) = Url::parse(url_str) else {
            ws_sink.emit(WsEvent {
                id,
                kind: "error".to_owned(),
                payload: json!({}),
            });
            ws_sink.emit(WsEvent {
                id,
                kind: "close".to_owned(),
                payload: json!({ "code": 1006, "reason": "", "wasClean": false }),
            });
            return;
        };
        match parsed_url.scheme() {
            "ws" | "wss" => {}
            _ => {
                ws_sink.emit(WsEvent {
                    id,
                    kind: "error".to_owned(),
                    payload: json!({}),
                });
                ws_sink.emit(WsEvent {
                    id,
                    kind: "close".to_owned(),
                    payload: json!({ "code": 1006, "reason": "", "wasClean": false }),
                });
                return;
            }
        }

        let protocols: Vec<String> = match body.get("protocols") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .filter(|s| !s.is_empty())
                .collect(),
            Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        };

        let cookie_header = self.jar.cookie_header_for_url(url_str);
        let user_agent = self.user_agent.clone();
        let url_owned = url_str.to_owned();
        let rustls_config = Arc::clone(&self.rustls_config);
        let dns_resolver = Arc::clone(&self.dns_resolver);
        let sockets = Arc::clone(&self.sockets);

        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<WsCommand>();

        let join_handle = tokio::spawn(async move {
            let build_req = || {
                let mut req = url_owned.as_str().into_client_request()?;
                let hdrs = req.headers_mut();
                hdrs.insert("Origin", HeaderValue::from_static(WEBSOCKET_ORIGIN));
                if let Ok(ua) = HeaderValue::from_str(&user_agent) {
                    hdrs.insert("User-Agent", ua);
                }
                if !protocols.is_empty() {
                    if let Ok(proto_val) = HeaderValue::from_str(&protocols.join(", ")) {
                        hdrs.insert("Sec-WebSocket-Protocol", proto_val);
                    }
                }
                if let Some(ref cookie_str) = cookie_header {
                    if let Ok(c_val) = HeaderValue::from_str(cookie_str) {
                        hdrs.insert("Cookie", c_val);
                    }
                }
                Ok::<_, tokio_tungstenite::tungstenite::Error>(req)
            };

            let Ok(request) = build_req() else {
                sockets.remove(&id);
                emit_abnormal_close(&ws_sink, id);
                return;
            };

            let mut ws_config = WebSocketConfig::default();
            ws_config.max_message_size = Some(MAX_WS_MESSAGE_BYTES);
            ws_config.max_frame_size = Some(MAX_WS_MESSAGE_BYTES);

            let connector = Connector::Rustls(Arc::clone(&rustls_config));
            let connect_res = connect_async_tls_with_config(
                request,
                Some(ws_config),
                false,
                Some(connector.clone()),
            )
            .await;

            // If OS DNS failed for a Zalo WebSocket host (`wsN-msg.chat.zalo.me`), retry via DoH.
            let connect_res = match connect_res {
                Ok(pair) => Ok(pair),
                Err(primary_err) => {
                    let host = parsed_url.host_str().unwrap_or("");
                    let port = parsed_url
                        .port_or_known_default()
                        .unwrap_or(if parsed_url.scheme() == "ws" { 80 } else { 443 });
                    if is_zalo_cdn(host) {
                        if let Ok(addrs) = dns_resolver.resolve_host(host).await {
                            let mut doh_connected = None;
                            for mut addr in addrs.into_iter().take(4) {
                                addr.set_port(port);
                                if let Ok(tcp) = TcpStream::connect(addr).await {
                                    if let Ok(retry_req) = build_req() {
                                        if let Ok(ok_pair) = client_async_tls_with_config(
                                            retry_req,
                                            tcp,
                                            Some(ws_config),
                                            Some(connector.clone()),
                                        )
                                        .await
                                        {
                                            doh_connected = Some(ok_pair);
                                            break;
                                        }
                                    }
                                }
                            }
                            doh_connected.ok_or(primary_err)
                        } else {
                            Err(primary_err)
                        }
                    } else {
                        Err(primary_err)
                    }
                }
            };

            let (ws_stream, handshake_resp) = match connect_res {
                Ok(pair) => pair,
                Err(err) => {
                    debug!("ws#{id} handshake failed for {url_owned}: {err}");
                    sockets.remove(&id);
                    emit_abnormal_close(&ws_sink, id);
                    return;
                }
            };

            // Emit `open` ONLY after HTTP 101 Switching Protocols succeeds.
            let negotiated_proto = handshake_resp
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();

            ws_sink.emit(WsEvent {
                id,
                kind: "open".to_owned(),
                payload: json!({ "protocol": negotiated_proto }),
            });

            let (mut ws_write, mut ws_read) = ws_stream.split();

            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(WsCommand::Send(msg)) => {
                                if let Err(err) = ws_write.send(msg).await {
                                    debug!("ws#{id} send failed: {err}");
                                    sockets.remove(&id);
                                    emit_abnormal_close(&ws_sink, id);
                                    break;
                                }
                            }
                            Some(WsCommand::Close { code, reason }) => {
                                let frame = CloseFrame {
                                    code: CloseCode::from(code),
                                    reason: reason.into(),
                                };
                                let _ = ws_write.send(Message::Close(Some(frame))).await;
                                let _ = ws_write.close().await;
                                sockets.remove(&id);
                                break;
                            }
                            None => {
                                sockets.remove(&id);
                                break;
                            }
                        }
                    }
                    incoming = ws_read.next() => {
                        match incoming {
                            Some(Ok(Message::Text(text))) => {
                                ws_sink.emit(WsEvent {
                                    id,
                                    kind: "text".to_owned(),
                                    payload: json!({ "text": text.to_string() }),
                                });
                            }
                            Some(Ok(Message::Binary(bin))) => {
                                trace!("ws#{id} in {}", describe_zalo_frame(&bin));
                                ws_sink.emit(WsEvent {
                                    id,
                                    kind: "binary".to_owned(),
                                    payload: json!({ "data": STANDARD.encode(&bin) }),
                                });
                            }
                            Some(Ok(Message::Ping(_))) => {
                                let _ = ws_write.flush().await;
                            }
                            Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => {}
                            Some(Ok(Message::Close(close_frame))) => {
                                sockets.remove(&id);
                                let (code, reason) = match close_frame {
                                    Some(cf) => (u16::from(cf.code), cf.reason.to_string()),
                                    None => (1000, String::new()),
                                };
                                ws_sink.emit(WsEvent {
                                    id,
                                    kind: "close".to_owned(),
                                    payload: json!({
                                        "code": code,
                                        "reason": reason,
                                        "wasClean": true,
                                    }),
                                });
                                break;
                            }
                            Some(Err(err)) => {
                                debug!("ws#{id} stream error: {err}");
                                sockets.remove(&id);
                                emit_abnormal_close(&ws_sink, id);
                                break;
                            }
                            None => {
                                // Underlying TCP/TLS stream dropped without a WebSocket Close frame.
                                sockets.remove(&id);
                                emit_abnormal_close(&ws_sink, id);
                                break;
                            }
                        }
                    }
                }
            }
        });

        self.sockets.insert(
            id,
            ActiveSocket {
                tx: cmd_tx,
                abort_handle: join_handle.abort_handle(),
            },
        );
    }

    fn send_socket(&self, body: &Map<String, Value>) {
        let Some(id) = parse_socket_id(body.get("id")) else {
            return;
        };
        let Some(sock) = self.sockets.get(&id) else {
            return;
        };

        let msg = if let Some(text) = body.get("text").and_then(|v| v.as_str()) {
            Message::Text(text.to_owned().into())
        } else {
            let b64 = body.get("data").and_then(|v| v.as_str()).unwrap_or("");
            let bytes = decode_b64(b64).unwrap_or_default();
            trace!("ws#{id} out {}", describe_zalo_frame(&bytes));
            Message::Binary(bytes.into())
        };

        let _ = sock.tx.send(WsCommand::Send(msg));
    }

    fn close_socket(&self, body: &Map<String, Value>, ws_sink: &WsEventSink) {
        let Some(id) = parse_socket_id(body.get("id")) else {
            return;
        };
        let Some((_, sock)) = self.sockets.remove(&id) else {
            return;
        };

        let code = body
            .get("code")
            .and_then(|v| v.as_u64())
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or(1000);
        let reason = body
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        let _ = sock.tx.send(WsCommand::Close {
            code,
            reason: reason.clone(),
        });

        ws_sink.emit(WsEvent {
            id,
            kind: "close".to_owned(),
            payload: json!({
                "code": code,
                "reason": reason,
                "wasClean": true,
            }),
        });
    }
}

/// Checks whether an HTTP URL is one of the Zalo lifecycle endpoints observed by the host
/// (`/api/session/kickout-info` or `qr-id.zaloapp.com/polling/qr/*`).
pub fn is_observed_zalo_endpoint(url_str: &str) -> bool {
    let Ok(url) = Url::parse(url_str) else {
        return false;
    };
    if url.path().ends_with("/api/session/kickout-info") {
        return true;
    }
    url.host_str()
        .is_some_and(|h| h.eq_ignore_ascii_case("qr-id.zaloapp.com") || h.starts_with("qr-id."))
        && url.path().starts_with("/polling/qr/")
}

/// Formats Zalo's 4-byte binary realtime frame header (`[version u8][cmd u16 LE][subCmd u8]`)
/// without logging payload contents (`NetworkProxy.swift:34-39`).
pub fn describe_zalo_frame(data: &[u8]) -> String {
    if data.len() < 4 {
        return format!("len={}", data.len());
    }
    let cmd = u16::from_le_bytes([data[1], data[2]]);
    format!(
        "v={} cmd={} sub={} len={}",
        data[0],
        cmd,
        data[3],
        data.len()
    )
}

fn emit_abnormal_close(ws_sink: &WsEventSink, id: u64) {
    ws_sink.emit(WsEvent {
        id,
        kind: "error".to_owned(),
        payload: json!({}),
    });
    ws_sink.emit(WsEvent {
        id,
        kind: "close".to_owned(),
        payload: json!({
            "code": 1006,
            "reason": "",
            "wasClean": false,
        }),
    });
}

fn parse_socket_id(val: Option<&Value>) -> Option<u64> {
    val.and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().filter(|&n| n >= 0).map(|n| n as u64))
    })
}

fn decode_b64(input: &str) -> std::result::Result<Vec<u8>, base64::DecodeError> {
    let trimmed = input.trim();
    STANDARD
        .decode(trimmed)
        .or_else(|_| STANDARD_NO_PAD.decode(trimmed))
}

fn build_rustls_client_config() -> Result<Arc<rustls::ClientConfig>> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("failed to configure rustls TLS protocol versions")?
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(Arc::new(config))
}

#[cfg(test)]
mod ctdozlite_proxy_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    #[test]
    fn dns_resolver_and_network_proxy_configure_upstream_proxy() {
        let resolver = ZaloDnsResolver::new()
            .with_proxy_url(Some("socks5h://127.0.0.1:1080"))
            .expect("socks5h proxy accepted");
        assert_eq!(resolver.proxy_url(), Some("socks5h://127.0.0.1:1080"));

        let cleared = resolver.with_proxy_url(None).expect("clear proxy");
        assert_eq!(cleared.proxy_url(), None);

        assert!(ZaloDnsResolver::new()
            .with_proxy_url(Some("ftp://127.0.0.1:21"))
            .is_err());

        let jar = ZaloCookieJar::new_in_memory();
        let proxy = NetworkProxy::new(jar)
            .expect("network proxy")
            .with_proxy_url(Some("socks5://127.0.0.1:1080"))
            .expect("attach socks5");
        assert_eq!(proxy.proxy_url(), Some("socks5://127.0.0.1:1080"));
    }

    #[tokio::test]
    async fn network_proxy_routes_http_through_configured_upstream_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock http proxy");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept proxy client");
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .await
                .expect("read request line");
            loop {
                let mut hdr = String::new();
                let n = reader.read_line(&mut hdr).await.expect("read header");
                if n == 0 || hdr == "\r\n" {
                    break;
                }
            }
            let body = b"proxied-ok";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            write_half.write_all(resp.as_bytes()).await.unwrap();
            write_half.write_all(body).await.unwrap();
            write_half.flush().await.unwrap();
            request_line
        });

        let jar = ZaloCookieJar::new_in_memory();
        let proxy_url = format!("http://{addr}");
        let net_proxy = NetworkProxy::new(jar)
            .expect("new proxy")
            .with_proxy_url(Some(&proxy_url))
            .expect("configure http proxy");

        let resp = net_proxy
            .handle_request(
                json!({
                    "op": "http",
                    "method": "GET",
                    "url": "http://example.invalid/ping"
                }),
                WsEventSink::noop(),
            )
            .await
            .expect("handle_request");

        assert_eq!(resp.get("status").and_then(Value::as_u64), Some(200));
        let b64 = resp.get("body").and_then(Value::as_str).unwrap_or("");
        assert_eq!(STANDARD.decode(b64).unwrap(), b"proxied-ok");

        let req_line = server.await.expect("proxy task");
        assert!(
            req_line.contains("http://example.invalid/ping"),
            "expected absolute-URI HTTP proxy request line, got: {req_line}"
        );
    }
}
