//! Stateless edge authentication gate (`--token` / `OLAZ_WEB_TOKEN`).
//!
//! Zero server-side session storage:
//! - Compares incoming `Authorization: Bearer <token>` or `POST /__olaz/auth` credentials
//!   in constant time using `subtle::ConstantTimeEq`.
//! - Generates a 32-byte CSPRNG HMAC-SHA256 secret purely in RAM on boot (never persisted to disk)
//!   to sign and verify `HttpOnly; SameSite=Strict` `olaz_gate` cookies.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Cookie name used for stateless browser gate authentication.
pub const GATE_COOKIE_NAME: &str = "olaz_gate";

/// Default gate cookie lifetime (`12 hours`).
pub const GATE_COOKIE_TTL_SECS: u64 = 12 * 3600;

/// Stateless edge auth gate.
#[derive(Clone)]
pub struct AuthGate {
    expected_token: Option<Vec<u8>>,
    boot_hmac_secret: [u8; 32],
}

impl std::fmt::Debug for AuthGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthGate")
            .field("enabled", &self.expected_token.is_some())
            .finish()
    }
}

impl AuthGate {
    /// Creates an [`AuthGate`] with an optional shared access token and a fresh per-boot CSPRNG HMAC key.
    pub fn new(token: Option<String>) -> Self {
        let cleaned = token
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .map(String::into_bytes);
        let mut boot_hmac_secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut boot_hmac_secret);
        Self {
            expected_token: cleaned,
            boot_hmac_secret,
        }
    }

    /// Returns `true` when `--token` / `OLAZ_WEB_TOKEN` protection is active.
    pub fn is_enabled(&self) -> bool {
        self.expected_token.is_some()
    }

    /// Verifies a candidate access token in constant time (`subtle::ConstantTimeEq`).
    ///
    /// Hashes both expected and candidate tokens with SHA-256 first so length differences
    /// do not leak the secret length through early-return timing!
    pub fn verify_token(&self, candidate: &str) -> bool {
        let Some(expected) = &self.expected_token else {
            return true;
        };
        use sha2::Digest;
        let expected_digest = Sha256::digest(expected);
        let candidate_digest = Sha256::digest(candidate.as_bytes());
        let digests_match = expected_digest.ct_eq(&candidate_digest);
        let lengths_match = (expected.len() as u64).ct_eq(&(candidate.len() as u64));
        bool::from(digests_match & lengths_match)
    }

    /// Checks whether an incoming HTTP request is authorized via `Authorization: Bearer <token>`
    /// or a valid signed `olaz_gate` cookie.
    pub fn is_authorized(
        &self,
        authorization_header: Option<&str>,
        cookie_header: Option<&str>,
    ) -> bool {
        if !self.is_enabled() {
            return true;
        }

        if let Some(auth) = authorization_header {
            let trimmed = auth.trim();
            if let Some(rest) = trimmed
                .strip_prefix("Bearer ")
                .or_else(|| trimmed.strip_prefix("bearer "))
            {
                if self.verify_token(rest.trim()) {
                    return true;
                }
            }
        }

        if let Some(cookies) = cookie_header {
            for part in cookies.split(';') {
                let part = part.trim();
                if let Some(val) = part.strip_prefix("olaz_gate=") {
                    if self.verify_gate_cookie_value(val.trim()) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Issues a signed stateless `Set-Cookie` header value for `olaz_gate`.
    pub fn issue_gate_set_cookie(&self, is_https: bool) -> String {
        let now = unix_now_secs();
        let exp = now.saturating_add(GATE_COOKIE_TTL_SECS);
        let mut nonce = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce);
        let nonce_hex = hex::encode(nonce);
        let payload = format!("{exp}.{nonce_hex}");
        let sig_hex = hex::encode(self.compute_hmac(payload.as_bytes()));
        let secure_attr = if is_https { "; Secure" } else { "" };
        format!(
            "{GATE_COOKIE_NAME}={payload}.{sig_hex}; Max-Age={GATE_COOKIE_TTL_SECS}; Path=/; HttpOnly; SameSite=Strict{secure_attr}"
        )
    }

    /// Verifies `<exp_unix>.<nonce_hex>.<hmac_hex>` in constant time.
    pub fn verify_gate_cookie_value(&self, raw: &str) -> bool {
        let mut parts = raw.splitn(3, '.');
        let (Some(exp_str), Some(nonce_hex), Some(sig_hex)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let Ok(exp) = exp_str.parse::<u64>() else {
            return false;
        };
        if exp <= unix_now_secs() {
            return false;
        }
        if nonce_hex.len() != 32 || !nonce_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        let Ok(candidate_sig) = hex::decode(sig_hex) else {
            return false;
        };
        if candidate_sig.len() != 32 {
            return false;
        }
        let payload = format!("{exp_str}.{nonce_hex}");
        let expected_sig = self.compute_hmac(payload.as_bytes());
        bool::from(expected_sig.ct_eq(candidate_sig.as_slice()))
    }

    fn compute_hmac(&self, data: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(&self.boot_hmac_secret)
            .expect("32-byte HMAC key is always valid");
        mac.update(data);
        let bytes = mac.finalize().into_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Clean, self-contained HTML unlock page served with `401 Unauthorized` on unauthenticated
/// browser navigation when `--token` / `OLAZ_WEB_TOKEN` is enabled.
pub const UNLOCK_PAGE_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
  <meta name="theme-color" content="#0068FF">
  <meta name="application-name" content="KV Zalo">
  <meta name="mobile-web-app-capable" content="yes">
  <meta name="apple-mobile-web-app-capable" content="yes">
  <meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">
  <meta name="apple-mobile-web-app-title" content="KV Zalo">
  <link rel="manifest" href="/manifest.webmanifest">
  <link rel="icon" type="image/svg+xml" href="/favicon.svg">
  <link rel="icon" type="image/png" sizes="192x192" href="/__olaz/icons/icon-192.png">
  <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">
  <title>KV Zalo — Unlock Web Relay</title>
  <style>
    :root { color-scheme: dark; accent-color: #0068FF; }
    * { box-sizing: border-box; }
    ::selection { background: rgba(0, 104, 255, 0.35); color: #f8fafc; }
    body {
      margin: 0;
      min-height: 100vh;
      min-height: 100dvh;
      display: flex;
      align-items: center;
      justify-content: center;
      background: radial-gradient(circle at 50% 0%, #132347 0%, #0b1120 68%);
      color: #e6edf3;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      padding: max(16px, env(safe-area-inset-top)) max(16px, env(safe-area-inset-right)) max(16px, env(safe-area-inset-bottom)) max(16px, env(safe-area-inset-left));
      -webkit-font-smoothing: antialiased;
    }
    .card {
      width: 100%;
      max-width: 400px;
      background: rgba(19, 28, 49, 0.94);
      border: 1px solid rgba(99, 148, 255, 0.22);
      border-radius: 18px;
      padding: clamp(20px, 5vw, 30px);
      box-shadow: 0 20px 48px rgba(0, 0, 0, 0.5);
    }
    .brand-logo {
      display: block;
      width: 56px;
      height: 56px;
      border-radius: 14px;
      margin-bottom: 16px;
    }
    h1 {
      margin: 0 0 8px;
      font-size: clamp(1.15rem, 2.5vw + 0.6rem, 1.4rem);
      font-weight: 650;
      letter-spacing: -0.015em;
      line-height: 1.25;
      color: #f8fafc;
    }
    p {
      margin: 0 0 20px;
      font-size: clamp(0.85rem, 1vw + 0.62rem, 0.94rem);
      line-height: 1.55;
      color: #94a3b8;
    }
    input[type="password"] {
      width: 100%;
      min-height: 46px;
      padding: 11px 14px;
      border-radius: 12px;
      border: 1px solid rgba(148, 163, 184, 0.24);
      background: #0b1222;
      color: #f8fafc;
      font-size: 16px;
      line-height: 1.35;
      margin-bottom: 12px;
      transition: border-color 0.15s ease, box-shadow 0.15s ease;
    }
    input[type="password"]::placeholder { color: #64748b; }
    input[type="password"]:focus {
      outline: none;
      border-color: #0068FF;
      box-shadow: 0 0 0 3px rgba(0, 104, 255, 0.25);
    }
    input[type="password"]:focus-visible,
    button:focus-visible {
      outline: 2px solid #388bfd;
      outline-offset: 2px;
    }
    button {
      width: 100%;
      min-height: 46px;
      padding: 12px 16px;
      border-radius: 12px;
      border: 0;
      background: #0068FF;
      color: #ffffff;
      font-size: 15px;
      font-weight: 600;
      cursor: pointer;
      touch-action: manipulation;
      transition: background 0.15s ease, transform 0.1s ease;
    }
    button:hover { background: #1a78ff; }
    button:active { transform: translateY(1px); }
    .err {
      margin-top: 12px;
      font-size: 13px;
      line-height: 1.4;
      color: #ff7b72;
      min-height: 18px;
    }
    @media (max-height: 480px) and (orientation: landscape) {
      .card { padding: 16px 20px; }
      .brand-logo { width: 44px; height: 44px; margin-bottom: 10px; }
      p { margin-bottom: 12px; }
      .err { margin-top: 8px; }
    }
  </style>
</head>
<body>
  <form class="card" id="gate-form">
    <img src="/__olaz/logo.svg" width="56" height="56" alt="KV Zalo" class="brand-logo">
    <h1>Unlock KV Zalo Web Relay</h1>
    <p>Enter your server access token to issue a memory-only gate cookie. Your Zalo session, keys, and messages remain 100% inside your browser.</p>
    <input id="token-input" type="password" name="token" placeholder="Server access token" autocomplete="current-password" required autofocus>
    <button type="submit">Unlock Relay</button>
    <div class="err" id="err-box" role="alert" aria-live="polite"></div>
  </form>
  <script>
    document.getElementById('gate-form').addEventListener('submit', async (ev) => {
      ev.preventDefault();
      const errBox = document.getElementById('err-box');
      errBox.textContent = '';
      const token = document.getElementById('token-input').value;
      try {
        const res = await fetch('/__olaz/auth', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ token }),
          credentials: 'same-origin'
        });
        if (res.ok) {
          window.location.reload();
        } else {
          errBox.textContent = 'Invalid access token.';
        }
      } catch (e) {
        errBox.textContent = 'Network error while contacting relay.';
      }
    });
  </script>
</body>
</html>
"##;
