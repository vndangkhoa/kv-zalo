//! Stateless HTTP, HTTP `Range` media, and RFC 6455 WebSocket relay engine.
//!
//! Security & Zero-Persistence Invariants:
//! - Builds `reqwest::Client` WITHOUT `.cookie_store(...)` or `.cookie_provider(...)`.
//!   Every request carries only the ephemeral `Cookie` header explicitly supplied in that
//!   single browser call (`X-Olaz-Cookie` or `__olaz_ws_open.cookie`) and drops all buffers
//!   immediately upon completion.
//! - Never writes any file, temp file, cache file, or log containing cookies or bodies.
//! - Validates every outbound HTTP/media/WS URL and redirect hop through [`AllowlistValidator`],
//!   connecting strictly to SSRF-verified `SocketAddr`s so DNS rebinding is impossible.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine as _;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, RANGE, SET_COOKIE, USER_AGENT};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::{Message, Role, WebSocketConfig};
use tokio_tungstenite::{client_async_tls_with_config, Connector, WebSocketStream};

use crate::allowlist::{AllowlistError, AllowlistValidator};

/// Official Zalo PC macOS User-Agent enforced on all relayed HTTP, media, and WS requests.
pub const ZALO_USER_AGENT: &str = "ZaloPC-darwin-23v694";

/// Default Electron `Origin` header sent on Zalo WebSocket upgrades.
pub const DEFAULT_WS_ORIGIN: &str = "file://";

/// Timeout waiting for the browser's initial `{"type":"__olaz_ws_open"}` frame (`15s`).
const WS_OPEN_FRAME_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum WebSocket message & frame payload size (`64 MiB`).
const MAX_WS_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Default outbound HTTP request timeout (`60s`).
const HTTP_RELAY_TIMEOUT: Duration = Duration::from_secs(60);

/// Wraps a `TcpStream` together with any unread bytes left over after the HTTP/1.1 `\r\n\r\n`
/// upgrade delimiter so pipelined WebSocket frames are never lost.
pub struct PrefixedTcpStream {
    prefix: Bytes,
    offset: usize,
    inner: TcpStream,
}

impl PrefixedTcpStream {
    pub fn new(prefix: Bytes, inner: TcpStream) -> Self {
        Self {
            prefix,
            offset: 0,
            inner,
        }
    }
}

impl AsyncRead for PrefixedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.offset < self.prefix.len() {
            let rem = &self.prefix[self.offset..];
            let to_copy = rem.len().min(buf.remaining());
            buf.put_slice(&rem[..to_copy]);
            self.offset += to_copy;
            if self.offset >= self.prefix.len() {
                self.prefix = Bytes::new();
                self.offset = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for PrefixedTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[derive(Debug, Deserialize)]
struct BrowserWsOpenFrame {
    #[serde(rename = "type")]
    kind: String,
    url: String,
    #[serde(default)]
    cookie: String,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    protocols: Vec<String>,
}

/// Stateless outbound relay shared across worker tasks.
#[derive(Clone)]
pub struct StatelessRelay {
    http_client: reqwest::Client,
    rustls_config: Arc<rustls::ClientConfig>,
    allowlist: AllowlistValidator,
}

impl std::fmt::Debug for StatelessRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatelessRelay")
            .field("allowlist", &self.allowlist)
            .finish()
    }
}

impl StatelessRelay {
    pub fn new(allowlist: AllowlistValidator) -> Result<Self> {
        let rustls_config = build_rustls_client_config()?;
        let redirect_allowlist = allowlist.clone();
        let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many redirects");
            }
            if let Err(err) = redirect_allowlist.validate_url_surface(attempt.url(), false) {
                return attempt.error(err);
            }
            attempt.follow()
        });

        // CRITICAL ZERO-PERSISTENCE INVARIANT:
        // Do NOT enable `cookie_store` or `cookie_provider`!
        let http_client = reqwest::Client::builder()
            .use_rustls_tls()
            .dns_resolver(Arc::new(allowlist.clone()))
            .redirect(redirect_policy)
            .user_agent(ZALO_USER_AGENT)
            .timeout(HTTP_RELAY_TIMEOUT)
            .build()
            .context("failed to build stateless reqwest::Client")?;

        Ok(Self {
            http_client,
            rustls_config,
            allowlist,
        })
    }

    pub fn allowlist(&self) -> &AllowlistValidator {
        &self.allowlist
    }

    /// Executes a stateless `POST /__olaz/http` request and streams the response onto `socket`.
    pub async fn handle_http_relay(
        &self,
        target_url_raw: &str,
        method_raw: &str,
        encoded_headers: Option<&str>,
        ephemeral_cookie: Option<&str>,
        body: Bytes,
        socket: &mut TcpStream,
    ) -> Result<()> {
        let (target_url, _addrs) = match self.allowlist.validate_http_target(target_url_raw).await {
            Ok(ok) => ok,
            Err(AllowlistError::InvalidUrl(msg)) => {
                write_json_response(
                    socket,
                    400,
                    "Bad Request",
                    &json!({ "error": "invalid_target_url", "detail": msg }),
                )
                .await?;
                return Ok(());
            }
            Err(err) => {
                write_json_response(
                    socket,
                    403,
                    "Forbidden",
                    &json!({ "error": "target_forbidden", "detail": err.to_string() }),
                )
                .await?;
                return Ok(());
            }
        };

        let method = Method::from_bytes(method_raw.trim().as_bytes()).unwrap_or(Method::GET);
        let mut req_headers = decode_caller_headers(encoded_headers);
        req_headers.insert(USER_AGENT, HeaderValue::from_static(ZALO_USER_AGENT));

        if let Some(cookie_str) = ephemeral_cookie.map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(hv) = HeaderValue::from_str(cookie_str) {
                req_headers.insert(COOKIE, hv);
            }
        } else {
            req_headers.remove(COOKIE);
        }

        let mut builder = self
            .http_client
            .request(method.clone(), target_url)
            .headers(req_headers);

        if !body.is_empty() && method != Method::GET && method != Method::HEAD {
            builder = builder.body(body);
        }

        let mut upstream = match builder.send().await {
            Ok(resp) => resp,
            Err(err) => {
                // If blocked by redirect policy allowlist error, return 403 Forbidden.
                let is_allowlist_err = std::error::Error::source(&err)
                    .and_then(|src| src.downcast_ref::<AllowlistError>())
                    .is_some();
                if is_allowlist_err {
                    write_json_response(
                        socket,
                        403,
                        "Forbidden",
                        &json!({ "error": "redirect_target_forbidden" }),
                    )
                    .await?;
                } else {
                    write_json_response(
                        socket,
                        502,
                        "Bad Gateway",
                        &json!({ "error": "upstream_unreachable" }),
                    )
                    .await?;
                }
                return Ok(());
            }
        };

        let upstream_status = upstream.status().as_u16();
        let mut set_cookies = Vec::new();
        for val in upstream.headers().get_all(SET_COOKIE) {
            if let Ok(s) = val.to_str() {
                set_cookies.push(s.to_owned());
            } else {
                set_cookies.push(String::from_utf8_lossy(val.as_bytes()).into_owned());
            }
        }

        let mut upstream_hdrs_map = BTreeMap::new();
        let mut content_type = "application/octet-stream".to_string();
        for (name, val) in upstream.headers().iter() {
            let lower = name.as_str().to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "set-cookie"
                    | "transfer-encoding"
                    | "connection"
                    | "keep-alive"
                    | "content-length"
                    | "strict-transport-security"
            ) {
                continue;
            }
            if let Ok(v_str) = val.to_str() {
                if lower == "content-type" {
                    content_type = v_str.to_owned();
                }
                upstream_hdrs_map.insert(lower, v_str.to_owned());
            }
        }

        let set_cookie_json = ascii_safe_json_header(&json!(set_cookies));
        let upstream_hdrs_json = ascii_safe_json_header(&json!(upstream_hdrs_map));

        // Stream response body with HTTP/1.1 chunked transfer encoding so arbitrarily large
        // responses never accumulate in server RAM.
        let head = format!(
            concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: {ct}\r\n",
                "Cache-Control: no-store\r\n",
                "X-Content-Type-Options: nosniff\r\n",
                "X-Olaz-Upstream-Status: {st}\r\n",
                "X-Olaz-Upstream-Headers: {uh}\r\n",
                "X-Olaz-Set-Cookie: {sc}\r\n",
                "Transfer-Encoding: chunked\r\n",
                "Connection: close\r\n",
                "\r\n"
            ),
            ct = sanitize_header_line(&content_type),
            st = upstream_status,
            uh = upstream_hdrs_json,
            sc = set_cookie_json,
        );
        socket.write_all(head.as_bytes()).await?;

        while let Some(chunk) = upstream.chunk().await? {
            if chunk.is_empty() {
                continue;
            }
            let prefix = format!("{:X}\r\n", chunk.len());
            socket.write_all(prefix.as_bytes()).await?;
            socket.write_all(&chunk).await?;
            socket.write_all(b"\r\n").await?;
        }
        socket.write_all(b"0\r\n\r\n").await?;
        socket.flush().await?;
        Ok(())
    }

    /// Executes a stateless `GET /__olaz/media?u=<encoded_url>` pass-through supporting HTTP `Range` (`206`).
    pub async fn handle_media_relay(
        &self,
        target_url_raw: &str,
        range_header: Option<&str>,
        ephemeral_cookie: Option<&str>,
        is_head: bool,
        socket: &mut TcpStream,
    ) -> Result<()> {
        let (target_url, _addrs) = match self.allowlist.validate_http_target(target_url_raw).await {
            Ok(ok) => ok,
            Err(AllowlistError::InvalidUrl(msg)) => {
                write_json_response(
                    socket,
                    400,
                    "Bad Request",
                    &json!({ "error": "invalid_media_url", "detail": msg }),
                )
                .await?;
                return Ok(());
            }
            Err(err) => {
                write_json_response(
                    socket,
                    403,
                    "Forbidden",
                    &json!({ "error": "media_target_forbidden", "detail": err.to_string() }),
                )
                .await?;
                return Ok(());
            }
        };

        let method = if is_head { Method::HEAD } else { Method::GET };
        let mut builder = self
            .http_client
            .request(method, target_url)
            .header(USER_AGENT, ZALO_USER_AGENT);

        if let Some(r) = range_header.map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(hv) = HeaderValue::from_str(r) {
                builder = builder.header(RANGE, hv);
            }
        }
        if let Some(c) = ephemeral_cookie.map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(hv) = HeaderValue::from_str(c) {
                builder = builder.header(COOKIE, hv);
            }
        }

        let mut upstream = match builder.send().await {
            Ok(resp) => resp,
            Err(_) => {
                write_json_response(
                    socket,
                    502,
                    "Bad Gateway",
                    &json!({ "error": "media_upstream_unreachable" }),
                )
                .await?;
                return Ok(());
            }
        };

        let status = upstream.status();
        let code = status.as_u16();
        let reason = status.canonical_reason().unwrap_or("OK");

        let mut resp_head = format!(
            "HTTP/1.1 {code} {reason}\r\nCache-Control: private, no-store\r\nX-Content-Type-Options: nosniff\r\n"
        );

        let mut forwarded_content_len: Option<u64> = None;
        for hdr_name in [
            "content-type",
            "content-range",
            "accept-ranges",
            "content-length",
            "etag",
            "last-modified",
        ] {
            if let Some(val) = upstream.headers().get(hdr_name).and_then(|v| v.to_str().ok()) {
                if hdr_name == "content-length" {
                    forwarded_content_len = val.parse::<u64>().ok();
                } else {
                    let clean = sanitize_header_line(val);
                    resp_head.push_str(&format!("{hdr_name}: {clean}\r\n"));
                }
            }
        }

        if let Some(cl) = forwarded_content_len {
            resp_head.push_str(&format!("Content-Length: {cl}\r\nConnection: close\r\n\r\n"));
            socket.write_all(resp_head.as_bytes()).await?;
            if !is_head {
                while let Some(chunk) = upstream.chunk().await? {
                    if !chunk.is_empty() {
                        socket.write_all(&chunk).await?;
                    }
                }
            }
        } else {
            resp_head.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
            socket.write_all(resp_head.as_bytes()).await?;
            if !is_head {
                while let Some(chunk) = upstream.chunk().await? {
                    if !chunk.is_empty() {
                        let prefix = format!("{:X}\r\n", chunk.len());
                        socket.write_all(prefix.as_bytes()).await?;
                        socket.write_all(&chunk).await?;
                        socket.write_all(b"\r\n").await?;
                    }
                }
                socket.write_all(b"0\r\n\r\n").await?;
            }
        }

        socket.flush().await?;
        Ok(())
    }

    /// Handles an upgraded browser WebSocket (`GET /__olaz/ws`):
    /// 1. Reads initial `{"type":"__olaz_ws_open","url":...,"cookie":...,"origin":"file://"}` frame
    ///    (so `zpw_sek` is NEVER exposed in URL query parameters or access logs).
    /// 2. Validates `url` via [`AllowlistValidator::validate_ws_target`].
    /// 3. Dials the validated upstream `SocketAddr` with `Origin: file://`, `User-Agent: ZaloPC-darwin-23v694`,
    ///    and ephemeral `Cookie` header.
    /// 4. Sends `{"type":"__olaz_ws_ready","protocol":"..."}` and pumps frames bidirectionally in RAM.
    pub async fn handle_ws_relay(&self, stream: PrefixedTcpStream) -> Result<()> {
        let mut ws_config = WebSocketConfig::default();
        ws_config.max_message_size = Some(MAX_WS_FRAME_BYTES);
        ws_config.max_frame_size = Some(MAX_WS_FRAME_BYTES);

        let mut client_ws =
            WebSocketStream::from_raw_socket(stream, Role::Server, Some(ws_config)).await;

        let first_frame = match tokio::time::timeout(WS_OPEN_FRAME_TIMEOUT, client_ws.next()).await
        {
            Ok(Some(Ok(Message::Text(txt)))) => txt.to_string(),
            Ok(Some(Ok(Message::Binary(bin)))) => match String::from_utf8(bin.to_vec()) {
                Ok(s) => s,
                Err(_) => {
                    send_ws_error_and_close(&mut client_ws, 400, "expected JSON open handshake")
                        .await;
                    return Ok(());
                }
            },
            _ => {
                send_ws_error_and_close(&mut client_ws, 400, "missing __olaz_ws_open frame").await;
                return Ok(());
            }
        };

        let open_req: BrowserWsOpenFrame =
            match serde_json::from_str::<BrowserWsOpenFrame>(&first_frame) {
                Ok(v) if v.kind == "__olaz_ws_open" => v,
                _ => {
                    send_ws_error_and_close(&mut client_ws, 400, "invalid __olaz_ws_open payload")
                        .await;
                    return Ok(());
                }
            };

        let (target_url, verified_addrs) =
            match self.allowlist.validate_ws_target(&open_req.url).await {
                Ok(ok) => ok,
                Err(err) => {
                    send_ws_error_and_close(
                        &mut client_ws,
                        403,
                        &format!("forbidden websocket target: {err}"),
                    )
                    .await;
                    return Ok(());
                }
            };

        let build_upstream_req = || -> Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
            let mut req = target_url
                .as_str()
                .into_client_request()
                .context("failed to build upstream WS client request")?;
            let origin_str = open_req
                .origin
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_WS_ORIGIN);
            let headers = req.headers_mut();
            if let Ok(hv) = HeaderValue::from_str(origin_str) {
                headers.insert("Origin", hv);
            }
            headers.insert("User-Agent", HeaderValue::from_static(ZALO_USER_AGENT));
            let trimmed_cookie = open_req.cookie.trim();
            if !trimmed_cookie.is_empty() {
                if let Ok(hv) = HeaderValue::from_str(trimmed_cookie) {
                    headers.insert("Cookie", hv);
                }
            }
            let protos: Vec<&str> = open_req
                .protocols
                .iter()
                .map(String::as_str)
                .filter(|s| !s.trim().is_empty())
                .collect();
            if !protos.is_empty() {
                if let Ok(hv) = HeaderValue::from_str(&protos.join(", ")) {
                    headers.insert("Sec-WebSocket-Protocol", hv);
                }
            }
            Ok(req)
        };

        let connector = if target_url.scheme() == "ws" {
            Connector::Plain
        } else {
            Connector::Rustls(Arc::clone(&self.rustls_config))
        };

        let mut upstream_connected = None;
        for addr in verified_addrs.into_iter().take(4) {
            let Ok(tcp) = TcpStream::connect(addr).await else {
                continue;
            };
            let _ = tcp.set_nodelay(true);
            let Ok(req) = build_upstream_req() else {
                break;
            };
            if let Ok(pair) = client_async_tls_with_config(
                req,
                tcp,
                Some(ws_config),
                Some(connector.clone()),
            )
            .await
            {
                upstream_connected = Some(pair);
                break;
            }
        }

        let Some((upstream_ws, handshake_resp)) = upstream_connected else {
            send_ws_error_and_close(&mut client_ws, 502, "failed to connect to upstream websocket")
                .await;
            return Ok(());
        };

        let negotiated_proto = handshake_resp
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let ready_msg = json!({
            "type": "__olaz_ws_ready",
            "protocol": negotiated_proto,
        })
        .to_string();

        if client_ws.send(Message::Text(ready_msg.into())).await.is_err() {
            return Ok(());
        }

        let (mut client_sink, mut client_stream) = client_ws.split();
        let (mut up_sink, mut up_stream) = upstream_ws.split();

        loop {
            tokio::select! {
                from_browser = client_stream.next() => {
                    match from_browser {
                        Some(Ok(Message::Binary(bin))) => {
                            if up_sink.send(Message::Binary(bin)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Text(txt))) => {
                            if up_sink.send(Message::Text(txt)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = up_sink.send(Message::Ping(p)).await;
                        }
                        Some(Ok(Message::Pong(p))) => {
                            let _ = up_sink.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(cf))) => {
                            let _ = up_sink.send(Message::Close(cf)).await;
                            let _ = up_sink.close().await;
                            break;
                        }
                        Some(Ok(Message::Frame(_))) => {}
                        Some(Err(_)) | None => {
                            let _ = up_sink.close().await;
                            break;
                        }
                    }
                }
                from_upstream = up_stream.next() => {
                    match from_upstream {
                        Some(Ok(Message::Binary(bin))) => {
                            if client_sink.send(Message::Binary(bin)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Text(txt))) => {
                            if client_sink.send(Message::Text(txt)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = client_sink.send(Message::Ping(p)).await;
                        }
                        Some(Ok(Message::Pong(p))) => {
                            let _ = client_sink.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(cf))) => {
                            let _ = client_sink.send(Message::Close(cf)).await;
                            let _ = client_sink.close().await;
                            break;
                        }
                        Some(Ok(Message::Frame(_))) => {}
                        Some(Err(_)) | None => {
                            let _ = client_sink.close().await;
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

async fn send_ws_error_and_close(
    ws: &mut WebSocketStream<PrefixedTcpStream>,
    status: u16,
    message: &str,
) {
    let payload = json!({
        "type": "__olaz_ws_error",
        "status": status,
        "message": message,
    })
    .to_string();
    let _ = ws.send(Message::Text(payload.into())).await;
    let _ = ws.send(Message::Close(None)).await;
    let _ = ws.close(None).await;
}

fn decode_caller_headers(raw_hdr: Option<&str>) -> HeaderMap {
    let mut out = HeaderMap::new();
    let Some(raw) = raw_hdr.map(str::trim).filter(|s| !s.is_empty()) else {
        return out;
    };

    let decoded_json: Option<Value> = if raw.starts_with('{') {
        serde_json::from_str(raw).ok()
    } else {
        STANDARD
            .decode(raw)
            .or_else(|_| STANDARD_NO_PAD.decode(raw))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    };

    let Some(obj) = decoded_json.as_ref().and_then(Value::as_object) else {
        return out;
    };

    for (k, v) in obj {
        let lower = k.trim().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host"
                | "cookie"
                | "user-agent"
                | "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "content-length"
                | "upgrade"
                | "proxy-authorization"
        ) {
            continue;
        }
        let Some(val_str) = v.as_str() else {
            continue;
        };
        if let (Ok(hn), Ok(hv)) = (
            HeaderName::from_bytes(lower.as_bytes()),
            HeaderValue::from_str(val_str),
        ) {
            out.insert(hn, hv);
        }
    }
    out
}

fn sanitize_header_line(raw: &str) -> String {
    raw.chars()
        .filter(|&c| c != '\r' && c != '\n' && c != '\0')
        .collect()
}

fn ascii_safe_json_header(val: &Value) -> String {
    let raw = serde_json::to_string(val).unwrap_or_else(|_| "null".to_string());
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii() && (' '..='~').contains(&ch) {
            out.push(ch);
        } else {
            let mut utf16_buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut utf16_buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

pub(crate) async fn write_json_response(
    socket: &mut TcpStream,
    status_code: u16,
    reason: &str,
    body_val: &Value,
) -> Result<()> {
    let body = serde_json::to_vec(body_val).unwrap_or_else(|_| b"{}".to_vec());
    let head = format!(
        concat!(
            "HTTP/1.1 {code} {reason}\r\n",
            "Content-Type: application/json; charset=utf-8\r\n",
            "Cache-Control: no-store\r\n",
            "X-Content-Type-Options: nosniff\r\n",
            "Content-Length: {len}\r\n",
            "Connection: close\r\n",
            "\r\n"
        ),
        code = status_code,
        reason = reason,
        len = body.len(),
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&body).await?;
    socket.flush().await?;
    Ok(())
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
