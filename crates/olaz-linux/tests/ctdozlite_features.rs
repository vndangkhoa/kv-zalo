//! Comprehensive integration tests for Ctdozlite power features in `olaz-linux`:
//! - Profile-scoped Unix domain socket instance lock, `zalo://` deep-link forwarding, and IPC JSON-RPC
//! - Atomic `PrivacyConfig` persistence and WebView `window.__olazSetPrivacy` init script
//! - Persistent `SchedulerEngine` (presets, overdue startup catch-up, `spawn_automation_loop`)
//! - Persistent `AutoReplyEngine` (Vietnamese accent folding, self-message guard, per-`(rule, conv)` cooldown)
//! - Loopback HTTP REST API (`127.0.0.1` + Bearer auth) and MCP JSON-RPC 2.0 server (`EngineControlBackend` & `SocketRpcBackend`)
//! - GitHub + Forgejo fallback release updater with `SHA256SUMS.txt` parsing and `sha2::Sha256` verification

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use olaz_linux::{
    acquire_or_notify_in_dir, check_for_updates_from_urls, compute_sha256_hex,
    download_and_verify_artifact, handle_mcp_jsonrpc, install_release, instance_socket_path,
    instance_socket_path_in_dir, is_newer_semver, make_instance_rpc_handler, parse_schedule_time_ms,
    parse_sha256sums_for_asset, run_mcp_io, send_instance_rpc_in_dir, spawn_automation_loop,
    spawn_local_api_server_with_token, verify_sha256_bytes, AcquireProfileInstanceOutcome,
    AutoReplyEngine, AutoReplyRule, ChatMessage, Conversation, ConversationPreview, DownloadLimits,
    EngineControlBackend, EnginePhase, InstallTarget, MatchMode, MessageStatus,
    OlazControlBackend, PrivacyConfig, ProfileInstanceEvent, ReleaseSource, ScheduleStatus,
    ScheduledMessage, SchedulerEngine, SocketRpcBackend, UserProfile, ZaloEngineState,
    CURRENT_OLAZ_VERSION, MCP_PROTOCOL_VERSION,
};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

// ============================================================================
// 1. Profile-Scoped Instance Lock, Deep-Link Forwarding & Unix Socket RPC
// ============================================================================

#[tokio::test]
async fn profile_scoped_sockets_isolate_profiles_forward_deeplinks_and_serve_rpc() {
    let tmp = TempDir::new().unwrap();
    let base_dir = tmp.path().join("olaz");

    // Default / empty profile preserves exact `olaz-instance.sock` path
    assert_eq!(
        instance_socket_path_in_dir(&base_dir, ""),
        instance_socket_path(&base_dir)
    );
    assert_eq!(
        instance_socket_path_in_dir(&base_dir, "default"),
        base_dir.join("olaz-instance.sock")
    );
    assert_eq!(
        instance_socket_path_in_dir(&base_dir, "work-2"),
        base_dir.join("olaz-instance-work-2.sock")
    );

    // Acquire BOTH `default` and `work` primary instances simultaneously without collision
    let (guard_default, mut rx_default) =
        match acquire_or_notify_in_dir(&base_dir, "default", false, None).unwrap() {
            AcquireProfileInstanceOutcome::Primary(g, r) => (g, r),
            AcquireProfileInstanceOutcome::ForwardedToPrimary => {
                panic!("expected primary for default profile")
            }
        };
    let (guard_work, mut rx_work) =
        match acquire_or_notify_in_dir(&base_dir, "work", true, None).unwrap() {
            AcquireProfileInstanceOutcome::Primary(g, r) => (g, r),
            AcquireProfileInstanceOutcome::ForwardedToPrimary => {
                panic!("expected independent primary for work profile")
            }
        };

    // Secondary invocation on `default` with `zalo://` deep link forwards `OpenDeepLink`
    let deeplink_url = "zalo://conversation?id=987654321";
    assert!(matches!(
        acquire_or_notify_in_dir(&base_dir, "default", false, Some(deeplink_url)).unwrap(),
        AcquireProfileInstanceOutcome::ForwardedToPrimary
    ));
    assert_eq!(
        rx_default.recv().await,
        Some(ProfileInstanceEvent::OpenDeepLink(deeplink_url.to_string()))
    );

    // Secondary `--minimized` invocation on `work` forwards `BackgroundPing`
    assert!(matches!(
        acquire_or_notify_in_dir(&base_dir, "work", true, None).unwrap(),
        AcquireProfileInstanceOutcome::ForwardedToPrimary
    ));
    assert_eq!(rx_work.recv().await, Some(ProfileInstanceEvent::BackgroundPing));

    // Register an async RPC handler on `work` and invoke `send_instance_rpc_in_dir`
    guard_work.set_rpc_handler(Arc::new(|req| {
        Box::pin(async move {
            let echo = req.get("hello").and_then(Value::as_str).unwrap_or("");
            json!({
                "ok": true,
                "reply": format!("pong:{echo}"),
            })
        })
    }));

    let base_for_rpc = base_dir.clone();
    let rpc_resp = tokio::task::spawn_blocking(move || {
        send_instance_rpc_in_dir(&base_for_rpc, "work", &json!({ "hello": "ctdozlite" }))
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rpc_resp["ok"], true);
    assert_eq!(rpc_resp["reply"], "pong:ctdozlite");

    drop(guard_default);
    drop(guard_work);
}

// ============================================================================
// 2. PrivacyConfig Persistence & JS Init Script
// ============================================================================

#[test]
fn privacy_config_atomic_persistence_and_js_init_script() {
    let tmp = TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("config/profiles/work");

    // Missing file defaults to all false
    let mut cfg = PrivacyConfig::load_from(&cfg_dir);
    assert_eq!(
        cfg,
        PrivacyConfig {
            block_typing: false,
            block_seen: false,
            block_delivered: false,
        }
    );

    cfg.apply_patch(&json!({
        "blockTyping": true,
        "block_seen": true,
    }));
    cfg.save_to(&cfg_dir).unwrap();

    let file_path = PrivacyConfig::file_path(&cfg_dir);
    assert!(file_path.exists());
    assert_eq!(
        std::fs::metadata(&file_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let reloaded = PrivacyConfig::load_from(&cfg_dir);
    assert!(reloaded.block_typing);
    assert!(reloaded.block_seen);
    assert!(!reloaded.block_delivered);

    let js = reloaded.to_js_init_script();
    assert!(js.contains("window.__olazSetPrivacy && window.__olazSetPrivacy("));
    assert!(js.contains("\"block_typing\":true"));
    assert!(js.contains("\"blockSeen\":true"));
    assert!(js.contains("\"block_delivered\":false"));
}

// ============================================================================
// 3. Persistent Message Scheduler & Background Automation Loop
// ============================================================================

#[tokio::test]
async fn scheduler_presets_persistence_overdue_catchup_and_automation_loop() {
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path();
    let now_ms = 1_762_000_000_000_i64;

    // Relative & clock presets
    assert_eq!(
        parse_schedule_time_ms("+15m", now_ms).unwrap(),
        now_ms + 15 * 60_000
    );
    assert_eq!(
        parse_schedule_time_ms("+1h", now_ms).unwrap(),
        now_ms + 3_600_000
    );
    let at_18 = parse_schedule_time_ms("18:00", now_ms).unwrap();
    assert!(at_18 > now_ms && at_18 <= now_ms + 86_400_000);
    assert!(parse_schedule_time_ms("-10m", now_ms).is_err());

    // Create & persist scheduled messages, plus an overdue item simulating downtime
    {
        let sched = SchedulerEngine::load_from(data_dir);
        let m1 = sched
            .schedule(
                "conv_1",
                Some("Nguyễn Văn A".into()),
                "Báo cáo sáng",
                now_ms + 5_000,
                now_ms,
            )
            .unwrap();
        let m2 = sched
            .schedule("conv_2", None, "Hủy trước khi gửi", now_ms + 60_000, now_ms)
            .unwrap();
        assert!(sched.cancel(&m2.id).unwrap());

        // Simulate an overdue pending item saved before app shutdown
        sched
            .insert_record(ScheduledMessage {
                id: "overdue_1".into(),
                conv_id: "conv_overdue".into(),
                target_name: Some("Khách hàng".into()),
                text: "Tin nhắn trễ giờ khi máy tắt".into(),
                send_at_ms: now_ms - 30_000,
                status: ScheduleStatus::Pending,
                created_at_ms: now_ms - 60_000,
                sent_at_ms: None,
                last_error: None,
            })
            .unwrap();

        assert_eq!(sched.list_pending(None).len(), 2);
        assert_eq!(m1.status, ScheduleStatus::Pending);
    }

    // Reload from disk and verify overdue catch-up + background Tokio runner
    let sched = Arc::new(SchedulerEngine::load_from(data_dir));
    let overdue_on_boot = sched.due_messages(now_ms);
    assert_eq!(overdue_on_boot.len(), 1);
    assert_eq!(overdue_on_boot[0].id, "overdue_1");

    // Schedule an item due in +40ms from real wall-clock time so `spawn_automation_loop` delivers both
    let real_now = olaz_linux::unix_now_ms();
    let live_item = sched
        .schedule(
            "conv_live",
            Some("Nhóm Dev".into()),
            "Ping tự động",
            real_now + 40,
            real_now,
        )
        .unwrap();

    let delivered: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let delivered_cb = Arc::clone(&delivered);

    let loop_handle = spawn_automation_loop(
        Arc::clone(&sched),
        Duration::from_millis(25),
        move |conv_id: String, text: String| {
            let delivered_cb = Arc::clone(&delivered_cb);
            async move {
                if conv_id == "conv_fail" {
                    anyhow::bail!("simulated network failure");
                }
                delivered_cb.lock().push((conv_id, text));
                Ok(())
            }
        },
    );

    // Wait briefly for the interval runner to deliver both `overdue_1`, `conv_1` (since `now_ms < real_now`), and `live_item`
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if delivered.lock().len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("automation loop should deliver overdue and due scheduled messages");

    loop_handle.stop();

    // Verify persisted Sent status
    let reloaded_after = SchedulerEngine::load_from(data_dir);
    let all = reloaded_after.list(None);
    let live_saved = all.iter().find(|m| m.id == live_item.id).unwrap();
    assert_eq!(live_saved.status, ScheduleStatus::Sent);
    assert!(live_saved.sent_at_ms.is_some());

    // Verify `mark_failed` records error cleanly
    let fail_msg = reloaded_after
        .schedule("conv_fail", None, "Thử lỗi", real_now + 500, real_now)
        .unwrap();
    assert!(reloaded_after
        .mark_failed(&fail_msg.id, "socket disconnected")
        .unwrap());
    let failed_entry = reloaded_after
        .list(Some("conv_fail"))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(failed_entry.status, ScheduleStatus::Failed);
    assert_eq!(
        failed_entry.last_error.as_deref(),
        Some("socket disconnected")
    );
}

// ============================================================================
// 4. Keyword / Away Auto-Reply Engine (Vietnamese Folding, Cooldown, Self Guard)
// ============================================================================

#[test]
fn autoreply_vietnamese_folding_cooldown_and_self_message_guard() {
    let tmp = TempDir::new().unwrap();
    let engine = AutoReplyEngine::load_from(tmp.path());

    engine
        .upsert_rule(AutoReplyRule {
            id: "r_price".into(),
            enabled: true,
            match_mode: MatchMode::Contains,
            keyword: "Đơn hàng báo giá".into(),
            reply_text: "Cảm ơn bạn! Bảng giá đã được gửi.".into(),
            cooldown_secs: 30,
            scope_conv_id: None,
            allow_groups: false,
        })
        .unwrap();

    engine
        .upsert_rule(AutoReplyRule {
            id: "r_away".into(),
            enabled: true,
            match_mode: MatchMode::Any,
            keyword: String::new(),
            reply_text: "Mình đang bận, lát mình phản hồi nhé.".into(),
            cooldown_secs: 60,
            scope_conv_id: Some("vip_conv".into()),
            allow_groups: true,
        })
        .unwrap();

    let t0 = 1_762_000_000_000_i64;

    // 1. Self-message guard (`is_mine = true` or `sender_id == self_user_id`) MUST NEVER trigger
    assert!(engine
        .evaluate_incoming(
            "u_1",
            "Cho xin đơn hàng báo giá",
            true,
            false,
            Some("me_123"),
            Some("me_123"),
            t0,
        )
        .is_none());
    assert!(engine
        .evaluate_incoming(
            "u_1",
            "Cho xin đơn hàng báo giá",
            false,
            false,
            Some("me_123"),
            Some("me_123"),
            t0,
        )
        .is_none());

    // 2. Group chat blocked when `allow_groups == false`
    assert!(engine
        .evaluate_incoming(
            "g_1",
            "don hang bao gia",
            false,
            true,
            Some("peer_1"),
            Some("me_123"),
            t0,
        )
        .is_none());

    // 3. Unaccented Vietnamese incoming message matches accented rule keyword!
    let hit = engine
        .evaluate_incoming(
            "u_1",
            "Shop oi cho minh xem DON HANG BAO GIA nhe!",
            false,
            false,
            Some("peer_1"),
            Some("me_123"),
            t0,
        )
        .expect("accent-insensitive Vietnamese keyword should match");
    assert_eq!(hit.id, "r_price");

    // 4. Cooldown blocks immediate repeat in the SAME conversation (`u_1` at t0 + 10s < 30s)
    assert!(engine
        .evaluate_incoming(
            "u_1",
            "don hang bao gia lan 2",
            false,
            false,
            Some("peer_1"),
            Some("me_123"),
            t0 + 10_000,
        )
        .is_none());

    // 5. Different conversation (`u_2`) is NOT blocked by `u_1`'s cooldown!
    assert!(engine
        .evaluate_incoming(
            "u_2",
            "đơn hàng báo giá",
            false,
            false,
            Some("peer_2"),
            Some("me_123"),
            t0 + 10_000,
        )
        .is_some());

    // 6. After 31s (`> 30s` cooldown), `u_1` triggers again
    assert!(engine
        .evaluate_incoming(
            "u_1",
            "don hang bao gia lan 3",
            false,
            false,
            Some("peer_1"),
            Some("me_123"),
            t0 + 31_000,
        )
        .is_some());

    // 7. Scoped `MatchMode::Any` away rule triggers on `vip_conv` (even in group), not on `other_conv`
    assert!(engine
        .evaluate_incoming(
            "other_conv",
            "Alo bạn ơi",
            false,
            false,
            Some("peer_3"),
            Some("me_123"),
            t0,
        )
        .is_none());
    let away_hit = engine
        .evaluate_incoming(
            "vip_conv",
            "Alo nhóm ơi",
            false,
            true,
            Some("peer_3"),
            Some("me_123"),
            t0,
        )
        .expect("scoped away rule should match vip_conv");
    assert_eq!(away_hit.id, "r_away");

    // Reload from disk preserves rules
    let reloaded = AutoReplyEngine::load_from(tmp.path());
    assert_eq!(reloaded.rules().len(), 2);
}

// ============================================================================
// 5. Local HTTP REST API, Bearer Auth, MCP JSON-RPC 2.0 & SocketRpcBackend
// ============================================================================

fn sample_engine_state() -> Arc<RwLock<ZaloEngineState>> {
    let mut state = ZaloEngineState::new();
    state.phase = EnginePhase::Ready;
    state.user = Some(UserProfile {
        user_id: "u_self_99".into(),
        display_name: "Khoa Võ".into(),
        zalo_name: Some("Khoa".into()),
        avatar: None,
        phone: None,
        raw: json!({}),
    });
    state.conv_actions.loaded = true;
    state.conv_actions.hidden_known = true;
    state.conv_actions.hidden_ids.insert("secret_conv".into());

    state.conversations = vec![
        Conversation {
            id: "conv_alpha".into(),
            is_group: false,
            name: "Trần Thị B".into(),
            avatar: None,
            pinned: true,
            members: 2,
            unread: 3,
            preview: Some(ConversationPreview {
                text: "Chào buổi sáng".into(),
                timestamp: Some(1_762_000_000_000),
                from_me: false,
            }),
            raw: json!({}),
        },
        Conversation {
            id: "secret_conv".into(),
            is_group: false,
            name: "Hội thoại ẩn PIN".into(),
            avatar: None,
            pinned: false,
            members: 2,
            unread: 5,
            preview: None,
            raw: json!({}),
        },
    ];

    state.messages.insert(
        "conv_alpha".into(),
        vec![ChatMessage {
            id: "msg_1".into(),
            msg_id: Some("msg_1".into()),
            cli_msg_id: None,
            conversation_id: "conv_alpha".into(),
            sender_id: "conv_alpha".into(),
            sender_name: "Trần Thị B".into(),
            sender_avatar: None,
            timestamp: 1_762_000_000_000,
            is_mine: false,
            status: MessageStatus::Seen,
            kind: "text".into(),
            text: "Chào buổi sáng".into(),
            mentions: Vec::new(),
            quote: None,
            reactions: Vec::new(),
            media: None,
            file: None,
            sticker: None,
            poll: None,
            card: None,
            encrypted: false,
            recalled: false,
            starred: false,
            raw: json!({}),
        }],
    );

    Arc::new(RwLock::new(state))
}

#[tokio::test]
async fn rest_api_endpoints_bearer_auth_mcp_stdio_and_socket_rpc_bridge() {
    let tmp = TempDir::new().unwrap();
    let base_dir = tmp.path().join("olaz");
    let config_dir = tmp.path().join("config");

    let sent_log: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let call_log: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    let sent_for_cb = Arc::clone(&sent_log);
    let call_for_cb = Arc::clone(&call_log);

    let scheduler = Arc::new(SchedulerEngine::load_from(&config_dir));
    let autoreply = Arc::new(AutoReplyEngine::load_from(&config_dir));
    let backend: Arc<dyn OlazControlBackend> = Arc::new(
        EngineControlBackend::new(
            sample_engine_state(),
            Arc::clone(&scheduler),
            Arc::clone(&autoreply),
            &config_dir,
        )
        .with_profile_slug("work")
        .with_send_callback(move |cid: String, text: String| {
            let sent_for_cb = Arc::clone(&sent_for_cb);
            async move {
                sent_for_cb.lock().push((cid, text));
                Ok(())
            }
        })
        .with_call_callback(Arc::new(move |target_id: String, video: bool| {
            let call_for_cb = Arc::clone(&call_for_cb);
            Box::pin(async move {
                call_for_cb.lock().push((target_id.clone(), video));
                Ok(json!({ "call_id": "call_777", "target_id": target_id, "video": video }))
            })
        })),
    );

    // 1. Spawn loopback HTTP REST + MCP server on ephemeral port with Bearer token
    let server = spawn_local_api_server_with_token(
        Arc::clone(&backend),
        0,
        Some("super-secret-token".into()),
    )
    .await
    .unwrap();
    let base_url = format!("http://{}", server.local_addr());
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // Missing or wrong Bearer token -> 401 Unauthorized
    let unauth = http
        .get(format!("{base_url}/api/v1/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status().as_u16(), 401);

    let wrong_tok = http
        .get(format!("{base_url}/api/v1/status"))
        .bearer_auth("wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_tok.status().as_u16(), 401);

    // Valid Bearer token -> GET /api/v1/status
    let status_json: Value = http
        .get(format!("{base_url}/api/v1/status"))
        .bearer_auth("super-secret-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status_json["ok"], true);
    assert_eq!(status_json["profile"], "work");
    assert_eq!(status_json["logged_in"], true);
    assert_eq!(status_json["conversation_count"], 1); // `secret_conv` filtered out!

    // GET /api/v1/threads & GET /api/v1/messages (including PIN-locked conversation rejection)
    let threads_json: Value = http
        .get(format!("{base_url}/api/v1/threads"))
        .bearer_auth("super-secret-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(threads_json["threads"].as_array().unwrap().len(), 1);
    assert_eq!(threads_json["threads"][0]["id"], "conv_alpha");

    let msgs_json: Value = http
        .get(format!(
            "{base_url}/api/v1/messages?conv_id=conv_alpha&limit=10"
        ))
        .bearer_auth("super-secret-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(msgs_json["messages"][0]["text"], "Chào buổi sáng");

    let locked_resp = http
        .get(format!("{base_url}/api/v1/messages?conv_id=secret_conv"))
        .bearer_auth("super-secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(locked_resp.status().as_u16(), 400);

    // POST /api/v1/send
    let send_resp: Value = http
        .post(format!("{base_url}/api/v1/send"))
        .bearer_auth("super-secret-token")
        .json(&json!({ "conv_id": "conv_alpha", "text": "Xin chào từ REST" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(send_resp["ok"], true);
    assert_eq!(
        sent_log.lock().as_slice(),
        &[("conv_alpha".to_string(), "Xin chào từ REST".to_string())]
    );

    // POST, GET, DELETE /api/v1/schedule
    let sched_create: Value = http
        .post(format!("{base_url}/api/v1/schedule"))
        .bearer_auth("super-secret-token")
        .json(&json!({
            "conv_id": "conv_alpha",
            "target_name": "Trần Thị B",
            "text": "Hẹn 15 phút nữa",
            "when": "+15m"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sched_id = sched_create["scheduled"]["id"].as_str().unwrap().to_string();

    let sched_list: Value = http
        .get(format!("{base_url}/api/v1/schedule"))
        .bearer_auth("super-secret-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sched_list["scheduled"].as_array().unwrap().len(), 1);

    let sched_del: Value = http
        .delete(format!("{base_url}/api/v1/schedule"))
        .bearer_auth("super-secret-token")
        .json(&json!({ "id": sched_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sched_del["cancelled"], true);

    // POST & GET /api/v1/autoreply
    let ar_post: Value = http
        .post(format!("{base_url}/api/v1/autoreply"))
        .bearer_auth("super-secret-token")
        .json(&json!({
            "action": "upsert",
            "id": "rule_hello",
            "keyword": "xin chào",
            "reply_text": "Chào bạn!",
            "match_mode": "prefix",
            "cooldown_secs": 45
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ar_post["ok"], true);
    let ar_get: Value = http
        .get(format!("{base_url}/api/v1/autoreply"))
        .bearer_auth("super-secret-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ar_get["rules"].as_array().unwrap().len(), 1);

    // POST & GET /api/v1/privacy
    let priv_post: Value = http
        .post(format!("{base_url}/api/v1/privacy"))
        .bearer_auth("super-secret-token")
        .json(&json!({ "block_typing": true, "block_delivered": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(priv_post["privacy"]["block_typing"], true);
    assert_eq!(priv_post["privacy"]["block_delivered"], true);

    let priv_get: Value = http
        .get(format!("{base_url}/api/v1/privacy"))
        .bearer_auth("super-secret-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(priv_get["privacy"]["block_typing"], true);

    // POST /api/v1/call
    let call_resp: Value = http
        .post(format!("{base_url}/api/v1/call"))
        .bearer_auth("super-secret-token")
        .json(&json!({ "target_id": "conv_alpha", "video": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(call_resp["ok"], true);
    assert_eq!(call_resp["call"]["call_id"], "call_777");
    assert_eq!(call_log.lock().as_slice(), &[("conv_alpha".to_string(), true)]);

    // POST /api/v1/mcp (HTTP MCP JSON-RPC 2.0)
    let mcp_http: Value = http
        .post(format!("{base_url}/api/v1/mcp"))
        .bearer_auth("super-secret-token")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        mcp_http["result"]["protocolVersion"],
        MCP_PROTOCOL_VERSION
    );

    server.stop();

    // 2. Test `SocketRpcBackend` + `run_mcp_io` (simulating `olaz --mcp` stdio talking to primary GUI over Unix socket!)
    let (primary_guard, _primary_rx) =
        match acquire_or_notify_in_dir(&base_dir, "work", false, None).unwrap() {
            AcquireProfileInstanceOutcome::Primary(g, r) => (g, r),
            AcquireProfileInstanceOutcome::ForwardedToPrimary => panic!("expected primary"),
        };
    primary_guard.set_rpc_handler(make_instance_rpc_handler(Arc::clone(&backend)));

    let proxy_backend: Arc<dyn OlazControlBackend> =
        Arc::new(SocketRpcBackend::for_profile_in_dir(&base_dir, "work"));

    // Verify notification returns `None`
    assert!(handle_mcp_jsonrpc(
        &proxy_backend,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .await
    .is_none());

    // Stream multiple JSON-RPC requests through `run_mcp_io`
    let mcp_input = [
        json!({ "jsonrpc": "2.0", "id": 10, "method": "ping" }).to_string(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
        json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/list" }).to_string(),
        json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "zalo_send_message",
                "arguments": { "conv_id": "conv_alpha", "text": "Gửi từ MCP qua Unix socket!" }
            }
        })
        .to_string(),
        json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "tools/call",
            "params": {
                "name": "zalo_set_privacy",
                "arguments": { "block_seen": true }
            }
        })
        .to_string(),
    ]
    .join("\n")
        + "\n";

    let mut mcp_output = Vec::new();
    run_mcp_io(
        Arc::clone(&proxy_backend),
        BufReader::new(mcp_input.as_bytes()),
        &mut mcp_output,
    )
    .await
    .unwrap();

    let out_lines: Vec<Value> = String::from_utf8(mcp_output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect();
    // Notification produced no output -> exactly 4 JSON-RPC responses
    assert_eq!(out_lines.len(), 4);
    assert_eq!(out_lines[0]["id"], 10);
    let tool_names: Vec<&str> = out_lines[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    for expected_tool in [
        "zalo_status",
        "zalo_list_threads",
        "zalo_get_messages",
        "zalo_send_message",
        "zalo_schedule_message",
        "zalo_list_scheduled",
        "zalo_cancel_scheduled",
        "zalo_set_autoreply",
        "zalo_set_privacy",
        "zalo_start_call",
    ] {
        assert!(
            tool_names.contains(&expected_tool),
            "missing MCP tool {expected_tool}"
        );
    }
    assert_eq!(out_lines[2]["result"]["isError"], false);
    assert_eq!(out_lines[3]["result"]["isError"], false);

    assert_eq!(sent_log.lock().len(), 2);
    assert!(PrivacyConfig::load_from(&config_dir).block_seen);

    drop(primary_guard);
}

// ============================================================================
// 6. Release Updater: GitHub -> Forgejo Fallback, SHA256SUMS & SHA-256 Verify
// ============================================================================

#[tokio::test]
async fn updater_github_to_forgejo_fallback_sha256sums_and_transactional_install() {
    assert!(is_newer_semver("v99.0.0", CURRENT_OLAZ_VERSION));
    assert!(is_newer_semver("v0.2.0", "0.1.0"));
    assert!(!is_newer_semver(CURRENT_OLAZ_VERSION, CURRENT_OLAZ_VERSION));
    assert!(is_newer_semver("0.2.0", "0.2.0-rc.1"));

    // Prepare fake ELF AppImage payload + real SHA-256 digest
    let mut fake_elf = b"\x7fELF".to_vec();
    fake_elf.extend_from_slice(&vec![0xAB_u8; 2048]);
    let artifact_sha = compute_sha256_hex(&fake_elf);
    verify_sha256_bytes(&fake_elf, &artifact_sha).unwrap();
    assert!(verify_sha256_bytes(
        &fake_elf,
        "0000000000000000000000000000000000000000000000000000000000000000"
    )
    .is_err());

    let asset_filename = format!("olaz-0.9.0-{}.AppImage", std::env::consts::ARCH);
    let sums_body = format!("{artifact_sha}  {asset_filename}\n");
    assert_eq!(
        parse_sha256sums_for_asset(&sums_body, &asset_filename).as_deref(),
        Some(artifact_sha.as_str())
    );

    // Mock GitHub server returning 503 Service Unavailable
    let gh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gh_addr = gh_listener.local_addr().unwrap();
    let gh_hits = Arc::new(AtomicUsize::new(0));
    let gh_hits_task = Arc::clone(&gh_hits);
    tokio::spawn(async move {
        if let Ok((mut s, _)) = gh_listener.accept().await {
            gh_hits_task.fetch_add(1, Ordering::SeqCst);
            let mut tmp_buf = [0u8; 1024];
            let _ = s.read(&mut tmp_buf).await;
            let _ = s
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
    });

    // Mock Forgejo server serving `/releases/latest`, `/SHA256SUMS.txt`, and `/artifact`
    let fg_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fg_addr = fg_listener.local_addr().unwrap();
    let asset_name_srv = asset_filename.clone();
    let sums_srv = sums_body.clone();
    let elf_srv = fake_elf.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = fg_listener.accept().await else {
                break;
            };
            let asset_name_srv = asset_name_srv.clone();
            let sums_srv = sums_srv.clone();
            let elf_srv = elf_srv.clone();
            tokio::spawn(async move {
                let mut req_buf = [0u8; 2048];
                let Ok(n) = s.read(&mut req_buf).await else {
                    return;
                };
                let req_str = String::from_utf8_lossy(&req_buf[..n]);
                let path = req_str
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");

                if path == "/releases/latest" {
                    let body = json!({
                        "tag_name": "v0.9.0",
                        "name": "Olaz v0.9.0",
                        "body": "Forgejo fallback release",
                        "html_url": format!("http://{fg_addr}/release/v0.9.0"),
                        "published_at": "2026-11-09T10:00:00Z",
                        "assets": [
                            {
                                "name": "SHA256SUMS.txt",
                                "size": sums_srv.len(),
                                "browser_download_url": format!("http://{fg_addr}/SHA256SUMS.txt")
                            },
                            {
                                "name": asset_name_srv,
                                "size": elf_srv.len(),
                                "browser_download_url": format!("http://{fg_addr}/download/appimage")
                            }
                        ]
                    })
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                } else if path == "/SHA256SUMS.txt" {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        sums_srv.len(),
                        sums_srv
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                } else if path == "/download/appimage" {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        elf_srv.len()
                    );
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&elf_srv).await;
                }
            });
        }
    });

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let info = check_for_updates_from_urls(
        &client,
        &format!("http://{gh_addr}/releases/latest"),
        &format!("http://{fg_addr}/releases/latest"),
        "0.1.0",
    )
    .await
    .unwrap();

    assert_eq!(gh_hits.load(Ordering::SeqCst), 1);
    assert_eq!(info.source, ReleaseSource::Forgejo);
    assert!(info.has_update);
    assert_eq!(info.latest_version, "0.9.0");
    assert_eq!(info.asset_name.as_deref(), Some(asset_filename.as_str()));
    assert_eq!(
        info.expected_sha256.as_deref(),
        Some(artifact_sha.as_str())
    );

    // Download & verify SHA-256 (and ensure wrong SHA-256 fails!)
    let limits = DownloadLimits {
        min_bytes: 64,
        max_bytes: 10 * 1024 * 1024,
        timeout: Duration::from_secs(10),
    };
    let dl_url = info.download_url.as_deref().unwrap();

    assert!(download_and_verify_artifact(
        &client,
        dl_url,
        &asset_filename,
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        limits,
        None,
    )
    .await
    .is_err());

    let tmp = TempDir::new().unwrap();
    let downloaded_path = tmp.path().join("downloaded.AppImage");
    let verified_bytes = download_and_verify_artifact(
        &client,
        dl_url,
        &asset_filename,
        &artifact_sha,
        limits,
        Some(&downloaded_path),
    )
    .await
    .unwrap();
    assert_eq!(verified_bytes, fake_elf);
    assert_eq!(std::fs::read(&downloaded_path).unwrap(), fake_elf);

    // Transactional portable replacement + rollback verification
    let target_bin = tmp.path().join("olaz-portable");
    std::fs::write(&target_bin, b"old-binary").unwrap();

    // Failed launcher rolls back to `old-binary`
    assert!(install_release(
        &verified_bytes,
        &InstallTarget::Portable(target_bin.clone()),
        |_| Err("simulated exec failure".into()),
    )
    .is_err());
    assert_eq!(std::fs::read(&target_bin).unwrap(), b"old-binary");

    // Successful launcher commits replacement
    install_release(
        &verified_bytes,
        &InstallTarget::Portable(target_bin.clone()),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(std::fs::read(&target_bin).unwrap(), verified_bytes);
}
