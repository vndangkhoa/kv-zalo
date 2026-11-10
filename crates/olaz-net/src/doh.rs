//! DNS-over-HTTPS fallback resolver strictly scoped to Zalo CDN & API domains.
//!
//! Replicates `DoHResolver.swift` and `MediaLoader.swift:554-666`:
//! - Strict `is_zalo_cdn` allowlist so third-party link preview hosts never bypass system DNS.
//! - 3-second system DNS timeout (`tokio::net::lookup_host`).
//! - `dead_hosts` negative cache (300s TTL on authoritative NXDOMAIN).
//! - `system_ok` cache (300s TTL).
//! - `doh_hosts` cache (20s initial fallback on system DNS failure, promoted to 600s sticky on DoH success).
//! - DoH resolution via `hickory_resolver::TokioAsyncResolver` (`google_https` + `cloudflare_https`
//!   pinned to `8.8.8.8:443` / `1.1.1.1:443` with TLS SNI) plus a rustls+webpki HTTPS JSON-DoH
//!   fallback against `8.8.8.8` / `1.1.1.1` when root certs are not compiled into `hickory-resolver`.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use dashmap::DashMap;
use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use hickory_resolver::error::ResolveErrorKind;
use hickory_resolver::proto::op::ResponseCode;
use hickory_resolver::TokioAsyncResolver;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

/// Strict allowlist of Zalo domains eligible for DoH fallback (`MediaLoader.dohHostSuffixes`).
pub const ZALO_CDN_SUFFIXES: [&str; 21] = [
    "zalo.me",
    "zalo.vn",
    "zalo.ai",
    "zaloapp.com",
    "zadn.vn",
    "zdn.vn",
    "z-cdn.me",
    "dlfl.vn",
    "dlfl.me",
    "dlfl.info",
    "flchat.vn",
    "pchat.vn",
    "dlmd.me",
    "mdchat.me",
    "zavi.me",
    "zapps.vn",
    "zing.vn",
    "zingcdn.me",
    "zingmp3.vn",
    "zingmp3.com",
    "zingtv.vn",
];

/// TTL for authoritative NXDOMAIN entries (`MediaLoader.deadHostTTL` = 300s).
pub const DEAD_HOST_TTL: Duration = Duration::from_secs(300);

/// TTL for verified working system DNS entries (`MediaLoader.systemResolves` = 300s).
pub const SYSTEM_OK_TTL: Duration = Duration::from_secs(300);

/// Initial fallback window when system DNS fails before DoH has succeeded (`20s`).
pub const DOH_INITIAL_TTL: Duration = Duration::from_secs(20);

/// Sticky DoH window once DoH succeeds for a host (`MediaLoader.dohStickiness` = 600s).
pub const DOH_STICKY_TTL: Duration = Duration::from_secs(600);

/// Maximum wait for `tokio::net::lookup_host` before triggering DoH on Zalo hosts (`3s`).
pub const SYSTEM_DNS_TIMEOUT: Duration = Duration::from_secs(3);

const MAX_CACHE_ENTRIES: usize = 2000;

/// Returns `true` if `host` is an exact or subdomain match for Zalo's CDN/API domains.
///
/// Never matches third-party domains (such as link preview targets or lookalike suffixes
/// like `evilzadn.vn`).
pub fn is_zalo_cdn(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    ZALO_CDN_SUFFIXES.iter().any(|suffix| {
        host == *suffix
            || (host.len() > suffix.len()
                && host.ends_with(suffix)
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.')
    })
}

/// Errors produced by [`ZaloDnsResolver`].
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DnsResolveError {
    #[error("DNS host not found (NXDOMAIN): {0}")]
    HostNotFound(String),
    #[error("system DNS lookup timed out for {0}")]
    SystemTimeout(String),
    #[error("DNS lookup failed for {host}: {message}")]
    LookupFailed { host: String, message: String },
}

/// Pluggable DNS lookup backends (enables deterministic offline tests while defaulting to OS + DoH).
pub trait DnsBackend: Send + Sync {
    fn system_lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>>;

    fn doh_lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, DnsResolveError>> + Send + 'a>>;
}

struct DefaultDnsBackend {
    hickory: TokioAsyncResolver,
    json_doh_client: Option<reqwest::Client>,
}

#[derive(Debug, Deserialize)]
struct DohJsonReply {
    #[serde(rename = "Status")]
    status: i32,
    #[serde(rename = "Answer")]
    answer: Option<Vec<DohJsonAnswer>>,
}

#[derive(Debug, Deserialize)]
struct DohJsonAnswer {
    #[serde(rename = "type")]
    record_type: u16,
    data: String,
}

/// Validates and constructs an upstream `reqwest::Proxy` for `socks5://`, `socks5h://`,
/// `http://`, or `https://` URLs. Returns `Ok(None)` when `raw` is empty/whitespace.
pub fn parse_upstream_proxy(raw: &str) -> anyhow::Result<Option<(String, reqwest::Proxy)>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed =
        url::Url::parse(trimmed).with_context(|| format!("invalid proxy URL `{trimmed}`"))?;
    match parsed.scheme() {
        "socks5" | "socks5h" | "http" | "https" => {}
        other => bail!(
            "unsupported proxy scheme `{other}` in `{trimmed}` (expected socks5, socks5h, http, or https)"
        ),
    }
    if parsed.host_str().map_or(true, str::is_empty) {
        bail!("proxy URL `{trimmed}` is missing a host");
    }
    let proxy = reqwest::Proxy::all(trimmed)
        .with_context(|| format!("failed to build reqwest::Proxy for `{trimmed}`"))?;
    Ok(Some((trimmed.to_owned(), proxy)))
}

impl DefaultDnsBackend {
    fn new(proxy: Option<reqwest::Proxy>) -> Self {
        let mut name_servers = NameServerConfigGroup::google_https();
        name_servers.merge(NameServerConfigGroup::cloudflare_https());
        let config = ResolverConfig::from_parts(None, Vec::new(), name_servers);
        let mut opts = ResolverOpts::default();
        opts.timeout = Duration::from_secs(5);
        opts.attempts = 2;
        let hickory = TokioAsyncResolver::tokio(config, opts);

        // Dedicated rustls+webpki HTTPS client pinned directly to 8.8.8.8 / 8.8.4.4 / 1.1.1.1 / 1.0.0.1
        // with proper TLS SNI (`dns.google` / `cloudflare-dns.com`), immune to broken ISP DNS.
        // When an upstream SOCKS5/HTTP proxy is configured (`OLAZ_PROXY`), route DoH HTTPS queries
        // through the proxy instead.
        let google_addrs = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)), 443),
        ];
        let cloudflare_addrs = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)), 443),
        ];
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(5));
        if let Some(p) = proxy {
            builder = builder.proxy(p);
        } else {
            builder = builder
                .no_proxy()
                .resolve_to_addrs("dns.google", &google_addrs)
                .resolve_to_addrs("cloudflare-dns.com", &cloudflare_addrs);
        }
        let json_doh_client = builder.build().ok();

        Self {
            hickory,
            json_doh_client,
        }
    }

    async fn query_json_doh(&self, host: &str) -> Result<Vec<SocketAddr>, DnsResolveError> {
        let Some(ref client) = self.json_doh_client else {
            return Err(DnsResolveError::LookupFailed {
                host: host.to_owned(),
                message: "JSON DoH client unavailable".to_owned(),
            });
        };

        let endpoints = [
            "https://dns.google/resolve",
            "https://cloudflare-dns.com/dns-query",
        ];

        for endpoint in endpoints {
            let Ok(mut url) = url::Url::parse(endpoint) else {
                continue;
            };
            url.query_pairs_mut()
                .append_pair("name", host)
                .append_pair("type", "A");

            let resp = match client
                .get(url)
                .header(reqwest::header::ACCEPT, "application/dns-json")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => continue,
            };

            let Ok(reply) = resp.json::<DohJsonReply>().await else {
                continue;
            };

            if reply.status == 3 {
                return Err(DnsResolveError::HostNotFound(host.to_owned()));
            }
            if reply.status != 0 {
                continue;
            }

            let addrs: Vec<SocketAddr> = reply
                .answer
                .unwrap_or_default()
                .into_iter()
                .filter(|rec| rec.record_type == 1)
                .filter_map(|rec| rec.data.parse::<Ipv4Addr>().ok())
                .map(|ip| SocketAddr::new(IpAddr::V4(ip), 443))
                .collect();

            if !addrs.is_empty() {
                return Ok(addrs);
            }
        }

        Err(DnsResolveError::LookupFailed {
            host: host.to_owned(),
            message: "all DoH endpoints failed".to_owned(),
        })
    }
}

impl DnsBackend for DefaultDnsBackend {
    fn system_lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 443)).await?.collect();
            if addrs.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no addresses returned for {host}"),
                ));
            }
            Ok(addrs)
        })
    }

    fn doh_lookup<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, DnsResolveError>> + Send + 'a>> {
        Box::pin(async move {
            match self.hickory.lookup_ip(host).await {
                Ok(lookup) => {
                    let addrs: Vec<SocketAddr> = lookup
                        .iter()
                        .map(|ip| SocketAddr::new(ip, 443))
                        .collect();
                    if !addrs.is_empty() {
                        return Ok(addrs);
                    }
                }
                Err(err) => {
                    if let ResolveErrorKind::NoRecordsFound { response_code, .. } = err.kind() {
                        if *response_code == ResponseCode::NXDomain {
                            return Err(DnsResolveError::HostNotFound(host.to_owned()));
                        }
                    }
                }
            }

            // Fallback to pinned rustls+webpki HTTPS JSON DoH (`8.8.8.8:443` / `1.1.1.1:443`).
            self.query_json_doh(host).await
        })
    }
}

/// Custom `reqwest::dns::Resolve` implementation with Zalo-only DoH fallback and caching.
#[derive(Clone)]
pub struct ZaloDnsResolver {
    backend: Arc<dyn DnsBackend>,
    uses_default_backend: bool,
    proxy_url: Option<String>,
    dead_hosts: Arc<DashMap<String, Instant>>,
    system_ok: Arc<DashMap<String, Instant>>,
    doh_hosts: Arc<DashMap<String, Instant>>,
    system_timeout: Duration,
}

impl std::fmt::Debug for ZaloDnsResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZaloDnsResolver")
            .field("proxy_url", &self.proxy_url)
            .field("dead_hosts_len", &self.dead_hosts.len())
            .field("system_ok_len", &self.system_ok.len())
            .field("doh_hosts_len", &self.doh_hosts.len())
            .field("system_timeout", &self.system_timeout)
            .finish()
    }
}

impl Default for ZaloDnsResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl ZaloDnsResolver {
    /// Creates a new `ZaloDnsResolver` backed by system DNS + Google/Cloudflare DoH,
    /// honoring `OLAZ_PROXY` when set.
    pub fn new() -> Self {
        let (proxy_url, proxy) = match std::env::var("OLAZ_PROXY") {
            Ok(raw) if !raw.trim().is_empty() => match parse_upstream_proxy(&raw) {
                Ok(Some((u, p))) => (Some(u), Some(p)),
                Ok(None) => (None, None),
                Err(err) => {
                    warn!("ignoring invalid OLAZ_PROXY in ZaloDnsResolver::new: {err}");
                    (None, None)
                }
            },
            _ => (None, None),
        };
        Self {
            backend: Arc::new(DefaultDnsBackend::new(proxy)),
            uses_default_backend: true,
            proxy_url,
            dead_hosts: Arc::new(DashMap::new()),
            system_ok: Arc::new(DashMap::new()),
            doh_hosts: Arc::new(DashMap::new()),
            system_timeout: SYSTEM_DNS_TIMEOUT,
        }
    }

    /// Creates a `ZaloDnsResolver` with a custom [`DnsBackend`] and system lookup timeout.
    pub fn with_backend(backend: Arc<dyn DnsBackend>, system_timeout: Duration) -> Self {
        Self {
            backend,
            uses_default_backend: false,
            proxy_url: None,
            dead_hosts: Arc::new(DashMap::new()),
            system_ok: Arc::new(DashMap::new()),
            doh_hosts: Arc::new(DashMap::new()),
            system_timeout,
        }
    }

    /// Configures (or clears when `None`) the upstream SOCKS5/HTTP(S) proxy URL on the DoH client.
    pub fn with_proxy_url(mut self, proxy_url: Option<&str>) -> anyhow::Result<Self> {
        let parsed = match proxy_url {
            Some(raw) => parse_upstream_proxy(raw)?,
            None => None,
        };
        let (url_opt, proxy_opt) = match parsed {
            Some((u, p)) => (Some(u), Some(p)),
            None => (None, None),
        };
        self.proxy_url = url_opt;
        if self.uses_default_backend {
            self.backend = Arc::new(DefaultDnsBackend::new(proxy_opt));
        }
        Ok(self)
    }

    /// Returns the active upstream proxy URL configured on this resolver, if any.
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    /// Returns `true` if `host` is currently cached as an authoritative NXDOMAIN dead host.
    pub fn is_dead_host(&self, host: &str) -> bool {
        let key = normalize_host(host);
        if let Some(until) = self.dead_hosts.get(&key) {
            if *until > Instant::now() {
                return true;
            }
            drop(until);
            self.dead_hosts.remove(&key);
        }
        false
    }

    /// Returns `true` if `host` currently prefers the DoH path (`doh_hosts` cache).
    pub fn uses_doh(&self, host: &str) -> bool {
        let key = normalize_host(host);
        if let Some(until) = self.doh_hosts.get(&key) {
            if *until > Instant::now() {
                return true;
            }
            drop(until);
            self.doh_hosts.remove(&key);
        }
        false
    }

    /// Returns `true` if `host` is currently marked healthy in `system_ok`.
    pub fn is_system_ok(&self, host: &str) -> bool {
        let key = normalize_host(host);
        if let Some(until) = self.system_ok.get(&key) {
            if *until > Instant::now() {
                return true;
            }
            drop(until);
            self.system_ok.remove(&key);
        }
        false
    }

    /// Returns the remaining TTL for `host` in `doh_hosts`, if active.
    pub fn doh_ttl_remaining(&self, host: &str) -> Option<Duration> {
        let key = normalize_host(host);
        let until = *self.doh_hosts.get(&key)?;
        until.checked_duration_since(Instant::now())
    }

    /// Clears cached DNS state for `host` (or all hosts via [`Self::clear_caches`]).
    pub fn invalidate(&self, host: &str) {
        let key = normalize_host(host);
        self.dead_hosts.remove(&key);
        self.system_ok.remove(&key);
        self.doh_hosts.remove(&key);
    }

    /// Clears all resolver state caches (`_electron_clear-host-resolver-cache`).
    pub fn clear_caches(&self) {
        self.dead_hosts.clear();
        self.system_ok.clear();
        self.doh_hosts.clear();
    }

    fn mark_dead(&self, host: &str) {
        if self.dead_hosts.len() >= MAX_CACHE_ENTRIES {
            self.dead_hosts.clear();
        }
        self.dead_hosts
            .insert(host.to_owned(), Instant::now() + DEAD_HOST_TTL);
        self.doh_hosts.remove(host);
        self.system_ok.remove(host);
    }

    fn mark_system_ok(&self, host: &str, ttl: Duration) {
        if self.system_ok.len() >= MAX_CACHE_ENTRIES {
            self.system_ok.clear();
        }
        self.system_ok.insert(host.to_owned(), Instant::now() + ttl);
    }

    fn mark_doh(&self, host: &str, ttl: Duration) {
        if self.doh_hosts.len() >= MAX_CACHE_ENTRIES {
            self.doh_hosts.clear();
        }
        self.doh_hosts.insert(host.to_owned(), Instant::now() + ttl);
    }

    /// Resolves `host` to socket addresses following the Zalo CDN DoH fallback state machine.
    pub async fn resolve_host(&self, raw_host: &str) -> Result<Vec<SocketAddr>, DnsResolveError> {
        let host = normalize_host(raw_host);
        if host.is_empty() {
            return Err(DnsResolveError::HostNotFound(String::new()));
        }

        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, 443)]);
        }

        if self.is_dead_host(&host) {
            return Err(DnsResolveError::HostNotFound(host));
        }

        let allow_doh = is_zalo_cdn(&host);

        // Known-bad system DNS for a Zalo CDN host: go straight to DoH.
        if allow_doh && self.uses_doh(&host) && !self.is_system_ok(&host) {
            match self.backend.doh_lookup(&host).await {
                Ok(addrs) => {
                    self.mark_doh(&host, DOH_STICKY_TTL);
                    return Ok(addrs);
                }
                Err(DnsResolveError::HostNotFound(_)) => {
                    self.mark_dead(&host);
                    return Err(DnsResolveError::HostNotFound(host));
                }
                Err(_) => {
                    // DoH path failed (e.g. captive portal); give system DNS a brief 30s chance.
                    self.doh_hosts.remove(&host);
                    self.mark_system_ok(&host, Duration::from_secs(30));
                }
            }
        }

        // System DNS lookup bounded by 3-second timeout.
        let sys_res = tokio::time::timeout(self.system_timeout, self.backend.system_lookup(&host)).await;
        match sys_res {
            Ok(Ok(addrs)) if !addrs.is_empty() => {
                self.mark_system_ok(&host, SYSTEM_OK_TTL);
                Ok(addrs)
            }
            Ok(Ok(_)) => {
                self.system_ok.remove(&host);
                let sys_err = DnsResolveError::HostNotFound(host.clone());
                if !allow_doh {
                    return Err(sys_err);
                }
                self.fallback_to_doh(&host, sys_err).await
            }
            Ok(Err(io_err)) => {
                self.system_ok.remove(&host);
                let sys_err = DnsResolveError::LookupFailed {
                    host: host.clone(),
                    message: io_err.to_string(),
                };
                if !allow_doh {
                    return Err(sys_err);
                }
                self.fallback_to_doh(&host, sys_err).await
            }
            Err(_elapsed) => {
                self.system_ok.remove(&host);
                let timeout_err = DnsResolveError::SystemTimeout(host.clone());
                if !allow_doh {
                    return Err(timeout_err);
                }
                self.fallback_to_doh(&host, timeout_err).await
            }
        }
    }

    async fn fallback_to_doh(
        &self,
        host: &str,
        system_error: DnsResolveError,
    ) -> Result<Vec<SocketAddr>, DnsResolveError> {
        // Record initial 20s DoH preference while probing DoH (`MediaLoader.swift:630`).
        self.mark_doh(host, DOH_INITIAL_TTL);
        match self.backend.doh_lookup(host).await {
            Ok(addrs) => {
                self.mark_doh(host, DOH_STICKY_TTL);
                Ok(addrs)
            }
            Err(DnsResolveError::HostNotFound(_)) => {
                self.mark_dead(host);
                Err(DnsResolveError::HostNotFound(host.to_owned()))
            }
            Err(_) => Err(system_error),
        }
    }
}

impl Resolve for ZaloDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let resolver = self.clone();
        Box::pin(async move {
            let addrs = resolver.resolve_host(&host).await?;
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zalo_cdn_allowlist_covers_media_file_video_and_ws_hosts_while_rejecting_non_zalo() {
        for suffix in ZALO_CDN_SUFFIXES {
            assert!(is_zalo_cdn(suffix), "exact suffix {suffix} must match");
            assert!(
                is_zalo_cdn(&format!("sub.{suffix}.")),
                "subdomain with trailing dot for {suffix} must match"
            );
        }

        for allowed in [
            "f12-zpcloud.zdn.vn",
            "b2.dlfl.vn",
            "b2.dlfl.me",
            "b2.dlfl.info",
            "v1.dlmd.me",
            "p1.flchat.vn",
            "c1.pchat.vn",
            "m1.mdchat.me",
            "media-ten.z-cdn.me",
            "ws.zavi.me",
            "F12-ZPCLOUD.ZDN.VN",
        ] {
            assert!(is_zalo_cdn(allowed), "expected {allowed} to be allowed");
        }

        for rejected in [
            "",
            "evil.com",
            "dlfl.vn.evil.com",
            "zdn.vn.evil.com",
            "evilzadn.vn",
            "notdlfl.vn",
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
        ] {
            assert!(
                !is_zalo_cdn(rejected),
                "expected {rejected} to be rejected by is_zalo_cdn"
            );
        }
    }
}
