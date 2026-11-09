//! Secure credential vaults: `SafeStorage` (`zsafe-storage.json`) and `KeychainCipher`
//! (`zprotocol` trusted-device envelope backed by D-Bus Secret Service / `oo7`).
//!
//! Port of `SafeStorage` and `KeychainCipher` in `Sources/Olaz/ZaloRuntime/ZaloMainProcess.swift:493-678`.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;

use base64::engine::general_purpose::STANDARD as B64_STANDARD;
use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use rand::RngCore;
use serde_json::{json, Map, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::crypto::aes256_cbc_pkcs7;
use crate::platform::{sanitize_profile_slug, ZaloPlatform};
use crate::wire;

type HmacSha256 = Hmac<Sha256>;

/// Errors produced by [`SecretBackend`] and [`KeychainCipher`].
#[derive(Debug, Error)]
pub enum VaultError {
    #[error("Secret backend is locked or authorization was denied: {0}")]
    Locked(String),
    #[error("No secret found for identifier '{0}'")]
    NotFound(String),
    #[error("Invalid encrypted data length")]
    InvalidLength,
    #[error("Invalid encrypted envelope header")]
    InvalidHeader,
    #[error("Invalid MAC")]
    InvalidMac,
    #[error("Cipher error: {0}")]
    Cipher(String),
    #[error("Secret backend error: {0}")]
    Backend(String),
}

// ============================================================================
// SafeStorage (`zsafestorage-execute`)
// ============================================================================

/// String vault backed by `electron-store` file `zsafe-storage.json` (`zsskv = "2"`,
/// values under `zsskd.<key>`), encrypted with Zalo main's fixed AES-256-CBC key & IV.
#[derive(Debug, Default, Clone, Copy)]
pub struct SafeStorage;

impl SafeStorage {
    pub const STORE_NAME: &'static str = "zsafe-storage";
    pub const CIPHER_VERSION: &'static str = "2";
    pub const KEY: &'static [u8; 32] = b"d9c07b9de9915af401607d3f360211ac";
    pub const IV: [u8; 16] = [
        155, 72, 37, 195, 9, 217, 80, 42, 145, 102, 171, 75, 214, 176, 134, 18,
    ];

    pub fn new() -> Self {
        Self
    }

    /// Executes a `zsafestorage-execute` IPC command (`encryptString`, `decryptString`, `get`, `set`).
    pub fn execute(
        &self,
        platform: &ZaloPlatform,
        method: &str,
        args: &[Value],
    ) -> Result<Value, String> {
        match method {
            "encryptString" => {
                let plain = args.first().and_then(Value::as_str).unwrap_or("");
                Ok(Value::String(Self::encrypt_string(plain)?))
            }
            "decryptString" => {
                let sealed = args.first().and_then(Value::as_str).unwrap_or("");
                Ok(Value::String(Self::decrypt_string(sealed)?))
            }
            "get" => {
                let Some(key) = args.first().and_then(Value::as_str) else {
                    return Ok(Value::Null);
                };
                if key.contains('.') {
                    return Ok(Value::Null);
                }
                let store = platform.read_store(Self::STORE_NAME);
                let Some(sealed) = store
                    .get("zsskd")
                    .and_then(Value::as_object)
                    .and_then(|m| m.get(key))
                    .and_then(Value::as_str)
                else {
                    return Ok(Value::Null);
                };
                match Self::decrypt_string(sealed) {
                    Ok(plain) => Ok(Value::String(plain)),
                    Err(_) => Ok(Value::Null),
                }
            }
            "set" => {
                let Some(key) = args.first().and_then(Value::as_str) else {
                    return Ok(Value::Null);
                };
                if key.contains('.') {
                    return Ok(Value::Null);
                }
                let mut store = platform.read_store(Self::STORE_NAME);
                let mut values: Map<String, Value> = store
                    .remove("zsskd")
                    .and_then(|v| match v {
                        Value::Object(m) => Some(m),
                        _ => None,
                    })
                    .unwrap_or_default();

                match args.get(1) {
                    Some(Value::String(val)) => {
                        values.insert(key.to_string(), Value::String(Self::encrypt_string(val)?));
                    }
                    _ => {
                        values.remove(key);
                    }
                }

                store.insert("zsskd".to_string(), Value::Object(values));
                store.insert("zsskv".to_string(), json!(Self::CIPHER_VERSION));
                platform
                    .write_store(Self::STORE_NAME, &store)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            other => Err(format!("zsafestorage: unknown method {other}")),
        }
    }

    pub fn encrypt_string(plain: &str) -> Result<String, String> {
        let ct = aes256_cbc_pkcs7(plain.as_bytes(), Self::KEY, &Self::IV, true)?;
        Ok(B64_STANDARD.encode(ct))
    }

    pub fn decrypt_string(sealed: &str) -> Result<String, String> {
        let raw = B64_STANDARD
            .decode(sealed.trim())
            .map_err(|_| "bad decrypt".to_string())?;
        let pt = aes256_cbc_pkcs7(&raw, Self::KEY, &Self::IV, false)?;
        String::from_utf8(pt).map_err(|_| "bad decrypt".to_string())
    }
}

// ============================================================================
// Pluggable SecretBackend (`oo7` Linux Secret Service + Memory test backend)
// ============================================================================

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Pluggable key storage backend for [`KeychainCipher`].
pub trait SecretBackend: Send + Sync {
    /// Looks up a secret for `(service, account)`.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` when the secret exists and was read.
    /// - `Ok(None)` **only** when the secret definitively does not exist.
    /// - `Err(VaultError::Locked(_))` or `Err(VaultError::Backend(_))` if the keyring
    ///   is locked, prompt dismissed, or inaccessible — callers MUST NOT mint a new key.
    fn get_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, VaultError>>;

    /// Stores or replaces the secret for `(service, account)`.
    fn set_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
        secret: &'a [u8],
    ) -> BoxFuture<'a, Result<(), VaultError>>;
}

/// Deterministic in-memory [`SecretBackend`] for fast unit tests, including simulated lock states.
#[derive(Debug, Default)]
pub struct MemorySecretBackend {
    inner: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    locked: bool,
    items: HashMap<(String, String), Vec<u8>>,
}

impl MemorySecretBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulates locking/unlocking the keyring (when locked, reads and writes fail with `VaultError::Locked`).
    pub fn set_locked(&self, locked: bool) {
        self.inner.lock().locked = locked;
    }

    pub fn is_locked(&self) -> bool {
        self.inner.lock().locked
    }
}

impl SecretBackend for MemorySecretBackend {
    fn get_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, VaultError>> {
        Box::pin(async move {
            let guard = self.inner.lock();
            if guard.locked {
                return Err(VaultError::Locked("memory keyring is locked".into()));
            }
            Ok(guard
                .items
                .get(&(service.to_string(), account.to_string()))
                .cloned())
        })
    }

    fn set_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
        secret: &'a [u8],
    ) -> BoxFuture<'a, Result<(), VaultError>> {
        let data = secret.to_vec();
        Box::pin(async move {
            let mut guard = self.inner.lock();
            if guard.locked {
                return Err(VaultError::Locked("memory keyring is locked".into()));
            }
            guard
                .items
                .insert((service.to_string(), account.to_string()), data);
            Ok(())
        })
    }
}

impl<T: SecretBackend + ?Sized> SecretBackend for std::sync::Arc<T> {
    fn get_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, VaultError>> {
        (**self).get_secret(service, account)
    }

    fn set_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
        secret: &'a [u8],
    ) -> BoxFuture<'a, Result<(), VaultError>> {
        (**self).set_secret(service, account, secret)
    }
}

/// Production Linux [`SecretBackend`] backed by `oo7::Keyring` (`org.freedesktop.secrets`),
/// with fallback to a `0600` local keyfile under `<base_dir>/keys/` **only** when D-Bus
/// Secret Service is completely unavailable (e.g. headless environment without a keyring daemon).
#[derive(Debug, Clone)]
pub struct LinuxSecretBackend {
    keys_dir: PathBuf,
    profile_slug: String,
}

fn normalize_vault_profile_slug(raw: &str) -> String {
    sanitize_profile_slug(raw).unwrap_or_else(|_| "default".to_string())
}

fn format_profile_scoped_account(profile_slug: &str, account: &str) -> String {
    if profile_slug.is_empty() || profile_slug == "default" {
        return account.to_string();
    }
    let prefix = format!("profile.{profile_slug}.");
    if account.starts_with(&prefix) {
        account.to_string()
    } else {
        format!("{prefix}{account}")
    }
}

impl LinuxSecretBackend {
    /// Creates a [`LinuxSecretBackend`] scoped to `profile_slug`.
    ///
    /// When `profile_slug == "default"`, D-Bus Secret Service `"account"` attributes remain
    /// `"zprotocol.<id>"` (100% backward-compatible). For non-default profiles, attributes
    /// are scoped as `"profile.<slug>.zprotocol.<id>"`.
    pub fn with_profile(keys_dir: impl Into<PathBuf>, profile_slug: impl Into<String>) -> Self {
        let raw_dir = keys_dir.into();
        let resolved_keys_dir = if raw_dir.file_name().is_some_and(|n| n == "keys") {
            raw_dir
        } else {
            raw_dir.join("keys")
        };
        let raw_slug = profile_slug.into();
        Self {
            keys_dir: resolved_keys_dir,
            profile_slug: normalize_vault_profile_slug(&raw_slug),
        }
    }

    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self::with_profile(base_dir, "default")
    }

    pub fn from_platform(platform: &ZaloPlatform) -> Self {
        Self::with_profile(platform.base_dir(), platform.profile_slug())
    }

    pub fn profile_slug(&self) -> &str {
        &self.profile_slug
    }

    /// Formats `account` (`"zprotocol.<id>"`) with profile scoping:
    /// `"zprotocol.<id>"` for `"default"` and `"profile.<slug>.zprotocol.<id>"` otherwise.
    pub fn scoped_account(&self, account: &str) -> String {
        format_profile_scoped_account(&self.profile_slug, account)
    }

    fn keyfile_path(&self, account: &str) -> PathBuf {
        let safe: String = account
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = if safe.is_empty() {
            "zprotocol.default".to_string()
        } else {
            safe
        };
        self.keys_dir.join(format!("{name}.key"))
    }

    fn read_keyfile(&self, account: &str) -> Result<Option<Vec<u8>>, VaultError> {
        let path = self.keyfile_path(account);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(VaultError::Backend(format!(
                "failed to read keyfile {}: {e}",
                path.display()
            ))),
        }
    }

    fn write_keyfile(&self, account: &str, secret: &[u8]) -> Result<(), VaultError> {
        let path = self.keyfile_path(account);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            if !self.keys_dir.exists() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&self.keys_dir)
                    .map_err(|e| {
                        VaultError::Backend(format!(
                            "failed to create keys dir {}: {e}",
                            self.keys_dir.display()
                        ))
                    })?;
            }
            let _ = std::fs::set_permissions(&self.keys_dir, std::fs::Permissions::from_mode(0o700));
            let mut tmp = NamedTempFile::new_in(&self.keys_dir).map_err(|e| {
                VaultError::Backend(format!(
                    "failed to create temp keyfile in {}: {e}",
                    self.keys_dir.display()
                ))
            })?;
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600)).map_err(
                |e| VaultError::Backend(format!("failed to chmod 0600 temp keyfile: {e}")),
            )?;
            tmp.write_all(secret)
                .and_then(|_| tmp.flush())
                .map_err(|e| VaultError::Backend(format!("failed to write keyfile: {e}")))?;
            tmp.persist(&path).map_err(|e| {
                VaultError::Backend(format!(
                    "failed to persist keyfile {}: {}",
                    path.display(),
                    e.error
                ))
            })?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&self.keys_dir)
                .and_then(|_| std::fs::write(&path, secret))
                .map_err(|e| VaultError::Backend(e.to_string()))
        }
    }
}

/// Classifies whether an error from `oo7::Keyring::new()` means D-Bus Secret Service
/// is genuinely absent (safe to fall back to `0600` local keyfile) vs a locked/auth/IO failure.
fn is_secret_service_unavailable(err: &oo7::Error) -> bool {
    match err {
        oo7::Error::DBus(dbus_err) => match dbus_err {
            oo7::dbus::Error::NotFound(_) => true,
            oo7::dbus::Error::ZBus(zerr) => match zerr {
                oo7::zbus::Error::Address(_)
                | oo7::zbus::Error::Connection(_, _)
                | oo7::zbus::Error::InputOutput(_)
                | oo7::zbus::Error::Handshake(_)
                | oo7::zbus::Error::InterfaceNotFound => true,
                oo7::zbus::Error::MethodError(name, _, _) => matches!(
                    name.as_str(),
                    "org.freedesktop.DBus.Error.ServiceUnknown"
                        | "org.freedesktop.DBus.Error.NameHasNoOwner"
                        | "org.freedesktop.DBus.Error.Spawn.Failed"
                        | "org.freedesktop.DBus.Error.Spawn.ExecFailed"
                        | "org.freedesktop.DBus.Error.Spawn.ChildExited"
                        | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
                ),
                oo7::zbus::Error::FDO(fdo) => matches!(
                    &**fdo,
                    oo7::zbus::fdo::Error::ServiceUnknown(_)
                        | oo7::zbus::fdo::Error::NameHasNoOwner(_)
                        | oo7::zbus::fdo::Error::SpawnFailed(_)
                        | oo7::zbus::fdo::Error::SpawnExecFailed(_)
                        | oo7::zbus::fdo::Error::SpawnChildExited(_)
                        | oo7::zbus::fdo::Error::SpawnServiceNotFound(_)
                ),
                _ => false,
            },
            _ => false,
        },
        oo7::Error::File(file_err) => matches!(file_err, oo7::file::Error::Portal(_)),
    }
}

fn map_oo7_active_error(err: oo7::Error) -> VaultError {
    match &err {
        oo7::Error::DBus(oo7::dbus::Error::Dismissed)
        | oo7::Error::DBus(oo7::dbus::Error::Service(oo7::dbus::ServiceError::IsLocked(_))) => {
            VaultError::Locked(err.to_string())
        }
        _ => VaultError::Backend(err.to_string()),
    }
}

async fn ensure_keyring_unlocked(keyring: &oo7::Keyring) -> Result<(), VaultError> {
    if let oo7::Keyring::DBus(collection) = keyring {
        if collection
            .is_locked()
            .await
            .map_err(|e| map_oo7_active_error(oo7::Error::DBus(e)))?
        {
            keyring.unlock().await.map_err(map_oo7_active_error)?;
            if collection
                .is_locked()
                .await
                .map_err(|e| map_oo7_active_error(oo7::Error::DBus(e)))?
            {
                return Err(VaultError::Locked(
                    "Secret Service collection remains locked after unlock request".into(),
                ));
            }
        }
    }
    Ok(())
}

impl SecretBackend for LinuxSecretBackend {
    fn get_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, VaultError>> {
        Box::pin(async move {
            let scoped = self.scoped_account(account);
            let keyring = match oo7::Keyring::new().await {
                Ok(kr) => kr,
                Err(err) if is_secret_service_unavailable(&err) => {
                    tracing::debug!(%err, "Secret Service unavailable; using 0600 local keyfile");
                    return self.read_keyfile(&scoped);
                }
                Err(err) => return Err(map_oo7_active_error(err)),
            };

            // CRITICAL: `Collection::search_items` returns empty if the collection is locked!
            // Always unlock and verify unlocked before trusting an empty search result.
            ensure_keyring_unlocked(&keyring).await?;

            let attrs = HashMap::from([("service", service), ("account", scoped.as_str())]);
            let items = keyring
                .search_items(&attrs)
                .await
                .map_err(map_oo7_active_error)?;

            let Some(item) = items.into_iter().next() else {
                return Ok(None);
            };

            if item.is_locked().await.map_err(map_oo7_active_error)? {
                item.unlock().await.map_err(map_oo7_active_error)?;
                if item.is_locked().await.map_err(map_oo7_active_error)? {
                    return Err(VaultError::Locked(format!(
                        "Keyring item '{scoped}' remains locked after unlock request"
                    )));
                }
            }

            let secret = item.secret().await.map_err(map_oo7_active_error)?;
            Ok(Some(secret.as_bytes().to_vec()))
        })
    }

    fn set_secret<'a>(
        &'a self,
        service: &'a str,
        account: &'a str,
        secret: &'a [u8],
    ) -> BoxFuture<'a, Result<(), VaultError>> {
        Box::pin(async move {
            let scoped = self.scoped_account(account);
            let keyring = match oo7::Keyring::new().await {
                Ok(kr) => kr,
                Err(err) if is_secret_service_unavailable(&err) => {
                    tracing::debug!(%err, "Secret Service unavailable; writing 0600 local keyfile");
                    return self.write_keyfile(&scoped, secret);
                }
                Err(err) => return Err(map_oo7_active_error(err)),
            };

            ensure_keyring_unlocked(&keyring).await?;

            let attrs = HashMap::from([("service", service), ("account", scoped.as_str())]);
            let label = format!("Olaz ({scoped})");
            keyring
                .create_item(&label, &attrs, oo7::Secret::blob(secret), true)
                .await
                .map_err(map_oo7_active_error)
        })
    }
}

// ============================================================================
// KeychainCipher (`zproto-encrypt-with-keychain` / `zproto-decrypt-with-keychain`)
// ============================================================================

/// Trusted-device (`zprotocol`) envelope cipher:
/// `[0x49 || iv(16) || salt(16) || ct || hmac_sha256(32)]` (`>= 65` bytes).
#[derive(Debug)]
pub struct KeychainCipher<B: SecretBackend = LinuxSecretBackend> {
    backend: B,
    profile_slug: String,
}

impl KeychainCipher<LinuxSecretBackend> {
    pub fn with_profile(keys_dir: impl Into<PathBuf>, profile_slug: impl Into<String>) -> Self {
        let slug = profile_slug.into();
        let backend = LinuxSecretBackend::with_profile(keys_dir, slug.clone());
        Self::new_with_profile(backend, slug)
    }

    pub fn for_platform(platform: &ZaloPlatform) -> Self {
        Self::with_profile(platform.base_dir(), platform.profile_slug())
    }
}

impl<B: SecretBackend> KeychainCipher<B> {
    pub const SERVICE: &'static str = "app.olaz.secure";
    pub const KDF_INFO: &'static [u8] = b"ZprotocolSecureKDF";
    pub const TYPE_RAW: u8 = 0x49;
    pub const MIN_ENVELOPE_LEN: usize = 1 + 16 + 16 + 32; // 65 bytes

    pub fn new(backend: B) -> Self {
        Self::new_with_profile(backend, "default")
    }

    pub fn new_with_profile(backend: B, profile_slug: impl Into<String>) -> Self {
        let raw_slug = profile_slug.into();
        Self {
            backend,
            profile_slug: normalize_vault_profile_slug(&raw_slug),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn profile_slug(&self) -> &str {
        &self.profile_slug
    }

    /// Formats the D-Bus Secret Service `"account"` attribute for `identifier`:
    /// `"zprotocol.<id>"` when `profile_slug == "default"`, and
    /// `"profile.<slug>.zprotocol.<id>"` for non-default profiles.
    pub fn account_for_identifier(&self, identifier: &str) -> String {
        let base = format!("zprotocol.{}", identifier.to_ascii_lowercase());
        format_profile_scoped_account(&self.profile_slug, &base)
    }

    /// Encrypts each plaintext item under the per-identifier 32-byte root key (minting and
    /// persisting a fresh root key **only** if `backend.get_secret` returns `Ok(None)`).
    pub async fn encrypt<T: AsRef<[u8]>>(
        &self,
        identifier: &str,
        items: &[T],
    ) -> Result<Vec<Vec<u8>>, VaultError> {
        let root = self.root_key(identifier, true).await?;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let mut iv = [0u8; 16];
            let mut salt = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut iv);
            rand::thread_rng().fill_bytes(&mut salt);

            let (enc_key, mac_key) = Self::derive_keys(&root, &salt)?;
            let ct = aes256_cbc_pkcs7(item.as_ref(), &enc_key, &iv, true)
                .map_err(VaultError::Cipher)?;
            let tag = Self::compute_mac(&mac_key, &ct, &iv, &salt);

            let mut sealed = Vec::with_capacity(1 + 16 + 16 + ct.len() + 32);
            sealed.push(Self::TYPE_RAW);
            sealed.extend_from_slice(&iv);
            sealed.extend_from_slice(&salt);
            sealed.extend_from_slice(&ct);
            sealed.extend_from_slice(&tag);
            out.push(sealed);
        }
        Ok(out)
    }

    /// Decrypts and authenticates each envelope item under the stored 32-byte root key.
    pub async fn decrypt<T: AsRef<[u8]>>(
        &self,
        identifier: &str,
        items: &[T],
    ) -> Result<Vec<Vec<u8>>, VaultError> {
        let root = self.root_key(identifier, false).await?;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let sealed = item.as_ref();
            if sealed.len() < Self::MIN_ENVELOPE_LEN {
                return Err(VaultError::InvalidLength);
            }
            if sealed[0] != Self::TYPE_RAW {
                return Err(VaultError::InvalidHeader);
            }

            let mut iv = [0u8; 16];
            let mut salt = [0u8; 16];
            iv.copy_from_slice(&sealed[1..17]);
            salt.copy_from_slice(&sealed[17..33]);
            let ct = &sealed[33..sealed.len() - 32];
            let tag = &sealed[sealed.len() - 32..];

            let (enc_key, mac_key) = Self::derive_keys(&root, &salt)?;
            let expected_tag = Self::compute_mac(&mac_key, ct, &iv, &salt);
            if !bool::from(expected_tag.ct_eq(tag)) {
                return Err(VaultError::InvalidMac);
            }

            let plain =
                aes256_cbc_pkcs7(ct, &enc_key, &iv, false).map_err(VaultError::Cipher)?;
            out.push(plain);
        }
        Ok(out)
    }

    /// JSON wire adapter for IPC channel `zproto-encrypt-with-keychain`.
    pub async fn encrypt_wire(&self, args: &[Value]) -> Result<Value, String> {
        let id = args.first().and_then(Value::as_str).unwrap_or("");
        let items = extract_wire_items(args.get(1));
        let encrypted = self.encrypt(id, &items).await.map_err(|e| e.to_string())?;
        Ok(Value::Array(
            encrypted.iter().map(|b| wire::encode_bytes(b)).collect(),
        ))
    }

    /// JSON wire adapter for IPC channel `zproto-decrypt-with-keychain`.
    pub async fn decrypt_wire(&self, args: &[Value]) -> Result<Value, String> {
        let id = args.first().and_then(Value::as_str).unwrap_or("");
        let items = extract_wire_items(args.get(1));
        let decrypted = self.decrypt(id, &items).await.map_err(|e| e.to_string())?;
        Ok(Value::Array(
            decrypted.iter().map(|b| wire::encode_bytes(b)).collect(),
        ))
    }

    fn derive_keys(root: &[u8], salt: &[u8; 16]) -> Result<([u8; 32], [u8; 32]), VaultError> {
        let hk = Hkdf::<Sha256>::new(Some(salt), root);
        let mut okm = [0u8; 64];
        hk.expand(Self::KDF_INFO, &mut okm)
            .map_err(|e| VaultError::Cipher(e.to_string()))?;
        let mut enc_key = [0u8; 32];
        let mut mac_key = [0u8; 32];
        enc_key.copy_from_slice(&okm[0..32]);
        mac_key.copy_from_slice(&okm[32..64]);
        Ok((enc_key, mac_key))
    }

    fn compute_mac(mac_key: &[u8; 32], ct: &[u8], iv: &[u8; 16], salt: &[u8; 16]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(mac_key).expect("32-byte HMAC key is valid");
        mac.update(ct);
        mac.update(iv);
        mac.update(salt);
        let bytes = mac.finalize().into_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    }

    async fn root_key(&self, identifier: &str, create: bool) -> Result<Vec<u8>, VaultError> {
        let account = self.account_for_identifier(identifier);
        // CRITICAL: Any error from `get_secret` (such as a locked keyring) propagates directly
        // without minting a new key so previously sealed credentials are never orphaned!
        if let Some(existing) = self.backend.get_secret(Self::SERVICE, &account).await? {
            if existing.len() == 32 {
                return Ok(existing);
            }
            return Err(VaultError::Backend(format!(
                "corrupt root key length for '{account}': {} bytes",
                existing.len()
            )));
        }

        if !create {
            return Err(VaultError::NotFound(identifier.to_string()));
        }

        let mut fresh = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut fresh);
        self.backend
            .set_secret(Self::SERVICE, &account, &fresh)
            .await?;
        Ok(fresh)
    }
}

fn extract_wire_items(val: Option<&Value>) -> Vec<Vec<u8>> {
    val.and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(wire::decode_bytes).collect())
        .unwrap_or_default()
}
