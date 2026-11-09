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
pub const UNLOCK_PAGE_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Olaz Web — Unlock Edge Relay</title>
  <style>
    :root { color-scheme: dark; }
    * { box-sizing: border-box; }
    body {
      margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
      background: radial-gradient(circle at 50% 0%, #162447 0%, #0b1020 65%);
      color: #e6edf3; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      padding: 20px;
    }
    .card {
      width: 100%; max-width: 400px; background: rgba(19, 27, 46, 0.92);
      border: 1px solid rgba(99, 140, 255, 0.25); border-radius: 16px;
      padding: 28px; box-shadow: 0 20px 50px rgba(0, 0, 0, 0.55);
    }
    .badge {
      display: inline-block; font-size: 11px; font-weight: 600; letter-spacing: 0.04em;
      text-transform: uppercase; color: #58a6ff; background: rgba(56, 139, 253, 0.15);
      padding: 4px 10px; border-radius: 999px; margin-bottom: 12px;
    }
    h1 { margin: 0 0 8px; font-size: 20px; font-weight: 600; }
    p { margin: 0 0 20px; font-size: 13px; line-height: 1.5; color: #9da7b3; }
    input {
      width: 100%; padding: 11px 14px; border-radius: 10px;
      border: 1px solid rgba(255, 255, 255, 0.16); background: #0d1324;
      color: #fff; font-size: 14px; outline: none; margin-bottom: 12px;
    }
    input:focus { border-color: #388bfd; }
    button {
      width: 100%; padding: 11px 14px; border-radius: 10px; border: 0;
      background: #1f6feb; color: #fff; font-size: 14px; font-weight: 600;
      cursor: pointer; transition: background 0.15s ease;
    }
    button:hover { background: #388bfd; }
    .err { margin-top: 12px; font-size: 13px; color: #ff7b72; min-height: 18px; }
  </style>
</head>
<body>
  <form class="card" id="gate-form">
    <span class="badge">Zero Server Persistence</span>
    <h1>Unlock Olaz Web Relay</h1>
    <p>Enter your server access token to issue a memory-only gate cookie. Your Zalo session, keys, and messages remain 100% inside your browser.</p>
    <input id="token-input" type="password" name="token" placeholder="Server access token" autocomplete="current-password" required autofocus>
    <button type="submit">Unlock Relay</button>
    <div class="err" id="err-box"></div>
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
"#;
