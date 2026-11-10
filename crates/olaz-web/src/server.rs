//! Production async HTTP/1.1 + RFC 6455 WebSocket edge server (`tokio::net::TcpListener`).
//!
//! Serves:
//! - `GET /__olaz/health` -> `200 OK` `{"ok":true,"stateless":true,"persistence":"browser-only"}`
//! - `POST /__olaz/auth` -> stateless constant-time token gate login (`olaz_gate` HMAC cookie)
//! - `GET /` -> `302 /login.html?fail_sqlite3=true` (preserving `?profile=<slot>`)
//! - `GET /boot.html`, `GET /__olaz/crypto_sync.js`, `GET /__olaz/bridge_table.js`,
//!   `GET /__olaz/preload.js`, `GET /olaz-sw.js`
//! - `POST /__olaz/http` -> stateless HTTP relay
//! - `GET /__olaz/media?u=<encoded_url>` -> stateless HTTP `Range` (`206 Partial Content`) media pass-through
//! - `GET /__olaz/ws` -> RFC 6455 `101 Switching Protocols` WebSocket upgrade + 1st-frame `__olaz_ws_open`
//! - `GET /<bundle_asset>` -> patched `BundleProvider` assets

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use bytes::{Bytes, BytesMut};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::allowlist::{AllowlistConfig, AllowlistValidator};
use crate::auth::{AuthGate, UNLOCK_PAGE_HTML};
use crate::bundle::{
    build_web_manifest_json, web_bridge_spec_script, BundleProvider, APPLE_TOUCH_ICON_PNG,
    BOOT_HTML, CRYPTO_SYNC_JS, FAVICON_ICO, ICON_192_PNG, ICON_512_PNG, ICON_MASKABLE_512_PNG,
    LOGO_SVG, MOBILE_CSS, PRELOAD_JS, SW_JS,
};
use crate::relay::{
    write_json_response, PrefixedTcpStream, StatelessRelay, ZALO_APP_VERSION, ZALO_CLIENT_TYPE,
    ZALO_CLIENT_VERSION,
};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Configuration for [`start_web_server`].
#[derive(Debug, Clone)]
pub struct WebServerConfig {
    pub listen_addr: SocketAddr,
    pub bundle: BundleProvider,
    pub token: Option<String>,
    pub allowlist: AllowlistConfig,
}

/// Handle to a running [`olaz-web`](crate) server instance.
pub struct WebServerHandle {
    local_addr: SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    accept_task: JoinHandle<()>,
}

impl WebServerHandle {
    /// Bound local `SocketAddr` (useful when binding port `0` in tests).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Gracefully stops the listener task.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.accept_task.await;
    }

    /// Waits until the listener task finishes (or Ctrl-C is handled externally).
    pub async fn wait(self) {
        let _ = self.accept_task.await;
    }
}

struct SharedState {
    bundle: BundleProvider,
    auth: AuthGate,
    relay: StatelessRelay,
    runtime_tag: String,
}

fn compute_runtime_tag() -> String {
    let mut h = Sha256::new();
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(PRELOAD_JS.as_bytes());
    h.update(CRYPTO_SYNC_JS.as_bytes());
    h.update(SW_JS.as_bytes());
    h.update(MOBILE_CSS.as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..8])
}

/// Binds `config.listen_addr` and spawns the stateless `olaz-web` edge server.
pub async fn start_web_server(config: WebServerConfig) -> Result<WebServerHandle> {
    let listener = TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("failed to bind TCP listener on {}", config.listen_addr))?;
    let local_addr = listener
        .local_addr()
        .context("failed to query listener local_addr")?;

    let validator = AllowlistValidator::new(config.allowlist);
    let relay = StatelessRelay::new(validator)?;
    let auth = AuthGate::new(config.token);

    let state = Arc::new(SharedState {
        bundle: config.bundle,
        auth,
        relay,
        runtime_tag: compute_runtime_tag(),
    });

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    info!(
        addr = %local_addr,
        auth_enabled = state.auth.is_enabled(),
        "olaz-web stateless relay listening (persistence: browser-only)"
    );

    let accept_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _peer)) => {
                            let _ = stream.set_nodelay(true);
                            let st = Arc::clone(&state);
                            tokio::spawn(async move {
                                if let Err(err) = handle_connection(stream, st).await {
                                    tracing::debug!("connection closed: {err}");
                                }
                            });
                        }
                        Err(err) => {
                            warn!("TCP accept error: {err}");
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                    }
                }
            }
        }
    });

    Ok(WebServerHandle {
        local_addr,
        shutdown_tx,
        accept_task,
    })
}

struct ParsedHttpHead {
    method: String,
    raw_target: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    leftover: BytesMut,
}

impl ParsedHttpHead {
    fn header(&self, lower_name: &str) -> Option<&str> {
        self.headers.get(lower_name).map(String::as_str)
    }
}

async fn read_http_head(stream: &mut TcpStream) -> Result<Option<ParsedHttpHead>> {
    let mut buf = BytesMut::with_capacity(4096);
    let mut temp = [0u8; 4096];

    let header_end = tokio::time::timeout(HEADER_READ_TIMEOUT, async {
        loop {
            if let Some(pos) = find_crlf_crlf(&buf) {
                return Ok::<Option<usize>, anyhow::Error>(Some(pos));
            }
            if buf.len() >= MAX_HEADER_BYTES {
                bail!("HTTP request headers exceeded {MAX_HEADER_BYTES} bytes");
            }
            let n = stream.read(&mut temp).await?;
            if n == 0 {
                if buf.is_empty() {
                    return Ok(None);
                }
                bail!("unexpected EOF while reading HTTP headers");
            }
            buf.extend_from_slice(&temp[..n]);
        }
    })
    .await
    .context("HTTP header read timed out")??;

    let Some(end_idx) = header_end else {
        return Ok(None);
    };

    let head_bytes = buf.split_to(end_idx + 4);
    let head_str =
        std::str::from_utf8(&head_bytes[..end_idx]).context("HTTP headers are not valid UTF-8")?;

    let mut lines = head_str.split("\r\n");
    let req_line = lines.next().context("missing HTTP request line")?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next().context("missing HTTP method")?.to_uppercase();
    let raw_target = parts.next().context("missing HTTP target")?.to_string();
    let _version = parts.next().context("missing HTTP version")?;

    let (path, query) = match raw_target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_target.clone(), String::new()),
    };

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, val)) = line.split_once(':') {
            let key = name.trim().to_ascii_lowercase();
            let v = val.trim().to_string();
            headers
                .entry(key.clone())
                .and_modify(|existing: &mut String| {
                    if key == "cookie" {
                        existing.push_str("; ");
                        existing.push_str(&v);
                    } else {
                        existing.push_str(", ");
                        existing.push_str(&v);
                    }
                })
                .or_insert(v);
        }
    }

    Ok(Some(ParsedHttpHead {
        method,
        raw_target,
        path,
        query,
        headers,
        leftover: buf,
    }))
}

async fn read_http_body(stream: &mut TcpStream, head: &mut ParsedHttpHead) -> Result<Bytes> {
    if head.method == "GET" || head.method == "HEAD" {
        return Ok(Bytes::new());
    }

    if let Some(te) = head.header("transfer-encoding") {
        if te.to_ascii_lowercase().contains("chunked") {
            return read_chunked_body(stream, &mut head.leftover).await;
        }
    }

    let content_len = match head.header("content-length") {
        Some(s) => s
            .trim()
            .parse::<usize>()
            .context("invalid Content-Length")?,
        None => return Ok(Bytes::new()),
    };

    if content_len == 0 {
        return Ok(Bytes::new());
    }
    if content_len > MAX_BODY_BYTES {
        bail!("HTTP body exceeds maximum size ({MAX_BODY_BYTES} bytes)");
    }

    while head.leftover.len() < content_len {
        let mut chunk = [0u8; 8192];
        let needed = (content_len - head.leftover.len()).min(chunk.len());
        let n = stream.read(&mut chunk[..needed]).await?;
        if n == 0 {
            bail!("unexpected EOF while reading HTTP body");
        }
        head.leftover.extend_from_slice(&chunk[..n]);
    }

    Ok(head.leftover.split_to(content_len).freeze())
}

async fn read_chunked_body(stream: &mut TcpStream, buf: &mut BytesMut) -> Result<Bytes> {
    let mut out = BytesMut::new();
    loop {
        let line_end = loop {
            if let Some(pos) = find_crlf(buf) {
                break pos;
            }
            let mut tmp = [0u8; 4096];
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                bail!("unexpected EOF in chunked transfer-encoding size line");
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let line_bytes = buf.split_to(line_end + 2);
        let size_str = std::str::from_utf8(&line_bytes[..line_end])?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let chunk_size = usize::from_str_radix(size_str, 16)
            .context("invalid hex chunk size in chunked body")?;
        if chunk_size == 0 {
            while buf.len() < 2 {
                let mut tmp = [0u8; 2];
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            if buf.len() >= 2 {
                let _ = buf.split_to(2);
            }
            break;
        }
        if out.len().saturating_add(chunk_size) > MAX_BODY_BYTES {
            bail!("chunked HTTP body exceeds maximum size");
        }
        while buf.len() < chunk_size + 2 {
            let mut tmp = [0u8; 8192];
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                bail!("unexpected EOF in chunked body data");
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let data_and_crlf = buf.split_to(chunk_size + 2);
        out.extend_from_slice(&data_and_crlf[..chunk_size]);
    }
    Ok(out.freeze())
}

async fn handle_connection(mut stream: TcpStream, state: Arc<SharedState>) -> Result<()> {
    let Some(mut head) = read_http_head(&mut stream).await? else {
        return Ok(());
    };

    // 1. Public gate login endpoint (`POST /__olaz/auth`) — accessible before authentication.
    if head.method == "POST" && head.path == "/__olaz/auth" {
        let body = read_http_body(&mut stream, &mut head).await?;
        return handle_auth_post(&mut stream, &head, &body, &state.auth).await;
    }

    // 1b. Public PWA manifest, brand icons, favicons & mobile stylesheet — accessible before
    //     authentication so mobile OS home-screen installers and unlock page always load cleanly.
    if head.method == "GET" || head.method == "HEAD" {
        let is_head = head.method == "HEAD";
        match head.path.as_str() {
            "/manifest.webmanifest" | "/manifest.json" => {
                let profile = extract_query_param(&head.query, "profile");
                let manifest_json = build_web_manifest_json(profile.as_deref());
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "application/manifest+json; charset=utf-8",
                    manifest_json.as_bytes(),
                    &[("Cache-Control", "no-cache")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/favicon.ico" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "image/x-icon",
                    FAVICON_ICO,
                    &[("Cache-Control", "public, max-age=86400")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/favicon.svg" | "/__olaz/logo.svg" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "image/svg+xml; charset=utf-8",
                    LOGO_SVG.as_bytes(),
                    &[("Cache-Control", "public, max-age=86400")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/apple-touch-icon.png" | "/apple-touch-icon-precomposed.png" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "image/png",
                    APPLE_TOUCH_ICON_PNG,
                    &[("Cache-Control", "public, max-age=86400")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/__olaz/icons/icon-192.png" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "image/png",
                    ICON_192_PNG,
                    &[("Cache-Control", "public, max-age=86400")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/__olaz/icons/icon-512.png" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "image/png",
                    ICON_512_PNG,
                    &[("Cache-Control", "public, max-age=86400")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/__olaz/icons/icon-maskable-512.png" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "image/png",
                    ICON_MASKABLE_512_PNG,
                    &[("Cache-Control", "public, max-age=86400")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/__olaz/mobile.css" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "text/css; charset=utf-8",
                    MOBILE_CSS.as_bytes(),
                    &[("Cache-Control", "no-cache")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            _ => {}
        }
    }

    // 2. Enforce optional `--token` / `OLAZ_WEB_TOKEN` edge gate on all other routes.
    if !state
        .auth
        .is_authorized(head.header("authorization"), head.header("cookie"))
    {
        let wants_html = (head.method == "GET" || head.method == "HEAD")
            && head.header("upgrade").is_none()
            && (head.path == "/"
                || head.path.ends_with(".html")
                || head
                    .header("accept")
                    .is_some_and(|a| a.contains("text/html")));

        if wants_html {
            write_static_response(
                &mut stream,
                401,
                "Unauthorized",
                "text/html; charset=utf-8",
                UNLOCK_PAGE_HTML.as_bytes(),
                &[("WWW-Authenticate", "Bearer realm=\"OlazWeb\"")],
                head.method == "HEAD",
            )
            .await?;
        } else {
            write_json_response(
                &mut stream,
                401,
                "Unauthorized",
                &json!({ "error": "unauthorized" }),
            )
            .await?;
        }
        return Ok(());
    }

    // 3. WebSocket upgrade endpoint (`GET /__olaz/ws`).
    if head.path == "/__olaz/ws" {
        return handle_ws_upgrade(stream, head, &state.relay).await;
    }

    // 4. Health probe (`GET /__olaz/health`).
    if (head.method == "GET" || head.method == "HEAD") && head.path == "/__olaz/health" {
        let payload = br#"{"ok":true,"stateless":true,"persistence":"browser-only"}"#;
        write_static_response(
            &mut stream,
            200,
            "OK",
            "application/json; charset=utf-8",
            payload,
            &[],
            head.method == "HEAD",
        )
        .await?;
        return Ok(());
    }

    // 5. Root redirect (`GET /` -> `302 /login.html?type=23&client_version=695&...`).
    if (head.method == "GET" || head.method == "HEAD") && head.path == "/" {
        let mut loc = format!(
            "/login.html?type={ZALO_CLIENT_TYPE}&client_version={ZALO_CLIENT_VERSION}&app_version={ZALO_APP_VERSION}&about_version=&new_install=0&fail_sqlite3=true&mode_lang="
        );
        if let Some(slot) = extract_query_param(&head.query, "profile") {
            let clean: String = slot
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if !clean.is_empty() && clean != "default" {
                loc.push_str("&profile=");
                loc.push_str(&clean);
            }
        }
        if let Some(logout) = extract_query_param(&head.query, "logout") {
            if logout == "1" {
                loc.push_str("&logout=1");
            }
        }
        let resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(resp.as_bytes()).await?;
        stream.flush().await?;
        return Ok(());
    }

    // 6. Embedded Olaz Browser Runtime Routes.
    if head.method == "GET" || head.method == "HEAD" {
        let is_head = head.method == "HEAD";
        match head.path.as_str() {
            "/boot.html" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "text/html; charset=utf-8",
                    BOOT_HTML.as_bytes(),
                    &[],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/__olaz/crypto_sync.js" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "application/javascript; charset=utf-8",
                    CRYPTO_SYNC_JS.as_bytes(),
                    &[],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/__olaz/bridge_table.js" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "application/javascript; charset=utf-8",
                    web_bridge_spec_script().as_bytes(),
                    &[],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/__olaz/preload.js" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "application/javascript; charset=utf-8",
                    PRELOAD_JS.as_bytes(),
                    &[],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            "/olaz-sw.js" => {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    "application/javascript; charset=utf-8",
                    SW_JS.as_bytes(),
                    &[("Service-Worker-Allowed", "/")],
                    is_head,
                )
                .await?;
                return Ok(());
            }
            _ => {}
        }
    }

    // 7. Stateless HTTP relay (`POST /__olaz/http`).
    if head.method == "POST" && head.path == "/__olaz/http" {
        let Some(target) = head.header("x-olaz-target").map(str::to_owned) else {
            write_json_response(
                &mut stream,
                400,
                "Bad Request",
                &json!({ "error": "missing_x_olaz_target" }),
            )
            .await?;
            return Ok(());
        };
        let method = head
            .header("x-olaz-method")
            .unwrap_or("GET")
            .to_owned();
        let encoded_hdrs = head.header("x-olaz-headers").map(str::to_owned);
        let ephemeral_cookie = head.header("x-olaz-cookie").map(str::to_owned);
        let body = read_http_body(&mut stream, &mut head).await?;
        return state
            .relay
            .handle_http_relay(
                &target,
                &method,
                encoded_hdrs.as_deref(),
                ephemeral_cookie.as_deref(),
                body,
                &mut stream,
            )
            .await;
    }

    // 8. Stateless HTTP Range media streaming (`GET /__olaz/media?u=<encoded_url>`).
    if (head.method == "GET" || head.method == "HEAD") && head.path == "/__olaz/media" {
        let Some(target) = extract_query_param(&head.query, "u") else {
            write_json_response(
                &mut stream,
                400,
                "Bad Request",
                &json!({ "error": "missing_u_query_parameter" }),
            )
            .await?;
            return Ok(());
        };
        let range = head.header("range").map(str::to_owned);
        let ephemeral_cookie = head.header("x-olaz-cookie").map(str::to_owned);
        return state
            .relay
            .handle_media_relay(
                &target,
                range.as_deref(),
                ephemeral_cookie.as_deref(),
                head.method == "HEAD",
                &mut stream,
            )
            .await;
    }

    // 9. Static Zalo bundle assets from read-only `BundleProvider`.
    if head.method == "GET" || head.method == "HEAD" {
        if let Some(asset) = state.bundle.get_asset(&head.raw_target) {
            let lower_path = head.path.to_ascii_lowercase();
            let is_html = lower_path.ends_with(".html") || lower_path.ends_with(".htm");
            if !is_html {
                let etag = format!("W/\"{}-{:x}\"", state.runtime_tag, asset.bytes.len());
                if head
                    .header("if-none-match")
                    .is_some_and(|inm| inm.split(',').any(|t| t.trim() == etag))
                {
                    let resp_304 = format!(
                        concat!(
                            "HTTP/1.1 304 Not Modified\r\n",
                            "ETag: {etag}\r\n",
                            "Cache-Control: private, max-age=300\r\n",
                            "Content-Length: 0\r\n",
                            "Connection: close\r\n",
                            "\r\n"
                        ),
                        etag = etag,
                    );
                    stream.write_all(resp_304.as_bytes()).await?;
                    stream.flush().await?;
                    return Ok(());
                }
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    &asset.content_type,
                    &asset.bytes,
                    &[
                        ("Cache-Control", "private, max-age=300"),
                        ("ETag", etag.as_str()),
                    ],
                    head.method == "HEAD",
                )
                .await?;
            } else {
                write_static_response(
                    &mut stream,
                    200,
                    "OK",
                    &asset.content_type,
                    &asset.bytes,
                    &[],
                    head.method == "HEAD",
                )
                .await?;
            }
            return Ok(());
        }
        write_json_response(&mut stream, 404, "Not Found", &json!({ "error": "not_found" }))
            .await?;
        return Ok(());
    }

    write_json_response(
        &mut stream,
        405,
        "Method Not Allowed",
        &json!({ "error": "method_not_allowed" }),
    )
    .await
}

async fn handle_auth_post(
    stream: &mut TcpStream,
    head: &ParsedHttpHead,
    body: &[u8],
    auth: &AuthGate,
) -> Result<()> {
    let candidate = extract_auth_token_from_body(head.header("content-type"), body);
    let Some(tok) = candidate else {
        write_json_response(
            stream,
            400,
            "Bad Request",
            &json!({ "ok": false, "error": "missing_token" }),
        )
        .await?;
        return Ok(());
    };

    if !auth.verify_token(&tok) {
        write_json_response(
            stream,
            401,
            "Unauthorized",
            &json!({ "ok": false, "error": "invalid_token" }),
        )
        .await?;
        return Ok(());
    }

    let is_https = head
        .header("x-forwarded-proto")
        .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    let set_cookie = auth.issue_gate_set_cookie(is_https);
    let resp_body = br#"{"ok":true}"#;
    let resp_head = format!(
        concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/json; charset=utf-8\r\n",
            "Cache-Control: no-store\r\n",
            "Set-Cookie: {sc}\r\n",
            "Content-Length: {len}\r\n",
            "Connection: close\r\n",
            "\r\n"
        ),
        sc = set_cookie,
        len = resp_body.len(),
    );
    stream.write_all(resp_head.as_bytes()).await?;
    stream.write_all(resp_body).await?;
    stream.flush().await?;
    Ok(())
}

fn extract_auth_token_from_body(content_type: Option<&str>, body: &[u8]) -> Option<String> {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if ct.contains("application/json") || body.first().copied() == Some(b'{') {
        let v: Value = serde_json::from_slice(body).ok()?;
        return v.get("token")?.as_str().map(str::to_owned);
    }
    let form_str = std::str::from_utf8(body).ok()?;
    extract_query_param(form_str, "token")
}

async fn handle_ws_upgrade(
    mut stream: TcpStream,
    head: ParsedHttpHead,
    relay: &StatelessRelay,
) -> Result<()> {
    let is_ws_upgrade = head
        .header("upgrade")
        .is_some_and(|u| u.to_ascii_lowercase().contains("websocket"));
    let Some(ws_key) = head.header("sec-websocket-key").map(str::trim) else {
        if !is_ws_upgrade {
            write_json_response(
                &mut stream,
                400,
                "Bad Request",
                &json!({ "error": "websocket_upgrade_required" }),
            )
            .await?;
            return Ok(());
        }
        write_json_response(
            &mut stream,
            400,
            "Bad Request",
            &json!({ "error": "missing_sec_websocket_key" }),
        )
        .await?;
        return Ok(());
    };

    let mut sha1 = Sha1::new();
    sha1.update(ws_key.as_bytes());
    sha1.update(WS_GUID.as_bytes());
    let accept_b64 = STANDARD.encode(sha1.finalize());

    let upgrade_resp = format!(
        concat!(
            "HTTP/1.1 101 Switching Protocols\r\n",
            "Upgrade: websocket\r\n",
            "Connection: Upgrade\r\n",
            "Sec-WebSocket-Accept: {accept}\r\n",
            "\r\n"
        ),
        accept = accept_b64,
    );
    stream.write_all(upgrade_resp.as_bytes()).await?;
    stream.flush().await?;

    let prefixed = PrefixedTcpStream::new(head.leftover.freeze(), stream);
    relay.handle_ws_relay(prefixed).await
}

async fn write_static_response(
    stream: &mut TcpStream,
    status_code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
    is_head: bool,
) -> Result<()> {
    let has_custom_cache_control = extra_headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("cache-control"));
    let mut head = format!(
        concat!(
            "HTTP/1.1 {code} {reason}\r\n",
            "Content-Type: {ct}\r\n",
            "X-Content-Type-Options: nosniff\r\n",
            "Referrer-Policy: no-referrer\r\n"
        ),
        code = status_code,
        reason = reason,
        ct = content_type,
    );
    if !has_custom_cache_control {
        head.push_str("Cache-Control: no-store\r\n");
    }
    for (k, v) in extra_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));

    stream.write_all(head.as_bytes()).await?;
    if !is_head {
        stream.write_all(body).await?;
    }
    stream.flush().await?;
    Ok(())
}

fn extract_query_param(query: &str, target_key: &str) -> Option<String> {
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == target_key {
            let plus_fixed = v.replace('+', " ");
            return percent_encoding::percent_decode_str(&plus_fixed)
                .decode_utf8()
                .ok()
                .map(|cow| cow.into_owned());
        }
    }
    None
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

fn find_crlf_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}
