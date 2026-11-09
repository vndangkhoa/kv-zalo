//! Core runtime, zero-extraction ASAR archive reader, userspace DMG installer,
//! Node.js `$znode.crypto` parity, credential vaults, sandboxed filesystem bridge,
//! and host script / ASAR bundle patchers for Olaz.

pub mod asar;
pub mod bundle_patcher;
pub mod crypto;
pub mod fs_bridge;
pub mod installer;
pub mod platform;
pub mod vault;
pub mod wire;

pub use asar::{mime_for_path, AsarArchive, AsarEntry};
pub use bundle_patcher::{
    bridge_ipc_table, bridge_spec_script, direct_ui_user_scripts, expose_webpack_require,
    host_script, ordered_user_scripts, patch_asar_bundle, patch_sync_global_id_resolver,
    patch_zalo_html_csp, patch_zalo_linux_renderer_bundle, popup_user_scripts, worker_prelude,
    IpcSpecEntry,
};
pub use crypto::CryptoBridge;
pub use fs_bridge::{copy_with_noise_mode, generate_video_thumbnail_sync, FsBridge, FsError};
pub use installer::{resolve_host_doh, InstallProgress, ZaloRelease, ZaloRuntimeManager};
pub use platform::{resolve_xdg_autostart_dir, ClientIdentity, ZaloPlatform};
pub use vault::{
    KeychainCipher, LinuxSecretBackend, MemorySecretBackend, SafeStorage, SecretBackend, VaultError,
};
pub use wire::WireStats;
