//! Comprehensive integration & unit tests for `olaz-linux`:
//! Omarchy theme/watcher, XDG portals helpers, notification policy & Unity badge D-Bus signal,
//! `ksni` StatusNotifierItem tray, `zcall-v2` wire transport & Phase 1 coordinator, and
//! `ZaloEngineState` (`merge_chat`, UTF-16 `ComposerDraft` / `FailedDraftStore`, `fold_vietnamese`,
//! `ConvActionsState` 60s auto-relock, `FileCard::may_execute`, and `apply_zstate_patch`).

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs as unix_fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use ksni::{MenuItem, Tray};
use serde_json::json;
use tempfile::TempDir;

use olaz_linux::call_stub::{
    CallChunkReassembler, CallChunker, CallFrameDecoder, CallTransportCrypto, CallTransportError,
    LinuxCallCoordinator, CALL_V2_KEY_BASE64, LINUX_CALL_UNSUPPORTED_MESSAGE,
};
use olaz_linux::engine::{
    byte_idx_to_utf16_offset, conv_action_flags, fold_vietnamese, merge_chat, utf16_len,
    utf16_offset_to_byte_idx, utf16_slice, ChatMessage, ComposerDraft, EnginePhase,
    FailedDraftStore, FileCard, Mention, MessageStatus, WaitingConfirm, ZaloEngineState,
    DEFAULT_KICKOUT_NOTICE,
};
use olaz_linux::notifications::{
    build_unity_launcher_signal, LinuxNotifier, NotificationIcon, NotificationPolicy,
    NotificationRequest, HIDDEN_PREVIEW_TEXT, UNITY_DESKTOP_URI, UNITY_LAUNCHER_INTERFACE,
    UNITY_LAUNCHER_MEMBER, UNITY_LAUNCHER_OBJECT_PATH,
};
use olaz_linux::omarchy::{
    compute_accent_fg, compute_linux_webview_env_overrides, load_omarchy_theme_with, parse_hex_rgb,
    query_hyprland_rounding_with, relative_luminance, resolve_language, spawn_omarchy_watcher_with,
    ACCENT_FG_DARK, ACCENT_FG_LIGHT, ZALO_ACCENT,
};
use olaz_linux::portals::{
    build_media_file_filter, prune_old_screenshots, stage_screenshot_file, str_uri_to_local_path,
    uri_to_local_path,
};
use olaz_linux::tray::{
    render_tray_icon_argb32, render_tray_pixmaps, OlazTray, OlazTrayEvent, PrivacyField,
    TrayPrivacyState, LABEL_BLOCK_DELIVERED, LABEL_BLOCK_SEEN, LABEL_BLOCK_TYPING,
    LABEL_NEW_PROFILE, LABEL_PRIVACY_MENU, LABEL_QUIT, LABEL_SCREENSHOT, LABEL_SHOW_OLAZ,
    LABEL_STATUS_BUSY, LABEL_STATUS_MENU, LABEL_STATUS_ONLINE,
};

// ============================================================================
// 1. Omarchy Theme, Rounding, Language, Watcher & WebView Env Tests
// ============================================================================

#[test]
fn desktop_theme_wcag_luminance_and_accent_fg() {
    assert_eq!(parse_hex_rgb("#0068FF"), Some((0, 104, 255)));
    assert_eq!(parse_hex_rgb("FFD60A"), Some((255, 214, 10)));
    assert_eq!(parse_hex_rgb("#fff"), Some((255, 255, 255)));
    assert_eq!(parse_hex_rgb("#12"), None);

    // Zalo blue (#0068FF) has relative luminance ~0.145 <= 0.179 -> white foreground.
    let zalo_lum = relative_luminance(ZALO_ACCENT).expect("valid luminance");
    assert!(zalo_lum < 0.179, "expected #0068FF luminance < 0.179, got {zalo_lum}");
    assert_eq!(compute_accent_fg(ZALO_ACCENT), ACCENT_FG_LIGHT);
    assert_eq!(compute_accent_fg("#000000"), ACCENT_FG_LIGHT);
    assert_eq!(compute_accent_fg("#1E1E2E"), ACCENT_FG_LIGHT);

    // High-luminance accents (yellow, mint, white) -> dark navy foreground (#081B3A).
    let yellow_lum = relative_luminance("#FFD60A").expect("valid luminance");
    assert!(yellow_lum > 0.179, "expected #FFD60A luminance > 0.179, got {yellow_lum}");
    assert_eq!(compute_accent_fg("#FFD60A"), ACCENT_FG_DARK);
    assert_eq!(compute_accent_fg("#FFFFFF"), ACCENT_FG_DARK);
    assert_eq!(compute_accent_fg("#A6E3A1"), ACCENT_FG_DARK);
}

#[test]
fn load_omarchy_theme_ubuntu_fallback_and_colors_toml_parsing() {
    let tmp = TempDir::new().unwrap();
    let missing_state = tmp.path().join("nonexistent-omarchy-state");
    let missing_cfg = tmp.path().join("nonexistent-omarchy-cfg");

    // Non-Omarchy Ubuntu GNOME environment: no colors.toml, no Hyprland sig, Vietnamese $LANG.
    let fallback = load_omarchy_theme_with(
        &missing_state,
        &missing_cfg,
        None,
        Path::new("/nonexistent/hyprctl"),
        Some("vi_VN.UTF-8"),
    );
    assert!(!fallback.is_omarchy);
    assert_eq!(fallback.mode, "dark");
    assert_eq!(fallback.accent, ZALO_ACCENT);
    assert_eq!(fallback.accent_fg, "#FFFFFF");
    assert_eq!(fallback.radius_px, 12);
    assert_eq!(fallback.language, "vi");

    // Omarchy light theme with custom colors.toml and language file.
    let state_dir = tmp.path().join("state/current");
    let theme_dir = state_dir.join("theme");
    let cfg_dir = tmp.path().join("config/omarchy");
    fs::create_dir_all(&theme_dir).unwrap();
    fs::create_dir_all(&cfg_dir).unwrap();

    fs::write(
        theme_dir.join("colors.toml"),
        r##"
mode = "light"
accent = "#FFD60A"
background = "#FAFAFA"
dark_background = "#F0F2F5"
darker_background = "#E5E7EB"
lighter_background = "#FFFFFF"
foreground = "#111827"
muted = "#6B7280"
selection = "#FEF08A"
red = "#DC2626"
green = "#16A34A"
yellow = "#CA8A04"
blue = "#2563EB"
"##,
    )
    .unwrap();
    fs::write(cfg_dir.join("language"), "pt-BR\n").unwrap();

    let omarchy_theme = load_omarchy_theme_with(
        &state_dir,
        &cfg_dir,
        Some("hypr_sig"),
        Path::new("/nonexistent/hyprctl"),
        Some("en_US.UTF-8"),
    );
    assert!(omarchy_theme.is_omarchy);
    assert_eq!(omarchy_theme.mode, "light");
    assert_eq!(omarchy_theme.accent, "#FFD60A");
    assert_eq!(omarchy_theme.accent_fg, "#081B3A");
    assert_eq!(omarchy_theme.background, "#FAFAFA");
    assert_eq!(omarchy_theme.radius_px, 0); // Hyprland default when hyprctl absent
    assert_eq!(omarchy_theme.language, "pt-BR");

    let css = omarchy_theme.to_css_vars();
    assert!(css.contains("--olaz-accent: #FFD60A;"));
    assert!(css.contains("--olaz-accent-fg: #081B3A;"));
    assert!(css.contains("--olaz-radius: 0px;"));
}

#[test]
fn hyprctl_rounding_parses_json_and_enforces_timeout() {
    let tmp = TempDir::new().unwrap();

    // 1. Fake hyprctl returning rounding = 14.
    let ok_script = tmp.path().join("fake-hyprctl-ok");
    fs::write(
        &ok_script,
        "#!/bin/sh\necho '{\"option\":\"decoration:rounding\",\"int\":14,\"set\":true}'\n",
    )
    .unwrap();
    fs::set_permissions(&ok_script, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        query_hyprland_rounding_with(Some("sig"), &ok_script, Duration::from_millis(500)),
        14
    );
    // Without HYPRLAND_INSTANCE_SIGNATURE, Ubuntu GNOME default (12) is used without running hyprctl.
    assert_eq!(
        query_hyprland_rounding_with(None, &ok_script, Duration::from_millis(500)),
        12
    );
    assert_eq!(
        query_hyprland_rounding_with(Some("   "), &ok_script, Duration::from_millis(500)),
        12
    );

    // 2. Hanging hyprctl must be killed at timeout and default to 0 on Hyprland.
    let hang_script = tmp.path().join("fake-hyprctl-hang");
    fs::write(&hang_script, "#!/bin/sh\nsleep 10\n").unwrap();
    fs::set_permissions(&hang_script, fs::Permissions::from_mode(0o755)).unwrap();

    let started = Instant::now();
    let radius =
        query_hyprland_rounding_with(Some("sig"), &hang_script, Duration::from_millis(120));
    assert_eq!(radius, 0);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "hanging hyprctl was not killed within timeout"
    );
}

#[test]
fn language_resolution_validates_regex_and_falls_back_to_env() {
    let tmp = TempDir::new().unwrap();
    let lang_file = tmp.path().join("language");

    fs::write(&lang_file, "vi\nignored-second-line\n").unwrap();
    assert_eq!(resolve_language(Some(&lang_file), Some("en_US.UTF-8")), "vi");

    fs::write(&lang_file, "zh-CN\n").unwrap();
    assert_eq!(resolve_language(Some(&lang_file), Some("vi_VN.UTF-8")), "zh-CN");

    // Invalid content rejected by `^[a-z]{2,3}(-[A-Za-z]{2})?$` -> falls back to $LANG.
    fs::write(&lang_file, "invalid_locale_string!\n").unwrap();
    assert_eq!(resolve_language(Some(&lang_file), Some("vi_VN.UTF-8")), "vi");
    assert_eq!(resolve_language(Some(&lang_file), Some("en_GB.UTF-8")), "en");
    assert_eq!(resolve_language(None, None), "en");
}

#[test]
fn webview_env_overrides_detects_wayland_plus_nvidia() {
    // 1. Wayland + NVIDIA + unset vars -> sets DMABUF=1, COMPOSITING_MODE=1, and GDK_BACKEND=wayland,x11.
    let overrides = compute_linux_webview_env_overrides(
        None,
        None,
        Some(OsStr::new("wayland-1")),
        None,
        None,
        true,
    );
    assert_eq!(
        overrides,
        vec![
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
            ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
            ("GDK_BACKEND", "wayland,x11"),
        ]
    );

    // 2. Wayland without NVIDIA (via WAYLAND_DISPLAY) -> sets DMABUF=1 and GDK_BACKEND=wayland,x11.
    let non_nvidia = compute_linux_webview_env_overrides(
        None,
        None,
        Some(OsStr::new("wayland-1")),
        None,
        None,
        false,
    );
    assert_eq!(
        non_nvidia,
        vec![
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
            ("GDK_BACKEND", "wayland,x11"),
        ]
    );

    // 3. Wayland detected via XDG_SESSION_TYPE="wayland" -> sets DMABUF=1 and GDK_BACKEND=wayland,x11.
    let xdg_wayland = compute_linux_webview_env_overrides(
        None,
        None,
        None,
        Some(OsStr::new("wayland")),
        None,
        false,
    );
    assert_eq!(
        xdg_wayland,
        vec![
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
            ("GDK_BACKEND", "wayland,x11"),
        ]
    );

    // 4. X11-only session -> only GDK_BACKEND=wayland,x11.
    let x11_only = compute_linux_webview_env_overrides(
        None,
        None,
        None,
        Some(OsStr::new("x11")),
        None,
        true,
    );
    assert_eq!(x11_only, vec![("GDK_BACKEND", "wayland,x11")]);

    // 5. Explicit user env vars are never clobbered.
    let user_set = compute_linux_webview_env_overrides(
        Some(OsStr::new("0")),
        Some(OsStr::new("0")),
        Some(OsStr::new("wayland-1")),
        Some(OsStr::new("wayland")),
        Some(OsStr::new("x11")),
        true,
    );
    assert!(user_set.is_empty());
}

#[test]
fn omarchy_watcher_rearms_across_atomic_symlink_swaps() {
    let tmp = TempDir::new().unwrap();
    let themes_repo = tmp.path().join("themes");
    let theme_a = themes_repo.join("tokyo-night");
    let theme_b = themes_repo.join("gruvbox-gold");
    let theme_c = themes_repo.join("catppuccin-mint");
    fs::create_dir_all(&theme_a).unwrap();
    fs::create_dir_all(&theme_b).unwrap();
    fs::create_dir_all(&theme_c).unwrap();

    fs::write(theme_a.join("colors.toml"), "mode = \"dark\"\naccent = \"#0068FF\"\n").unwrap();
    fs::write(theme_b.join("colors.toml"), "mode = \"dark\"\naccent = \"#FFD60A\"\n").unwrap();
    fs::write(theme_c.join("colors.toml"), "mode = \"dark\"\naccent = \"#22C55E\"\n").unwrap();

    let state_current = tmp.path().join("state/omarchy/current");
    let config_omarchy = tmp.path().join("config/omarchy");
    fs::create_dir_all(&state_current).unwrap();
    fs::create_dir_all(&config_omarchy).unwrap();
    fs::write(config_omarchy.join("language"), "en\n").unwrap();

    let theme_link = state_current.join("theme");
    unix_fs::symlink(&theme_a, &theme_link).unwrap();

    let (tx, rx) = mpsc::channel();
    let handle = spawn_omarchy_watcher_with(
        state_current.clone(),
        config_omarchy.clone(),
        Some("hypr_sig".to_string()),
        tmp.path().join("no-hyprctl"),
        Some("en_US.UTF-8".to_string()),
        Duration::from_millis(80),
        move |theme| {
            let _ = tx.send(theme);
        },
    );

    // Give the watcher thread a brief moment to register initial inotify descriptors.
    std::thread::sleep(Duration::from_millis(80));

    // Atomic symlink swap #1 (`ln -sfn` pattern: create temp symlink + atomic rename over `theme`).
    let staging_1 = state_current.join(".theme.tmp.1");
    unix_fs::symlink(&theme_b, &staging_1).unwrap();
    fs::rename(&staging_1, &theme_link).unwrap();

    let updated_1 = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("watcher should fire after first atomic symlink swap");
    assert_eq!(updated_1.accent, "#FFD60A");
    assert_eq!(updated_1.accent_fg, "#081B3A");

    // Atomic symlink swap #2: proves watches were re-armed after the first replacement!
    let staging_2 = state_current.join(".theme.tmp.2");
    unix_fs::symlink(&theme_c, &staging_2).unwrap();
    fs::rename(&staging_2, &theme_link).unwrap();

    let updated_2 = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("watcher should fire after second atomic symlink swap");
    assert_eq!(updated_2.accent, "#22C55E");

    // In-place edit inside the newly symlinked target (`theme_c/colors.toml`) must also be observed!
    fs::write(theme_c.join("colors.toml"), "mode = \"light\"\naccent = \"#EF4444\"\n").unwrap();
    let updated_3 = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("watcher should fire on colors.toml edit inside newly swapped symlink target");
    assert_eq!(updated_3.mode, "light");
    assert_eq!(updated_3.accent, "#EF4444");

    handle.stop();
}

// ============================================================================
// 2. XDG Desktop Portals Helper Tests
// ============================================================================

#[test]
fn portal_uri_conversion_handles_utf8_vietnamese_and_spaces() {
    let raw = "file:///home/khoa/T%C3%A0i%20li%E1%BB%87u/%E1%BA%A2nh%20ch%E1%BB%A5p%20m%C3%A0n%20h%C3%ACnh.png";
    let path = str_uri_to_local_path(raw).expect("valid file:// URI");
    assert_eq!(
        path,
        Path::new("/home/khoa/Tài liệu/Ảnh chụp màn hình.png")
    );

    let parsed = url::Url::parse("https://zalo.me/file.png").unwrap();
    assert!(uri_to_local_path(&parsed).is_none());
    assert!(str_uri_to_local_path("not a uri").is_none());

    // Ensure media filter constructs without error.
    let _filter = build_media_file_filter();
}

#[test]
fn screenshot_staging_copies_with_private_permissions_and_prunes_expired() {
    let tmp = TempDir::new().unwrap();
    let src_png = tmp.path().join("portal-out.png");
    let png_bytes = b"\x89PNG\r\n\x1a\nfake-png-payload";
    fs::write(&src_png, png_bytes).unwrap();

    let staging_root = tmp.path().join("olaz-screenshots");
    let staged = stage_screenshot_file(&src_png, &staging_root).unwrap();

    assert!(staged.exists());
    assert_eq!(staged.file_name().and_then(|n| n.to_str()), Some("screenshot.png"));
    assert_eq!(fs::read(&staged).unwrap(), png_bytes);

    let root_mode = fs::metadata(&staging_root).unwrap().permissions().mode() & 0o777;
    let shot_dir = staged.parent().unwrap();
    let dir_mode = fs::metadata(shot_dir).unwrap().permissions().mode() & 0o777;
    let file_mode = fs::metadata(&staged).unwrap().permissions().mode() & 0o777;
    assert_eq!(root_mode, 0o700);
    assert_eq!(dir_mode, 0o700);
    assert_eq!(file_mode, 0o600);

    // Prune with zero max_age removes staged UUID directory.
    std::thread::sleep(Duration::from_millis(10));
    prune_old_screenshots(&staging_root, Duration::ZERO);
    assert!(!shot_dir.exists());
}

// ============================================================================
// 3. Notifications Policy, Chime Fallback & Ubuntu Unity Badge Tests
// ============================================================================

#[test]
fn notification_policy_suppression_and_pin_lock_redaction() {
    // Actively viewing the same conversation with < 180s idle -> suppress.
    assert!(NotificationPolicy::should_suppress(
        true,
        true,
        Some("g_team"),
        "g_team",
        12.5
    ));
    // User is AFK (>= 180s idle) -> DO NOT suppress.
    assert!(!NotificationPolicy::should_suppress(
        true,
        true,
        Some("g_team"),
        "g_team",
        180.0
    ));
    // Different open conversation, unfocused window, or hidden window -> DO NOT suppress.
    assert!(!NotificationPolicy::should_suppress(
        true,
        true,
        Some("g_other"),
        "g_team",
        0.0
    ));
    assert!(!NotificationPolicy::should_suppress(
        false,
        true,
        Some("g_team"),
        "g_team",
        0.0
    ));
    assert!(!NotificationPolicy::should_suppress(
        true,
        false,
        Some("g_team"),
        "g_team",
        0.0
    ));

    // Redaction when `hidden_known` is false (fail-closed before renderer reports hidden chats).
    let empty_locks = HashSet::new();
    let unknown_redacted = NotificationPolicy::redact(
        "Nguyễn Văn A",
        "Mật khẩu ngân hàng là 123456",
        "u_1",
        true,
        false,
        &empty_locks,
    );
    assert_eq!(unknown_redacted.title, HIDDEN_PREVIEW_TEXT);
    assert_eq!(unknown_redacted.body, HIDDEN_PREVIEW_TEXT);
    assert!(!unknown_redacted.allow_inline_reply);
    assert!(unknown_redacted.redacted_by_lock);

    // Redaction when conversation is in `locked_ids`.
    let locked: HashSet<String> = ["u_secret".to_string()].into_iter().collect();
    let pin_redacted = NotificationPolicy::redact(
        "Trần Thị B",
        "Tin nhắn ẩn",
        "u_secret",
        true,
        true,
        &locked,
    );
    assert_eq!(pin_redacted.title, "Bạn có tin nhắn mới");
    assert_eq!(pin_redacted.body, "Bạn có tin nhắn mới");
    assert!(!pin_redacted.allow_inline_reply);
    assert!(pin_redacted.redacted_by_lock);

    // Normal conversation with preview disabled keeps sender name but masks body.
    let preview_off = NotificationPolicy::redact(
        "Trần Thị B",
        "Nội dung riêng tư",
        "u_normal",
        false,
        true,
        &locked,
    );
    assert_eq!(preview_off.title, "Trần Thị B");
    assert_eq!(preview_off.body, "Bạn có tin nhắn mới");
    assert!(preview_off.allow_inline_reply);
    assert!(!preview_off.redacted_by_lock);

    // Normal conversation with preview enabled preserves both.
    let full = NotificationPolicy::redact(
        "Trần Thị B",
        "Chiều nay họp lúc 3h nhé",
        "u_normal",
        true,
        true,
        &locked,
    );
    assert_eq!(full.title, "Trần Thị B");
    assert_eq!(full.body, "Chiều nay họp lúc 3h nhé");
    assert!(full.allow_inline_reply);
}

#[test]
fn notification_icons_magic_bytes_sound_allowlist_and_unity_signal() {
    let ipc = json!([
        "Nhóm Dự Án",
        {
            "body": "Đã gửi một ảnh",
            "tag": "g_999",
            "silent": true,
            "icon": "https://s120-ava-talk.zadn.vn/avatar.jpg"
        }
    ]);
    let req = NotificationRequest::parse_ipc_args(&ipc).expect("valid IPC notification");
    assert_eq!(req.title, "Nhóm Dự Án");
    assert_eq!(req.tag, "g_999");
    assert!(req.silent);
    assert_eq!(
        req.icon,
        Some(NotificationIcon::Remote(
            "https://s120-ava-talk.zadn.vn/avatar.jpg".to_string()
        ))
    );

    assert_eq!(
        NotificationRequest::parse_icon("data:image/png;base64,iVBORw0KGgo="),
        Some(NotificationIcon::Inline(vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]))
    );
    assert_eq!(
        NotificationRequest::parse_icon("assets/default-ava.png?ver=42"),
        Some(NotificationIcon::Bundled("assets/default-ava.png".to_string()))
    );

    assert_eq!(
        NotificationPolicy::attachment_type(&[0x89, 0x50, 0x4E, 0x47, 0x00]),
        Some(("image/png", "png"))
    );
    assert_eq!(
        NotificationPolicy::attachment_type(&[0xFF, 0xD8, 0xFF, 0xE0]),
        Some(("image/jpeg", "jpg"))
    );
    assert_eq!(
        NotificationPolicy::attachment_type(b"GIF89a"),
        Some(("image/gif", "gif"))
    );
    assert_eq!(
        NotificationPolicy::attachment_type(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
        Some(("image/webp", "webp"))
    );
    assert_eq!(NotificationPolicy::attachment_type(b"not-an-image"), None);

    assert!(NotificationPolicy::is_sound_url_allowed(
        "https://stc-chat.zdn.vn/pc/v1/sound/noti.mp3"
    ));
    assert!(!NotificationPolicy::is_sound_url_allowed(
        "http://stc-chat.zdn.vn/pc/v1/sound/noti.mp3"
    ));
    assert!(!NotificationPolicy::is_sound_url_allowed(
        "https://evil.example.com/sound/noti.mp3"
    ));
    assert!(!NotificationPolicy::is_sound_url_allowed(
        "https://stc-chat.zdn.vn/pc/v1/sound/other.mp3"
    ));

    // Verify Ubuntu Dock Unity LauncherEntry D-Bus signal construction (`(sa{sv})`).
    let msg_unread = build_unity_launcher_signal(7).expect("valid D-Bus signal");
    let hdr = msg_unread.header();
    assert_eq!(hdr.path().map(|p| p.as_str()), Some(UNITY_LAUNCHER_OBJECT_PATH));
    assert_eq!(hdr.interface().map(|i| i.as_str()), Some(UNITY_LAUNCHER_INTERFACE));
    assert_eq!(hdr.member().map(|m| m.as_str()), Some(UNITY_LAUNCHER_MEMBER));

    let (app_uri, props): (String, HashMap<String, zbus::zvariant::OwnedValue>) =
        msg_unread.body().deserialize().unwrap();
    assert_eq!(app_uri, UNITY_DESKTOP_URI);
    assert_eq!(i64::try_from(props.get("count").unwrap()).unwrap(), 7);
    assert!(bool::try_from(props.get("count-visible").unwrap()).unwrap());

    let msg_zero = build_unity_launcher_signal(0).unwrap();
    let (_, zero_props): (String, HashMap<String, zbus::zvariant::OwnedValue>) =
        msg_zero.body().deserialize().unwrap();
    assert!(!bool::try_from(zero_props.get("count-visible").unwrap()).unwrap());

    // Graceful rodio no-op on empty/corrupt MP3 bytes.
    assert!(!LinuxNotifier::play_chime_bytes_sync(&[]));
    assert!(!LinuxNotifier::play_chime_bytes_sync(b"not-valid-mp3"));
}

// ============================================================================
// 4. StatusNotifierItem (`ksni`) Tray Icon & Menu Tests
// ============================================================================

#[test]
fn tray_icon_argb32_pixmaps_and_menu_actions() {
    let clean_icons = render_tray_pixmaps(false);
    let unread_icons = render_tray_pixmaps(true);
    assert_eq!(clean_icons.len(), 3);
    assert_eq!(unread_icons.len(), 3);
    assert_eq!(
        clean_icons.iter().map(|i| (i.width, i.height)).collect::<Vec<_>>(),
        vec![(24, 24), (32, 32), (48, 48)]
    );

    let icon48_clean = render_tray_icon_argb32(48, false);
    let icon48_unread = render_tray_icon_argb32(48, true);
    assert_eq!(icon48_clean.data.len(), 48 * 48 * 4);

    // Outer corner (0, 0) is transparent outside rounded squircle.
    assert_eq!(&icon48_clean.data[0..4], &[0, 0, 0, 0]);

    // Center pixel (24, 24) lies inside the white speech-bubble mark.
    let center_idx = (24 * 48 + 24) * 4;
    assert_eq!(icon48_clean.data[center_idx], 255); // alpha
    assert!(icon48_clean.data[center_idx + 1] > 240); // R
    assert!(icon48_clean.data[center_idx + 2] > 240); // G
    assert!(icon48_clean.data[center_idx + 3] > 240); // B

    // Top-right unread badge center (~ (37, 10) on 48x48) turns bright red (#FF3B30) when unread!
    let dot_idx = (10 * 48 + 37) * 4;
    assert!(
        icon48_unread.data[dot_idx + 1] > 220 && icon48_unread.data[dot_idx + 3] < 80,
        "expected red unread dot at top-right, got ARGB={:?}",
        &icon48_unread.data[dot_idx..dot_idx + 4]
    );
    assert_ne!(
        &icon48_clean.data[dot_idx..dot_idx + 4],
        &icon48_unread.data[dot_idx..dot_idx + 4]
    );

    // Exercise `OlazTray` menu & click activation callbacks.
    let events: Arc<Mutex<Vec<OlazTrayEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_sink = Arc::clone(&events);
    let mut tray = OlazTray::new(move |e| events_sink.lock().unwrap().push(e));

    assert_eq!(tray.id(), "olaz");
    assert_eq!(tray.title(), "Olaz");
    tray.set_unread_count(3);
    assert_eq!(tray.title(), "Olaz (3)");
    assert!(tray.tool_tip().description.contains("3"));

    tray.activate(10, 10);

    assert_eq!(tray.online_busy_index(), 0);
    assert_eq!(tray.privacy_state(), TrayPrivacyState::default());

    let mut menu = tray.menu();
    // Show, Status submenu, Privacy submenu, Screenshot, New Profile, Separator, Quit
    assert_eq!(menu.len(), 7);

    let mut labels = Vec::new();
    for item in &mut menu {
        match item {
            MenuItem::Standard(std_item) => {
                labels.push(std_item.label.clone());
                (std_item.activate)(&mut tray);
            }
            MenuItem::SubMenu(sub) if sub.label == LABEL_STATUS_MENU => {
                assert_eq!(sub.icon_name, "user-available-symbolic");
                assert_eq!(sub.submenu.len(), 1);
                match &mut sub.submenu[0] {
                    MenuItem::RadioGroup(radio) => {
                        assert_eq!(radio.selected, 0);
                        assert_eq!(radio.options.len(), 2);
                        assert_eq!(radio.options[0].label, LABEL_STATUS_ONLINE);
                        assert_eq!(radio.options[1].label, LABEL_STATUS_BUSY);
                        (radio.select)(&mut tray, 1);
                        assert_eq!(tray.online_busy_index(), 1);
                        (radio.select)(&mut tray, 0);
                        assert_eq!(tray.online_busy_index(), 0);
                    }
                    other => panic!("expected RadioGroup in Status submenu, got {other:?}"),
                }
            }
            MenuItem::SubMenu(sub) if sub.label == LABEL_PRIVACY_MENU => {
                assert_eq!(sub.submenu.len(), 3);
                let mut check_labels = Vec::new();
                for sub_item in &mut sub.submenu {
                    match sub_item {
                        MenuItem::Checkmark(chk) => {
                            assert!(!chk.checked);
                            check_labels.push(chk.label.clone());
                            (chk.activate)(&mut tray);
                        }
                        other => panic!("expected CheckmarkItem in Privacy submenu, got {other:?}"),
                    }
                }
                assert_eq!(
                    check_labels,
                    vec![
                        LABEL_BLOCK_TYPING.to_string(),
                        LABEL_BLOCK_SEEN.to_string(),
                        LABEL_BLOCK_DELIVERED.to_string(),
                    ]
                );
            }
            _ => {}
        }
    }
    assert_eq!(
        labels,
        vec![
            LABEL_SHOW_OLAZ.to_string(),
            LABEL_SCREENSHOT.to_string(),
            LABEL_NEW_PROFILE.to_string(),
            LABEL_QUIT.to_string(),
        ]
    );

    // Verify updated menu reflects privacy checkmarks and busy radio selection.
    assert_eq!(
        tray.privacy_state(),
        TrayPrivacyState {
            block_typing: true,
            block_seen: true,
            block_delivered: true,
        }
    );
    tray.set_online_busy_index(1);
    let refreshed_menu = tray.menu();
    match &refreshed_menu[1] {
        MenuItem::SubMenu(sub) => {
            assert_eq!(sub.icon_name, "user-busy-symbolic");
            match &sub.submenu[0] {
                MenuItem::RadioGroup(radio) => assert_eq!(radio.selected, 1),
                other => panic!("expected RadioGroup, got {other:?}"),
            }
        }
        other => panic!("expected Status SubMenu, got {other:?}"),
    }
    match &refreshed_menu[2] {
        MenuItem::SubMenu(sub) => {
            for sub_item in &sub.submenu {
                match sub_item {
                    MenuItem::Checkmark(chk) => assert!(chk.checked),
                    other => panic!("expected CheckmarkItem, got {other:?}"),
                }
            }
        }
        other => panic!("expected Privacy SubMenu, got {other:?}"),
    }

    tray.set_privacy_state(TrayPrivacyState {
        block_typing: false,
        block_seen: true,
        block_delivered: false,
    });
    assert!(!tray.privacy_state().get(PrivacyField::BlockTyping));
    assert!(tray.privacy_state().get(PrivacyField::BlockSeen));
    assert!(!tray.privacy_state().get(PrivacyField::BlockDelivered));

    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            recorded[0].clone(),
            recorded[1].clone(),
            OlazTrayEvent::SetUserStatusBusy,
            OlazTrayEvent::SetUserStatusOnline,
            OlazTrayEvent::TogglePrivacy {
                field: PrivacyField::BlockTyping,
                enabled: true,
            },
            OlazTrayEvent::TogglePrivacy {
                field: PrivacyField::BlockSeen,
                enabled: true,
            },
            OlazTrayEvent::TogglePrivacy {
                field: PrivacyField::BlockDelivered,
                enabled: true,
            },
            OlazTrayEvent::Screenshot,
            OlazTrayEvent::LaunchSecondaryProfile,
            OlazTrayEvent::Quit,
        ]
    );
    assert!(matches!(recorded[0], OlazTrayEvent::ToggleWindow { .. }));
    assert!(matches!(recorded[1], OlazTrayEvent::ShowWindow { .. }));
}

// ============================================================================
// 5. CallTransport (`AES-128-CBC` Golden Vectors, Chunker, Decoder & Coordinator)
// ============================================================================

#[test]
fn call_transport_crypto_matches_nodejs_golden_vectors() {
    assert_eq!(CALL_V2_KEY_BASE64, "yjAF9oqMWl6XfXYJn9mA7w==");
    assert_eq!(CallTransportCrypto::default_key().len(), 16);

    let ready_plain = r#"{"type": "update", "command": "native-ready"}"#;
    let ready_hex = "aa86e40365c48c1c63fe6b0e0ba67b1e6714eff63021aea7244d4a005c2af764eb549f87c9aa3970730fbe89426fdb22";
    assert_eq!(CallTransportCrypto::encrypt(ready_plain).unwrap(), ready_hex);
    assert_eq!(
        CallTransportCrypto::decrypt(ready_hex).as_deref(),
        Some(ready_plain)
    );
    assert_eq!(
        CallTransportCrypto::decrypt(&ready_hex.to_uppercase()).as_deref(),
        Some(ready_plain)
    );

    let make_call_plain = r#"{"type":"request","command":"makeCall","data":{"partner":[{"id":"123","name":"Nguyễn Văn Á 😀"}],"type":1}}"#;
    let make_call_hex = "4d8c2a9b25501aa04cb54e198212e58862cba9a3ebf2b085db3fae014058cc44934645a90f7b159785adfa1838e03fe7a31469c518d7a0b38e3753b51a932e02748a204ea14a71609d4cae6c4b61734608756225655836b743a8c3e7a3c175958342d42bdcfab6a5c055b10163783b98a9c38f30385fc2a0d7c812c19ed1944d";
    assert_eq!(
        CallTransportCrypto::encrypt(make_call_plain).unwrap(),
        make_call_hex
    );
    assert_eq!(
        CallTransportCrypto::decrypt(make_call_hex).as_deref(),
        Some(make_call_plain)
    );

    // Empty string produces one full 16-byte PKCS#7 padding block (32 hex chars).
    let empty_hex = CallTransportCrypto::encrypt("").unwrap();
    assert_eq!(empty_hex.len(), 32);
    assert_eq!(CallTransportCrypto::decrypt(&empty_hex).as_deref(), Some(""));

    // Invalid key length, non-hex, odd length, non-16-byte block, and bad PKCS#7 padding fail cleanly.
    assert_eq!(
        CallTransportCrypto::encrypt_with_key("hi", &[1, 2, 3]),
        Err(CallTransportError::InvalidKeyLength(3))
    );
    assert_eq!(CallTransportCrypto::decrypt(""), None);
    assert_eq!(CallTransportCrypto::decrypt("zz"), None);
    assert_eq!(CallTransportCrypto::decrypt("deadbeef"), None);
    assert_eq!(
        CallTransportCrypto::decrypt("000102030405060708090a0b0c0d0e0f"),
        None
    );
}

#[test]
fn call_chunker_and_reassembler_match_zalo_wire_protocol() {
    let mut chunker = CallChunker::new();
    assert_eq!(chunker.chunks_at("", 1000), vec!["$"]);
    assert_eq!(chunker.chunks_at("abcdef", 1001), vec!["abcdef$"]);

    let exact_4000 = "a".repeat(4000);
    assert_eq!(
        chunker.chunks_at(&exact_4000, 1002),
        vec![format!("{exact_4000}$")]
    );

    let over_4001 = "b".repeat(4001);
    let two_chunks = chunker.chunks_at(&over_4001, 1003);
    assert_eq!(two_chunks.len(), 2);
    assert!(two_chunks[0].ends_with("#1003#2#0#$"));
    assert!(two_chunks[1].ends_with("#1003#2#1#$"));

    // Exact golden test from `CallTransportTests.swift:83-102`.
    let mut chunker2 = CallChunker::new();
    let payload_10112 = "0123456789abcdef".repeat(632); // 10,112 bytes
    let parts = chunker2.chunks_at(&payload_10112, 77);
    assert_eq!(parts.len(), 3);
    assert_eq!(
        parts.iter().map(String::len).collect::<Vec<_>>(),
        vec![4009, 4009, 2121]
    );
    assert!(parts[0].ends_with("#77#3#0#$"));
    assert!(parts[1].ends_with("#77#3#1#$"));
    assert!(parts[2].ends_with("#77#3#2#$"));

    // Rapid second call at same ms advances sequence monotonically to 78.
    let next_parts = chunker2.chunks_at(&payload_10112, 77);
    assert!(next_parts[0].ends_with("#78#3#0#$"));

    // Round-trip encrypted multi-chunk payload through CallChunkReassembler.
    let big_json = json!({
        "type": "request",
        "command": "syncSDP",
        "sdp": "v=0\r\n".repeat(900)
    })
    .to_string();
    let big_hex = CallTransportCrypto::encrypt(&big_json).unwrap();
    let wire_chunks = chunker2.chunks_at(&big_hex, 5000);
    assert!(wire_chunks.len() >= 2);

    let mut reassembler = CallChunkReassembler::new();
    let mut completed = None;
    for chunk in wire_chunks.iter().rev() {
        let without_dollar = chunk.strip_suffix('$').unwrap();
        if let Some(joined) = reassembler.feed_frame(without_dollar) {
            completed = Some(joined);
        }
    }
    let reassembled_hex = completed.expect("all chunks arrived");
    assert_eq!(
        CallTransportCrypto::decrypt(&reassembled_hex).as_deref(),
        Some(big_json.as_str())
    );
}

#[test]
fn call_frame_decoder_handles_splits_nul_bytes_garbage_and_4mib_cap() {
    let f1 = format!(
        "{}$",
        CallTransportCrypto::encrypt(r#"{"command":"native-ready","seq":1}"#).unwrap()
    );
    let f2 = format!(
        "{}$",
        CallTransportCrypto::encrypt("{\"command\":\"ping\",\"note\":\"a\0b\"}").unwrap()
    );
    let stream = format!("{f1}{f2}");
    let bytes = stream.as_bytes();

    // Feed 7 bytes at a time across frame boundaries.
    let mut decoder = CallFrameDecoder::new();
    let mut dropped = 0usize;
    let mut decoded = Vec::new();
    for slice in bytes.chunks(7) {
        decoded.extend(decoder.feed(slice, &mut dropped));
    }
    assert_eq!(dropped, 0);
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0].get("command").and_then(|v| v.as_str()), Some("native-ready"));
    assert_eq!(decoded[1].get("note").and_then(|v| v.as_str()), Some("ab"));

    // Garbage frame + empty `$$` + valid frame.
    let mut dropped2 = 0usize;
    let mixed = format!("not-valid-hex$${f1}");
    let out = decoder.feed(mixed.as_bytes(), &mut dropped2);
    assert_eq!(dropped2, 1);
    assert_eq!(out.len(), 1);

    // Buffer cap overflow (custom 128-byte cap for fast test; production default is 4 MiB).
    assert_eq!(CallFrameDecoder::MAX_BUFFER_BYTES, 4 * 1024 * 1024);
    let mut capped = CallFrameDecoder::with_max_buffer_bytes(128);
    let mut cap_dropped = 0usize;
    assert!(capped.feed(&vec![b'a'; 200], &mut cap_dropped).is_empty());
    assert_eq!(cap_dropped, 1);
    assert_eq!(capped.buffered_len(), 0);

    // Recovers cleanly on the very next valid frame.
    let recovered = capped.feed(f1.as_bytes(), &mut cap_dropped);
    assert_eq!(recovered.len(), 1);
    assert_eq!(cap_dropped, 1);
}

#[test]
fn linux_call_coordinator_reports_unsupported_outgoing_and_parses_incoming_voip() {
    let mut coord = LinuxCallCoordinator::new();
    assert!(!coord.can_call());

    let outgoing = coord.start_outgoing_call("u_123", true);
    assert!(!outgoing.can_call);
    assert_eq!(outgoing.reason, LINUX_CALL_UNSUPPORTED_MESSAGE);
    assert_eq!(
        coord.last_notice.as_deref(),
        Some("Chưa hỗ trợ gọi thoại/video trực tiếp trên Linux — vui lòng nhận cuộc gọi trên điện thoại")
    );

    // Busy incoming call is ignored.
    assert!(coord
        .handle_voip_event(&json!({
            "call": { "kind": "incoming", "callerId": "u_1", "name": "An", "busy": true }
        }))
        .is_none());

    // Incoming 1-1 video call.
    let one_to_one = coord
        .handle_voip_event(&json!({
            "call": {
                "kind": "incoming",
                "callId": "c_42",
                "callerId": "u_88",
                "name": "Lê Minh",
                "video": true,
                "avatar": "https://s120-ava-talk.zadn.vn/leminh.jpg"
            }
        }))
        .expect("incoming 1-1 call");
    assert_eq!(one_to_one.conv_id, "u_88");
    assert_eq!(one_to_one.notification_title(), "Cuộc gọi video đến");
    assert!(one_to_one.notification_body().contains("Lê Minh đang gọi cho bạn"));
    let notif_req = one_to_one.to_notification_request();
    assert_eq!(notif_req.tag, "u_88");

    // Incoming group call keeps separate group `name` and `caller`.
    let group_call = coord
        .handle_voip_event(&json!({
            "voip": {
                "event": "ring",
                "group": true,
                "groupId": "g_555",
                "name": "Team Backend",
                "caller": "Hoàng"
            }
        }))
        .expect("incoming group call");
    assert!(group_call.is_group);
    assert_eq!(group_call.notification_title(), "Cuộc gọi nhóm đến");
    assert_eq!(
        group_call.notification_body(),
        "Hoàng đang gọi trong Team Backend — vui lòng nhận cuộc gọi trên điện thoại"
    );

    // Hangup clears active incoming state.
    assert!(coord.handle_voip_event(&json!({"call": {"kind": "ended"}})).is_none());
    assert!(coord.active_incoming.is_none());
}

// ============================================================================
// 6. Engine Models, Vietnamese Folding, UTF-16 Drafts, Chat Merge & zstate Tests
// ============================================================================

#[test]
fn fold_vietnamese_strips_diacritics_and_normalizes_d_stroke() {
    assert_eq!(
        fold_vietnamese("Đặng Nguyễn Quốc Ánh"),
        "dang nguyen quoc anh"
    );
    assert_eq!(fold_vietnamese("Phở Bò Đặc Biệt"), "pho bo dac biet");
    assert_eq!(fold_vietnamese("đồng hồ Đẹp"), "dong ho dep");

    // Precomposed NFC ("tiếng") vs decomposed NFD ("tie\u{0302}\u{0301}ng") fold identically.
    assert_eq!(
        fold_vietnamese("tiếng Việt"),
        fold_vietnamese("tie\u{0302}\u{0301}ng Vie\u{0323}\u{0302}t")
    );
}

#[test]
fn file_card_may_execute_blocks_dangerous_extensions_across_platforms() {
    for dangerous in [
        "install.sh",
        "payload.DESKTOP",
        "Olaz-x86_64.AppImage",
        "package.deb",
        "package.rpm",
        "setup.exe",
        "invoice.pdf.bat",
        "script.command",
        "Application.app",
        "installer.pkg",
        "autorun.py",
        "trick.sh...",
        "trick.desktop   ",
        "https://cdn.zalo.me/files/dropper.AppImage?token=abc#frag",
    ] {
        assert!(
            FileCard::may_execute(dangerous),
            "expected {dangerous:?} to be flagged as executable"
        );
    }

    for safe in [
        "bao-cao-quy-4.pdf",
        "anh-gia-dinh.png",
        "hop-dong.docx",
        "notes.txt",
        "archive.zip",
        "README",
        "",
        "...",
    ] {
        assert!(
            !FileCard::may_execute(safe),
            "expected {safe:?} to be treated as non-executable"
        );
    }

    assert!(FileCard::may_execute_either("/tmp/downloaded.bin", "report.pdf"));
    assert!(FileCard::may_execute_either("/tmp/cache_12345", "run_me.sh"));
    assert!(!FileCard::may_execute_either("/tmp/report.pdf", "report.pdf"));
}

#[test]
fn utf16_conversions_and_composer_draft_mention_shifting_with_vietnamese_and_emojis() {
    // 1. Exact test vector from `Tests/OlazTests/FailedDraftTests.swift:17-25`:
    // "hi @An" (6 UTF-16 units) + "\n" (1) + "😀 @Bo" ("😀" is 2 UTF-16 units + " " is 1 -> "@Bo" at 3 + 7 = 10).
    let a = ComposerDraft::with_mentions(
        "hi @An",
        vec![Mention {
            uid: "1".to_string(),
            pos: 3,
            len: 3,
            is_all: false,
        }],
    );
    let b = ComposerDraft::with_mentions(
        "😀 @Bo",
        vec![Mention {
            uid: "2".to_string(),
            pos: 3,
            len: 3,
            is_all: false,
        }],
    );
    let merged = a.append(&b);
    assert_eq!(merged.text, "hi @An\n😀 @Bo");
    assert_eq!(
        merged.mentions,
        vec![
            Mention {
                uid: "1".to_string(),
                pos: 3,
                len: 3,
                is_all: false
            },
            Mention {
                uid: "2".to_string(),
                pos: 10,
                len: 3,
                is_all: false
            },
        ]
    );
    assert_eq!(utf16_slice(&merged.text, 3, 3), Some("@An"));
    assert_eq!(utf16_slice(&merged.text, 10, 3), Some("@Bo"));

    // Mid-surrogate-pair UTF-16 offset ("😀" occupies UTF-16 offsets 7..9; offset 8 is invalid boundary).
    assert_eq!(utf16_offset_to_byte_idx(&merged.text, 8), None);
    assert_eq!(utf16_slice(&merged.text, 8, 2), None);

    // 2. Complex Vietnamese diacritics + ZWJ family emoji + skin-tone emoji:
    // "Chào 👋🏽 @Đức" ->
    //   "Chào " = 5 chars (5 UTF-16 units, 6 UTF-8 bytes)
    //   "👋🏽" = 2 scalar values, each 2 UTF-16 units = 4 UTF-16 units (8 UTF-8 bytes)
    //   " " = 1 UTF-16 unit
    //   "@Đức" = 4 UTF-16 units ("@", "Đ", "ứ", "c") -> starts at UTF-16 pos 10, len 4.
    let prefix = "Chào 👋🏽 @Đức";
    assert_eq!(utf16_len(prefix), 14);
    assert_eq!(utf16_slice(prefix, 10, 4), Some("@Đức"));
    let byte_at_mention = utf16_offset_to_byte_idx(prefix, 10).unwrap();
    assert_eq!(byte_idx_to_utf16_offset(prefix, byte_at_mention), Some(10));

    let d1 = ComposerDraft::with_mentions(
        prefix,
        vec![Mention {
            uid: "u_duc".to_string(),
            pos: 10,
            len: 4,
            is_all: false,
        }],
    );
    // "👨‍👩‍👧‍👦 " = 4 supplementary emojis (4 * 2 = 8) + 3 ZWJs (3 * 1 = 3) + space (1) = 12 UTF-16 units!
    let suffix = "👨‍👩‍👧‍👦 @Nguyễn Ánh!";
    assert_eq!(utf16_slice(suffix, 12, 11), Some("@Nguyễn Ánh"));
    let d2 = ComposerDraft::with_mentions(
        suffix,
        vec![Mention {
            uid: "u_anh".to_string(),
            pos: 12,
            len: 11,
            is_all: false,
        }],
    );
    let combined = d1.append(&d2);
    // Shift = utf16_len(prefix) (14) + 1 ('\n') = 15 -> second mention moves to 12 + 15 = 27!
    assert_eq!(combined.mentions[1].pos, 27);
    assert_eq!(
        utf16_slice(&combined.text, combined.mentions[1].pos, combined.mentions[1].len),
        Some("@Nguyễn Ánh")
    );

    // 3. `trimmed_for_wire` rebases mentions after leading spaces/emoji-adjacent whitespace and drops stale ones.
    let padded = ComposerDraft::with_mentions(
        "  \nHello @An   ",
        vec![
            Mention {
                uid: "u_an".to_string(),
                pos: 9,
                len: 3,
                is_all: false,
            },
            Mention {
                uid: "u_stale".to_string(),
                pos: 3,
                len: 4, // points at "Hello", does not start with '@'
                is_all: false,
            },
        ],
    );
    let (wire_text, wire_mentions) = padded.trimmed_for_wire();
    assert_eq!(wire_text, "Hello @An");
    assert_eq!(
        wire_mentions,
        vec![Mention {
            uid: "u_an".to_string(),
            pos: 6,
            len: 3,
            is_all: false
        }]
    );

    // 4. `FailedDraftStore` consecutive failure concatenation & silent -> loud notice upgrade.
    let mut store = FailedDraftStore::new();
    store.stash_with_silence("c1", ComposerDraft::new("first"), true);
    assert_eq!(store.revision(), 1);
    let (taken_silent, show_notice_1) = store.take_with_notice("c1").unwrap();
    assert_eq!(taken_silent.text, "first");
    assert!(!show_notice_1);

    store.stash_with_silence("c2", ComposerDraft::new("line 1"), true);
    store.stash("c2", ComposerDraft::new("line 2")); // loud failure upgrades notice!
    let (taken_loud, show_notice_2) = store.take_with_notice("c2").unwrap();
    assert_eq!(taken_loud.text, "line 1\nline 2");
    assert!(show_notice_2);
}

#[test]
fn conv_actions_enforces_fail_closed_visibility_and_60s_auto_relock() {
    let mut state = ZaloEngineState::new();
    state.apply_zstate_patch(&json!({
        "conversations": [
            { "id": "u_public", "name": "Nguyễn An", "unread": 2 },
            { "id": "u_hidden", "name": "Bí Mật", "unread": 5 }
        ]
    }));

    // Fail-closed: before `hidden_known` arrives, no conversations are exposed!
    assert!(state.visible_conversations().is_empty());
    assert_eq!(state.total_unread(), 0);

    // Hidden state arrives: `u_hidden` is hidden and locked; only `u_public` is visible.
    state.apply_zstate_patch(&json!({
        "convActions": {
            "hidden": { "enabled": true, "hasPin": true, "ids": ["u_hidden"] },
            "autoDelete": { "u_public": 86400 },
            "flags": { "u_hidden": conv_action_flags::HIDDEN }
        }
    }));
    assert!(state.conv_actions.hidden_known);
    assert!(state.conv_actions.pin_set);
    assert_eq!(state.conv_actions.auto_delete.get("u_public"), Some(&86_400));
    assert_eq!(
        state.visible_conversations().iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        vec!["u_public"]
    );
    assert_eq!(state.total_unread(), 2);

    // Unlocking `u_hidden` reveals it and starts the 60s inactivity timer.
    let t0 = Instant::now();
    state.conv_actions.unlock("u_hidden", t0);
    assert_eq!(state.visible_conversations().len(), 2);
    assert_eq!(state.total_unread(), 7);

    // At +59s, still unlocked.
    assert!(!state.conv_actions.check_auto_relock(t0 + Duration::from_secs(59)));
    assert!(!state.conv_actions.is_locked("u_hidden"));

    // Activity at +40s refreshes timer so +90s (50s since activity) is still unlocked.
    state.conv_actions.note_activity(t0 + Duration::from_secs(40));
    assert!(!state.conv_actions.check_auto_relock(t0 + Duration::from_secs(90)));

    // At +100s (60s after last activity at +40s), auto-relock fires!
    assert!(state.conv_actions.check_auto_relock(t0 + Duration::from_secs(100)));
    assert!(state.conv_actions.is_locked("u_hidden"));
    assert_eq!(state.visible_conversations().len(), 1);
}

#[test]
fn merge_chat_preserves_climsgid_identity_chronological_order_and_history_ticks() {
    let mut messages = HashMap::new();
    let mut has_more = HashMap::new();
    let mut ticks = HashMap::new();

    // 1. Initial reset snapshot with an optimistic outgoing message (`cliMsgId` only).
    let outcome1 = merge_chat(
        "c_1",
        &json!({
            "reset": true,
            "hasMore": true,
            "messages": [
                { "msgId": "m_10", "timestamp": 1000, "isMine": false, "text": "Xin chào" },
                { "cliMsgId": "cli_99", "timestamp": 2000, "isMine": true, "status": "sending", "text": "Đang gửi..." }
            ]
        }),
        &mut messages,
        &mut has_more,
        &mut ticks,
    );
    assert!(outcome1.messages_changed);
    assert!(outcome1.has_more_changed);
    assert!(!outcome1.tick_incremented);
    assert_eq!(messages["c_1"][1].id, "cli_99");
    assert_eq!(messages["c_1"][1].status, MessageStatus::Sending);

    // 2. Server confirms optimistic message (`cliMsgId: "cli_99"`, now assigned `msgId: "m_11"`).
    // `id` MUST stay `"cli_99"` so UI list keys never remount/flicker!
    let outcome2 = merge_chat(
        "c_1",
        &json!({
            "reset": false,
            "changed": [
                { "msgId": "m_11", "cliMsgId": "cli_99", "timestamp": 2001, "isMine": true, "status": "delivered", "text": "Đang gửi..." }
            ]
        }),
        &mut messages,
        &mut has_more,
        &mut ticks,
    );
    assert!(outcome2.messages_changed);
    assert_eq!(messages["c_1"].len(), 2);
    assert_eq!(messages["c_1"][1].id, "cli_99");
    assert_eq!(messages["c_1"][1].msg_id.as_deref(), Some("m_11"));
    assert_eq!(messages["c_1"][1].status, MessageStatus::Delivered);

    // 3. Older history page loaded (`grew: true`, `hasMore: false`) + recall/removal of `m_10`.
    let outcome3 = merge_chat(
        "c_1",
        &json!({
            "reset": false,
            "grew": true,
            "hasMore": false,
            "changed": [
                { "msgId": "m_01", "timestamp": 500, "isMine": false, "text": "Tin nhắn cũ hơn" }
            ],
            "removed": ["m_10"]
        }),
        &mut messages,
        &mut has_more,
        &mut ticks,
    );
    assert!(outcome3.messages_changed);
    assert!(outcome3.has_more_changed);
    assert!(outcome3.tick_incremented);
    assert_eq!(ticks.get("c_1"), Some(&1));
    assert_eq!(has_more.get("c_1"), Some(&false));
    assert_eq!(
        messages["c_1"].iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["m_01", "cli_99"]
    );

    // Verify rich attachment extraction from `kind` object (`chat-adapter.js` shape).
    let file_msg = ChatMessage::from_json(
        &json!({
            "msgId": "m_file",
            "timestamp": 3000,
            "kind": { "type": "file", "name": "tailieu.pdf", "size": 4096, "url": "https://zalo.me/f.pdf" },
            "reactions": [{ "emoji": "❤️", "count": 2, "includesMe": true }]
        }),
        "c_1",
    )
    .unwrap();
    assert_eq!(file_msg.kind, "file");
    assert_eq!(file_msg.file.as_ref().unwrap().name, "tailieu.pdf");
    assert_eq!(file_msg.reactions.len(), 1);
    assert_eq!(file_msg.reactions[0].count, 2);
}

#[test]
fn apply_zstate_patch_updates_all_14_slices_and_searches_vietnamese_conversations() {
    let mut state = ZaloEngineState::new();
    assert_eq!(state.phase, EnginePhase::Booting);

    // Runtime setup -> Login QR -> WaitingConfirm -> Ready + all slices.
    let s1 = state.apply_zstate_patch(&json!({
        "runtimeSetup": { "step": "extracting", "fraction": 0.45, "detail": "pc-dist" }
    }));
    assert_eq!(s1, vec!["phase"]);
    assert!(matches!(
        state.phase,
        EnginePhase::InstallingRuntime { ref step, fraction, .. } if step == "extracting" && (fraction - 0.45).abs() < 1e-6
    ));

    let s2 = state.apply_zstate_patch(&json!({
        "page": "login",
        "qr": "data:image/png;base64,AAA",
        "waitingConfirm": { "name": "Khoa Võ", "avatar": "https://ava.zdn.vn/k.jpg" }
    }));
    assert_eq!(s2, vec!["phase"]);
    assert_eq!(
        state.phase,
        EnginePhase::Login {
            qr: Some("data:image/png;base64,AAA".to_string()),
            qr_expired: false,
            waiting_confirm: Some(WaitingConfirm {
                name: "Khoa Võ".to_string(),
                avatar: Some("https://ava.zdn.vn/k.jpg".to_string()),
            }),
            kickout_notice: None,
            error_notice: None,
        }
    );

    let full_patch = json!({
        "page": "main",
        "user": { "userId": "u_me", "displayName": "Khoa Võ", "phone": "+84901234567" },
        "conversations": [
            { "id": "u_1", "name": "Đặng Quốc Bảo", "pinned": false, "unread": 1, "preview": { "text": "Alo", "ts": 1700000001000_i64 } },
            { "id": "g_1", "isGroup": true, "name": "Nhóm Kỹ Thuật", "pinned": true, "unread": 3, "preview": { "text": "Deploy xong", "ts": 1700000000000_i64 } }
        ],
        "conversationExtras": {
            "selfConversationId": "u_me",
            "listIds": ["g_1", "u_1"],
            "labels": [{ "id": "lbl_work", "name": "Công việc", "color": "#0068FF", "convIds": ["g_1"] }],
            "strangerIds": ["u_stranger"],
            "strangerBox": { "chats": 2, "unreadChats": 1, "unreadMessages": 4, "newestId": "u_stranger" }
        },
        "convActions": {
            "hidden": { "enabled": true, "hasPin": false, "ids": [] }
        },
        "typing": { "convId": "u_1", "name": "Bảo" },
        "contacts": { "friends": [{ "id": "u_1", "name": "Đặng Quốc Bảo" }] },
        "zcloud": { "quotaUsed": 1024, "quotaTotal": 1048576 },
        "transferAvailable": { "canStart": true },
        "transfer": { "phase": "syncing", "progress": 0.5, "syncedConversations": 10, "totalConversations": 20 },
        "poll": { "id": "p_1", "detail": { "question": "Trưa ăn gì?" } },
        "stickers": { "packs": ["pack_1"] },
        "pins": { "convId": "g_1", "items": [{ "msgId": "m_pin" }] },
        "notifications": { "preview": true, "sound": true }
    });

    let changed = state.apply_zstate_patch(&full_patch);
    for expected in [
        "phase",
        "user",
        "conversations",
        "conv_extras",
        "conv_actions",
        "typing",
        "contacts",
        "cloud",
        "transfer",
        "polls",
        "stickers",
        "pins",
        "notifications",
    ] {
        assert!(
            changed.contains(&expected.to_string()),
            "missing expected changed slice {expected:?} in {changed:?}"
        );
    }

    assert_eq!(state.phase, EnginePhase::Ready);
    assert_eq!(state.self_conversation_id.as_deref(), Some("u_me"));
    // Pinned `g_1` sorts ahead of unpinned `u_1` even though `u_1` has a newer timestamp.
    assert_eq!(state.conversations[0].id, "g_1");
    assert_eq!(state.conv_extras.label_by_conv.get("g_1").map(String::as_str), Some("lbl_work"));
    assert!(state.conv_extras.stranger_ids.contains("u_stranger"));
    assert_eq!(state.conv_extras.stranger_box.unread_messages, 4);
    assert_eq!(state.typing.get("u_1").map(String::as_str), Some("Bảo"));
    assert!(state.transfer.can_start);
    assert_eq!(state.transfer.synced_conversations, 10);

    // Re-applying the exact same patch is a clean no-op!
    assert!(state.apply_zstate_patch(&full_patch).is_empty());

    // Incoming peer message on `u_1` emits `"chat:u_1"` AND clears `"typing"` for `u_1`.
    let chat_slices = state.apply_zstate_patch(&json!({
        "chat": {
            "convId": "u_1",
            "reset": true,
            "messages": [{ "msgId": "m_new", "timestamp": 1700000002000_i64, "isMine": false, "text": "Mình tới rồi!" }]
        }
    }));
    assert_eq!(chat_slices, vec!["chat:u_1", "typing"]);
    assert!(!state.typing.contains_key("u_1"));

    // Accent-insensitive Vietnamese QuickSwitcher search!
    let hits = state.search_conversations("dang quoc");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "u_1");

    // Kickout resets phase to Login with default Vietnamese kickout notice.
    let kick_slices = state.apply_zstate_patch(&json!({ "kickout": true }));
    assert_eq!(kick_slices, vec!["phase"]);
    assert_eq!(
        state.phase,
        EnginePhase::Login {
            qr: None,
            qr_expired: false,
            waiting_confirm: None,
            kickout_notice: Some(DEFAULT_KICKOUT_NOTICE.to_string()),
            error_notice: None,
        }
    );
}

#[test]
fn login_qr_rejects_svg_spinner_handles_invalid_and_preserves_waiting_confirm() {
    let mut state = ZaloEngineState::new();

    // (a) 42x42 SVG loading spinner (~1,430 bytes, `data:image/svg+xml...`) MUST be rejected as QR.
    let svg_spinner = format!("data:image/svg+xml;base64,{}", "A".repeat(1430));
    let changed_svg = state.apply_zstate_patch(&json!({
        "page": "login",
        "qr": svg_spinner,
    }));
    assert_eq!(changed_svg, vec!["phase"]);
    assert_eq!(
        state.phase,
        EnginePhase::Login {
            qr: None,
            qr_expired: false,
            waiting_confirm: None,
            kickout_notice: None,
            error_notice: None,
        }
    );

    // Real PNG QR (`data:image/png;base64,...`) is accepted.
    let png_qr_1 = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB";
    let changed_png = state.apply_zstate_patch(&json!({
        "page": "login",
        "qr": png_qr_1,
    }));
    assert_eq!(changed_png, vec!["phase"]);
    assert_eq!(
        state.phase,
        EnginePhase::Login {
            qr: Some(png_qr_1.to_string()),
            qr_expired: false,
            waiting_confirm: None,
            kickout_notice: None,
            error_notice: None,
        }
    );

    // (b) `"invalid": true` flips `qr_expired = true` and emits `"phase"` in `changed`.
    let changed_inv = state.apply_zstate_patch(&json!({
        "page": "login",
        "qr": png_qr_1,
        "invalid": true,
    }));
    assert_eq!(changed_inv, vec!["phase"]);
    assert!(matches!(
        state.phase,
        EnginePhase::Login {
            qr_expired: true,
            ..
        }
    ));

    // (c) Scanned account sets `waiting_confirm`, and repeated 1.5s DOM poll with the SAME `qr`
    // MUST preserve `waiting_confirm` (and only reset when `qr` actually changes!).
    state.phase = EnginePhase::Login {
        qr: Some(png_qr_1.to_string()),
        qr_expired: false,
        waiting_confirm: Some(WaitingConfirm {
            name: "Nguyễn Khoa".to_string(),
            avatar: Some("https://ava.zdn.vn/nk.jpg".to_string()),
        }),
        kickout_notice: None,
        error_notice: None,
    };
    let poll_same_qr = state.apply_zstate_patch(&json!({
        "page": "login",
        "qr": png_qr_1,
    }));
    assert!(
        poll_same_qr.is_empty(),
        "repeated login poll with identical QR must not clobber waiting_confirm"
    );
    assert_eq!(
        state.phase,
        EnginePhase::Login {
            qr: Some(png_qr_1.to_string()),
            qr_expired: false,
            waiting_confirm: Some(WaitingConfirm {
                name: "Nguyễn Khoa".to_string(),
                avatar: Some("https://ava.zdn.vn/nk.jpg".to_string()),
            }),
            kickout_notice: None,
            error_notice: None,
        }
    );

    // A genuinely new QR resets `waiting_confirm`, `qr_expired`, and `error_notice`.
    let png_qr_2 = "data:image/png;base64,NEW_QR_DATA_222";
    let changed_new_qr = state.apply_zstate_patch(&json!({
        "page": "login",
        "qr": png_qr_2,
    }));
    assert_eq!(changed_new_qr, vec!["phase"]);
    assert_eq!(
        state.phase,
        EnginePhase::Login {
            qr: Some(png_qr_2.to_string()),
            qr_expired: false,
            waiting_confirm: None,
            kickout_notice: None,
            error_notice: None,
        }
    );
}

#[test]
fn gst_video_sink_overrides_and_staged_scaletempo_deinterlace_and_zfile_plugin() {
    use olaz_linux::{
        compute_linux_gst_video_sink_overrides, ensure_olaz_gst_plugin_staged, olaz_gst_plugin_dir,
    };

    let overrides = compute_linux_gst_video_sink_overrides(
        None,
        Some(OsStr::new("1")),
        None,
        None,
    );
    assert_eq!(
        overrides,
        vec![
            ("WEBKIT_GST_DMABUF_SINK_DISABLED", "1"),
            ("WEBKIT_GST_DISABLE_GL_SINK", "1"),
        ]
    );

    let hwaccel_overrides = compute_linux_gst_video_sink_overrides(
        Some(OsStr::new("0")),
        Some(OsStr::new("0")),
        None,
        None,
    );
    assert!(hwaccel_overrides.is_empty());

    let preconfigured = compute_linux_gst_video_sink_overrides(
        Some(OsStr::new("1")),
        None,
        Some(OsStr::new("0")),
        None,
    );
    assert_eq!(preconfigured, vec![("WEBKIT_GST_DMABUF_SINK_DISABLED", "1")]);

    let staged_dir =
        ensure_olaz_gst_plugin_staged().expect("libgstolazauto.so must stage cleanly");
    assert!(staged_dir.join("libgstolazauto.so").is_file());

    if let Ok(inspect_status) = std::process::Command::new("gst-inspect-1.0")
        .arg("--version")
        .output()
    {
        if inspect_status.status.success() {
            let gst_dir = olaz_gst_plugin_dir();
            let tmp_reg = TempDir::new().unwrap();
            let reg_file = tmp_reg.path().join("registry.bin");

            for (elem, needle) in [
                ("scaletempo", "Filter/Effect/Audio"),
                ("olazmediasrc", "zfile"),
                ("olazavdemux", "Codec/Demuxer"),
            ] {
                let out = std::process::Command::new("gst-inspect-1.0")
                    .env("GST_PLUGIN_PATH_1_0", &gst_dir)
                    .env("GST_REGISTRY_1_0", &reg_file)
                    .env("OLAZ_GST_FORCE_SCALETEMPO", "1")
                    .arg(elem)
                    .output()
                    .expect("run gst-inspect-1.0");
                let text = String::from_utf8_lossy(&out.stdout);
                assert!(
                    out.status.success() && text.contains(needle),
                    "gst-inspect-1.0 {elem} failed (status={:?}): stdout={text} stderr={}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }
}
