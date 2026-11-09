//! Outbound SSRF & open-proxy prevention for `/__olaz/http`, `/__olaz/media`, and `/__olaz/ws`.
//!
//! Enforces three layers of protection before any outbound TCP/TLS socket is opened:
//! 1. **Scheme check**: strictly `https` (HTTP/media) or `wss` (WebSocket) in production.
//! 2. **Domain allowlist**: exact or subdomain match on `.zalo.me`, `.zaloapp.com`,
//!    `.zadn.vn`, `.zdn.vn`, `.zing.vn`.
//! 3. **Public IP verification**: resolves the host and rejects loopback (`127.0.0.0/8`, `::1`),
//!    RFC1918 private (`10/8`, `172.16/12`, `192.168/16`), link-local (`169.254/16`, `fe80::/10`),
//!    CGNAT (`100.64/10`), ULA (`fc00::/7`), multicast, broadcast, documentation, and unspecified
//!    addresses — returning pinned `Vec<SocketAddr>` so DNS rebinding cannot occur.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use olaz_net::ZaloDnsResolver;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use thiserror::Error;
use url::Url;

/// Exact domain suffixes permitted through the stateless Zalo web relay.
pub const ALLOWED_ZALO_SUFFIXES: [&str; 5] = [
    "zalo.me",
    "zaloapp.com",
    "zadn.vn",
    "zdn.vn",
    "zing.vn",
];

/// Configuration for [`AllowlistValidator`].
#[derive(Debug, Clone, Default)]
pub struct AllowlistConfig {
    /// Test-only switch (`false` in production CLI!) allowing `http://` / `ws://` and
    /// `127.0.0.1` / `::1` / `localhost` (or DNS overrides resolving to loopback) so
    /// `cargo test` can run local mock upstream servers offline without internet access.
    ///
    /// Even when `true`, non-Zalo external domains (e.g. `evil.example.com`) and non-loopback
    /// RFC1918/link-local/CGNAT addresses (`10.0.0.1`, `169.254.169.254`, etc.) remain forbidden!
    pub allow_loopback_upstream_for_tests: bool,

    /// Optional deterministic hostname -> IP overrides for offline unit/integration tests.
    pub dns_overrides: HashMap<String, Vec<IpAddr>>,
}

/// Errors returned when an outbound target violates SSRF or domain allowlist rules.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AllowlistError {
    #[error("invalid target URL: {0}")]
    InvalidUrl(String),
    #[error("URL credentials are not permitted")]
    CredentialsNotAllowed,
    #[error("disallowed URL scheme '{0}'")]
    DisallowedScheme(String),
    #[error("target host '{0}' is outside the Zalo domain allowlist")]
    DisallowedHost(String),
    #[error("target resolved to non-public IP address {0}")]
    DisallowedIp(IpAddr),
    #[error("DNS resolution failed for '{host}': {reason}")]
    DnsFailed { host: String, reason: String },
}

/// Returns `true` if `host` is an exact or subdomain match for [`ALLOWED_ZALO_SUFFIXES`].
pub fn is_allowed_zalo_domain(host: &str) -> bool {
    let normalized = normalize_host(host);
    if normalized.is_empty() || normalized.parse::<IpAddr>().is_ok() {
        return false;
    }
    ALLOWED_ZALO_SUFFIXES.iter().any(|suffix| {
        normalized == *suffix
            || (normalized.len() > suffix.len()
                && normalized.ends_with(suffix)
                && normalized.as_bytes()[normalized.len() - suffix.len() - 1] == b'.')
    })
}

/// Returns `true` if `ip` is a globally routable public unicast IP address.
///
/// Rejects loopback, RFC1918 private, link-local, CGNAT (`100.64.0.0/10`), ULA (`fc00::/7`),
/// multicast, broadcast, documentation/benchmarking, unspecified, and IPv4-mapped private ranges.
pub fn is_globally_routable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_globally_routable_ipv4(v4),
        IpAddr::V6(v6) => is_globally_routable_ipv6(v6),
    }
}

fn is_globally_routable_ipv4(v4: Ipv4Addr) -> bool {
    let octets = v4.octets();
    if v4.is_unspecified()
        || octets[0] == 0 // 0.0.0.0/8 ("this network")
        || v4.is_loopback() // 127.0.0.0/8
        || v4.is_private() // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
        || v4.is_link_local() // 169.254.0.0/16
        || v4.is_multicast() // 224.0.0.0/4
        || v4.is_broadcast() // 255.255.255.255
        || octets[0] >= 240 // 240.0.0.0/4 reserved Class E
    {
        return false;
    }
    // Carrier-Grade NAT (RFC 6598): 100.64.0.0/10
    if octets[0] == 100 && (octets[1] & 0b1100_0000) == 64 {
        return false;
    }
    // IETF Protocol Assignments: 192.0.0.0/24
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return false;
    }
    // TEST-NET-1: 192.0.2.0/24
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 2 {
        return false;
    }
    // Deprecated 6to4 relay anycast: 192.88.99.0/24
    if octets[0] == 192 && octets[1] == 88 && octets[2] == 99 {
        return false;
    }
    // Benchmarking (RFC 2544): 198.18.0.0/15
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return false;
    }
    // TEST-NET-2: 198.51.100.0/24
    if octets[0] == 198 && octets[1] == 51 && octets[2] == 100 {
        return false;
    }
    // TEST-NET-3: 203.0.113.0/24
    if octets[0] == 203 && octets[1] == 0 && octets[2] == 113 {
        return false;
    }
    true
}

fn is_globally_routable_ipv6(v6: Ipv6Addr) -> bool {
    if let Some(mapped_v4) = v6.to_ipv4_mapped() {
        return is_globally_routable_ipv4(mapped_v4);
    }
    let segs = v6.segments();
    if v6.is_unspecified() // ::
        || v6.is_loopback() // ::1
        || v6.is_multicast() // ff00::/8
    {
        return false;
    }
    // IPv4-compatible `::ffff:0:w.x.y.z` or `::w.x.y.z`
    if segs[0] == 0 && segs[1] == 0 && segs[2] == 0 && segs[3] == 0 && segs[4] == 0 && segs[5] == 0
    {
        let v4 = Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            (segs[6] & 0xff) as u8,
            (segs[7] >> 8) as u8,
            (segs[7] & 0xff) as u8,
        );
        return is_globally_routable_ipv4(v4);
    }
    // Unique Local Address (RFC 4193): fc00::/7
    if (segs[0] & 0xfe00) == 0xfc00 {
        return false;
    }
    // Link-Local Unicast: fe80::/10
    if (segs[0] & 0xffc0) == 0xfe80 {
        return false;
    }
    // Documentation (RFC 3849): 2001:db8::/32
    if segs[0] == 0x2001 && segs[1] == 0x0db8 {
        return false;
    }
    // 6to4 (2002::/16) — embeds arbitrary IPv4 in segments 1..2; inspect embedded IPv4
    if segs[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (segs[1] >> 8) as u8,
            (segs[1] & 0xff) as u8,
            (segs[2] >> 8) as u8,
            (segs[2] & 0xff) as u8,
        );
        return is_globally_routable_ipv4(embedded);
    }
    true
}

fn is_loopback_host_literal(host: &str) -> bool {
    let h = normalize_host(host);
    if h == "localhost" {
        return true;
    }
    h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Validates outbound HTTP/media/WebSocket URLs and resolves SSRF-verified socket addresses.
#[derive(Clone)]
pub struct AllowlistValidator {
    config: AllowlistConfig,
    dns_resolver: Arc<ZaloDnsResolver>,
}

impl std::fmt::Debug for AllowlistValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllowlistValidator")
            .field(
                "allow_loopback_upstream_for_tests",
                &self.config.allow_loopback_upstream_for_tests,
            )
            .field("dns_overrides_len", &self.config.dns_overrides.len())
            .finish()
    }
}

impl AllowlistValidator {
    pub fn new(config: AllowlistConfig) -> Self {
        Self {
            config,
            dns_resolver: Arc::new(ZaloDnsResolver::new()),
        }
    }

    pub fn config(&self) -> &AllowlistConfig {
        &self.config
    }

    /// Checks scheme + host synchronously (used for fast fail and redirect policies).
    pub fn validate_url_surface(&self, url: &Url, is_websocket: bool) -> Result<String, AllowlistError> {
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AllowlistError::CredentialsNotAllowed);
        }

        let scheme = url.scheme();
        let scheme_ok = if is_websocket {
            scheme == "wss" || (self.config.allow_loopback_upstream_for_tests && scheme == "ws")
        } else {
            scheme == "https" || (self.config.allow_loopback_upstream_for_tests && scheme == "http")
        };
        if !scheme_ok {
            return Err(AllowlistError::DisallowedScheme(scheme.to_string()));
        }

        let raw_host = url
            .host_str()
            .ok_or_else(|| AllowlistError::InvalidUrl("missing host".to_string()))?;
        let host = normalize_host(raw_host);

        // Check literal IP hosts first so literal private/loopback IPs get `DisallowedIp` when appropriate.
        if let Ok(ip) = host.parse::<IpAddr>() {
            if self.config.allow_loopback_upstream_for_tests && ip.is_loopback() {
                return Ok(host);
            }
            if !is_globally_routable_ip(ip) {
                return Err(AllowlistError::DisallowedIp(ip));
            }
            // Even public raw IP literals are rejected because targets must match Zalo domain suffixes!
            return Err(AllowlistError::DisallowedHost(host));
        }

        if is_allowed_zalo_domain(&host)
            || (self.config.allow_loopback_upstream_for_tests && is_loopback_host_literal(&host))
        {
            Ok(host)
        } else {
            Err(AllowlistError::DisallowedHost(host))
        }
    }

    fn check_resolved_ip(&self, ip: IpAddr) -> Result<(), AllowlistError> {
        if is_globally_routable_ip(ip)
            || (self.config.allow_loopback_upstream_for_tests && ip.is_loopback())
        {
            Ok(())
        } else {
            Err(AllowlistError::DisallowedIp(ip))
        }
    }

    /// Resolves `host` and verifies every returned IP address is globally routable
    /// (or loopback when `allow_loopback_upstream_for_tests == true`).
    pub async fn resolve_validated_host(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, AllowlistError> {
        let normalized = normalize_host(host);

        if let Ok(ip) = normalized.parse::<IpAddr>() {
            if !(self.config.allow_loopback_upstream_for_tests && ip.is_loopback()) {
                self.check_resolved_ip(ip)?;
                return Err(AllowlistError::DisallowedHost(normalized));
            }
            return Ok(vec![SocketAddr::new(ip, port)]);
        }

        if !is_allowed_zalo_domain(&normalized)
            && !(self.config.allow_loopback_upstream_for_tests && normalized == "localhost")
        {
            return Err(AllowlistError::DisallowedHost(normalized));
        }

        if let Some(overridden_ips) = self.config.dns_overrides.get(&normalized) {
            if overridden_ips.is_empty() {
                return Err(AllowlistError::DnsFailed {
                    host: normalized,
                    reason: "empty dns override".to_string(),
                });
            }
            let mut out = Vec::with_capacity(overridden_ips.len());
            for &ip in overridden_ips {
                self.check_resolved_ip(ip)?;
                out.push(SocketAddr::new(ip, port));
            }
            return Ok(out);
        }

        if self.config.allow_loopback_upstream_for_tests && normalized == "localhost" {
            return Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)]);
        }

        let resolved = self
            .dns_resolver
            .resolve_host(&normalized)
            .await
            .map_err(|err| AllowlistError::DnsFailed {
                host: normalized.clone(),
                reason: err.to_string(),
            })?;

        if resolved.is_empty() {
            return Err(AllowlistError::DnsFailed {
                host: normalized,
                reason: "no addresses returned".to_string(),
            });
        }

        let mut verified = Vec::with_capacity(resolved.len());
        for mut addr in resolved {
            self.check_resolved_ip(addr.ip())?;
            addr.set_port(port);
            verified.push(addr);
        }
        Ok(verified)
    }

    /// Validates an outbound `https://` (or test `http://`) URL and returns `(Url, Vec<SocketAddr>)`.
    pub async fn validate_http_target(
        &self,
        raw_url: &str,
    ) -> Result<(Url, Vec<SocketAddr>), AllowlistError> {
        let parsed = Url::parse(raw_url.trim())
            .map_err(|e| AllowlistError::InvalidUrl(e.to_string()))?;
        let host = self.validate_url_surface(&parsed, false)?;
        let default_port = if parsed.scheme() == "http" { 80 } else { 443 };
        let port = parsed.port_or_known_default().unwrap_or(default_port);
        let addrs = self.resolve_validated_host(&host, port).await?;
        Ok((parsed, addrs))
    }

    /// Validates an outbound `wss://` (or test `ws://`) URL and returns `(Url, Vec<SocketAddr>)`.
    pub async fn validate_ws_target(
        &self,
        raw_url: &str,
    ) -> Result<(Url, Vec<SocketAddr>), AllowlistError> {
        let parsed = Url::parse(raw_url.trim())
            .map_err(|e| AllowlistError::InvalidUrl(e.to_string()))?;
        let host = self.validate_url_surface(&parsed, true)?;
        let default_port = if parsed.scheme() == "ws" { 80 } else { 443 };
        let port = parsed.port_or_known_default().unwrap_or(default_port);
        let addrs = self.resolve_validated_host(&host, port).await?;
        Ok((parsed, addrs))
    }
}

impl Resolve for AllowlistValidator {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let validator = self.clone();
        Box::pin(async move {
            let addrs = validator.resolve_validated_host(&host, 0).await?;
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}
