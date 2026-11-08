//! Concurrent media loader, header sanitizer, FNV-1a disk cache with 2 GiB LRU eviction,
//! magic-byte MIME sniffer, and confined HTTP `206 Partial Content` byte-range file responder.
//!
//! Port of `MediaLoader.swift` plus local media range serving for `olaz-media://` HTML5 playback.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use thiserror::Error;
use tokio::sync::{watch, Semaphore};
use tokio::task::AbortHandle;
use url::Url;
use uuid::Uuid;

use crate::doh::{is_zalo_cdn, DnsResolveError, ZaloDnsResolver};
use crate::proxy::DEFAULT_USER_AGENT;

/// Default concurrency cap for media downloads (`MediaLoader.swift:95` = 6).
pub const DEFAULT_CONCURRENCY_LIMIT: usize = 6;

/// Default disk cache cap for `<cache_dir>/files` (`MediaLoader.defaultFilesCacheLimit` = 2 GiB).
pub const DEFAULT_FILES_CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// Maximum downloadable file size (`MediaLoader.maxFileBytes` = 2 GiB).
pub const MAX_MEDIA_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Maximum in-memory photo payload (`DownloadManager.photoByteCap` = 64 MiB).
pub const MAX_PHOTO_BYTES: u64 = 64 << 20;

/// Default image download deadline (`MediaLoader.imageDeadline` = 25s).
pub const DEFAULT_IMAGE_DEADLINE: Duration = Duration::from_secs(25);

/// Per-chunk stall timeout for streaming file transfers (`MediaLoader.fileIdleTimeout` = 30s).
pub const FILE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Age threshold for cleaning orphaned `.part` files (`MediaLoader.stalePartAge` = 600s).
pub const STALE_PART_AGE: Duration = Duration::from_secs(600);

/// Maximum HTTP redirect hops followed per media request (`MediaLoader.swift:357` = 3).
pub const MAX_REDIRECT_HOPS: usize = 3;

/// Headers allowed ONLY on `https://` Zalo CDN hosts and stripped across cross-host redirects.
pub const SENSITIVE_HEADER_NAMES: [&str; 4] = [
    "cloud-viewer-key",
    "cookie",
    "authorization",
    "proxy-authorization",
];

/// Hop-by-hop and framing headers callers are never allowed to inject (`MediaLoader.swift:416`).
pub const FORBIDDEN_HEADER_NAMES: [&str; 11] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
    "expect",
    "accept-encoding",
];

/// Typed media download errors mapped cleanly into Zalo PC download error codes.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MediaLoadError {
    #[error("unsupported or invalid media URL")]
    BadUrl,
    #[error("media request failed with HTTP {0}")]
    Http(u16),
    #[error("media host could not be resolved")]
    HostNotFound,
    #[error("time-to-first-byte deadline expired")]
    TtfbTimeout,
    #[error("media download timed out")]
    TimedOut,
    #[error("media payload exceeds size limit")]
    TooLarge,
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("connection refused: {0}")]
    ConnectionRefused(String),
    #[error("TLS handshake failed: {0}")]
    TlsError(String),
    #[error("network disconnected: {0}")]
    Disconnected(String),
    #[error("network request failed: {0}")]
    Network(String),
    #[error("I/O error: {0}")]
    Io(String),
}

impl MediaLoadError {
    /// Maps a `reqwest::Error` (inspecting its full error source chain) to [`MediaLoadError`].
    pub fn from_reqwest(err: &reqwest::Error) -> Self {
        let mut cursor: Option<&(dyn std::error::Error + 'static)> = Some(err);
        while let Some(source) = cursor {
            if let Some(dns_err) = source.downcast_ref::<DnsResolveError>() {
                return match dns_err {
                    DnsResolveError::HostNotFound(_)
                    | DnsResolveError::SystemTimeout(_)
                    | DnsResolveError::LookupFailed { .. } => Self::HostNotFound,
                };
            }
            if let Some(io_err) = source.downcast_ref::<std::io::Error>() {
                match io_err.kind() {
                    std::io::ErrorKind::ConnectionRefused => {
                        return Self::ConnectionRefused(io_err.to_string());
                    }
                    std::io::ErrorKind::TimedOut => {
                        return Self::TimedOut;
                    }
                    std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::BrokenPipe => {
                        return Self::Disconnected(io_err.to_string());
                    }
                    _ => {
                        let msg = io_err.to_string().to_ascii_lowercase();
                        if msg.contains("failed to lookup address")
                            || msg.contains("name or service not known")
                            || msg.contains("no such host")
                            || msg.contains("nodename nor servname")
                        {
                            return Self::HostNotFound;
                        }
                    }
                }
            }
            cursor = source.source();
        }

        if err.is_timeout() {
            return Self::TimedOut;
        }

        let lower = err.to_string().to_ascii_lowercase();
        if lower.contains("dns error")
            || lower.contains("failed to lookup address")
            || lower.contains("name not resolved")
            || lower.contains("nxdomain")
        {
            return Self::HostNotFound;
        }
        if lower.contains("certificate")
            || lower.contains("tls")
            || lower.contains("ssl")
            || lower.contains("handshake")
        {
            return Self::TlsError(err.to_string());
        }
        if lower.contains("connection refused") {
            return Self::ConnectionRefused(err.to_string());
        }

        Self::Network(err.to_string())
    }
}

struct FlightState<T: Clone + Send + Sync + 'static> {
    rx: watch::Receiver<Option<std::result::Result<T, MediaLoadError>>>,
    waiters: Arc<AtomicUsize>,
    abort_handle: AbortHandle,
}

struct InFlightGroup<T: Clone + Send + Sync + 'static> {
    flights: Arc<DashMap<String, FlightState<T>>>,
}

impl<T: Clone + Send + Sync + 'static> Default for InFlightGroup<T> {
    fn default() -> Self {
        Self {
            flights: Arc::new(DashMap::new()),
        }
    }
}

impl<T: Clone + Send + Sync + 'static> Clone for InFlightGroup<T> {
    fn clone(&self) -> Self {
        Self {
            flights: Arc::clone(&self.flights),
        }
    }
}

struct WaiterDropGuard<T: Clone + Send + Sync + 'static> {
    key: String,
    flights: Arc<DashMap<String, FlightState<T>>>,
    waiters: Arc<AtomicUsize>,
    abort_handle: AbortHandle,
    completed: bool,
}

impl<T: Clone + Send + Sync + 'static> Drop for WaiterDropGuard<T> {
    fn drop(&mut self) {
        let prev = self.waiters.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 && !self.completed {
            self.flights.remove(&self.key);
            self.abort_handle.abort();
        }
    }
}

impl<T: Clone + Send + Sync + 'static> InFlightGroup<T> {
    async fn join<F, Fut>(&self, key: String, start: F) -> std::result::Result<T, MediaLoadError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, MediaLoadError>> + Send + 'static,
    {
        let (mut rx, waiters, abort_handle) = {
            if let Some(existing) = self.flights.get(&key) {
                existing.waiters.fetch_add(1, Ordering::AcqRel);
                (
                    existing.rx.clone(),
                    Arc::clone(&existing.waiters),
                    existing.abort_handle.clone(),
                )
            } else {
                let (tx, rx) = watch::channel(None);
                let waiters = Arc::new(AtomicUsize::new(1));
                let flights_clone = Arc::clone(&self.flights);
                let key_clone = key.clone();
                let fut = start();
                let handle = tokio::spawn(async move {
                    let out = fut.await;
                    flights_clone.remove(&key_clone);
                    let _ = tx.send(Some(out));
                });
                let abort_handle = handle.abort_handle();
                self.flights.insert(
                    key.clone(),
                    FlightState {
                        rx: rx.clone(),
                        waiters: Arc::clone(&waiters),
                        abort_handle: abort_handle.clone(),
                    },
                );
                (rx, waiters, abort_handle)
            }
        };

        let mut guard = WaiterDropGuard {
            key,
            flights: Arc::clone(&self.flights),
            waiters,
            abort_handle,
            completed: false,
        };

        loop {
            if let Some(res) = rx.borrow().clone() {
                guard.completed = true;
                return res;
            }
            if rx.changed().await.is_err() {
                if let Some(res) = rx.borrow().clone() {
                    guard.completed = true;
                    return res;
                }
                return Err(MediaLoadError::Network(
                    "in-flight download task cancelled".to_owned(),
                ));
            }
        }
    }
}

/// Concurrent media fetcher, redirect sanitizer, and 2 GiB LRU disk cache.
#[derive(Clone)]
pub struct MediaLoader {
    client: reqwest::Client,
    dns_resolver: Arc<ZaloDnsResolver>,
    semaphore: Arc<Semaphore>,
    cache_dir: PathBuf,
    files_dir: PathBuf,
    temp_dir: PathBuf,
    cache_limit_bytes: u64,
    byte_flights: InFlightGroup<Bytes>,
    file_flights: InFlightGroup<PathBuf>,
}

impl std::fmt::Debug for MediaLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaLoader")
            .field("files_dir", &self.files_dir)
            .field("cache_limit_bytes", &self.cache_limit_bytes)
            .field("available_permits", &self.semaphore.available_permits())
            .finish()
    }
}

impl MediaLoader {
    /// Creates a `MediaLoader` rooted at `cache_dir` with 6 concurrency slots and a 2 GiB LRU file cache.
    pub fn new(
        cache_dir: impl Into<PathBuf>,
        dns_resolver: Arc<ZaloDnsResolver>,
    ) -> Result<Self> {
        Self::with_config(
            cache_dir,
            dns_resolver,
            DEFAULT_CONCURRENCY_LIMIT,
            DEFAULT_FILES_CACHE_LIMIT,
        )
    }

    /// Creates a `MediaLoader` with custom concurrency cap and disk cache byte limit.
    pub fn with_config(
        cache_dir: impl Into<PathBuf>,
        dns_resolver: Arc<ZaloDnsResolver>,
        max_concurrent_downloads: usize,
        cache_limit_bytes: u64,
    ) -> Result<Self> {
        let cache_dir = cache_dir.into();
        let files_dir = cache_dir.join("files");
        let temp_dir = cache_dir.join("tmp");
        fs::create_dir_all(&files_dir)
            .with_context(|| format!("failed to create media files dir {}", files_dir.display()))?;
        fs::create_dir_all(&temp_dir)
            .with_context(|| format!("failed to create media temp dir {}", temp_dir.display()))?;

        // `Policy::none()` ensures we validate every redirect hop manually and strip sensitive headers cross-host.
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(Policy::none())
            .dns_resolver(Arc::clone(&dns_resolver))
            .user_agent(DEFAULT_USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("failed to build MediaLoader reqwest::Client")?;

        Ok(Self {
            client,
            dns_resolver,
            semaphore: Arc::new(Semaphore::new(max_concurrent_downloads.max(1))),
            cache_dir,
            files_dir,
            temp_dir,
            cache_limit_bytes,
            byte_flights: InFlightGroup::default(),
            file_flights: InFlightGroup::default(),
        })
    }

    /// Returns `<cache_dir>/files`.
    pub fn files_dir(&self) -> &Path {
        &self.files_dir
    }

    /// Returns `<cache_dir>/tmp`.
    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    /// Returns `<cache_dir>`.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Returns the shared [`ZaloDnsResolver`].
    pub fn dns_resolver(&self) -> &Arc<ZaloDnsResolver> {
        &self.dns_resolver
    }

    /// Returns the semaphore limiting concurrent media downloads (6 slots by default).
    pub fn semaphore(&self) -> &Arc<Semaphore> {
        &self.semaphore
    }

    /// Fetches media bytes in memory (de-duplicated across concurrent callers, concurrency-capped,
    /// bounded by `deadline` and [`MAX_PHOTO_BYTES`]).
    pub async fn fetch_bytes(
        &self,
        url_str: &str,
        deadline: Duration,
    ) -> std::result::Result<Bytes, MediaLoadError> {
        let normalized = normalize_media_url(url_str)?;
        let key = format!("b:{normalized}");
        let this = self.clone();

        self.byte_flights
            .join(key, move || async move {
                let _permit = this
                    .semaphore
                    .acquire()
                    .await
                    .map_err(|_| MediaLoadError::Network("semaphore closed".to_owned()))?;

                tokio::time::timeout(deadline, async {
                    let resp = this
                        .send_following_redirects(normalized, &HashMap::new(), None)
                        .await?;
                    if let Some(len) = resp.content_length() {
                        if len > MAX_PHOTO_BYTES {
                            return Err(MediaLoadError::TooLarge);
                        }
                    }
                    let mut buf = Vec::new();
                    let mut stream = resp.bytes_stream();
                    while let Some(chunk_res) =
                        tokio::time::timeout(FILE_IDLE_TIMEOUT, stream.next())
                            .await
                            .map_err(|_| MediaLoadError::TimedOut)?
                    {
                        let chunk = chunk_res.map_err(|e| MediaLoadError::from_reqwest(&e))?;
                        if (buf.len() + chunk.len()) as u64 > MAX_PHOTO_BYTES {
                            return Err(MediaLoadError::TooLarge);
                        }
                        buf.extend_from_slice(&chunk);
                    }
                    Ok(Bytes::from(buf))
                })
                .await
                .map_err(|_| MediaLoadError::TimedOut)?
            })
            .await
    }

    /// Downloads `url_str` into `<cache_dir>/files/<fnv1a64>.<ext>` (reusing cached file when present),
    /// using `.part` -> `sync_all()` -> atomic `rename` and enforcing 2 GiB LRU eviction.
    pub async fn local_file(&self, url_str: &str) -> std::result::Result<PathBuf, MediaLoadError> {
        let normalized = normalize_media_url(url_str)?;
        let cache_name = cache_file_name(&normalized);
        let final_path = self.files_dir.join(cache_name);

        if is_non_empty_regular_file(&final_path) {
            touch_file_mtime(&final_path);
            return Ok(final_path);
        }

        let key = format!("f:{}", final_path.display());
        let this = self.clone();
        let target_path = final_path.clone();

        let out_path = self
            .file_flights
            .join(key, move || async move {
                let _permit = this
                    .semaphore
                    .acquire()
                    .await
                    .map_err(|_| MediaLoadError::Network("semaphore closed".to_owned()))?;

                if is_non_empty_regular_file(&target_path) {
                    touch_file_mtime(&target_path);
                    return Ok(target_path);
                }

                fs::create_dir_all(&this.files_dir)
                    .map_err(|e| MediaLoadError::Io(e.to_string()))?;
                let part_path = this
                    .files_dir
                    .join(format!(".{}.part", Uuid::new_v4()));

                let stream_res = this
                    .stream_to_part_file(
                        normalized,
                        &HashMap::new(),
                        &part_path,
                        MAX_MEDIA_FILE_BYTES,
                        None,
                    )
                    .await;

                if let Err(err) = stream_res {
                    let _ = fs::remove_file(&part_path);
                    return Err(err);
                }

                if let Err(err) = fs::rename(&part_path, &target_path) {
                    let _ = fs::remove_file(&part_path);
                    if !is_non_empty_regular_file(&target_path) {
                        return Err(MediaLoadError::Io(err.to_string()));
                    }
                }

                Ok(target_path)
            })
            .await?;

        let files_dir = self.files_dir.clone();
        let limit = self.cache_limit_bytes;
        let keep = out_path.clone();
        tokio::task::spawn_blocking(move || {
            trim_cache_dir(&files_dir, limit, Some(&keep), STALE_PART_AGE);
        });

        Ok(out_path)
    }

    /// Streams a GET request with caller-supplied headers (such as Zalo Cloud's `cloud-viewer-key`)
    /// to an owned temporary file inside `<cache_dir>/tmp`.
    ///
    /// Sensitive headers are validated against `is_zalo_cdn` + `https` and automatically stripped
    /// on cross-host redirects (up to 3 hops).
    pub async fn download_with_headers(
        &self,
        url_str: &str,
        headers: &HashMap<String, String>,
        max_bytes: u64,
        ttfb: Option<Duration>,
    ) -> std::result::Result<PathBuf, MediaLoadError> {
        let normalized = normalize_media_url(url_str)?;
        fs::create_dir_all(&self.temp_dir).map_err(|e| MediaLoadError::Io(e.to_string()))?;
        let part_path = self.temp_dir.join(format!(".{}.part", Uuid::new_v4()));

        if let Err(err) = self
            .stream_to_part_file(normalized, headers, &part_path, max_bytes, ttfb)
            .await
        {
            let _ = fs::remove_file(&part_path);
            return Err(err);
        }

        Ok(part_path)
    }

    async fn stream_to_part_file(
        &self,
        initial_url: Url,
        headers: &HashMap<String, String>,
        part_path: &Path,
        max_bytes: u64,
        ttfb: Option<Duration>,
    ) -> std::result::Result<u64, MediaLoadError> {
        let resp = self
            .send_following_redirects(initial_url, headers, ttfb)
            .await?;

        if let Some(expected_len) = resp.content_length() {
            if expected_len > max_bytes {
                return Err(MediaLoadError::TooLarge);
            }
        }

        let mut file = File::create(part_path).map_err(|e| MediaLoadError::Io(e.to_string()))?;
        let mut written: u64 = 0;
        let mut stream = resp.bytes_stream();

        loop {
            let next_timeout = if written == 0 {
                ttfb.unwrap_or(FILE_IDLE_TIMEOUT)
            } else {
                FILE_IDLE_TIMEOUT
            };

            let maybe_chunk = match tokio::time::timeout(next_timeout, stream.next()).await {
                Ok(c) => c,
                Err(_) => {
                    return Err(if written == 0 && ttfb.is_some() {
                        MediaLoadError::TtfbTimeout
                    } else {
                        MediaLoadError::TimedOut
                    });
                }
            };

            let Some(chunk_res) = maybe_chunk else {
                break;
            };
            let chunk = chunk_res.map_err(|e| MediaLoadError::from_reqwest(&e))?;
            written = written.saturating_add(chunk.len() as u64);
            if written > max_bytes {
                return Err(MediaLoadError::TooLarge);
            }
            file.write_all(&chunk)
                .map_err(|e| MediaLoadError::Io(e.to_string()))?;
        }

        file.sync_all()
            .map_err(|e| MediaLoadError::Io(e.to_string()))?;
        Ok(written)
    }

    async fn send_following_redirects(
        &self,
        initial_url: Url,
        raw_headers: &HashMap<String, String>,
        ttfb: Option<Duration>,
    ) -> std::result::Result<reqwest::Response, MediaLoadError> {
        let origin_host = initial_url
            .host_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut current_url = initial_url;
        let mut current_headers = sanitize_headers(raw_headers, &current_url);

        for hop in 0..=MAX_REDIRECT_HOPS {
            if let Some(host) = current_url.host_str() {
                if self.dns_resolver.is_dead_host(host) {
                    return Err(MediaLoadError::HostNotFound);
                }
            }

            let mut req_headers = HeaderMap::new();
            for (k, v) in &current_headers {
                if let (Ok(hn), Ok(hv)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    req_headers.insert(hn, hv);
                }
            }

            let send_fut = self
                .client
                .get(current_url.clone())
                .headers(req_headers)
                .send();

            let resp = if let Some(ttfb_dur) = ttfb {
                match tokio::time::timeout(ttfb_dur, send_fut).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => return Err(MediaLoadError::from_reqwest(&e)),
                    Err(_) => return Err(MediaLoadError::TtfbTimeout),
                }
            } else {
                match tokio::time::timeout(FILE_IDLE_TIMEOUT, send_fut).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => return Err(MediaLoadError::from_reqwest(&e)),
                    Err(_) => return Err(MediaLoadError::TimedOut),
                }
            };

            let status = resp.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECT_HOPS {
                    return Err(MediaLoadError::TooManyRedirects);
                }
                let Some(loc_str) = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                else {
                    return Err(MediaLoadError::Http(status));
                };
                let joined = current_url
                    .join(loc_str)
                    .map_err(|_| MediaLoadError::BadUrl)?;
                let next_url = normalize_media_url(joined.as_str())?;
                current_headers =
                    strip_sensitive_headers_for_redirect(&current_headers, &origin_host, &next_url);
                current_url = next_url;
                continue;
            }

            if !(200..300).contains(&status) {
                return Err(MediaLoadError::Http(status));
            }

            return Ok(resp);
        }

        Err(MediaLoadError::TooManyRedirects)
    }
}

/// Validates caller-supplied HTTP headers against RFC 7230 token/value syntax, drops forbidden
/// hop-by-hop headers, and restricts [`SENSITIVE_HEADER_NAMES`] exclusively to `https://` Zalo CDN hosts.
pub fn sanitize_headers(headers: &HashMap<String, String>, url: &Url) -> HashMap<String, String> {
    let to_secure_zalo = url.scheme().eq_ignore_ascii_case("https")
        && is_zalo_cdn(url.host_str().unwrap_or(""));

    let mut out = HashMap::new();
    for (name, value) in headers {
        if !is_valid_rfc7230_header_name(name) || !is_valid_rfc7230_header_value(value) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if FORBIDDEN_HEADER_NAMES.contains(&lower.as_str()) {
            continue;
        }
        if SENSITIVE_HEADER_NAMES.contains(&lower.as_str()) && !to_secure_zalo {
            continue;
        }
        out.insert(name.clone(), value.clone());
    }
    out
}

/// Strips [`SENSITIVE_HEADER_NAMES`] when an HTTP redirect leaves `origin_host`, downgrades from
/// `https`, or targets a host outside `is_zalo_cdn`.
pub fn strip_sensitive_headers_for_redirect(
    headers: &HashMap<String, String>,
    origin_host: &str,
    next_url: &Url,
) -> HashMap<String, String> {
    let next_host = next_url.host_str().unwrap_or("").to_ascii_lowercase();
    let same_zalo_origin = !origin_host.is_empty()
        && next_host == origin_host.to_ascii_lowercase()
        && next_url.scheme().eq_ignore_ascii_case("https")
        && is_zalo_cdn(&next_host);

    if same_zalo_origin {
        return headers.clone();
    }

    headers
        .iter()
        .filter(|(name, _)| {
            let lower = name.to_ascii_lowercase();
            !SENSITIVE_HEADER_NAMES.contains(&lower.as_str())
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Computes the FNV-1a 64-bit lowercase hex hash (`MediaLoader.stableHash`).
pub fn fnv1a_64_hex(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in text.as_bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    format!("{hash:x}")
}

/// Evicts stale `.part` files and trims least-recently-used cached files in `directory` down to `limit_bytes`.
pub fn trim_cache_dir(
    directory: &Path,
    limit_bytes: u64,
    keeping: Option<&Path>,
    stale_part_age: Duration,
) {
    let Ok(read_dir) = fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    let keep_canonical = keeping.and_then(|p| fs::canonicalize(p).ok());

    struct CacheEntry {
        path: PathBuf,
        size: u64,
        last_used: SystemTime,
    }

    let mut entries = Vec::new();
    for entry_res in read_dir {
        let Ok(entry) = entry_res else {
            continue;
        };
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str.ends_with(".part") {
            if name_str.ends_with(".part") {
                if let Ok(age) = now.duration_since(modified) {
                    if age > stale_part_age {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
            continue;
        }
        entries.push(CacheEntry {
            path,
            size: meta.len(),
            last_used: modified,
        });
    }

    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    if total <= limit_bytes {
        return;
    }

    entries.sort_by_key(|e| e.last_used);
    for entry in entries {
        if total <= limit_bytes {
            break;
        }
        if let Some(ref keep_canon) = keep_canonical {
            if fs::canonicalize(&entry.path).ok().as_ref() == Some(keep_canon) {
                continue;
            }
        }
        if keeping.is_some_and(|k| k == entry.path) {
            continue;
        }
        if fs::remove_file(&entry.path).is_ok() {
            total = total.saturating_sub(entry.size);
        }
    }
}

/// Sniffs media MIME type from magic header bytes with extension fallback via `mime_guess`.
///
/// Inspects PNG, JPEG, GIF, WebP (`RIFF....WEBP`), MP3/`ID3`, Ogg, and ISO-BMFF `ftyp` at offset 4
/// (`mp4`, `M4A `, `qt  `, `3gp`).
pub fn sniff_media_mime(bytes: &[u8], fallback_path: &Path) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" {
        if &bytes[8..12] == b"WEBP" {
            return "image/webp";
        }
        if &bytes[8..12] == b"WAVE" {
            return "audio/wav";
        }
    }
    if bytes.starts_with(b"ID3") {
        return "audio/mpeg";
    }
    // MPEG-1/2/2.5 Audio Layer I/II/III sync word (excludes layer=00 reserved and version=01 reserved).
    if bytes.len() >= 2
        && bytes[0] == 0xFF
        && (bytes[1] & 0xE0) == 0xE0
        && (bytes[1] & 0x18) != 0x08
        && (bytes[1] & 0x06) != 0x00
    {
        return "audio/mpeg";
    }
    if bytes.starts_with(b"OggS") {
        return "audio/ogg";
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return "video/webm";
    }
    // ISO Base Media File Format (`ftyp` box at byte offset 4..8, major brand at 8..12).
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        if brand == b"M4A " || brand == b"M4B " || brand == b"M4P " {
            return "audio/mp4";
        }
        if brand == b"qt  " {
            return "video/quicktime";
        }
        if &brand[0..3] == b"3gp" {
            return "video/3gpp";
        }
        if &brand[0..3] == b"3g2" {
            return "video/3gpp2";
        }
        if brand == b"avif" || brand == b"avis" {
            return "image/avif";
        }
        if brand == b"heic" || brand == b"heix" || brand == b"mif1" || brand == b"msf1" {
            return "image/heic";
        }
        return "video/mp4";
    }

    mime_guess::from_path(fallback_path)
        .first_raw()
        .unwrap_or("application/octet-stream")
}

/// HTTP response descriptor returned by [`serve_local_media_range`] for `olaz-media://` requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRangeResponse {
    pub status: u16,
    pub status_text: &'static str,
    pub mime_type: &'static str,
    pub content_length: u64,
    pub total_length: u64,
    pub content_range: Option<String>,
    pub accept_ranges: &'static str,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

/// Serves a local media file strictly confined within `allowed_roots`, supporting HTTP `200 OK`
/// and `206 Partial Content` (`Content-Range: bytes start-end/total`, `Accept-Ranges: bytes`)
/// for smooth HTML5 `<video>` and `<audio>` scrubbing.
pub fn serve_local_media_range(
    path: &Path,
    range_header: Option<&str>,
    allowed_roots: &[PathBuf],
) -> Result<MediaRangeResponse> {
    if allowed_roots.is_empty() {
        bail!("access denied: no allowed media roots configured");
    }

    let path_str = path.to_string_lossy();
    if path_str.contains('\0') {
        bail!("invalid path containing NUL byte");
    }

    let canonical_target = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve media path {}", path.display()))?;

    let is_confined = allowed_roots.iter().any(|root| {
        fs::canonicalize(root)
            .ok()
            .is_some_and(|canon_root| {
                canonical_target != canon_root && canonical_target.starts_with(&canon_root)
            })
    });

    if !is_confined {
        bail!(
            "access denied: {} escapes allowed media roots",
            path.display()
        );
    }

    let mut file = File::open(&canonical_target)
        .with_context(|| format!("failed to open media file {}", canonical_target.display()))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        bail!("media path is not a regular file: {}", canonical_target.display());
    }

    let raw_len = meta.len();

    // Detect Zalo's 6-byte `fileNoise` trailer (`07 7a 41 6c 4f 07`) at EOF; when present,
    // logical length excludes the trailer and bytes at offsets `0..100` are XORed with `0x93`.
    const ZALO_NOISE_SIGN: [u8; 6] = [0x07, 0x7a, 0x41, 0x6c, 0x4f, 0x07];
    const ZALO_NOISE_XOR_LEN: u64 = 100;
    const ZALO_NOISE_XOR_KEY: u8 = 0x93;

    let has_noise = if raw_len > ZALO_NOISE_SIGN.len() as u64 {
        let sig_off = raw_len - ZALO_NOISE_SIGN.len() as u64;
        let mut tail = [0u8; 6];
        file.seek(SeekFrom::Start(sig_off)).is_ok()
            && file.read_exact(&mut tail).is_ok()
            && tail == ZALO_NOISE_SIGN
    } else {
        false
    };
    let total_len = if has_noise {
        raw_len - ZALO_NOISE_SIGN.len() as u64
    } else {
        raw_len
    };
    let denoise_slice = |offset: u64, buf: &mut [u8]| {
        if has_noise && offset < ZALO_NOISE_XOR_LEN && !buf.is_empty() {
            let xor_end = ((ZALO_NOISE_XOR_LEN - offset) as usize).min(buf.len());
            for b in &mut buf[..xor_end] {
                *b ^= ZALO_NOISE_XOR_KEY;
            }
        }
    };

    // Sniff magic bytes from first 64 logical bytes without reading the entire file into memory.
    file.seek(SeekFrom::Start(0))?;
    let mut magic_buf = [0u8; 64];
    let max_magic = (total_len as usize).min(magic_buf.len());
    let magic_read = if max_magic > 0 {
        file.read(&mut magic_buf[..max_magic])?
    } else {
        0
    };
    denoise_slice(0, &mut magic_buf[..magic_read]);
    let mime_type = sniff_media_mime(&magic_buf[..magic_read], &canonical_target);

    let trimmed_range = range_header.map(str::trim).filter(|s| !s.is_empty());
    let Some(range_raw) = trimmed_range else {
        file.seek(SeekFrom::Start(0))?;
        let mut body = Vec::with_capacity(total_len as usize);
        (&mut file).take(total_len).read_to_end(&mut body)?;
        denoise_slice(0, &mut body);

        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_owned(), mime_type.to_owned());
        headers.insert("Content-Length".to_owned(), total_len.to_string());
        headers.insert("Accept-Ranges".to_owned(), "bytes".to_owned());

        return Ok(MediaRangeResponse {
            status: 200,
            status_text: "OK",
            mime_type,
            content_length: total_len,
            total_length: total_len,
            content_range: None,
            accept_ranges: "bytes",
            headers,
            body,
        });
    };

    // Per RFC 9110 §14.2: ignore unknown range units and return 200 OK.
    let Some(spec_part) = range_raw
        .strip_prefix("bytes=")
        .or_else(|| range_raw.strip_prefix("BYTES="))
    else {
        file.seek(SeekFrom::Start(0))?;
        let mut body = Vec::with_capacity(total_len as usize);
        (&mut file).take(total_len).read_to_end(&mut body)?;
        denoise_slice(0, &mut body);

        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_owned(), mime_type.to_owned());
        headers.insert("Content-Length".to_owned(), total_len.to_string());
        headers.insert("Accept-Ranges".to_owned(), "bytes".to_owned());

        return Ok(MediaRangeResponse {
            status: 200,
            status_text: "OK",
            mime_type,
            content_length: total_len,
            total_length: total_len,
            content_range: None,
            accept_ranges: "bytes",
            headers,
            body,
        });
    };

    let first_range = spec_part.split(',').next().unwrap_or("").trim();
    let Some((start, end)) = parse_byte_range_bounds(first_range, total_len) else {
        let unsatisfied = format!("bytes */{total_len}");
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_owned(), mime_type.to_owned());
        headers.insert("Content-Length".to_owned(), "0".to_owned());
        headers.insert("Accept-Ranges".to_owned(), "bytes".to_owned());
        headers.insert("Content-Range".to_owned(), unsatisfied.clone());

        return Ok(MediaRangeResponse {
            status: 416,
            status_text: "Range Not Satisfiable",
            mime_type,
            content_length: 0,
            total_length: total_len,
            content_range: Some(unsatisfied),
            accept_ranges: "bytes",
            headers,
            body: Vec::new(),
        });
    };

    let slice_len = end - start + 1;
    file.seek(SeekFrom::Start(start))?;
    let mut body = vec![0u8; slice_len as usize];
    file.read_exact(&mut body)?;
    denoise_slice(start, &mut body);

    let content_range = format!("bytes {start}-{end}/{total_len}");
    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_owned(), mime_type.to_owned());
    headers.insert("Content-Length".to_owned(), slice_len.to_string());
    headers.insert("Accept-Ranges".to_owned(), "bytes".to_owned());
    headers.insert("Content-Range".to_owned(), content_range.clone());

    Ok(MediaRangeResponse {
        status: 206,
        status_text: "Partial Content",
        mime_type,
        content_length: slice_len,
        total_length: total_len,
        content_range: Some(content_range),
        accept_ranges: "bytes",
        headers,
        body,
    })
}

fn parse_byte_range_bounds(spec: &str, total_len: u64) -> Option<(u64, u64)> {
    if total_len == 0 {
        return None;
    }
    let (left, right) = spec.split_once('-')?;
    let left = left.trim();
    let right = right.trim();

    if left.is_empty() {
        // Suffix byte-range-spec: `bytes=-N` (last N bytes).
        let suffix_len: u64 = right.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = total_len.saturating_sub(suffix_len);
        Some((start, total_len - 1))
    } else {
        let start: u64 = left.parse().ok()?;
        if start >= total_len {
            return None;
        }
        let end = if right.is_empty() {
            total_len - 1
        } else {
            let parsed_end: u64 = right.parse().ok()?;
            if parsed_end < start {
                return None;
            }
            parsed_end.min(total_len - 1)
        };
        Some((start, end))
    }
}

fn normalize_media_url(url_str: &str) -> std::result::Result<Url, MediaLoadError> {
    let mut url = Url::parse(url_str).map_err(|_| MediaLoadError::BadUrl)?;
    let scheme = url.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(MediaLoadError::BadUrl);
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Err(MediaLoadError::BadUrl);
    };
    if host.is_empty() {
        return Err(MediaLoadError::BadUrl);
    }

    // Upgrade remote HTTP media URLs to HTTPS (`MediaLoader.swift:533`), while preserving
    // loopback HTTP URLs (`127.0.0.1`, `localhost`, `::1`) for local integration test servers.
    let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "::1" || host == "[::1]";
    if scheme == "http" && !is_loopback {
        let _ = url.set_scheme("https");
    }

    Ok(url)
}

fn cache_file_name(url: &Url) -> String {
    let hash = fnv1a_64_hex(url.as_str());
    let ext: String = Path::new(url.path())
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if ext.is_empty() {
        hash
    } else {
        format!("{hash}.{ext}")
    }
}

fn is_non_empty_regular_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn touch_file_mtime(path: &Path) {
    if let Ok(file) = OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

fn is_valid_rfc7230_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn is_valid_rfc7230_header_value(value: &str) -> bool {
    value.chars().all(|c| {
        let cp = c as u32;
        cp == 0x09 || (cp >= 0x20 && cp != 0x7F)
    })
}
