//! Olaz Web (`olaz-web`) — Zero-Server-Persistence Self-Hosted Web Relay & Browser Host Runtime.
//!
//! Architecture Invariant:
//! - **100% Browser-Only User Persistence**: All Zalo sessions (`zpw_sek`), SafeStorage & Keychain
//!   vault secrets, E2EE keys, virtual filesystem blobs, and IndexedDB message history reside
//!   strictly inside the user's browser (`IndexedDB` + `localStorage` + `OPFS`).
//! - **Zero Server State**: The Rust server holds zero cookie jars (`cookie_store` disabled),
//!   writes zero files or temp files, logs zero headers/cookies/bodies, and runs cleanly inside
//!   a `chmod 0555` or `ReadOnlyPaths=/` (`--read-only`) container/systemd sandbox.

pub mod allowlist;
pub mod auth;
pub mod bundle;
pub mod relay;
pub mod server;

pub use allowlist::{
    is_allowed_zalo_domain, is_globally_routable_ip, AllowlistConfig, AllowlistError,
    AllowlistValidator, ALLOWED_ZALO_SUFFIXES,
};
pub use auth::{AuthGate, GATE_COOKIE_NAME, GATE_COOKIE_TTL_SECS, UNLOCK_PAGE_HTML};
pub use bundle::{
    transform_bundle_asset, transform_bundle_asset_opt, transform_zalo_html,
    web_bridge_spec_script, AssetBytes, BundleProvider, ServedAsset, BOOT_HTML, CRYPTO_SYNC_JS,
    PRELOAD_JS, SW_JS,
};
pub use relay::{
    PrefixedTcpStream, StatelessRelay, DEFAULT_WS_ORIGIN, ZALO_APP_VERSION, ZALO_CLIENT_TYPE,
    ZALO_CLIENT_VERSION, ZALO_USER_AGENT,
};
pub use server::{start_web_server, WebServerConfig, WebServerHandle};
