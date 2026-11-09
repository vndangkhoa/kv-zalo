//! Core runtime, zero-extraction ASAR archive reader, userspace DMG installer,
//! Node.js `$znode.crypto` parity, credential vaults, sandboxed filesystem bridge,
//! and host script / ASAR bundle patchers for Olaz.

pub mod asar;
pub mod bundle_patcher;
pub mod crypto;
pub mod db_cross;
pub mod file_utilities;
pub mod fs_bridge;
pub mod installer;
pub mod platform;
pub mod profiles;
pub mod vault;
pub mod wire;
pub mod zwalker;

pub use asar::{mime_for_path, AsarArchive, AsarEntry};
pub use bundle_patcher::{
    bridge_ipc_table, bridge_spec_script, direct_ui_user_scripts, expose_webpack_require,
    host_script, ordered_user_scripts, patch_asar_bundle, patch_sync_global_id_resolver,
    patch_zalo_html_csp, patch_zalo_linux_renderer_bundle, popup_user_scripts, worker_prelude,
    IpcSpecEntry,
};
pub use crypto::CryptoBridge;
pub use db_cross::{
    decompress_and_decrypt_db_v1, decompress_and_decrypt_db_v2, derive_zdb4_key,
    dispatch_db_utils_async, dispatch_db_utils_sync, parse_bin_net,
};
pub use file_utilities::{
    cancel_job, detect_filesystem, detect_hardlinks_in_scope, detect_hardlinks_info,
    dispatch_file_utilities_async, dispatch_file_utilities_sync, get_directory_size,
    get_directory_size_by_glob, get_directory_size_tree, get_directory_size_with_options,
    register_job, unregister_job, DirectorySizeResult, DirectoryTreeNode, FilesystemInfo,
    HardlinkInfo,
};
pub use fs_bridge::{
    copy_with_noise_mode, file_utils_get_disk_usage, generate_video_thumbnail_sync,
    zfile_get_disk_info, zfile_get_info, BridgeResult, FsBridge, FsError, PlatformPaths,
};
pub use installer::{resolve_host_doh, InstallProgress, ZaloRelease, ZaloRuntimeManager};
pub use platform::{resolve_xdg_autostart_dir, ClientIdentity, ZaloPlatform};
pub use profiles::{
    ProfileMetadata, ProfileRegistry, PROFILES_REGISTRY_FILENAME, PROFILES_REGISTRY_VERSION,
};
pub use vault::{
    KeychainCipher, LinuxSecretBackend, MemorySecretBackend, SafeStorage, SecretBackend, VaultError,
};
pub use wire::WireStats;
pub use zwalker::{
    delete_empty_folders, delete_homeless_files, handle_zwalker_ipc, handle_zwalker_ipc_with_state,
    scan_directory, stat_unmarked_files, update_reference_message_id, DeleteResult,
    EmptyFolderResult, FolderBasicInfo, ReferenceUpdate, ScanResult, StatResult, TreeState,
    UpdateResult, ZWalkerState,
};
