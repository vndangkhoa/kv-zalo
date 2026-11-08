use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::{Cursor, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aes::cipher::{KeyIvInit, StreamCipher};
use aes::Aes256;
use aes_gcm::aead::{consts::U16, Aead, KeyInit};
use aes_gcm::{Aes256Gcm, AesGcm, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ctr::Ctr128BE;
use futures_util::{SinkExt, StreamExt};
use olaz_net::cookies::{
    is_zalo_cookie_domain, ZaloCookieJar, AUTH_DOMAIN, SESSION_COOKIE_DOMAINS,
};
use olaz_net::doh::{is_zalo_cdn, DnsBackend, DnsResolveError, ZaloDnsResolver};
use olaz_net::media_loader::{
    fnv1a_64_hex, sanitize_headers, serve_local_media_range, sniff_media_mime,
    strip_sensitive_headers_for_redirect, trim_cache_dir, MediaLoadError, MediaLoader,
};
use olaz_net::proxy::{
    describe_zalo_frame, is_observed_zalo_endpoint, NetworkProxy, WsEvent, WsEventSink,
    DEFAULT_USER_AGENT, WEBSOCKET_ORIGIN,
};
use olaz_net::secure_stream::{
    Aes256CtrBlock, Aes256CtrLinear, Aes256GcmLinear, DecryptError, DownloadFailure,
    DownloadManager, SecureMode, SecureSpec, SecureStream,
};
use parking_lot::Mutex;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request as WsHandshakeReq, Response as WsHandshakeResp};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use url::Url;

// ============================================================================
// 1. ZaloCookieJar tests
// ============================================================================

#[test]
fn cookie_jar_domain_cookies_ws_matching_stale_cleanup_and_0600_persistence() {
    let tmp = TempDir::new().expect("tempdir");
    let cookie_file = tmp.path().join("cookies.json");

    let jar = ZaloCookieJar::open(&cookie_file).expect("open cookie jar");
    assert!(!jar.has_session(), "empty jar must not report active session");

    // Simulate stale host-only `zpw_sek` cookies previously set by HTTP `Set-Cookie` without `Domain=`.
    jar.insert_set_cookie(
        "zpw_sek=stale-host-wpa; Path=/; Secure; HttpOnly",
        "https://wpa.zaloapp.com/api/login",
    )
    .expect("insert stale wpa host-only cookie");
    jar.insert_set_cookie(
        "zpw_sek=stale-host-ws5; Path=/; Secure; HttpOnly",
        "https://ws5-msg.chat.zalo.me/",
    )
    .expect("insert stale ws5 host-only cookie");
    // Also insert an unrelated Zalo cookie and non-Zalo cookie to verify selective removal.
    jar.set_cookie("zpsid", "sid-123", Some("https://chat.zalo.me/"), Some(".zalo.me"))
        .expect("set zpsid");
    jar.insert_set_cookie(
        "ext_pref=keep-me; Path=/; Max-Age=86400; Secure",
        "https://example.org/",
    )
    .expect("insert non-zalo cookie");

    assert!(jar.has_session(), "non-empty stale zpw_sek initially counts as session");

    // Now install the real `zpw_sek` domain cookie across all SESSION_COOKIE_DOMAINS.
    let fresh_sek = "sek_prod_998877665544332211";
    jar.set_app_cookie(fresh_sek).expect("set_app_cookie");
    assert!(jar.has_session(), "jar must report active session on AUTH_DOMAIN");

    // Verify all stored `zpw_sek` entries are RFC 6265 domain cookies (`host_only == false`, leading dot)
    // across exact `SESSION_COOKIE_DOMAINS`, and NO stale host-only `zpw_sek` remains!
    let sek_entries = jar.get_cookies(None, None, Some("zpw_sek"));
    assert_eq!(sek_entries.len(), SESSION_COOKIE_DOMAINS.len());
    for entry in &sek_entries {
        assert_eq!(entry.value, fresh_sek);
        assert!(
            !entry.host_only,
            "zpw_sek must be a domain cookie (HostOnly=false), got {entry:?}"
        );
        assert!(
            entry.domain.starts_with('.'),
            "serialized CookieEntry domain must carry leading dot: {}",
            entry.domain
        );
        assert_eq!(entry.path, "/");
        assert!(entry.secure);
        assert!(entry.http_only);
        assert_eq!(entry.same_site.as_deref(), Some("no_restriction"));
        assert!(entry.expiration_date.is_some());
    }

    // Verify subdomain matching for HTTPS and WSS (including HttpOnly zpw_sek!).
    for target in [
        AUTH_DOMAIN,
        "https://wpa.zaloapp.com/api/login/getServerInfo",
        "https://tt-profile-wpa.zaloapp.com/api/profile",
        "https://ws5-msg.chat.zalo.me/",
        "wss://ws2-msg.chat.zalo.me/?zpw_ver=694&zpw_type=23",
        "wss://ws-chat.zalo.me/ws",
    ] {
        let hdr = jar
            .cookie_header_for_url(target)
            .unwrap_or_else(|| panic!("expected Cookie header for {target}"));
        assert!(
            hdr.split("; ").any(|kv| kv == format!("zpw_sek={fresh_sek}")),
            "target {target} missing fresh zpw_sek in header: {hdr}"
        );
        assert!(
            !hdr.contains("stale-host"),
            "target {target} leaked stale host-only zpw_sek: {hdr}"
        );
    }

    // Must NOT send `Secure` `zpw_sek` over plain `http://` or `ws://`, nor to lookalike hosts.
    assert!(jar.cookie_header_for_url("http://ws5-msg.chat.zalo.me/").is_none());
    assert!(jar.cookie_header_for_url("ws://ws2-msg.chat.zalo.me/").is_none());
    assert!(jar.cookie_header_for_url("https://evilzalo.me/").is_none());
    assert!(jar.cookie_header_for_url("https://zaloapp.com.attacker.example/").is_none());

    // Verify 0600 file permissions and clean atomic write (no leftover .tmp files).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&cookie_file)
            .expect("cookie_file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "cookies.json must have 0600 permissions, got {mode:o}");
    }
    for dir_entry in fs::read_dir(tmp.path()).expect("read_dir") {
        let name = dir_entry.expect("entry").file_name().to_string_lossy().into_owned();
        assert!(
            !name.ends_with(".tmp"),
            "atomic persistence left temporary file behind: {name}"
        );
    }

    // Reload from disk in a brand-new `ZaloCookieJar` instance and verify session & WSS cookies survive!
    let reloaded = ZaloCookieJar::open(&cookie_file).expect("reload cookie jar");
    assert!(reloaded.has_session());
    let reloaded_ws_hdr = reloaded
        .cookie_header_for_url("wss://ws2-msg.chat.zalo.me/")
        .expect("reloaded wss cookie header");
    assert!(reloaded_ws_hdr.contains(&format!("zpw_sek={fresh_sek}")));
    assert!(reloaded_ws_hdr.contains("zpsid=sid-123"));

    // Test Electron IPC dispatchers (`_electron_get-cookies`, `_electron_delete-cookie`, `_electron_clear-cookie`).
    let ipc_got = reloaded
        .handle_ipc(
            "_electron_get-cookies",
            &[json!({ "url": "https://ws5-msg.chat.zalo.me/", "name": "zpw_sek" })],
        )
        .expect("ipc get-cookies");
    assert!(!ipc_got.as_array().expect("array").is_empty());

    reloaded
        .handle_ipc(
            "_electron_delete-cookie",
            &[json!("https://chat.zalo.me/"), json!("zpsid")],
        )
        .expect("ipc delete-cookie");
    assert!(reloaded.get_cookies(None, None, Some("zpsid")).is_empty());

    assert!(is_zalo_cookie_domain(".chat.zalo.me"));
    assert!(!is_zalo_cookie_domain("example.org"));

    reloaded
        .handle_ipc("_electron_clear-cookie", &[])
        .expect("ipc clear-cookie");
    assert!(!reloaded.has_session(), "session must be cleared after clear_cookies");
    assert!(
        reloaded
            .cookie_header_for_url("wss://ws2-msg.chat.zalo.me/")
            .is_none()
    );
    // Non-Zalo cookie must remain untouched by `_electron_clear-cookie`.
    assert_eq!(
        reloaded.cookie_header_for_url("https://example.org/").as_deref(),
        Some("ext_pref=keep-me")
    );
}

// ============================================================================
// 2. DoH allowlist, resolver state machine, and sensitive header stripping
// ============================================================================

#[test]
fn is_zalo_cdn_strict_allowlist_and_subdomain_matching() {
    for valid in [
        "zadn.vn",
        "f10-zfcloud.zadn.vn",
        "b2.zdn.vn",
        "B2.ZDN.VN.",
        "zalo.me",
        "chat.zalo.me",
        "ws5-msg.chat.zalo.me",
        "zaloapp.com",
        "wpa.zaloapp.com",
        "tt-profile-wpa.zaloapp.com",
        "zingcdn.me",
        "s120.ava.talk.zingcdn.me",
        "zalo.vn",
        "zing.vn",
    ] {
        assert!(is_zalo_cdn(valid), "expected {valid} to match Zalo CDN allowlist");
    }

    for invalid in [
        "",
        ".",
        "localhost",
        "127.0.0.1",
        "evilzadn.vn",
        "notzalo.me",
        "myzaloapp.com",
        "zadn.vn.evil.com",
        "zalo.me.attacker.org",
        "youtube.com",
        "github.com",
    ] {
        assert!(
            !is_zalo_cdn(invalid),
            "expected {invalid} NOT to match Zalo CDN allowlist"
        );
    }
}

struct MockDnsBackend {
    sys_calls: AtomicUsize,
    doh_calls: AtomicUsize,
    sys_delay: Duration,
    sys_result: Mutex<HashMap<String, Result<Vec<SocketAddr>, std::io::ErrorKind>>>,
    doh_result: Mutex<HashMap<String, Result<Vec<SocketAddr>, DnsResolveError>>>,
}

impl DnsBackend for MockDnsBackend {
    fn system_lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        self.sys_calls.fetch_add(1, Ordering::SeqCst);
        let delay = self.sys_delay;
        let res = self.sys_result.lock().get(host).cloned();
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            match res {
                Some(Ok(addrs)) => Ok(addrs),
                Some(Err(kind)) => Err(std::io::Error::new(kind, "mock system dns failure")),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "unconfigured host",
                )),
            }
        })
    }

    fn doh_lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, DnsResolveError>> + Send + 'a>> {
        self.doh_calls.fetch_add(1, Ordering::SeqCst);
        let res = self.doh_result.lock().get(host).cloned();
        Box::pin(async move {
            res.unwrap_or_else(|| Err(DnsResolveError::HostNotFound(host.to_owned())))
        })
    }
}

#[tokio::test]
async fn doh_resolver_sticky_fallback_dead_host_cache_and_third_party_isolation() {
    let backend = Arc::new(MockDnsBackend {
        sys_calls: AtomicUsize::new(0),
        doh_calls: AtomicUsize::new(0),
        sys_delay: Duration::ZERO,
        sys_result: Mutex::new(HashMap::new()),
        doh_result: Mutex::new(HashMap::new()),
    });

    let cdn_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(103, 199, 76, 10)), 443);
    backend
        .doh_result
        .lock()
        .insert("f10.zadn.vn".to_owned(), Ok(vec![cdn_addr]));
    backend.doh_result.lock().insert(
        "dead.zadn.vn".to_owned(),
        Err(DnsResolveError::HostNotFound("dead.zadn.vn".to_owned())),
    );

    let resolver = ZaloDnsResolver::with_backend(backend.clone(), Duration::from_secs(3));

    // 1. System DNS fails on Zalo CDN host -> falls back to DoH and becomes sticky (600s).
    let resolved = resolver
        .resolve_host("f10.zadn.vn")
        .await
        .expect("DoH fallback must resolve f10.zadn.vn");
    assert_eq!(resolved, vec![cdn_addr]);
    assert!(resolver.uses_doh("f10.zadn.vn"));
    assert!(
        resolver
            .doh_ttl_remaining("f10.zadn.vn")
            .expect("doh ttl")
            > Duration::from_secs(100),
        "successful DoH lookup must promote TTL to 600s sticky window"
    );
    assert_eq!(backend.sys_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.doh_calls.load(Ordering::SeqCst), 1);

    // Second lookup on sticky DoH host skips broken system DNS completely!
    let resolved_again = resolver.resolve_host("f10.zadn.vn").await.expect("sticky DoH");
    assert_eq!(resolved_again, vec![cdn_addr]);
    assert_eq!(
        backend.sys_calls.load(Ordering::SeqCst),
        1,
        "sticky DoH host must bypass system DNS"
    );
    assert_eq!(backend.doh_calls.load(Ordering::SeqCst), 2);

    // 2. Authoritative NXDOMAIN caches in `dead_hosts` (300s) and short-circuits repeat lookups.
    let err = resolver
        .resolve_host("dead.zadn.vn")
        .await
        .expect_err("dead.zadn.vn must fail with NXDOMAIN");
    assert_eq!(err, DnsResolveError::HostNotFound("dead.zadn.vn".to_owned()));
    assert!(resolver.is_dead_host("dead.zadn.vn"));
    let doh_before = backend.doh_calls.load(Ordering::SeqCst);
    let sys_before = backend.sys_calls.load(Ordering::SeqCst);
    let cached_err = resolver
        .resolve_host("dead.zadn.vn")
        .await
        .expect_err("cached dead host must fail immediately");
    assert_eq!(cached_err, DnsResolveError::HostNotFound("dead.zadn.vn".to_owned()));
    assert_eq!(backend.doh_calls.load(Ordering::SeqCst), doh_before);
    assert_eq!(backend.sys_calls.load(Ordering::SeqCst), sys_before);

    // 3. Third-party link preview domain MUST NEVER trigger DoH fallback when system DNS fails!
    let doh_before_ext = backend.doh_calls.load(Ordering::SeqCst);
    let _ = resolver
        .resolve_host("link-preview.nytimes.com")
        .await
        .expect_err("broken system DNS for non-Zalo host must fail without DoH");
    assert_eq!(
        backend.doh_calls.load(Ordering::SeqCst),
        doh_before_ext,
        "third-party host must NEVER be sent to DoH"
    );
}

#[test]
fn header_sanitization_and_cross_host_redirect_stripping() {
    let mut raw = HashMap::new();
    raw.insert("Cloud-Viewer-Key".to_owned(), "cvk_secret_123".to_owned());
    raw.insert("Cookie".to_owned(), "zpw_sek=topsecret".to_owned());
    raw.insert("Authorization".to_owned(), "Bearer tok_xyz".to_owned());
    raw.insert("Proxy-Authorization".to_owned(), "Basic dXNlcjpwYXNz".to_owned());
    raw.insert("Accept".to_owned(), "image/webp,image/*".to_owned());
    raw.insert("X-Zalo-Req".to_owned(), "ok".to_owned());
    // Forbidden & malformed CRLF injection headers must be rejected.
    raw.insert("Host".to_owned(), "evil.example".to_owned());
    raw.insert("Transfer-Encoding".to_owned(), "chunked".to_owned());
    raw.insert("X-Injected".to_owned(), "bad\r\nEvil: 1".to_owned());
    raw.insert("Bad Name".to_owned(), "val".to_owned());

    // 1. HTTPS Zalo CDN host keeps sensitive headers (and drops forbidden/CRLF headers).
    let zalo_https = Url::parse("https://f10-zfcloud.zadn.vn/file.enc").unwrap();
    let sanitized = sanitize_headers(&raw, &zalo_https);
    let has_ci = |m: &HashMap<String, String>, name: &str| {
        m.keys().any(|k| k.eq_ignore_ascii_case(name))
    };
    assert!(has_ci(&sanitized, "cloud-viewer-key"));
    assert!(has_ci(&sanitized, "cookie"));
    assert!(has_ci(&sanitized, "authorization"));
    assert!(has_ci(&sanitized, "proxy-authorization"));
    assert!(has_ci(&sanitized, "accept"));
    assert!(has_ci(&sanitized, "x-zalo-req"));
    assert!(!has_ci(&sanitized, "host"));
    assert!(!has_ci(&sanitized, "transfer-encoding"));
    assert!(!has_ci(&sanitized, "x-injected"));
    assert!(!has_ci(&sanitized, "bad name"));

    // 2. Same-host HTTPS redirect on Zalo CDN preserves sensitive headers.
    let same_host_next = Url::parse("https://f10-zfcloud.zadn.vn/v2/file.enc").unwrap();
    let after_same = strip_sensitive_headers_for_redirect(
        &sanitized,
        zalo_https.host_str().unwrap(),
        &same_host_next,
    );
    assert!(has_ci(&after_same, "cloud-viewer-key"));
    assert!(has_ci(&after_same, "cookie"));

    // 3. Cross-host redirect (even to another subdomain or external CDN) strips ALL sensitive headers!
    let cross_host = Url::parse("https://f11-zfcloud.zadn.vn/v2/file.enc").unwrap();
    let after_cross = strip_sensitive_headers_for_redirect(
        &after_same,
        zalo_https.host_str().unwrap(),
        &cross_host,
    );
    assert!(!has_ci(&after_cross, "cloud-viewer-key"));
    assert!(!has_ci(&after_cross, "cookie"));
    assert!(!has_ci(&after_cross, "authorization"));
    assert!(!has_ci(&after_cross, "proxy-authorization"));
    // Non-sensitive headers survive redirect.
    assert_eq!(after_cross.get("Accept").unwrap(), "image/webp,image/*");
    assert_eq!(after_cross.get("X-Zalo-Req").unwrap(), "ok");

    // 4. Non-Zalo HTTPS host or plain HTTP Zalo host never gets sensitive headers initially either!
    let external_https = Url::parse("https://cdn.example.com/image.png").unwrap();
    let ext_hdrs = sanitize_headers(&raw, &external_https);
    assert!(!has_ci(&ext_hdrs, "cloud-viewer-key"));
    assert!(!has_ci(&ext_hdrs, "cookie"));
    assert!(!has_ci(&ext_hdrs, "authorization"));
    assert!(!has_ci(&ext_hdrs, "proxy-authorization"));

    let plain_http_zalo = Url::parse("http://f10-zfcloud.zadn.vn/file.enc").unwrap();
    let http_hdrs = sanitize_headers(&raw, &plain_http_zalo);
    assert!(!has_ci(&http_hdrs, "cloud-viewer-key"));
    assert!(!has_ci(&http_hdrs, "cookie"));
}

// ============================================================================
// 3. SecureStream & DownloadManager tests (Linear CTR, Block CTR, Streaming GCM, 14998)
// ============================================================================

/// Helper adapter forcing `Read::read` to yield small irregular chunks (e.g. 137 bytes)
/// to prove streaming ciphers handle arbitrary chunk boundaries accurately.
struct ShortChunkReader<R> {
    inner: R,
    max_step: usize,
}

impl<R: Read> Read for ShortChunkReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let cap = buf.len().min(self.max_step);
        self.inner.read(&mut buf[..cap])
    }
}

fn deterministic_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut out = vec![0u8; len];
    for byte in &mut out {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = (state & 0xFF) as u8;
    }
    out
}

#[test]
fn secure_stream_aes256_ctr_linear_streaming_roundtrip() {
    let tmp = TempDir::new().expect("tempdir");
    let key: [u8; 32] = deterministic_bytes(32, 101).try_into().unwrap();
    let iv: [u8; 16] = deterministic_bytes(16, 202).try_into().unwrap();

    // 2 MiB + 517 bytes spans multiple 1 MiB STREAM_CHUNK_BYTES buffers.
    let plaintext = deterministic_bytes(2 * 1024 * 1024 + 517, 303);
    let mut ciphertext = plaintext.clone();
    let mut enc = Ctr128BE::<Aes256>::new((&key).into(), (&iv).into());
    enc.apply_keystream(&mut ciphertext);
    assert_ne!(ciphertext, plaintext);

    let enc_path = tmp.path().join("linear.enc");
    let dec_path = tmp.path().join("linear.dec");
    fs::write(&enc_path, &ciphertext).expect("write linear.enc");

    let spec = SecureSpec::parse(&json!({
        "type": "e2ee",
        "cryptography": {
            "alg": "aes-256-ctr",
            "type": "linear",
            "key": STANDARD.encode(key),
            "iv": STANDARD.encode(iv),
        }
    }));
    let SecureSpec::Stream(stream) = spec else {
        panic!("expected valid Aes256CtrLinear stream, got {spec:?}");
    };
    assert_eq!(stream.mode(), &Aes256CtrLinear { key, iv });

    let written = stream
        .decrypt_file(&enc_path, &dec_path)
        .expect("decrypt linear ctr");
    assert_eq!(written, plaintext.len() as u64);
    assert!(!SecureStream::part_path_for(&dec_path).exists());
    assert_eq!(fs::read(&dec_path).expect("read linear.dec"), plaintext);
}

#[test]
fn secure_stream_aes256_ctr_block_resets_iv_every_4096_and_skips_373_byte_tail() {
    let tmp = TempDir::new().expect("tempdir");
    let key: [u8; 32] = deterministic_bytes(32, 404).try_into().unwrap();
    let iv: [u8; 16] = deterministic_bytes(16, 505).try_into().unwrap();

    // Exact test size required: `2 * 4096 + 373` = 8565 bytes!
    // Put identical 4096-byte content in block 0 and block 1 so we explicitly prove
    // per-block IV reset produces IDENTICAL ciphertexts for identical 4096-byte blocks,
    // and put distinct bytes in the 373-byte tail to prove `skipOverflow` leaves them UNENCRYPTED!
    let block0 = deterministic_bytes(4096, 606);
    let block1 = block0.clone();
    let tail = deterministic_bytes(373, 707);

    let mut plaintext = Vec::with_capacity(2 * 4096 + 373);
    plaintext.extend_from_slice(&block0);
    plaintext.extend_from_slice(&block1);
    plaintext.extend_from_slice(&tail);
    assert_eq!(plaintext.len(), 2 * 4096 + 373);

    // Construct Zalo `AES-256-CTR` `block` ciphertext:
    // - Block 0 (0..4096): encrypted with Ctr128BE(key, iv)
    // - Block 1 (4096..8192): cipher RE-INITIALIZED with the SAME (key, iv)
    // - Tail (8192..8565, 373 bytes < 4096): written VERBATIM without encryption!
    let mut ciphertext = Vec::with_capacity(plaintext.len());
    for chunk in [&block0[..], &block1[..]] {
        let mut buf = chunk.to_vec();
        let mut cipher = Ctr128BE::<Aes256>::new((&key).into(), (&iv).into());
        cipher.apply_keystream(&mut buf);
        ciphertext.extend_from_slice(&buf);
    }
    assert_eq!(
        &ciphertext[0..4096],
        &ciphertext[4096..8192],
        "identical blocks must encrypt identically when IV resets every 4096 bytes"
    );
    ciphertext.extend_from_slice(&tail);
    assert_eq!(
        &ciphertext[8192..],
        &tail[..],
        "trailing 373 bytes (< 4096) must remain unencrypted in wire format"
    );

    // Parse via `pcloud` JSON using `[u8]` array & `{"__bytes": ...}` representations.
    let key_json_array: Vec<u8> = key.to_vec();
    let spec = SecureSpec::parse(&json!({
        "type": "pcloud",
        "cryptography": {
            "alg": "aes-256-ctr",
            "type": "block",
            "blockSize": 4096,
            "key": key_json_array,
            "iv": { "__bytes": STANDARD.encode(iv) },
        }
    }));
    let SecureSpec::Stream(stream) = spec else {
        panic!("expected valid Aes256CtrBlock stream, got {spec:?}");
    };
    assert_eq!(
        stream.mode(),
        &Aes256CtrBlock {
            key,
            iv,
            block_size: 4096,
        }
    );

    // 1. File-based streaming decrypt.
    let enc_path = tmp.path().join("block_8565.enc");
    let dec_path = tmp.path().join("block_8565.dec");
    fs::write(&enc_path, &ciphertext).expect("write block_8565.enc");

    let written = stream
        .decrypt_file(&enc_path, &dec_path)
        .expect("decrypt_file block ctr");
    assert_eq!(written, (2 * 4096 + 373) as u64);
    assert!(!SecureStream::part_path_for(&dec_path).exists());
    assert_eq!(fs::read(&dec_path).expect("read block_8565.dec"), plaintext);

    // 2. Fragmented short-read streaming decrypt (137 bytes per read() call!) to prove
    // short reads NEVER prematurely trigger `skipOverflow` mid-stream!
    let dec_short_path = tmp.path().join("block_8565_short.dec");
    let short_reader = ShortChunkReader {
        inner: Cursor::new(&ciphertext),
        max_step: 137,
    };
    let written_short = stream
        .decrypt_reader_to_path(short_reader, &dec_short_path)
        .expect("decrypt fragmented block ctr");
    assert_eq!(written_short, (2 * 4096 + 373) as u64);
    assert_eq!(
        fs::read(&dec_short_path).expect("read block_8565_short.dec"),
        plaintext
    );
}

#[test]
fn secure_stream_aes256_gcm_multi_megabyte_roundtrip_16byte_iv_and_tamper_deletes_part() {
    let tmp = TempDir::new().expect("tempdir");
    let key: [u8; 32] = deterministic_bytes(32, 808).try_into().unwrap();
    let iv12: [u8; 12] = deterministic_bytes(12, 909).try_into().unwrap();

    // 3 MiB + 777 bytes (~3.15 MB): spans 4 x 1 MiB streaming chunks and has a non-16-byte-multiple tail (9 bytes)!
    let plaintext = deterministic_bytes(3 * 1024 * 1024 + 777, 1010);

    let aead = Aes256Gcm::new((&key).into());
    let mut sealed = aead
        .encrypt(Nonce::from_slice(&iv12), plaintext.as_slice())
        .expect("aes-256-gcm encrypt");
    let tag_offset = sealed.len() - 16;
    let tag: [u8; 16] = sealed[tag_offset..].try_into().unwrap();
    sealed.truncate(tag_offset);
    let ciphertext = sealed;
    assert_eq!(ciphertext.len(), plaintext.len());

    let enc_path = tmp.path().join("gcm_3mb.enc");
    let dec_path = tmp.path().join("gcm_3mb.dec");
    fs::write(&enc_path, &ciphertext).expect("write gcm_3mb.enc");

    let spec = SecureSpec::parse(&json!({
        "type": "e2ee",
        "cryptography": {
            "alg": "aes-256-gcm",
            "type": "linear",
            "key": STANDARD.encode(key),
            "iv": STANDARD.encode(iv12),
            "tag": STANDARD.encode(tag),
        }
    }));
    let SecureSpec::Stream(stream) = spec else {
        panic!("expected valid Aes256GcmLinear stream, got {spec:?}");
    };
    assert!(matches!(stream.mode(), SecureMode::Aes256GcmLinear { .. }));

    // 1. Valid multi-MiB streaming roundtrip (also test fragmented reads across 16-byte GHash blocks).
    let written = stream
        .decrypt_file(&enc_path, &dec_path)
        .expect("streaming GCM decrypt must succeed");
    assert_eq!(written, plaintext.len() as u64);
    assert!(!SecureStream::part_path_for(&dec_path).exists());
    assert_eq!(fs::read(&dec_path).expect("read gcm_3mb.dec"), plaintext);

    // 2. Tamper a single bit in the middle of the 3 MiB ciphertext -> MUST reject with TagMismatch,
    // delete `<dest>.part`, and NEVER create `<dest>`!
    let mut tampered = ciphertext.clone();
    tampered[1_500_000] ^= 0x01;
    let tampered_enc_path = tmp.path().join("gcm_tampered.enc");
    let tampered_dec_path = tmp.path().join("gcm_tampered.dec");
    let tampered_part_path = SecureStream::part_path_for(&tampered_dec_path);
    fs::write(&tampered_enc_path, &tampered).expect("write gcm_tampered.enc");

    let err = stream
        .decrypt_file(&tampered_enc_path, &tampered_dec_path)
        .expect_err("tampered GCM ciphertext must fail authentication");
    assert!(
        matches!(err, DecryptError::TagMismatch),
        "expected TagMismatch, got {err:?}"
    );
    assert!(
        !tampered_part_path.exists(),
        ".part file MUST be deleted when GCM authentication fails!"
    );
    assert!(
        !tampered_dec_path.exists(),
        "unauthenticated destination file MUST NEVER be created!"
    );

    // 3. Also test 16-byte IV GCM (Zalo E2EE / Node crypto non-96-bit IV compatibility).
    let iv16: [u8; 16] = deterministic_bytes(16, 1111).try_into().unwrap();
    let pt16 = deterministic_bytes(131_072 + 53, 1212);
    let aead16 = AesGcm::<Aes256, U16>::new((&key).into());
    let mut sealed16 = aead16
        .encrypt((&iv16).into(), pt16.as_slice())
        .expect("aes-256-gcm 16-byte iv encrypt");
    let tag16_off = sealed16.len() - 16;
    let tag16: [u8; 16] = sealed16[tag16_off..].try_into().unwrap();
    sealed16.truncate(tag16_off);

    let stream16 = SecureStream::new(Aes256GcmLinear {
        key,
        iv: iv16.to_vec(),
        tag: tag16,
    });
    let dec16_path = tmp.path().join("gcm_iv16.dec");
    let written16 = stream16
        .decrypt_reader_to_path(Cursor::new(&sealed16), &dec16_path)
        .expect("16-byte IV GCM decrypt");
    assert_eq!(written16, pt16.len() as u64);
    assert_eq!(fs::read(&dec16_path).expect("read gcm_iv16.dec"), pt16);
}

#[tokio::test]
async fn secure_spec_fails_closed_14998_and_download_manager_negative_cache_and_abort() {
    let tmp = TempDir::new().expect("tempdir");
    let cache_dir = tmp.path().join("cache");
    let save_dir = tmp.path().join("downloads");
    fs::create_dir_all(&save_dir).expect("create save_dir");

    // Verify malformed `e2ee` & `pcloud` specs parse to `SecureSpec::Invalid` (fail closed!).
    for malformed in [
        json!({ "type": "e2ee" }),
        json!({ "type": "e2ee", "cryptography": { "alg": "AES-128-CBC", "key": STANDARD.encode([0u8; 32]), "iv": STANDARD.encode([0u8; 16]) } }),
        json!({ "type": "pcloud", "cryptography": { "alg": "AES-256-CTR", "mode": "unknown_mode", "key": STANDARD.encode([0u8; 32]), "iv": STANDARD.encode([0u8; 16]) } }),
        json!({ "type": "e2ee", "cryptography": { "alg": "AES-256-CTR", "mode": "linear", "key": STANDARD.encode([0u8; 16]), "iv": STANDARD.encode([0u8; 16]) } }),
        json!({ "type": "e2ee", "cryptography": { "alg": "AES-256-GCM", "mode": "linear", "key": STANDARD.encode([0u8; 32]), "iv": STANDARD.encode([0u8; 12]) } }), // missing tag
        json!({ "type": "e2ee", "cryptography": { "alg": "AES-256-GCM", "mode": "linear", "key": STANDARD.encode([0u8; 32]), "iv": STANDARD.encode([0u8; 8]), "tag": STANDARD.encode([0u8; 16]) } }), // short IV < 12
    ] {
        assert_eq!(
            SecureSpec::parse(&malformed),
            SecureSpec::Invalid,
            "malformed spec must fail closed: {malformed}"
        );
    }

    // Verify error code mappings match `DownloadManager.swift`.
    assert_eq!(DownloadFailure::http(404).code, 75404);
    assert_eq!(DownloadFailure::http(403).code, 75403);
    assert_eq!(
        DownloadFailure::from_media_error(&MediaLoadError::HostNotFound).code,
        30034
    );
    assert_eq!(
        DownloadFailure::from_media_error(&MediaLoadError::TimedOut).code,
        30047
    );
    assert_eq!(
        DownloadFailure::from_media_error(&MediaLoadError::TtfbTimeout).code,
        80027
    );
    assert_eq!(DownloadFailure::decrypt_failed().code, 14998);
    assert_eq!(DownloadFailure::decrypt_failed().name, "INS_OF_ERROR_ERR");

    // Spin up a local HTTP server serving:
    // - `/cipher.bin` -> 200 OK with raw ciphertext
    // - `/missing.bin` -> 404 Not Found (counting hits)
    // - `/slow.bin` -> hangs for 10s so we can test `abort`
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    let server_hits = Arc::new(AtomicUsize::new(0));
    let missing_hits = Arc::new(AtomicUsize::new(0));

    let hits_clone = Arc::clone(&server_hits);
    let missing_clone = Arc::clone(&missing_hits);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let hits = Arc::clone(&hits_clone);
            let missing = Arc::clone(&missing_clone);
            tokio::spawn(async move {
                let mut req_buf = [0u8; 4096];
                let n = stream.read(&mut req_buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let req_text = String::from_utf8_lossy(&req_buf[..n]);
                let first_line = req_text.lines().next().unwrap_or("");
                if first_line.contains("/missing.bin") {
                    missing.fetch_add(1, Ordering::SeqCst);
                    let _ = stream
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found")
                        .await;
                } else if first_line.contains("/slow.bin") {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                } else {
                    let body = b"RAW_CIPHERTEXT_MUST_NEVER_BE_SAVED";
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(body).await;
                }
            });
        }
    });

    let loader =
        MediaLoader::new(&cache_dir, Arc::new(ZaloDnsResolver::new())).expect("MediaLoader");
    let dm = DownloadManager::new(
        loader,
        tmp.path().join("tmp"),
        Some(vec![save_dir.clone()]),
    );

    // 1. Malformed `e2ee` request MUST fail closed with `14998` BEFORE touching the network
    // and MUST NOT write raw ciphertext to `save_dir/secret.pdf`!
    let bad_dest = save_dir.join("secret.pdf");
    let resp = dm
        .handle_message(json!({
            "channel": "new-download-file",
            "data": {
                "key": "dl-bad-e2ee",
                "downloadFrom": "msg",
                "type": "File",
                "data": {
                    "url": format!("http://{addr}/cipher.bin"),
                    "fileName": "secret.pdf",
                },
                "options": {
                    "saveDir": save_dir.to_str().unwrap(),
                },
                "secure": {
                    "type": "e2ee",
                    "cryptography": {
                        "alg": "UNKNOWN-CIPHER",
                    }
                }
            }
        }))
        .await
        .expect("download response");

    assert_eq!(resp["channel"], "new-download-file-resp");
    assert_eq!(resp["data"]["downloadError"]["code"], 14998);
    assert_eq!(resp["data"]["downloadError"]["name"], "INS_OF_ERROR_ERR");
    assert!(resp["data"]["savePath"].is_null());
    assert!(!bad_dest.exists(), "raw ciphertext must NEVER be written on 14998");
    assert_eq!(
        server_hits.load(Ordering::SeqCst),
        0,
        "fail-closed spec must reject before issuing HTTP request"
    );

    // 2. 404 negative URL cache: cached on automatic download, bypassed when `srcAction == "Manual"`.
    let make_404_req = |manual: bool| {
        json!({
            "channel": "new-download-file",
            "data": {
                "key": "dl-404",
                "downloadFrom": "chat",
                "type": "Photo",
                "data": {
                    "url": format!("http://{addr}/missing.bin"),
                    "fileName": "missing.jpg",
                },
                "options": {
                    "saveDir": save_dir.to_str().unwrap(),
                    "srcAction": if manual { "Manual" } else { "Auto" },
                }
            }
        })
    };

    let r1 = dm.handle_message(make_404_req(false)).await.unwrap();
    assert_eq!(r1["data"]["downloadError"]["code"], 75404);
    assert_eq!(missing_hits.load(Ordering::SeqCst), 1);

    // Second automatic attempt hits the 600s negative cache without contacting the server!
    let r2 = dm.handle_message(make_404_req(false)).await.unwrap();
    assert_eq!(r2["data"]["downloadError"]["code"], 75404);
    assert_eq!(
        missing_hits.load(Ordering::SeqCst),
        1,
        "auto retry must be served from 404 negative cache"
    );

    // Manual user retry (`srcAction == "Manual"`) bypasses the negative cache!
    let r3 = dm.handle_message(make_404_req(true)).await.unwrap();
    assert_eq!(r3["data"]["downloadError"]["code"], 75404);
    assert_eq!(
        missing_hits.load(Ordering::SeqCst),
        2,
        "srcAction == Manual must bypass 404 negative cache"
    );

    // 3. Cancellation via `"abort"` channel returns code `1009`.
    let dm_abort = dm.clone();
    let save_dir_str = save_dir.to_str().unwrap().to_owned();
    let slow_task = tokio::spawn(async move {
        dm_abort
            .handle_message(json!({
                "channel": "new-download-file",
                "data": {
                    "key": "slow-key-1",
                    "downloadFrom": "chat",
                    "type": "File",
                    "data": {
                        "url": format!("http://{addr}/slow.bin"),
                        "fileName": "slow.bin",
                    },
                    "options": {
                        "saveDir": save_dir_str,
                    }
                }
            }))
            .await
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    let abort_ret = dm
        .handle_message(json!({
            "channel": "abort",
            "data": {
                "key": "slow-key-1",
                "downloadFrom": "chat",
            }
        }))
        .await;
    assert!(abort_ret.is_none());

    let aborted_resp = tokio::time::timeout(Duration::from_secs(2), slow_task)
        .await
        .expect("abort must cancel promptly")
        .expect("join")
        .expect("response");
    assert_eq!(aborted_resp["data"]["downloadError"]["code"], 1009);
}

// ============================================================================
// 4. MediaLoader concurrency semaphore, deduplication, MIME sniffing & HTTP 206 Range
// ============================================================================

#[tokio::test]
async fn media_loader_6_slot_semaphore_deduplication_and_lru_cache_trim() {
    let tmp = TempDir::new().expect("tempdir");
    let cache_dir = tmp.path().join("media_cache");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();

    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let dedup_hits = Arc::new(AtomicUsize::new(0));

    let active_srv = Arc::clone(&active);
    let peak_srv = Arc::clone(&peak);
    let dedup_srv = Arc::clone(&dedup_hits);

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let active = Arc::clone(&active_srv);
            let peak = Arc::clone(&peak_srv);
            let dedup = Arc::clone(&dedup_srv);
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(cur, Ordering::SeqCst);

                let req_str = String::from_utf8_lossy(&buf[..n]);
                if req_str.contains("/same-asset.png") {
                    dedup.fetch_add(1, Ordering::SeqCst);
                }

                tokio::time::sleep(Duration::from_millis(50)).await;
                let payload = b"\x89PNG\r\n\x1a\npayload-bytes";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.write_all(payload).await;
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });

    let loader =
        MediaLoader::new(&cache_dir, Arc::new(ZaloDnsResolver::new())).expect("MediaLoader::new");

    // 1. Fire 15 distinct concurrent fetches -> peak concurrency must be capped at <= 6!
    let mut handles = Vec::new();
    for i in 0..15 {
        let l = loader.clone();
        let url = format!("http://{addr}/distinct-{i}.png");
        handles.push(tokio::spawn(async move {
            l.fetch_bytes(&url, Duration::from_secs(5)).await
        }));
    }
    for h in handles {
        let data = h.await.expect("join").expect("fetch_bytes");
        assert!(data.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        (1..=6).contains(&observed_peak),
        "6-slot semaphore violated: peak server concurrency was {observed_peak}"
    );

    // 2. Fire 10 concurrent requests for the SAME URL -> in-flight deduplication must hit server once!
    let mut dup_handles = Vec::new();
    for _ in 0..10 {
        let l = loader.clone();
        let url = format!("http://{addr}/same-asset.png");
        dup_handles.push(tokio::spawn(async move {
            l.fetch_bytes(&url, Duration::from_secs(5)).await
        }));
    }
    for h in dup_handles {
        let data = h.await.expect("join").expect("dedup fetch");
        assert_eq!(&data[..], b"\x89PNG\r\n\x1a\npayload-bytes");
    }
    assert_eq!(
        dedup_hits.load(Ordering::SeqCst),
        1,
        "concurrent fetches for identical URL must deduplicate into 1 upstream request"
    );

    // 3. Verify `local_file` FNV-1a cache naming and `trim_cache_dir` LRU eviction.
    let cached_url = format!("http://{addr}/video-clip.mp4");
    let cached_path = loader.local_file(&cached_url).await.expect("local_file");
    let expected_name = format!("{}.mp4", fnv1a_64_hex(&cached_url));
    assert_eq!(
        cached_path.file_name().and_then(|s| s.to_str()),
        Some(expected_name.as_str())
    );

    let files_dir = loader.files_dir();
    let old_file = files_dir.join("oldest.bin");
    let new_file = files_dir.join("newest.bin");
    fs::write(&old_file, vec![b'A'; 4096]).unwrap();
    let past = SystemTime::now() - Duration::from_secs(3600);
    fs::File::open(&old_file)
        .unwrap()
        .set_modified(past)
        .unwrap();
    fs::write(&new_file, vec![b'B'; 4096]).unwrap();

    // Trim budget to 5000 bytes -> `oldest.bin` (4096B) must be evicted while `newest.bin` survives!
    trim_cache_dir(&files_dir, 5000, None, Duration::from_secs(600));
    assert!(!old_file.exists(), "LRU trim must evict oldest mtime file first");
    assert!(new_file.exists(), "LRU trim must keep newest file within budget");
}

#[test]
fn sniff_media_mime_and_serve_local_media_range_206_and_confinement() {
    let dummy = PathBuf::from("unknown.bin");
    assert_eq!(
        sniff_media_mime(b"\x89PNG\r\n\x1a\n0000", &dummy),
        "image/png"
    );
    assert_eq!(sniff_media_mime(b"\xFF\xD8\xFF\xE00000", &dummy), "image/jpeg");
    assert_eq!(sniff_media_mime(b"GIF89a0000", &dummy), "image/gif");
    assert_eq!(sniff_media_mime(b"RIFF\x24\x00\x00\x00WEBP", &dummy), "image/webp");
    assert_eq!(sniff_media_mime(b"ID3\x04\x00\x00", &dummy), "audio/mpeg");
    assert_eq!(sniff_media_mime(b"OggS\x00\x02", &dummy), "audio/ogg");
    assert_eq!(
        sniff_media_mime(b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00", &dummy),
        "video/mp4"
    );
    assert_eq!(
        sniff_media_mime(b"\x00\x00\x00\x20ftypM4A \x00\x00\x00\x00", &dummy),
        "audio/mp4"
    );
    assert_eq!(
        sniff_media_mime(b"\x00\x00\x00\x14ftypqt  \x00\x00\x00\x00", &dummy),
        "video/quicktime"
    );
    assert_eq!(
        sniff_media_mime(b"\x00\x00\x00\x14ftyp3gp5\x00\x00\x00\x00", &dummy),
        "video/3gpp"
    );

    // Build a 1024-byte synthetic MP4 file inside `allowed_root` and a secret file outside it.
    let tmp = TempDir::new().expect("tempdir");
    let allowed_root = tmp.path().join("allowed_media");
    let outside_root = tmp.path().join("outside_secrets");
    fs::create_dir_all(&allowed_root).unwrap();
    fs::create_dir_all(&outside_root).unwrap();

    let mut mp4_bytes = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00".to_vec();
    while mp4_bytes.len() < 1024 {
        mp4_bytes.push((mp4_bytes.len() & 0xFF) as u8);
    }
    let video_path = allowed_root.join("clip.mp4");
    fs::write(&video_path, &mp4_bytes).unwrap();

    let secret_path = outside_root.join("passwd.txt");
    fs::write(&secret_path, b"top-secret").unwrap();

    let roots = vec![allowed_root.clone()];

    // 1. Full `200 OK` when no `Range` header is supplied.
    let full = serve_local_media_range(&video_path, None, &roots).expect("200 OK");
    assert_eq!(full.status, 200);
    assert_eq!(full.mime_type, "video/mp4");
    assert_eq!(full.accept_ranges, "bytes");
    assert_eq!(full.content_range, None);
    assert_eq!(full.content_length, 1024);
    assert_eq!(full.body, mp4_bytes);

    // 2. `206 Partial Content` closed byte range (`bytes=100-299`).
    let slice =
        serve_local_media_range(&video_path, Some("bytes=100-299"), &roots).expect("206 range");
    assert_eq!(slice.status, 206);
    assert_eq!(slice.status_text, "Partial Content");
    assert_eq!(slice.mime_type, "video/mp4");
    assert_eq!(slice.accept_ranges, "bytes");
    assert_eq!(slice.content_range.as_deref(), Some("bytes 100-299/1024"));
    assert_eq!(slice.headers.get("Content-Range").map(String::as_str), Some("bytes 100-299/1024"));
    assert_eq!(slice.headers.get("Accept-Ranges").map(String::as_str), Some("bytes"));
    assert_eq!(slice.content_length, 200);
    assert_eq!(slice.body, &mp4_bytes[100..=299]);

    // 3. `206 Partial Content` open-ended (`bytes=900-`) and suffix (`bytes=-64`) ranges.
    let open_end =
        serve_local_media_range(&video_path, Some("bytes=900-"), &roots).expect("206 open range");
    assert_eq!(open_end.status, 206);
    assert_eq!(open_end.content_range.as_deref(), Some("bytes 900-1023/1024"));
    assert_eq!(open_end.body, &mp4_bytes[900..1024]);

    let suffix =
        serve_local_media_range(&video_path, Some("bytes=-64"), &roots).expect("206 suffix range");
    assert_eq!(suffix.status, 206);
    assert_eq!(suffix.content_range.as_deref(), Some("bytes 960-1023/1024"));
    assert_eq!(suffix.body, &mp4_bytes[960..1024]);

    // 4. Out-of-bounds range returns `416 Range Not Satisfiable`.
    let unsat = serve_local_media_range(&video_path, Some("bytes=2048-4096"), &roots)
        .expect("416 out of bounds");
    assert_eq!(unsat.status, 416);
    assert_eq!(unsat.content_range.as_deref(), Some("bytes */1024"));
    assert!(unsat.body.is_empty());

    // 5. Path traversal (`../`) and symlink escape outside `allowed_roots` MUST fail!
    let traversal_path = allowed_root.join("../outside_secrets/passwd.txt");
    assert!(
        serve_local_media_range(&traversal_path, None, &roots).is_err(),
        "path traversal escaping allowed_roots must be rejected"
    );
    assert!(
        serve_local_media_range(&secret_path, None, &roots).is_err(),
        "direct path outside allowed_roots must be rejected"
    );
    assert!(
        serve_local_media_range(&allowed_root, None, &roots).is_err(),
        "allowed_root directory itself must not be served as a file"
    );

    #[cfg(unix)]
    {
        let symlink_escape = allowed_root.join("escape_link.mp4");
        std::os::unix::fs::symlink(&secret_path, &symlink_escape).expect("symlink");
        assert!(
            serve_local_media_range(&symlink_escape, None, &roots).is_err(),
            "symlink pointing outside allowed_roots must be rejected"
        );
    }
}

// ============================================================================
// 5. NetworkProxy HTTP (multipart, observer, cookies) & WebSocket (101, frames, 1006 drop)
// ============================================================================

#[tokio::test]
async fn network_proxy_http_multipart_observer_and_websocket_upgrade_and_1006_drop() {
    let tmp = TempDir::new().expect("tempdir");
    let cookie_path = tmp.path().join("proxy_cookies.json");
    let jar = ZaloCookieJar::open(&cookie_path).expect("open jar");

    let observed: Arc<Mutex<Vec<(String, u16, Vec<u8>)>>> = Arc::new(Mutex::new(Vec::new()));
    let obs_clone = Arc::clone(&observed);
    let proxy = NetworkProxy::new(jar.clone())
        .expect("NetworkProxy::new")
        .with_response_observer(Arc::new(move |url, status, bytes| {
            obs_clone
                .lock()
                .push((url.to_owned(), status, bytes.to_vec()));
        }));

    assert!(is_observed_zalo_endpoint(
        "https://wpa.zaloapp.com/api/session/kickout-info?zpw_ver=694"
    ));
    assert!(is_observed_zalo_endpoint(
        "https://qr-id.zaloapp.com/polling/qr/abc123"
    ));
    assert_eq!(
        describe_zalo_frame(&[1, 0x09, 0x02, 3, 0xAA, 0xBB]),
        "v=1 cmd=521 sub=3 len=6"
    );

    // 1. Local HTTP server verifying User-Agent, multipart form-data, Set-Cookie persistence, & observer.
    let http_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind http");
    let http_addr = http_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = http_listener.accept().await.expect("accept http");
        let mut req_bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut expected_total = None;
        loop {
            let n = stream.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            req_bytes.extend_from_slice(&chunk[..n]);
            if expected_total.is_none() {
                if let Some(hdr_end) = req_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let hdr_str = String::from_utf8_lossy(&req_bytes[..hdr_end]);
                    let cl = hdr_str
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            if k.eq_ignore_ascii_case("content-length") {
                                v.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    expected_total = Some(hdr_end + 4 + cl);
                }
            }
            if let Some(total) = expected_total {
                if req_bytes.len() >= total {
                    break;
                }
            }
        }

        let req_text = String::from_utf8_lossy(&req_bytes);
        assert!(req_text.contains(&format!("user-agent: {DEFAULT_USER_AGENT}").to_ascii_lowercase())
            || req_text.contains(DEFAULT_USER_AGENT));
        assert!(req_text.contains("multipart/form-data; boundary="));
        assert!(req_text.contains("hello-field-val"));
        assert!(req_text.contains("avatar.png"));
        assert!(req_text.contains("PNGDATA"));

        let resp_body = br#"{"error_code":0,"kickout":false}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             X-Zalo-Edge: vn-hcm\r\n\
             Set-Cookie: local_sek=ws_upgrade_secret; Path=/; Max-Age=3600; HttpOnly\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            resp_body.len()
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.write_all(resp_body).await;
    });

    let http_resp = proxy
        .handle_request(
            json!({
                "op": "http",
                "url": format!("http://{http_addr}/api/session/kickout-info"),
                "method": "POST",
                "headers": {
                    "Content-Type": "multipart/form-data",
                    "X-Custom": "1",
                },
                "body": {
                    "kind": "formdata",
                    "parts": [
                        { "name": "caption", "value": "hello-field-val" },
                        {
                            "name": "file",
                            "filename": "avatar.png",
                            "type": "image/png",
                            "data": STANDARD.encode(b"PNGDATA"),
                        }
                    ]
                }
            }),
            WsEventSink::noop(),
        )
        .await
        .expect("handle_request http");

    assert_eq!(http_resp["status"], 200);
    assert_eq!(http_resp["headers"]["x-zalo-edge"], "vn-hcm");
    assert!(
        http_resp["headers"].get("set-cookie").is_none(),
        "Set-Cookie must not leak into XHR headers map"
    );
    let decoded_body = STANDARD
        .decode(http_resp["body"].as_str().unwrap())
        .expect("b64 body");
    assert_eq!(decoded_body, br#"{"error_code":0,"kickout":false}"#);

    // Observer must have recorded `/api/session/kickout-info`, and HttpOnly `local_sek` must be persisted!
    assert_eq!(observed.lock().len(), 1);
    assert_eq!(
        jar.cookie_header_for_url(&format!("ws://{http_addr}/ws"))
            .as_deref(),
        Some("local_sek=ws_upgrade_secret")
    );

    // 2. Local WebSocket server verifying headers (`Origin: file://`, `User-Agent`, `Cookie`,
    // `Sec-WebSocket-Protocol`), HTTP 101 `open`, text + binary frames, and abnormal drop -> error + 1006!
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ws");
    let ws_addr = ws_listener.local_addr().unwrap();

    // Insert HttpOnly cookie for `ws_addr` so we verify `ws.open` sends HttpOnly cookies on upgrade!
    jar.insert_set_cookie(
        "zpw_sek=ws_cookie_tok_777; Path=/; Max-Age=3600; HttpOnly",
        &format!("http://{ws_addr}/"),
    )
    .expect("insert ws cookie");

    let captured_headers: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let hdr_capture = Arc::clone(&captured_headers);

    tokio::spawn(async move {
        let (tcp, _) = ws_listener.accept().await.expect("accept ws");
        let mut ws_stream = tokio_tungstenite::accept_hdr_async(
            tcp,
            move |req: &WsHandshakeReq, mut resp: WsHandshakeResp| {
                let mut map = hdr_capture.lock();
                for (k, v) in req.headers() {
                    if let Ok(s) = v.to_str() {
                        map.insert(k.as_str().to_ascii_lowercase(), s.to_owned());
                    }
                }
                resp.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    "zalo-v1".parse().unwrap(),
                );
                Ok(resp)
            },
        )
        .await
        .expect("ws handshake");

        // Receive text frame from client and echo back a text frame.
        if let Some(Ok(WsMessage::Text(t))) = ws_stream.next().await {
            ws_stream
                .send(WsMessage::Text(format!("ack:{t}").into()))
                .await
                .unwrap();
        }
        // Receive binary frame from client and echo back a binary frame, then drop raw TCP abruptly
        // WITHOUT sending a WebSocket Close frame!
        if let Some(Ok(WsMessage::Binary(b))) = ws_stream.next().await {
            let mut reply = vec![1u8, 0x10, 0x00, 0u8];
            reply.extend_from_slice(&b);
            ws_stream
                .send(WsMessage::Binary(reply.into()))
                .await
                .unwrap();
        }
        // Abruptly drop `ws_stream` without Close handshake -> client must emit `error` then `close` 1006!
        drop(ws_stream);
    });

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<WsEvent>();
    let sink = WsEventSink::from_sender(ev_tx);

    proxy
        .handle_request(
            json!({
                "op": "ws.open",
                "id": 42,
                "url": format!("ws://{ws_addr}/ws"),
                "protocols": ["zalo-v1", "zalo-v2"],
            }),
            sink.clone(),
        )
        .await
        .expect("ws.open");

    let open_ev = tokio::time::timeout(Duration::from_secs(2), ev_rx.recv())
        .await
        .expect("open timeout")
        .expect("open event");
    assert_eq!(open_ev.id, 42);
    assert_eq!(open_ev.kind, "open");
    assert_eq!(open_ev.payload["protocol"], "zalo-v1");

    {
        let hdrs = captured_headers.lock();
        assert_eq!(hdrs.get("origin").map(String::as_str), Some(WEBSOCKET_ORIGIN));
        assert_eq!(
            hdrs.get("user-agent").map(String::as_str),
            Some(DEFAULT_USER_AGENT)
        );
        assert_eq!(
            hdrs.get("sec-websocket-protocol").map(String::as_str),
            Some("zalo-v1, zalo-v2")
        );
        let cookie_hdr = hdrs.get("cookie").expect("Cookie header on WS upgrade");
        assert!(cookie_hdr.contains("zpw_sek=ws_cookie_tok_777"));
        assert!(cookie_hdr.contains("local_sek=ws_upgrade_secret"));
    }

    // Send text frame and assert incoming text event.
    proxy
        .handle_request(
            json!({ "op": "ws.send", "id": 42, "text": "ping-zalo" }),
            sink.clone(),
        )
        .await
        .expect("ws.send text");

    let text_ev = tokio::time::timeout(Duration::from_secs(2), ev_rx.recv())
        .await
        .expect("text timeout")
        .expect("text event");
    assert_eq!(text_ev.kind, "text");
    assert_eq!(text_ev.payload["text"], "ack:ping-zalo");

    // Send binary frame and assert incoming binary event, followed by abnormal `error` + `close` (1006).
    let raw_bin = [1u8, 2, 3, 4, 5, 6];
    proxy
        .handle_request(
            json!({ "op": "ws.send", "id": 42, "data": STANDARD.encode(raw_bin) }),
            sink,
        )
        .await
        .expect("ws.send binary");

    let bin_ev = tokio::time::timeout(Duration::from_secs(2), ev_rx.recv())
        .await
        .expect("binary timeout")
        .expect("binary event");
    assert_eq!(bin_ev.kind, "binary");
    let received_bin = STANDARD
        .decode(bin_ev.payload["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(received_bin, [1u8, 0x10, 0x00, 0u8, 1, 2, 3, 4, 5, 6]);

    let err_ev = tokio::time::timeout(Duration::from_secs(2), ev_rx.recv())
        .await
        .expect("error timeout")
        .expect("error event");
    assert_eq!(err_ev.id, 42);
    assert_eq!(err_ev.kind, "error");

    let close_ev = tokio::time::timeout(Duration::from_secs(2), ev_rx.recv())
        .await
        .expect("close timeout")
        .expect("close event");
    assert_eq!(close_ev.id, 42);
    assert_eq!(close_ev.kind, "close");
    assert_eq!(close_ev.payload["code"], 1006);
    assert_eq!(close_ev.payload["reason"], "");
    assert_eq!(close_ev.payload["wasClean"], false);
}

// ============================================================================
// 6. Additional end-to-end encrypted DownloadManager, redirect cap, & DNS timeout tests
// ============================================================================

#[tokio::test]
async fn doh_resolver_system_dns_timeout_triggers_doh_only_for_zalo_cdn() {
    let backend = Arc::new(MockDnsBackend {
        sys_calls: AtomicUsize::new(0),
        doh_calls: AtomicUsize::new(0),
        // Simulate hung ISP DNS (250ms > 40ms resolver timeout).
        sys_delay: Duration::from_millis(250),
        sys_result: Mutex::new(HashMap::new()),
        doh_result: Mutex::new(HashMap::new()),
    });
    let expected_ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(103, 199, 77, 88)), 443);
    backend
        .doh_result
        .lock()
        .insert("ws5-msg.chat.zalo.me".to_owned(), Ok(vec![expected_ip]));

    let resolver = ZaloDnsResolver::with_backend(backend.clone(), Duration::from_millis(40));

    // Zalo host times out on system DNS after 40ms and succeeds via DoH!
    let addrs = resolver
        .resolve_host("ws5-msg.chat.zalo.me")
        .await
        .expect("DoH must rescue timed-out Zalo DNS");
    assert_eq!(addrs, vec![expected_ip]);
    assert!(resolver.uses_doh("ws5-msg.chat.zalo.me"));
    assert_eq!(backend.doh_calls.load(Ordering::SeqCst), 1);

    // Non-Zalo host times out after 40ms and does NOT call DoH!
    let err = resolver
        .resolve_host("preview.github.io")
        .await
        .expect_err("non-Zalo DNS timeout must fail without DoH");
    assert!(matches!(err, DnsResolveError::SystemTimeout(_)));
    assert_eq!(
        backend.doh_calls.load(Ordering::SeqCst),
        1,
        "non-Zalo DNS timeout must never invoke DoH"
    );
}

#[tokio::test]
async fn download_manager_end_to_end_encrypted_http_and_redirect_limit() {
    let tmp = TempDir::new().expect("tempdir");
    let cache_dir = tmp.path().join("cache");
    let save_dir = tmp.path().join("saved");
    fs::create_dir_all(&save_dir).unwrap();

    let key: [u8; 32] = deterministic_bytes(32, 4242).try_into().unwrap();
    let iv_ctr: [u8; 16] = deterministic_bytes(16, 4343).try_into().unwrap();
    let plaintext = deterministic_bytes(2 * 4096 + 373, 4444);

    // Encrypt using Zalo `pcloud` block CTR (resetting IV every 4096B + 373B verbatim tail).
    let mut block_ct = Vec::with_capacity(plaintext.len());
    for chunk in plaintext[..8192].chunks_exact(4096) {
        let mut buf = chunk.to_vec();
        let mut c = Ctr128BE::<Aes256>::new((&key).into(), (&iv_ctr).into());
        c.apply_keystream(&mut buf);
        block_ct.extend_from_slice(&buf);
    }
    block_ct.extend_from_slice(&plaintext[8192..]);

    // Also prepare a tampered GCM payload to test end-to-end 14998 over HTTP.
    let iv_gcm: [u8; 12] = deterministic_bytes(12, 4545).try_into().unwrap();
    let aead = Aes256Gcm::new((&key).into());
    let mut gcm_sealed = aead
        .encrypt(Nonce::from_slice(&iv_gcm), b"hello e2ee".as_slice())
        .unwrap();
    let gcm_tag: [u8; 16] = gcm_sealed[gcm_sealed.len() - 16..].try_into().unwrap();
    gcm_sealed.truncate(gcm_sealed.len() - 16);
    gcm_sealed[0] ^= 0xFF; // corrupt ciphertext!

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    let block_ct_srv = Arc::new(block_ct);
    let gcm_bad_srv = Arc::new(gcm_sealed);

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let block_ct = Arc::clone(&block_ct_srv);
            let gcm_bad = Arc::clone(&gcm_bad_srv);
            tokio::spawn(async move {
                let mut req_buf = [0u8; 2048];
                let n = stream.read(&mut req_buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let line = String::from_utf8_lossy(&req_buf[..n]);
                let first = line.lines().next().unwrap_or("");
                if first.contains("/loop") {
                    let resp = format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{addr}/loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                } else if first.contains("/bad-gcm.enc") {
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        gcm_bad.len()
                    );
                    let _ = stream.write_all(hdr.as_bytes()).await;
                    let _ = stream.write_all(&gcm_bad).await;
                } else {
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        block_ct.len()
                    );
                    let _ = stream.write_all(hdr.as_bytes()).await;
                    let _ = stream.write_all(&block_ct).await;
                }
            });
        }
    });

    let loader =
        MediaLoader::new(&cache_dir, Arc::new(ZaloDnsResolver::new())).expect("MediaLoader");
    let dm = DownloadManager::new(
        loader.clone(),
        tmp.path().join("tmp"),
        Some(vec![save_dir.clone()]),
    );

    // 1. End-to-end `pcloud` block CTR download -> decrypts to `save_dir/doc.bin`.
    let req_ok = json!({
        "channel": "new-download-file",
        "data": {
            "key": "pcloud-1",
            "downloadFrom": "cloud",
            "type": "File",
            "data": {
                "url": format!("http://{addr}/pcloud.enc"),
                "fileName": "doc.bin",
            },
            "options": {
                "saveDir": save_dir.to_str().unwrap(),
            },
            "secure": {
                "type": "pcloud",
                "cryptography": {
                    "alg": "aes-256-ctr",
                    "type": "block",
                    "blockSize": 4096,
                    "key": STANDARD.encode(key),
                    "iv": STANDARD.encode(iv_ctr),
                }
            }
        }
    });
    let resp_ok = dm.handle_message(req_ok.clone()).await.unwrap();
    assert!(resp_ok["data"]["downloadError"].is_null());
    assert_eq!(resp_ok["data"]["isCached"], false);
    let saved_path = PathBuf::from(resp_ok["data"]["savePath"].as_str().unwrap());
    assert_eq!(fs::read(&saved_path).unwrap(), plaintext);

    // Second request for already-saved destination returns `isCached: true`.
    let resp_cached = dm.handle_message(req_ok).await.unwrap();
    assert_eq!(resp_cached["data"]["isCached"], true);

    // 2. End-to-end tampered GCM download -> returns `14998` and leaves neither `.part` nor `tampered.bin`.
    let tampered_dest = save_dir.join("tampered.bin");
    let resp_tampered = dm
        .handle_message(json!({
            "channel": "new-download-file",
            "data": {
                "key": "gcm-tampered-http",
                "downloadFrom": "msg",
                "type": "File",
                "data": {
                    "url": format!("http://{addr}/bad-gcm.enc"),
                    "fileName": "tampered.bin",
                },
                "options": {
                    "saveDir": save_dir.to_str().unwrap(),
                },
                "secure": {
                    "type": "e2ee",
                    "cryptography": {
                        "alg": "aes-256-gcm",
                        "type": "linear",
                        "key": STANDARD.encode(key),
                        "iv": STANDARD.encode(iv_gcm),
                        "tag": STANDARD.encode(gcm_tag),
                    }
                }
            }
        }))
        .await
        .unwrap();
    assert_eq!(resp_tampered["data"]["downloadError"]["code"], 14998);
    assert!(!tampered_dest.exists());
    assert!(!SecureStream::part_path_for(&tampered_dest).exists());

    // 3. Redirect loop (> 3 hops) fails with `TooManyRedirects` (`30310`).
    let loop_err = loader
        .fetch_bytes(&format!("http://{addr}/loop"), Duration::from_secs(5))
        .await
        .expect_err("redirect loop must be capped at 3 hops");
    assert_eq!(loop_err, MediaLoadError::TooManyRedirects);
    assert_eq!(DownloadFailure::from_media_error(&loop_err).code, 30310);
}
