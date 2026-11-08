//! Persistent RFC 6265 cookie jar with domain-cookie semantics for Zalo PC.
//!
//! Mirrors the cookie storage and `_electron_*` IPC channels from
//! `ZaloMainProcess.swift` and WebSocket upgrade cookie injection from `NetworkProxy.swift`.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use cookie::{Cookie as RawCookie, SameSite};
use cookie_store::{Cookie as StoredCookie, CookieDomain, CookieExpiration, CookieStore};
use parking_lot::Mutex;
use reqwest_cookie_store::CookieStoreMutex;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Auth domain whose `zpw_sek` cookie indicates an active Zalo PC login (`XPAe.DEFAULT_AUTH_DOMAIN`).
pub const AUTH_DOMAIN: &str = "https://wpa.zaloapp.com";

/// Domains across which `_electron_set-app-cookie` installs the `zpw_sek` session domain cookie.
pub const SESSION_COOKIE_DOMAINS: [&str; 3] = ["zaloapp.com", "zalo.me", "chat.zalo.me"];

/// Domains wiped by `_electron_clear-cookie` (`ZaloMainProcess.isZaloDomain`).
const ZALO_COOKIE_CLEAR_DOMAINS: [&str; 7] = [
    "zalo.me",
    "zaloapp.com",
    "zadn.vn",
    "zdn.vn",
    "zing.vn",
    "zingcdn.me",
    "zalo.vn",
];

/// Serializable cookie descriptor matching Electron's `session.cookies.get(...)` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieEntry {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration_date: Option<i64>,
}

/// Thread-safe persistent cookie jar wrapping [`CookieStoreMutex`] with atomic `0600` disk persistence.
#[derive(Debug, Clone)]
pub struct ZaloCookieJar {
    store: Arc<CookieStoreMutex>,
    persist_path: Option<PathBuf>,
    io_lock: Arc<Mutex<()>>,
}

impl Default for ZaloCookieJar {
    fn default() -> Self {
        Self::new_in_memory()
    }
}

impl ZaloCookieJar {
    /// Creates an ephemeral in-memory cookie jar (useful for isolated tests).
    pub fn new_in_memory() -> Self {
        Self {
            store: Arc::new(CookieStoreMutex::new(CookieStore::default())),
            persist_path: None,
            io_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Opens or creates a persistent cookie jar backed by `path` (typically `cookies.json`).
    ///
    /// Existing unexpired persistent cookies are loaded if `path` exists; if the file does not
    /// exist yet, an empty jar is initialized and persisted with mode `0600` on first write
    /// (or immediately via [`Self::save`]).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let loaded_store = if path.exists() {
            let meta = fs::metadata(&path)
                .with_context(|| format!("failed to stat cookie store at {}", path.display()))?;
            if meta.len() == 0 {
                CookieStore::default()
            } else {
                let file = fs::File::open(&path).with_context(|| {
                    format!("failed to open cookie store at {}", path.display())
                })?;
                let reader = BufReader::new(file);
                cookie_store::serde::json::load(reader).map_err(|err| {
                    anyhow::anyhow!(
                        "failed to deserialize cookie store from {}: {err}",
                        path.display()
                    )
                })?
            }
        } else {
            CookieStore::default()
        };

        Ok(Self {
            store: Arc::new(CookieStoreMutex::new(loaded_store)),
            persist_path: Some(path),
            io_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Returns the underlying `Arc<CookieStoreMutex>` for `reqwest::ClientBuilder::cookie_provider`.
    pub fn inner(&self) -> Arc<CookieStoreMutex> {
        Arc::clone(&self.store)
    }

    /// Returns the configured persistence path, if any.
    pub fn path(&self) -> Option<&Path> {
        self.persist_path.as_deref()
    }

    /// Atomically persists all unexpired persistent cookies to `cookies.json` with `0600` permissions.
    pub fn save(&self) -> Result<()> {
        let Some(ref target_path) = self.persist_path else {
            return Ok(());
        };

        let _io_guard = self.io_lock.lock();

        // Snapshot serialized JSON while holding the store lock briefly.
        let mut json_buf = Vec::new();
        {
            let guard = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("cookie store mutex poisoned"))?;
            let mut writer = BufWriter::new(&mut json_buf);
            cookie_store::serde::json::save(&guard, &mut writer)
                .map_err(|err| anyhow::anyhow!("failed to serialize cookies: {err}"))?;
            writer.flush()?;
        }

        if let Some(parent) = target_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create cookie directory {}", parent.display())
                })?;
            }
        }

        let file_name = target_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cookies.json");
        let tmp_path = target_path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

        let write_result = (|| -> Result<()> {
            let mut opts = OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut tmp_file = opts.open(&tmp_path).with_context(|| {
                format!("failed to create temp cookie file {}", tmp_path.display())
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
            }
            tmp_file.write_all(&json_buf)?;
            tmp_file.sync_all()?;
            drop(tmp_file);

            fs::rename(&tmp_path, target_path).with_context(|| {
                format!(
                    "failed to atomically rename {} -> {}",
                    tmp_path.display(),
                    target_path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(target_path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        write_result
    }

    /// Checks whether an active, unexpired, non-empty `zpw_sek` session cookie matches [`AUTH_DOMAIN`].
    pub fn has_session(&self) -> bool {
        let Ok(auth_url) = Url::parse(&format!("{}/", AUTH_DOMAIN.trim_end_matches('/'))) else {
            return false;
        };
        let Ok(guard) = self.store.lock() else {
            return false;
        };
        guard
            .matches(&auth_url)
            .into_iter()
            .any(|c| c.name() == "zpw_sek" && !c.value().is_empty())
    }

    /// Installs the `zpw_sek` session cookie across all [`SESSION_COOKIE_DOMAINS`] (`_electron_set-app-cookie`).
    ///
    /// Removes all existing `zpw_sek` cookies first (including stale host-only cookies from earlier
    /// `Set-Cookie` responses) so subdomains like `ws5-msg.chat.zalo.me` and
    /// `tt-profile-wpa.zaloapp.com` receive the exact new domain cookie (`HostOnly = false`,
    /// `Path=/`, `Secure`, `HttpOnly`, `SameSite=None`, 7-day expiry).
    pub fn set_app_cookie(&self, zpw_sek: &str) -> Result<()> {
        {
            let mut guard = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("cookie store mutex poisoned"))?;

            let stale_keys: Vec<(String, String, String)> = guard
                .iter_any()
                .filter(|c| c.name() == "zpw_sek")
                .map(|c| {
                    (
                        String::from(&c.domain),
                        String::from(&c.path),
                        c.name().to_owned(),
                    )
                })
                .collect();

            for (domain, path, name) in stale_keys {
                guard.remove(&domain, &path, &name);
            }

            for domain in SESSION_COOKIE_DOMAINS {
                Self::insert_domain_cookie_locked(
                    &mut guard,
                    "zpw_sek",
                    zpw_sek,
                    domain,
                    "/",
                    7 * 86_400,
                    true,
                    true,
                    SameSite::None,
                )?;
            }
        }

        self.save()
    }

    /// Sets a generic Electron IPC domain cookie (`_electron_set-cookie` in `ZaloMainProcess.swift:278-283`).
    ///
    /// Resolves the target domain from `domain`, else the host of `url`, else `"zalo.me"`,
    /// with 365-day expiration, `Secure`, `SameSite=None`, `HttpOnly = false`, and `HostOnly = false`.
    pub fn set_cookie(
        &self,
        name: &str,
        value: &str,
        url: Option<&str>,
        domain: Option<&str>,
    ) -> Result<()> {
        let resolved_domain = domain
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                url.and_then(|u| Url::parse(u).ok())
                    .and_then(|u| u.host_str().map(str::to_owned))
            })
            .unwrap_or_else(|| "zalo.me".to_owned());

        let is_plain_http = url.is_some_and(|u| u.trim().starts_with("http://"));

        {
            let mut guard = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("cookie store mutex poisoned"))?;

            Self::insert_domain_cookie_locked(
                &mut guard,
                name,
                value,
                &resolved_domain,
                "/",
                365 * 86_400,
                !is_plain_http,
                false,
                if is_plain_http {
                    SameSite::Lax
                } else {
                    SameSite::None
                },
            )?;
        }

        self.save()
    }

    /// Parses and inserts a raw `Set-Cookie` header received from `request_url` and persists to disk.
    ///
    /// When the `Set-Cookie` header has no `Domain=` attribute, RFC 6265 stores it as a `HostOnly` cookie.
    pub fn insert_set_cookie(&self, set_cookie_header: &str, request_url: &str) -> Result<()> {
        let url = Url::parse(request_url)
            .with_context(|| format!("invalid request URL for Set-Cookie: {request_url}"))?;
        {
            let mut guard = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("cookie store mutex poisoned"))?;
            guard
                .parse(set_cookie_header, &url)
                .map_err(|e| anyhow::anyhow!("failed to parse Set-Cookie header: {e}"))?;
        }
        self.save()
    }

    /// Lists unexpired cookies matching optional URL, domain, and name filters.
    pub fn get_cookies(
        &self,
        url: Option<&str>,
        domain: Option<&str>,
        name: Option<&str>,
    ) -> Vec<CookieEntry> {
        let parsed_url = url.and_then(|u| Self::normalize_cookie_url(u));
        let normalized_domain = domain.map(|d| d.trim().trim_start_matches('.').to_ascii_lowercase());

        let Ok(guard) = self.store.lock() else {
            return Vec::new();
        };

        let candidates: Vec<&StoredCookie<'static>> = if let Some(ref target_url) = parsed_url {
            guard.matches(target_url)
        } else {
            guard.iter_unexpired().collect()
        };

        let mut out: Vec<CookieEntry> = candidates
            .into_iter()
            .filter(|c| {
                if let Some(expected_name) = name {
                    if c.name() != expected_name {
                        return false;
                    }
                }
                if let Some(ref dom_filter) = normalized_domain {
                    let cookie_dom = String::from(&c.domain).to_ascii_lowercase();
                    let matches_dom = cookie_dom == *dom_filter
                        || (cookie_dom.len() > dom_filter.len()
                            && cookie_dom.ends_with(dom_filter.as_str())
                            && cookie_dom.as_bytes()[cookie_dom.len() - dom_filter.len() - 1]
                                == b'.');
                    if !matches_dom {
                        return false;
                    }
                }
                true
            })
            .map(Self::to_cookie_entry)
            .collect();

        out.sort_by(|a, b| {
            a.domain
                .cmp(&b.domain)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    /// Deletes cookies matching `name` (and optional `url_or_domain`) and persists the store.
    pub fn delete_cookie(&self, url_or_domain: Option<&str>, name: &str) -> Result<usize> {
        let removed = {
            let mut guard = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("cookie store mutex poisoned"))?;

            let target_url = url_or_domain.and_then(Self::normalize_cookie_url);
            let target_domain = url_or_domain.map(|s| {
                if let Some(ref u) = target_url {
                    u.host_str().unwrap_or(s).to_ascii_lowercase()
                } else {
                    s.trim().trim_start_matches('.').to_ascii_lowercase()
                }
            });

            let keys: Vec<(String, String, String)> = guard
                .iter_any()
                .filter(|c| {
                    if c.name() != name {
                        return false;
                    }
                    if let Some(ref u) = target_url {
                        return c.domain.matches(u);
                    }
                    if let Some(ref d) = target_domain {
                        return String::from(&c.domain).eq_ignore_ascii_case(d);
                    }
                    true
                })
                .map(|c| {
                    (
                        String::from(&c.domain),
                        String::from(&c.path),
                        c.name().to_owned(),
                    )
                })
                .collect();

            let count = keys.len();
            for (d, p, n) in keys {
                guard.remove(&d, &p, &n);
            }
            count
        };

        self.save()?;
        Ok(removed)
    }

    /// Clears all Zalo-domain cookies (`_electron_clear-cookie` in `ZaloMainProcess.swift:478-488`) and persists.
    pub fn clear_cookies(&self) -> Result<usize> {
        let removed = {
            let mut guard = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("cookie store mutex poisoned"))?;

            let keys: Vec<(String, String, String)> = guard
                .iter_any()
                .filter(|c| is_zalo_cookie_domain(&String::from(&c.domain)))
                .map(|c| {
                    (
                        String::from(&c.domain),
                        String::from(&c.path),
                        c.name().to_owned(),
                    )
                })
                .collect();

            let count = keys.len();
            for (domain, path, name) in keys {
                guard.remove(&domain, &path, &name);
            }
            count
        };

        self.save()?;
        Ok(removed)
    }

    /// Dispatches Electron cookie IPC channels (`_electron_set-app-cookie`, `_electron_set-cookie`,
    /// `_electron_clear-cookie`, `_electron_get-cookies`, `_electron_delete-cookie`).
    pub fn handle_ipc(
        &self,
        channel: &str,
        args: &[serde_json::Value],
    ) -> Result<serde_json::Value> {
        match channel {
            "_electron_set-app-cookie" => {
                if let Some(value) = args.first().and_then(|v| v.as_str()) {
                    self.set_app_cookie(value)?;
                }
                Ok(serde_json::Value::Null)
            }
            "_electron_set-cookie" => {
                if let (Some(name), Some(value)) = (
                    args.first().and_then(|v| v.as_str()),
                    args.get(1).and_then(|v| v.as_str()),
                ) {
                    let url = args.get(2).and_then(|v| v.as_str());
                    let domain = args.get(3).and_then(|v| v.as_str());
                    self.set_cookie(name, value, url, domain)?;
                }
                Ok(serde_json::Value::Null)
            }
            "_electron_clear-cookie" => {
                self.clear_cookies()?;
                Ok(serde_json::Value::Null)
            }
            "_electron_get-cookies" => {
                let filter = args.first().and_then(|v| v.as_object());
                let url = filter.and_then(|f| f.get("url")).and_then(|v| v.as_str());
                let domain = filter
                    .and_then(|f| f.get("domain"))
                    .and_then(|v| v.as_str());
                let name = filter.and_then(|f| f.get("name")).and_then(|v| v.as_str());
                Ok(serde_json::to_value(self.get_cookies(url, domain, name))?)
            }
            "_electron_delete-cookie" => {
                let url_or_domain = args.first().and_then(|v| v.as_str());
                if let Some(name) = args.get(1).and_then(|v| v.as_str()) {
                    self.delete_cookie(url_or_domain, name)?;
                }
                Ok(serde_json::Value::Null)
            }
            other => anyhow::bail!("unsupported cookie IPC channel: {other}"),
        }
    }

    /// Formats matching cookies (including `HttpOnly` cookies like `zpw_sek`) as `"k1=v1; k2=v2"`
    /// for HTTP or WebSocket (`ws://` / `wss://`) upgrade requests.
    pub fn cookie_header_for_url(&self, url_str: &str) -> Option<String> {
        let url = Self::normalize_cookie_url(url_str)?;
        let guard = self.store.lock().ok()?;
        let mut matches = guard.matches(&url);
        if matches.is_empty() {
            return None;
        }

        // RFC 6265 §5.4: cookies with longer paths are ordered first; tie-break deterministically.
        matches.sort_by(|a, b| {
            let pa = String::from(&a.path);
            let pb = String::from(&b.path);
            pb.len()
                .cmp(&pa.len())
                .then_with(|| a.name().cmp(b.name()))
                .then_with(|| String::from(&a.domain).cmp(&String::from(&b.domain)))
        });

        let mut seen = HashSet::new();
        let mut pairs = Vec::with_capacity(matches.len());
        for c in matches {
            let key = (c.name().to_owned(), c.value().to_owned());
            if seen.insert(key) {
                pairs.push(format!("{}={}", c.name(), c.value()));
            }
        }

        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
    }

    fn normalize_cookie_url(url_str: &str) -> Option<Url> {
        let mut url = Url::parse(url_str).ok()?;
        match url.scheme() {
            "wss" => {
                url.set_scheme("https").ok()?;
            }
            "ws" => {
                url.set_scheme("http").ok()?;
            }
            "https" | "http" => {}
            _ => return None,
        }
        Some(url)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_domain_cookie_locked(
        store: &mut CookieStore,
        name: &str,
        value: &str,
        domain: &str,
        path: &str,
        max_age_secs: i64,
        secure: bool,
        http_only: bool,
        same_site: SameSite,
    ) -> Result<()> {
        let clean_domain = domain
            .trim()
            .trim_start_matches('.')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if clean_domain.is_empty() {
            anyhow::bail!("cookie domain must not be empty");
        }

        let is_ip = clean_domain.parse::<std::net::IpAddr>().is_ok();
        let scheme = if secure { "https" } else { "http" };
        let host_for_url = if clean_domain.contains(':') && !clean_domain.starts_with('[') {
            format!("[{clean_domain}]")
        } else {
            clean_domain.clone()
        };
        let request_url = Url::parse(&format!("{scheme}://{host_for_url}/"))
            .with_context(|| format!("invalid cookie domain: {clean_domain}"))?;

        let mut builder = RawCookie::build((name.to_owned(), value.to_owned()))
            .path(path.to_owned())
            .secure(secure)
            .http_only(http_only)
            .same_site(same_site)
            .max_age(cookie::time::Duration::seconds(max_age_secs));
        if !is_ip {
            builder = builder.domain(clean_domain.clone());
        }
        let raw_cookie = builder.build();

        let stored = StoredCookie::try_from_raw_cookie(&raw_cookie, &request_url)
            .map_err(|e| anyhow::anyhow!("failed to construct domain cookie for {clean_domain}: {e}"))?
            .into_owned();

        debug_assert!(
            is_ip || matches!(stored.domain, CookieDomain::Suffix(_)),
            "expected RFC 6265 Suffix domain cookie (HostOnly = false)"
        );

        store
            .insert(stored, &request_url)
            .map_err(|e| anyhow::anyhow!("failed to insert cookie into store for {clean_domain}: {e}"))?;
        Ok(())
    }

    fn to_cookie_entry(cookie: &StoredCookie<'static>) -> CookieEntry {
        let (domain, host_only) = match &cookie.domain {
            CookieDomain::Suffix(d) => (format!(".{d}"), false),
            CookieDomain::HostOnly(h) => (h.clone(), true),
            CookieDomain::Empty | CookieDomain::NotPresent => (String::new(), true),
        };
        let same_site = cookie.same_site().map(|ss| match ss {
            SameSite::Strict => "strict".to_owned(),
            SameSite::Lax => "lax".to_owned(),
            SameSite::None => "no_restriction".to_owned(),
        });
        let expiration_date = match cookie.expires {
            CookieExpiration::AtUtc(dt) => Some(dt.unix_timestamp()),
            CookieExpiration::SessionEnd => None,
        };

        CookieEntry {
            name: cookie.name().to_owned(),
            value: cookie.value().to_owned(),
            domain,
            host_only,
            path: String::from(&cookie.path),
            secure: cookie.secure().unwrap_or(false),
            http_only: cookie.http_only().unwrap_or(false),
            same_site,
            expiration_date,
        }
    }
}

/// Checks whether `domain` belongs to Zalo's cookie domains (`ZaloMainProcess.isZaloDomain`).
pub fn is_zalo_cookie_domain(domain: &str) -> bool {
    let d = domain
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    ZALO_COOKIE_CLEAR_DOMAINS.iter().any(|suffix| {
        d == *suffix
            || (d.len() > suffix.len()
                && d.ends_with(suffix)
                && d.as_bytes()[d.len() - suffix.len() - 1] == b'.')
    })
}
