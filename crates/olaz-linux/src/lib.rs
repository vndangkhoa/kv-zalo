//! Linux desktop integration (Omarchy & Ubuntu) and `ZaloEngine` state management for Olaz.
//!
//! Modules:
//! - [`omarchy`]: Omarchy `colors.toml` theme loading, WCAG relative-luminance contrast, Hyprland
//!   window rounding (`hyprctl`), `~/.config/omarchy/language` watcher with symlink-swap re-arming,
//!   and WebKit2GTK Wayland/NVIDIA startup configuration.
//! - [`portals`]: XDG Desktop Portal `FileChooser` (`OpenFile`/`SaveFile`) and interactive
//!   `Screenshot` capture via `ashpd` (`zbus` + `tokio`).
//! - [`notifications`]: `NativeNotifier.swift` suppression & PIN-locked hidden-chat privacy policy,
//!   `org.freedesktop.Notifications` banners, `rodio` MP3 chimes, and `com.canonical.Unity.LauncherEntry`
//!   unread dock badges.
//! - [`tray`]: Pure-Rust D-Bus `StatusNotifierItem` tray via `ksni` (`0.3`) with vector/SDF ARGB32
//!   pixmaps and unread dot indicator.
//! - [`call_stub`]: Byte-exact `CallTransport.swift` wire transport (`AES-128-CBC` PKCS#7,
//!   `CallChunker`, `CallFrameDecoder`) and Phase 1 `LinuxCallCoordinator`.
//! - [`engine`]: Rich `ZaloEngineState`, `zstate` reducer, `merge_chat`, `ComposerDraft` /
//!   `FailedDraftStore` UTF-16 <-> UTF-8 mention shifting, `FileCard::may_execute`, and `fold_vietnamese`.

pub mod api_mcp;
pub mod automation;
pub mod autostart;
pub mod call_stub;
pub mod engine;
pub mod instance;
pub mod notifications;
pub mod omarchy;
pub mod portals;
pub mod privacy;
pub mod tray;
pub mod updater;

pub use api_mcp::{
    dispatch_control_rpc, handle_http_rest_request, handle_mcp_jsonrpc, make_instance_rpc_handler,
    mcp_tools_catalog, resolve_api_token, run_local_api_server, run_mcp_io, run_mcp_stdio,
    spawn_local_api_server, spawn_local_api_server_with_token, EngineControlBackend,
    LocalApiServerHandle, OlazControlBackend, SocketRpcBackend, StartCallCallback,
    DEFAULT_API_PORT, MCP_PROTOCOL_VERSION,
};
pub use automation::{
    dispatch_due_scheduled_once, parse_schedule_time_ms, spawn_automation_loop, unix_now_ms,
    AutoReplyEngine, AutoReplyRule, AutomationLoopHandle, MatchMode, ScheduleStatus,
    ScheduledMessage, SchedulerEngine, SendMessageCallback, AUTOREPLY_RULES_FILENAME,
    DEFAULT_AUTOMATION_TICK, SCHEDULED_MESSAGES_FILENAME,
};
pub use autostart::{
    format_autostart_desktop_entry, format_desktop_exec_arg, parse_existing_desktop_entry,
    reload_user_systemd_nonblocking, resolve_olaz_executable_path, AutostartManager,
    AutostartState, AUTOSTART_DESKTOP_FILENAME, AUTOSTART_STATE_FILENAME,
};
pub use instance::{
    acquire_or_notify_for_profile, acquire_or_notify_in_dir, acquire_or_notify_primary,
    default_instance_base_dir, instance_socket_path, instance_socket_path_for_profile,
    instance_socket_path_in_dir, sanitize_instance_profile, send_instance_rpc,
    send_instance_rpc_in_dir, send_instance_rpc_to_socket, send_instance_rpc_to_socket_async,
    AcquireInstanceOutcome, AcquireProfileInstanceOutcome, InstanceEvent, InstanceRpcHandle,
    ProfileInstanceEvent, RpcHandler, SingleInstanceGuard,
};
pub use privacy::{PrivacyConfig, PRIVACY_CONFIG_FILENAME};
pub use updater::{
    check_for_updates, check_for_updates_from_urls, compute_sha256_hex,
    download_and_verify_artifact, download_release, install_release, is_newer_semver,
    parse_release_manifest, parse_sha256sums_for_asset, select_platform_asset_score,
    stage_portable_release, validate_asset_name, verify_sha256_bytes, DeferredPortableInstall,
    DownloadLimits, DownloadRequest, InstallTarget, ReleaseSource, ReleaseUpdateInfo,
    CURRENT_OLAZ_VERSION, DEFAULT_REPO_SLUG, FORGEJO_LATEST_RELEASE_URL, GITHUB_LATEST_RELEASE_URL,
};

pub use call_stub::{
    CallChunkReassembler, CallChunker, CallFrameDecoder, CallTransportCrypto, CallTransportError,
    IncomingVoipCall, LinuxCallCoordinator, OutgoingCallStatus, CALL_V2_KEY_BASE64,
    LINUX_CALL_UNSUPPORTED_MESSAGE, ZERO_IV,
};
pub use engine::{
    byte_idx_to_utf16_offset, conv_action_flags, fold_vietnamese, merge_chat, utf16_len,
    utf16_offset_to_byte_idx, utf16_slice, CardAttachment, ChatMergeOutcome, ChatMessage,
    ComposerDraft, ConvActionsState, Conversation, ConversationExtras, ConversationLabel,
    ConversationPreview, EnginePhase, FailedDraftStore, FileCard, MediaAttachment, Mention,
    MessageQuote, MessageReaction, MessageStatus, PollAttachment, StickerAttachment, StrangerBox,
    TransferSlice, UserProfile, WaitingConfirm, ZaloEngineState, DEFAULT_KICKOUT_NOTICE,
};
pub use notifications::{
    build_unity_launcher_signal, LinuxNotifier, NotificationEvent, NotificationIcon,
    NotificationPolicy, NotificationRequest, RedactedNotification, DEFAULT_IDLE_THRESHOLD_SECS,
    HIDDEN_PREVIEW_TEXT, UNITY_DESKTOP_URI, UNITY_LAUNCHER_INTERFACE, UNITY_LAUNCHER_MEMBER,
    UNITY_LAUNCHER_OBJECT_PATH,
};
pub use omarchy::{
    compute_accent_fg, compute_linux_gst_video_sink_overrides, compute_linux_webview_env_overrides,
    configure_linux_webview_env, ensure_olaz_gst_plugin_staged, has_gstreamer_audio_sink,
    load_omarchy_theme, load_omarchy_theme_with, olaz_gst_plugin_dir, parse_hex_rgb,
    query_hyprland_rounding_with, relative_luminance, resolve_language, spawn_omarchy_watcher,
    spawn_omarchy_watcher_with, system_has_nvidia_driver, DesktopTheme, OmarchyWatcherHandle,
    ACCENT_FG_DARK, ACCENT_FG_LIGHT, HYPRCTL_TIMEOUT, WATCHER_DEBOUNCE, ZALO_ACCENT,
};
pub use portals::{
    build_media_file_filter, capture_interactive_screenshot, pick_open_dialog, pick_open_directory,
    pick_open_files, pick_save_file, pick_save_file_with_dir, prune_old_screenshots,
    stage_screenshot_file, str_uri_to_local_path, uri_to_local_path, SCREENSHOT_RETENTION,
    SCREENSHOT_STAGING_DIR,
};
pub use tray::{
    render_tray_icon_argb32, render_tray_pixmaps, spawn_tray, OlazTray, OlazTrayEvent,
    OlazTrayHandle, LABEL_QUIT, LABEL_SCREENSHOT, LABEL_SHOW_OLAZ, LABEL_STATUS_BUSY,
    LABEL_STATUS_MENU, LABEL_STATUS_ONLINE,
};
