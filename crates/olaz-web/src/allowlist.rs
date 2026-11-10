//! Outbound SSRF & open-proxy prevention for `/__olaz/http`, `/__olaz/media`, and `/__olaz/ws`.
//!
//! Enforces three layers of protection before any outbound TCP/TLS socket is opened:
//! 1. **Scheme check**: strictly `https` (HTTP/media) or `wss` (WebSocket) in production.
//! 2. **Domain allowlist**: exact or subdomain match on [`ALLOWED_ZALO_SUFFIXES`]
//!    (Zalo API, WebSocket, photo/file/video/GIF CDNs, Zavi, and Zing media domains).
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
pub const ALLOWED_ZALO_SUFFIXES: [&str; 21] = [
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allowlist_permits_zalo_26_11_10_media_file_video_and_ws_domains_and_blocks_non_zalo_or_private_ips(
    ) {
        for suffix in ALLOWED_ZALO_SUFFIXES {
            assert!(
                is_allowed_zalo_domain(suffix),
                "exact suffix {suffix} must be allowed"
            );
        }

        let public_ip = IpAddr::V4(Ipv4Addr::new(203, 119, 9, 10));
        let mut dns_overrides = HashMap::new();
        for host in [
            "f12-zpcloud.zdn.vn",
            "b2.dlfl.vn",
            "v1.dlmd.me",
            "p1.flchat.vn",
            "c1.pchat.vn",
            "m1.mdchat.me",
            "media-ten.z-cdn.me",
            "ws.zavi.me",
        ] {
            assert!(
                is_allowed_zalo_domain(host),
                "expected {host} to match ALLOWED_ZALO_SUFFIXES"
            );
            dns_overrides.insert(host.to_string(), vec![public_ip]);
        }
        dns_overrides.insert(
            "rebind.dlfl.vn".to_string(),
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))],
        );

        let validator = AllowlistValidator::new(AllowlistConfig {
            allow_loopback_upstream_for_tests: false,
            dns_overrides,
        });

        for url in [
            "https://f12-zpcloud.zdn.vn/jpg/encrypted-photo.jpg",
            "https://b2.dlfl.vn/dl/hd-attachment.bin",
            "https://v1.dlmd.me/stream/video.mp4",
            "https://p1.flchat.vn/file/doc.pdf",
            "https://c1.pchat.vn/photo/thumb.jpg",
            "https://m1.mdchat.me/media/clip.mp4",
            "https://media-ten.z-cdn.me/gif/sticker.gif",
        ] {
            let (_, addrs) = validator
                .validate_http_target(url)
                .await
                .unwrap_or_else(|e| panic!("expected {url} to be permitted, got {e}"));
            assert_eq!(addrs, vec![SocketAddr::new(public_ip, 443)]);
        }

        let (_, ws_addrs) = validator
            .validate_ws_target("wss://ws.zavi.me/ws")
            .await
            .expect("wss://ws.zavi.me/ws must be permitted");
        assert_eq!(ws_addrs, vec![SocketAddr::new(public_ip, 443)]);

        // Arbitrary & suffix-spoofed external hosts must fail with DisallowedHost
        for bad_url in [
            "https://evil.com/payload",
            "https://dlfl.vn.evil.com/payload",
            "https://notzdn.vn/payload",
        ] {
            assert!(
                matches!(
                    validator.validate_http_target(bad_url).await,
                    Err(AllowlistError::DisallowedHost(_))
                ),
                "expected DisallowedHost for {bad_url}"
            );
        }
        assert!(matches!(
            validator
                .validate_ws_target("wss://dlfl.vn.evil.com/ws")
                .await,
            Err(AllowlistError::DisallowedHost(_))
        ));

        // Private / loopback / link-local / CGNAT IPs (literal or via DNS rebinding) must fail with DisallowedIp
        for bad_ip_url in [
            "https://127.0.0.1/secret",
            "https://10.0.0.1/secret",
            "https://192.168.1.1/secret",
            "https://169.254.169.254/latest/meta-data",
            "https://100.64.0.1/cgnat",
            "https://rebind.dlfl.vn/rebound",
        ] {
            assert!(
                matches!(
                    validator.validate_http_target(bad_ip_url).await,
                    Err(AllowlistError::DisallowedIp(_))
                ),
                "expected DisallowedIp for {bad_ip_url}"
            );
        }
    }
}
