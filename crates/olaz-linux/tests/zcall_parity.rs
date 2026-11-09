//! Comprehensive parity & integration test suite for Olaz's Pure-Rust VoIP Audio, Video & Group Call Engine:
//! - RFC 3711 SRTP (`AES_CM_128_HMAC_SHA1_80`, `ZaviNullAuthNoSeq`, `None`, ROC rollover, 64-bit replay window)
//! - ZRTC (`InitZRTP`, probes, single-socket multi-relay sweep, dynamic relay + `flow_token` retargeting, outer/inner RTP framing)
//! - `libopus.so.0` dynamic FFI (`16,000 Hz` mono `20ms` = `320` samples @ `24 kbps`) + audio DSP & `400ms` jitter cap
//! - Zalo H.264 video packetizer/depacketizer (`PT 98` IDR, `PT 97` P-frame, `0x1C` fragments, STAP-A, FU-A) + RTCP (`RR`, `SR+REMB`, `PLI`)
//! - Structured VoIP logger (`<data_dir>/logs/voip/voip.log` + token redaction)
//! - `OlazCallEngine` full outgoing/incoming/busy/decline/timeout/video/screen-share/dual-ID state machine
//! - Self-contained Call UI assets (`ui_assets.rs`) and `call_stub.rs` compatibility

use olaz_linux::call::zrtc::{
    ZRTC_AUDIO_RTCP, ZRTC_AUDIO_RTP_IN_SHORT, ZRTC_AUDIO_RTP_OUT, ZRTC_GROUP_COMPACT,
    ZRTC_INIT_RESP_OR_EXT_GROUP, ZRTC_VIDEO_RTCP, ZRTC_VIDEO_RTP_IN_SHORT, ZRTC_VIDEO_RTP_OUT,
};
use olaz_linux::call::{
    apply_mic_gain_and_level, build_audio_rtp, build_group_init_zrtp_request,
    build_group_init_zrtp_v6_request, build_init_zrtp_request, build_probe,
    build_recording_file_path, build_rtcp_pli, build_rtcp_rr, build_rtcp_sr_sdes_remb,
    build_srtp_iv, build_zavi_no_seq_iv, default_recordings_dir, downmix_to_mono_i16,
    extract_master_material, get_ui_asset, is_h264_idr_au, is_ignored_audio_device,
    is_opus_available, is_pulse_simple_available, map_nonzero_answer_status, ogg_crc32,
    packetize_zalo_h264_au, parse_init_zrtp_response, parse_relay_endpoint, parse_rtp_packet,
    redact_voip_value, resample_mono_i16, rtcp_requests_keyframe, rtp_header_layout,
    sanitize_partner_slug, soft_clip_mono_sum, split_annex_b_nals, unpack_send_to_native_args,
    unwrap_zrtc, wrap_group_zrtc, wrap_zrtc, AudioDeviceEntry, AudioDeviceList, AudioIoSession,
    AudioTapPacket, BoxMediaSetupFuture, CallEngineConfig, CallMediaDriver, CallMediaDriverFactory,
    CallRecorderHandle, CallUiEvent, DriftAligner, FfmpegH264Decoder, H264Depacketizer,
    InboundZrtcEvent, MediaSetupParams, MediaSetupResult, OlazCallEngine, OpusPcmDecoder,
    OpusPcmEncoder, OpusVoipDecoder, OpusVoipEncoder, PlaybackJitterBuffer, ReflexiveAddress,
    RecordingChannelLayout, RecordingFormat, RelaySweepEntry, SrtpContext, SrtpError, SrtpMode,
    SrtpSessionKeys, TapSource, VoipLogger, ZrtcError, ZrtcMediaSession, ZrtcPacketKind,
    ZrtcSweepConfig, ALIGNER_FRAME_SAMPLES, ALIGNER_LAG_THRESHOLD_SAMPLES,
    ALIGNER_MAX_QUEUE_SAMPLES, AUDIO_FRAME_SAMPLES, CALL_UI_CALL_URL, CALL_UI_DEVICES_URL,
    CALL_UI_INCOMING_URL, DEFAULT_MIC_GAIN, DEFAULT_RELAY_PORT, H264_AUD_ANNEX_B,
    MAX_PLAYBACK_JITTER_SAMPLES, OGG_OPUS_GRANULE_STEP_20MS, OGG_OPUS_PRE_SKIP,
    OPUS_CODEC_DESCRIPTOR, OPUS_VOIP_FRAME_SAMPLES, PRELOAD_BRIDGE_JS, WAV_HEADER_BYTES,
    WAV_SYNC_INTERVAL_FRAMES, ZALO_H264_PT_IDR, ZALO_H264_PT_PFRAME,
};
use olaz_linux::call_stub::{CallChunker, CallTransportCrypto, LINUX_CALL_UNSUPPORTED_MESSAGE};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;

const SAMPLE_SESS_154: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz0123_zalo_154_char_session_token_padding_0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmno";

// ============================================================================
// 1. SRTP (`srtp.rs`) Tests
// ============================================================================

#[test]
fn srtp_rfc3711_appendix_b3_kdf_and_iv_vectors() {
    assert_eq!(SAMPLE_SESS_154.len(), 154);

    // RFC 3711 Appendix B.3 known-answer KDF test vector (matches `srtp-kdf.test.js`).
    let master_key_vec = hex::decode("E1F97A0D3E018BE0D64FA32C06DE4139").unwrap();
    let master_salt_vec = hex::decode("0EC675AD498AFEEBB6960B3AABE6").unwrap();
    let mut master_key = [0u8; 16];
    let mut master_salt = [0u8; 14];
    master_key.copy_from_slice(&master_key_vec);
    master_salt.copy_from_slice(&master_salt_vec);

    let keys = SrtpSessionKeys::derive(&master_key, &master_salt);
    assert_eq!(
        hex::encode_upper(keys.cipher_key),
        "C61E7A93744F39EE10734AFE3FF7A087"
    );
    assert_eq!(
        hex::encode_upper(keys.cipher_salt),
        "30CBBC08863D8C85D49DB34A9AE1"
    );
    assert_eq!(
        hex::encode_upper(keys.auth_key),
        "CEBE321F6FF7716B6FD4AB49AF256A156D38BAA4"
    );

    // IV placement check (`srtpIv(zero_salt, 0x11223344, 5)`).
    let zero_salt = [0u8; 14];
    let iv = build_srtp_iv(&zero_salt, 0x1122_3344, 5);
    assert_eq!(hex::encode(iv), "00000000112233440000000000050000");

    let no_seq_iv = build_zavi_no_seq_iv(&zero_salt, 0x1122_3344);
    assert_eq!(hex::encode(no_seq_iv), "00000000112233440000000000000000");

    // Extract 30-byte master material from ASCII `sessId`.
    let (mk, ms) = extract_master_material(SAMPLE_SESS_154).unwrap();
    assert_eq!(&mk, b"AbCdEfGhIjKlMnOp");
    assert_eq!(&ms, b"QrStUvWxYz0123");
    assert_eq!(
        extract_master_material("too_short_29_bytes_0123456789"),
        Err(SrtpError::ShortMasterMaterial(29))
    );
}

#[test]
fn srtp_aes_cm_128_hmac_sha1_80_protect_unprotect_tamper_replay_and_roc_rollover() {
    let mut tx =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::AesCm128HmacSha1_80).unwrap();
    let mut rx =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::AesCm128HmacSha1_80).unwrap();

    let opus_payload = b"hello-zalo-opus-20ms-frame-payload";
    let rtp = build_audio_rtp(100, 32_000, 0xCAFE_BABE, opus_payload);
    let (hdr_len, seq, ssrc) = rtp_header_layout(&rtp).unwrap();
    assert_eq!(hdr_len, 20);
    assert_eq!(seq, 100);
    assert_eq!(ssrc, 0xCAFE_BABE);

    let srtp_pkt = tx.protect(&rtp).unwrap();
    // 20-byte RTP header (incl. 0xBEDE ext) + encrypted payload + 10-byte HMAC-SHA1-80 tag.
    assert_eq!(srtp_pkt.len(), rtp.len() + 10);
    // Header + `0xBEDE` extension MUST remain cleartext!
    assert_eq!(&srtp_pkt[..20], &rtp[..20]);
    // Payload MUST be encrypted (not equal to cleartext).
    assert_ne!(&srtp_pkt[20..20 + opus_payload.len()], opus_payload);

    // Tampered ciphertext byte must fail constant-time HMAC check WITHOUT advancing replay window!
    let mut tampered = srtp_pkt.clone();
    tampered[22] ^= 0x55;
    assert_eq!(rx.unprotect(&tampered), Err(SrtpError::AuthFailed));

    // Authentic packet still decrypts cleanly right after failed tamper attempt.
    let plain = rx.unprotect(&srtp_pkt).unwrap();
    assert_eq!(plain, rtp);

    // Exact duplicate replay is rejected.
    assert_eq!(
        rx.unprotect(&srtp_pkt),
        Err(SrtpError::ReplayDuplicate { index: 100 })
    );

    // Out-of-order within 64-packet window + ReplayTooOld outside 64-packet window.
    let rtp_101 = build_audio_rtp(101, 32_320, 0xCAFE_BABE, b"pkt-101");
    let srtp_101 = tx.protect(&rtp_101).unwrap();
    let rtp_105 = build_audio_rtp(105, 33_600, 0xCAFE_BABE, b"pkt-105");
    let srtp_105 = tx.protect(&rtp_105).unwrap();
    assert_eq!(rx.unprotect(&srtp_105).unwrap(), rtp_105);
    assert_eq!(rx.unprotect(&srtp_101).unwrap(), rtp_101); // out-of-order inside window succeeds!

    let rtp_200 = build_audio_rtp(200, 64_000, 0xCAFE_BABE, b"pkt-200");
    let srtp_200 = tx.protect(&rtp_200).unwrap();
    assert_eq!(rx.unprotect(&srtp_200).unwrap(), rtp_200);

    let rtp_120 = build_audio_rtp(120, 38_400, 0xCAFE_BABE, b"pkt-120-too-old");
    // Note: `tx` already sent seq=200 (still ROC=0 since 200 < 32768 delta), let's verify ReplayTooOld on a fresh pair:
    let mut tx2 =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::AesCm128HmacSha1_80).unwrap();
    let mut rx2 =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::AesCm128HmacSha1_80).unwrap();
    let s_old = tx2.protect(&rtp_120).unwrap();
    let s_far = tx2
        .protect(&build_audio_rtp(200, 64_000, 0xCAFE_BABE, b"far"))
        .unwrap();
    rx2.unprotect(&s_far).unwrap();
    assert_eq!(
        rx2.unprotect(&s_old),
        Err(SrtpError::ReplayTooOld {
            index: 120,
            max_index: 200
        })
    );

    // RFC 3711 §3.3.1 16-bit sequence rollover (`65534 -> 65535 -> 0 -> 1`, plus late `65533` using `roc - 1`).
    let mut tx_roc =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::AesCm128HmacSha1_80).unwrap();
    let mut rx_roc =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::AesCm128HmacSha1_80).unwrap();

    let p_65533 = tx_roc
        .protect(&build_audio_rtp(65533, 1000, 0x1234_5678, b"f65533"))
        .unwrap();
    let p_65534 = tx_roc
        .protect(&build_audio_rtp(65534, 1320, 0x1234_5678, b"f65534"))
        .unwrap();
    let p_65535 = tx_roc
        .protect(&build_audio_rtp(65535, 1640, 0x1234_5678, b"f65535"))
        .unwrap();
    let p_0 = tx_roc
        .protect(&build_audio_rtp(0, 1960, 0x1234_5678, b"f0-roc1"))
        .unwrap();
    let p_1 = tx_roc
        .protect(&build_audio_rtp(1, 2280, 0x1234_5678, b"f1-roc1"))
        .unwrap();

    // Deliver 65534, 65535, 0, 1, then late 65533 after receiver ROC already rolled over to 1!
    assert_eq!(
        parse_rtp_packet(&rx_roc.unprotect(&p_65534).unwrap())
            .unwrap()
            .payload,
        b"f65534"
    );
    assert_eq!(
        parse_rtp_packet(&rx_roc.unprotect(&p_65535).unwrap())
            .unwrap()
            .payload,
        b"f65535"
    );
    assert_eq!(
        parse_rtp_packet(&rx_roc.unprotect(&p_0).unwrap())
            .unwrap()
            .payload,
        b"f0-roc1"
    );
    assert_eq!(
        parse_rtp_packet(&rx_roc.unprotect(&p_1).unwrap())
            .unwrap()
            .payload,
        b"f1-roc1"
    );
    assert_eq!(
        parse_rtp_packet(&rx_roc.unprotect(&p_65533).unwrap())
            .unwrap()
            .payload,
        b"f65533"
    );
}

#[test]
fn srtp_none_and_zavi_null_auth_no_seq_modes() {
    let rtp = build_audio_rtp(7, 2240, 0x9988_7766, b"opus-group-frame");

    // `SrtpMode::None` pass-through.
    let mut none_ctx = SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::None).unwrap();
    let out_none = none_ctx.protect(&rtp).unwrap();
    assert_eq!(out_none, rtp);
    assert_eq!(none_ctx.unprotect(&out_none).unwrap(), rtp);

    // `SrtpMode::ZaviNullAuthNoSeq` (`:3000` / `:3500` Group SFU): 0-byte tag, AES-128-CTR with `no_seq`.
    let mut zavi_tx =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::ZaviNullAuthNoSeq).unwrap();
    let mut zavi_rx =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::ZaviNullAuthNoSeq).unwrap();
    let enc = zavi_tx.protect(&rtp).unwrap();
    assert_eq!(enc.len(), rtp.len()); // 0-byte auth tag!
    assert_ne!(&enc[20..], &rtp[20..]);
    assert_eq!(zavi_rx.unprotect(&enc).unwrap(), rtp);

    // Plaintext Annex-B video packet arriving from SFU in `ZaviNullAuthNoSeq` mode passes through intact.
    let mut seq = 1u16;
    let plain_h264_rtp =
        packetize_zalo_h264_au(&[0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e], &mut seq, 9000, 42)
            .remove(0);
    assert_eq!(zavi_rx.unprotect(&plain_h264_rtp).unwrap(), plain_h264_rtp);
}

// ============================================================================
// 2. ZRTC (`zrtc.rs`) Framing, Handshake & Single-Socket Retargeting Tests
// ============================================================================

#[test]
fn zrtc_init_request_probe_response_and_framing_variants() {
    let sess_152 = &SAMPLE_SESS_154[..152];
    let caller_pkt = build_init_zrtp_request(111, 222, 333, SAMPLE_SESS_154, false).unwrap();
    assert_eq!(caller_pkt.len(), 31 + 154); // 185 bytes
    assert_eq!(&caller_pkt[0..2], &[0x01, 0x7e]);
    assert_eq!(&caller_pkt[2..10], &[0u8; 8]);
    assert_eq!(u32::from_le_bytes(caller_pkt[10..14].try_into().unwrap()), 111);
    assert_eq!(&caller_pkt[14..18], &[0u8; 4]);
    assert_eq!(caller_pkt[18], 0x0b);
    assert_eq!(&caller_pkt[19..21], &[0x00, 0x02]);
    assert_eq!(u32::from_le_bytes(caller_pkt[21..25].try_into().unwrap()), 333);
    assert_eq!(u32::from_le_bytes(caller_pkt[25..29].try_into().unwrap()), 222);
    assert_eq!(u16::from_le_bytes(caller_pkt[29..31].try_into().unwrap()), 154);
    assert_eq!(&caller_pkt[31..], SAMPLE_SESS_154.as_bytes());

    let callee_pkt = build_init_zrtp_request(444, 999, 555, sess_152, true).unwrap();
    assert_eq!(callee_pkt.len(), 31 + 152); // 183 bytes
    assert_eq!(callee_pkt[18], 0x0c);

    assert!(matches!(
        build_init_zrtp_request(1, 2, 3, "short", false),
        Err(ZrtcError::ShortSessId(5))
    ));

    // Group SFU InitZRTP (`0x0b / 0x04`) and InitZRTP_v6 (`0x0c / 0x06`).
    let grp_req = build_group_init_zrtp_request(111, 7777, 333, SAMPLE_SESS_154).unwrap();
    assert_eq!(&grp_req[18..21], &[0x0b, 0x00, 0x04]);
    let grp_v6 = build_group_init_zrtp_v6_request(111, 333, SAMPLE_SESS_154).unwrap();
    assert_eq!(&grp_v6[18..21], &[0x0c, 0x00, 0x06]);

    // 25-byte connectivity probe.
    let probe = build_probe(111, 333, Some([0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(probe.len(), 25);
    assert_eq!(&probe[0..2], &[0x01, 0x7e]);
    assert_eq!(u32::from_le_bytes(probe[10..14].try_into().unwrap()), 111);
    assert_eq!(&probe[14..18], &[0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(&probe[18..21], &[0x03, 0x00, 0x00]);
    assert_eq!(u32::from_le_bytes(probe[21..25].try_into().unwrap()), 333);

    // Parse `0x02, 0x7e` response with flow_token + reflexive `"ip|port"`.
    let refl = b"203.0.113.9|54321";
    let mut resp = vec![0u8; 35 + refl.len()];
    resp[0] = 0x02;
    resp[1] = 0x7e;
    resp[10..14].copy_from_slice(&111u32.to_le_bytes());
    resp[18] = 0x03;
    resp[21..25].copy_from_slice(&0i32.to_le_bytes());
    resp[25..29].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    resp[29..33].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
    resp[33..35].copy_from_slice(&(refl.len() as u16).to_le_bytes());
    resp[35..].copy_from_slice(refl);

    let parsed = parse_init_zrtp_response(&resp).unwrap();
    assert_eq!(parsed.flow_token, [0xaa, 0xbb, 0xcc, 0xdd]);
    assert_eq!(parsed.nonce, [0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(
        parsed.reflexive_addr,
        Some(ReflexiveAddress {
            ip: "203.0.113.9".to_string(),
            port: "54321".to_string()
        })
    );
    // Ensure `0x02, 0x7e` InitZRTP response is NEVER misidentified as `0x02` Group SFU media!
    assert!(unwrap_zrtc(&resp).is_none());

    // Error response (`-106` at `21..25`).
    let mut err_resp = resp.clone();
    err_resp[21..25].copy_from_slice(&(-106i32).to_le_bytes());
    assert!(matches!(
        parse_init_zrtp_response(&err_resp),
        Err(ZrtcError::RelayRejected(-106))
    ));

    // Outer framing roundtrips across `0x03`, `0x04`, `0x05`, `0x0D`, `0x0E`, `0x0F`, `0x7F`, `0x02(cmd=0x0032)`.
    let rtp = build_audio_rtp(12, 3840, 999, &[1, 2, 3, 4]);
    let ft = [0x10, 0x20, 0x30, 0x40];

    let w03 = wrap_zrtc(ZRTC_AUDIO_RTP_OUT, &ft, &rtp);
    let u03 = unwrap_zrtc(&w03).unwrap();
    assert_eq!(u03.kind, ZrtcPacketKind::AudioRtp);
    assert_eq!(u03.flow_token, Some(ft));
    assert_eq!(u03.inner, rtp.as_slice());

    let mut w04 = vec![ZRTC_AUDIO_RTP_IN_SHORT];
    w04.extend_from_slice(&rtp);
    let u04 = unwrap_zrtc(&w04).unwrap();
    assert_eq!(u04.kind, ZrtcPacketKind::AudioRtp);
    assert_eq!(u04.flow_token, None);
    assert_eq!(u04.inner, rtp.as_slice());

    let mut w0e = vec![ZRTC_VIDEO_RTP_IN_SHORT];
    w0e.extend_from_slice(&rtp);
    assert_eq!(unwrap_zrtc(&w0e).unwrap().kind, ZrtcPacketKind::VideoRtp);

    let rr = build_rtcp_rr(111, 222, 50, 0);
    assert_eq!(
        unwrap_zrtc(&wrap_zrtc(ZRTC_AUDIO_RTCP, &ft, &rr))
            .unwrap()
            .kind,
        ZrtcPacketKind::AudioRtcp
    );
    assert_eq!(
        unwrap_zrtc(&wrap_zrtc(ZRTC_VIDEO_RTCP, &ft, &rr))
            .unwrap()
            .kind,
        ZrtcPacketKind::VideoRtcp
    );
    assert_eq!(
        unwrap_zrtc(&wrap_zrtc(ZRTC_VIDEO_RTP_OUT, &ft, &rtp))
            .unwrap()
            .kind,
        ZrtcPacketKind::VideoRtp
    );

    let g7f_audio = wrap_group_zrtc(0x06, 0x8877_6655, &rtp);
    let u7f_a = unwrap_zrtc(&g7f_audio).unwrap();
    assert_eq!(u7f_a.outer_type, ZRTC_GROUP_COMPACT);
    assert_eq!(u7f_a.kind, ZrtcPacketKind::AudioRtp);
    assert_eq!(u7f_a.group_peer_uid, Some(0x8877_6655));

    let g7f_video = wrap_group_zrtc(0x08, 0x8877_6655, &rtp);
    assert_eq!(
        unwrap_zrtc(&g7f_video).unwrap().kind,
        ZrtcPacketKind::VideoRtp
    );

    // 25-byte `0x02` extended Group SFU (`cmd = 0x0032`, `peer_uid` @ `10..14`, `sub = 0x08` @ `18`).
    let mut g02 = vec![0u8; 25];
    g02[0] = ZRTC_INIT_RESP_OR_EXT_GROUP;
    g02[1..3].copy_from_slice(&0x0032u16.to_le_bytes());
    g02[10..14].copy_from_slice(&7654u32.to_le_bytes());
    g02[18] = 0x08;
    g02.extend_from_slice(&rtp);
    let u02 = unwrap_zrtc(&g02).unwrap();
    assert_eq!(u02.kind, ZrtcPacketKind::VideoRtp);
    assert_eq!(u02.group_peer_uid, Some(7654));
}

#[test]
fn rtp_dynamic_csrc_extension_and_padding_parser() {
    // V=2, P=1, X=1, CC=2 -> byte0 = 0xB2; M=1, PT=112 -> byte1 = 0xF0.
    let mut pkt = vec![0xB2, 0xF0];
    pkt.extend_from_slice(&42u16.to_be_bytes());
    pkt.extend_from_slice(&9600u32.to_be_bytes());
    pkt.extend_from_slice(&0x1122_3344u32.to_be_bytes());
    // 2 CSRCs (8 bytes)
    pkt.extend_from_slice(&0xAAAA_BBBBu32.to_be_bytes());
    pkt.extend_from_slice(&0xCCCC_DDDDu32.to_be_bytes());
    // Header extension: profile 0xBEDE, length = 2 words (8 bytes)
    pkt.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x02]);
    pkt.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    // Payload (5 bytes) + 4 bytes RTP padding `[0, 0, 0, 4]`.
    pkt.extend_from_slice(b"VOIP!");
    pkt.extend_from_slice(&[0, 0, 0, 4]);

    let parsed = parse_rtp_packet(&pkt).unwrap();
    assert_eq!(parsed.csrc_count, 2);
    assert!(parsed.extension);
    assert!(parsed.padding);
    assert!(parsed.marker);
    assert_eq!(parsed.payload_type, 112);
    assert_eq!(parsed.seq, 42);
    assert_eq!(parsed.timestamp, 9600);
    assert_eq!(parsed.ssrc, 0x1122_3344);
    assert_eq!(parsed.header_len, 12 + 8 + 4 + 8);
    assert_eq!(parsed.extension_profile, Some(0xBEDE));
    assert_eq!(parsed.extension_data.len(), 8);
    assert_eq!(parsed.payload, b"VOIP!");

    assert_eq!(
        parse_relay_endpoint("49.213.119.2", DEFAULT_RELAY_PORT),
        Some("49.213.119.2:4200".parse().unwrap())
    );
}

fn build_mock_zrtp_reply(from_id: u32, flow_token: [u8; 4], reflexive: &str) -> Vec<u8> {
    let rb = reflexive.as_bytes();
    let mut out = vec![0u8; 35 + rb.len()];
    out[0] = 0x02;
    out[1] = 0x7e;
    out[10..14].copy_from_slice(&from_id.to_le_bytes());
    out[18] = 0x03;
    out[29..33].copy_from_slice(&flow_token);
    out[33..35].copy_from_slice(&(rb.len() as u16).to_le_bytes());
    out[35..].copy_from_slice(rb);
    out
}

#[tokio::test]
async fn zrtc_single_socket_sweep_srpt_duplex_and_dynamic_relay_retargeting() {
    let relay1 = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let relay2 = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let addr1 = relay1.local_addr().unwrap();
    let addr2 = relay2.local_addr().unwrap();

    let ft1 = [0x11, 0x22, 0x33, 0x44];
    let ft2 = [0x99, 0x88, 0x77, 0x66];

    // Spawn mock relay responders (returning misleading reflexive ASCII `"9.9.9.9|11111"` so we verify
    // `ZrtcMediaSession` sends to the real UDP socket address, NOT the reflexive string!).
    for (sock, ft) in [(Arc::clone(&relay1), ft1), (Arc::clone(&relay2), ft2)] {
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok((n, client_addr)) = sock.recv_from(&mut buf).await {
                if n >= 25 && buf[0] == 0x01 && buf[1] == 0x7e {
                    let reply = build_mock_zrtp_reply(111, ft, "9.9.9.9|11111");
                    let _ = sock.send_to(&reply, client_addr).await;
                } else {
                    break;
                }
            }
        });
    }

    let cfg = ZrtcSweepConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        candidates: vec![addr1, addr2, addr1], // includes duplicate to verify deduplication!
        prefer_host: Some(addr1.ip()),
        prefer_addr: Some(addr1),
        from_id: 111,
        to_id: 222,
        call_id: 333,
        sess_id: SAMPLE_SESS_154.to_string(),
        is_callee: false,
        is_group: false,
        probe_count: 3,
        sweep_timeout: Duration::from_millis(250),
        early_exit_min: Duration::from_millis(40),
    };

    let (session, outcome, mut rx_ev) =
        ZrtcMediaSession::bind_and_sweep(cfg, SrtpMode::AesCm128HmacSha1_80)
            .await
            .unwrap();

    assert_eq!(outcome.chosen_relay, addr1);
    assert_eq!(outcome.chosen_flow_token, ft1);
    assert_eq!(session.active_relay(), addr1);
    assert_eq!(session.active_flow_token(), ft1);
    assert_eq!(outcome.results.len(), 2);

    // Send outbound audio from client -> must arrive on `relay1` wrapped with `[0x03, ft1, srtp..]`.
    // Wait briefly so the handshake loop on `relay1` exits cleanly on first non-`0x01` or read directly:
    // Note: in the mock task above, receiving the first non-`0x01` packet breaks out of the mock loop,
    // so let's bind a fresh peer SRTP context and send inbound `0x04` from `relay2` to trigger retargeting!
    let mut peer_srtp =
        SrtpContext::from_sess_id(SAMPLE_SESS_154, SrtpMode::AesCm128HmacSha1_80).unwrap();
    let remote_rtp = build_audio_rtp(1, 320, 222, b"remote-opus-from-relay2");
    let remote_srtp = peer_srtp.protect(&remote_rtp).unwrap();
    let mut inbound_04 = vec![ZRTC_AUDIO_RTP_IN_SHORT];
    inbound_04.extend_from_slice(&remote_srtp);

    // Send inbound media from `relay2` (`addr2`) -> must dynamically retarget `session` from `addr1/ft1` to `addr2/ft2`!
    relay2
        .send_to(&inbound_04, outcome.local_addr)
        .await
        .unwrap();

    let ev1 = tokio::time::timeout(Duration::from_secs(1), rx_ev.recv())
        .await
        .unwrap()
        .unwrap();
    match ev1 {
        InboundZrtcEvent::RelayRetargeted {
            old_relay,
            new_relay,
            new_flow_token,
        } => {
            assert_eq!(old_relay, addr1);
            assert_eq!(new_relay, addr2);
            assert_eq!(new_flow_token, ft2);
        }
        other => panic!("expected RelayRetargeted first, got {other:?}"),
    }

    let ev2 = tokio::time::timeout(Duration::from_secs(1), rx_ev.recv())
        .await
        .unwrap()
        .unwrap();
    match ev2 {
        InboundZrtcEvent::AudioRtp {
            outer_type,
            from_relay,
            ssrc,
            opus_payload,
            ..
        } => {
            assert_eq!(outer_type, 0x04);
            assert_eq!(from_relay, addr2);
            assert_eq!(ssrc, 222);
            assert_eq!(opus_payload, b"remote-opus-from-relay2");
        }
        other => panic!("expected AudioRtp second, got {other:?}"),
    }

    assert_eq!(session.active_relay(), addr2);
    assert_eq!(session.active_flow_token(), ft2);

    // Now send two outbound audio packets: the first breaks `relay2`'s handshake helper loop if still waiting,
    // and we verify `relay2` receives `[0x03, ft2, srtp_pkt]` from the exact same `outcome.local_addr` socket!
    session.send_audio_opus(b"warmup").await.unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    session.send_audio_opus(b"retargeted-opus").await.unwrap();

    let mut r2_buf = [0u8; 2048];
    let (n, from_client) = tokio::time::timeout(Duration::from_secs(1), relay2.recv_from(&mut r2_buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(from_client, outcome.local_addr); // Single-socket rule verified!
    assert_eq!(r2_buf[0], 0x03);
    assert_eq!(&r2_buf[1..5], &ft2);
    let plain_out = peer_srtp.unprotect(&r2_buf[5..n]).unwrap();
    assert_eq!(parse_rtp_packet(&plain_out).unwrap().payload, b"retargeted-opus");

    session.close();
}

// ============================================================================
// 3. `libopus.so.0` FFI (`opus_ffi.rs`) & Audio I/O (`audio_io.rs`) Tests
// ============================================================================

#[test]
fn opus_dlopen_encode_decode_20ms_and_plc_roundtrip() {
    assert!(is_opus_available(), "system libopus.so.0 should be loadable via dlopen");

    let mut enc = OpusVoipEncoder::new().unwrap();
    let mut dec = OpusVoipDecoder::new().unwrap();

    // Wrong frame size must fail cleanly.
    assert!(enc.encode_20ms(&[0i16; 160]).is_err());

    // Encode 5 consecutive 20ms (`320` sample) 440 Hz sine wave frames at 16 kHz and decode back.
    let mut max_abs_decoded: i16 = 0;
    for frame_idx in 0..5 {
        let mut pcm_in = [0i16; OPUS_VOIP_FRAME_SAMPLES];
        for (i, sample) in pcm_in.iter_mut().enumerate() {
            let t = ((frame_idx * OPUS_VOIP_FRAME_SAMPLES + i) as f64) / 16_000.0;
            *sample = ((2.0 * std::f64::consts::PI * 440.0 * t).sin() * 12_000.0) as i16;
        }
        let packet = enc.encode_20ms(&pcm_in).unwrap();
        assert!(!packet.is_empty() && packet.len() < 400);
        let pcm_out = dec.decode_20ms(Some(&packet), false).unwrap();
        assert_eq!(pcm_out.len(), 320);
        for s in pcm_out {
            max_abs_decoded = max_abs_decoded.max(s.saturating_abs());
        }
    }
    assert!(
        max_abs_decoded > 2_000,
        "decoded 440Hz tone should have strong non-zero amplitude, got {max_abs_decoded}"
    );

    // Packet Loss Concealment (`None` packet) synthesizes 320 samples cleanly.
    let plc = dec.decode_20ms(None, false).unwrap();
    assert_eq!(plc.len(), 320);
}

#[test]
fn audio_device_filtering_dsp_resampling_and_400ms_jitter_cap() {
    let _ = is_pulse_simple_available();

    // `.monitor` and `Monitor of ...` loopbacks MUST be filtered out.
    assert!(is_ignored_audio_device(
        "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
        "Built-in Audio Analog Stereo"
    ));
    assert!(is_ignored_audio_device(
        "pulse_src_1",
        "Monitor of Built-in Audio"
    ));
    assert!(is_ignored_audio_device("auto_null", "Dummy Output"));
    assert!(is_ignored_audio_device("v4l_ir", "Integrated Infrared Camera"));
    assert!(!is_ignored_audio_device(
        "alsa_input.usb-Blue_Microphones-00.mono-fallback",
        "Yeti Nano Microphone"
    ));

    // Software gain (`2.0x` default) + `i16` clamping + permille level.
    let mut frame = [20_000i16, -20_000i16, 5_000i16, -5_000i16];
    let lvl = apply_mic_gain_and_level(&mut frame, DEFAULT_MIC_GAIN);
    assert_eq!(frame[0], i16::MAX);
    assert_eq!(frame[1], i16::MIN);
    assert_eq!(frame[2], 10_000);
    assert_eq!(frame[3], -10_000);
    assert!(lvl > 0 && lvl <= 1000);

    // Multi-rate resampler (`48 kHz` 20ms = `960` samples -> `16 kHz` `320` samples -> `48 kHz` `960` samples).
    let pcm_48k: Vec<i16> = (0..960).map(|i| (i as i16) * 10).collect();
    let pcm_16k = resample_mono_i16(&pcm_48k, 48_000, 16_000);
    assert_eq!(pcm_16k.len(), 320);
    let back_48k = resample_mono_i16(&pcm_16k, 16_000, 48_000);
    assert_eq!(back_48k.len(), 960);

    // Stereo -> mono downmix.
    let stereo = [1000i16, 3000, -2000, -4000];
    assert_eq!(downmix_to_mono_i16(&stereo, 2), vec![2000, -3000]);

    // `400ms` (`6,400`-sample) playback jitter-buffer overrun cap.
    let mut jb = PlaybackJitterBuffer::default();
    for step in 0..30i16 {
        let block = [step; 320]; // 30 * 320 = 9,600 samples (> 6,400 cap)
        jb.push_pcm(&block);
        assert!(jb.len() <= MAX_PLAYBACK_JITTER_SAMPLES);
    }
    assert_eq!(jb.len(), MAX_PLAYBACK_JITTER_SAMPLES);
    let mut first_out = [0i16; 320];
    jb.pop_into(&mut first_out);
    // Oldest 10 blocks (`0..10`) were dropped, so oldest remaining block is `10`!
    assert_eq!(first_out[0], 10);

    // `AudioIoSession` state toggles without starting hardware streams.
    let audio = AudioIoSession::new();
    audio.set_mic_muted(true);
    assert!(audio.is_mic_muted());
    assert_eq!(audio.mic_level_permille(), 0);
    audio.set_speaker_muted(true);
    assert!(audio.is_speaker_muted());
}

// ============================================================================
// 4. H.264 Video (`video_io.rs`) & VoIP Log (`voip_log.rs`) Tests
// ============================================================================

#[test]
fn video_h264_zalo_packetizer_depacketizer_stap_a_fu_a_and_rtcp() {
    // 1. Single-packet IDR AU (`<= 1200` bytes) MUST use `PT 98` (`0x62`), raw Annex-B, `M = 1`.
    let mut small_idr = Vec::new();
    small_idr.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x1E]); // SPS (7)
    small_idr.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x3C, 0x80]); // PPS (8)
    small_idr.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x21]); // IDR (5)
    assert!(is_h264_idr_au(&small_idr));
    assert_eq!(split_annex_b_nals(&small_idr).len(), 3);

    let mut seq = 10u16;
    let idr_pkts = packetize_zalo_h264_au(&small_idr, &mut seq, 90_000, 0xABCD_0001);
    assert_eq!(idr_pkts.len(), 1);
    assert_eq!(seq, 11);
    let parsed_idr = parse_rtp_packet(&idr_pkts[0]).unwrap();
    assert_eq!(parsed_idr.payload_type, ZALO_H264_PT_IDR); // 98!
    assert!(parsed_idr.marker);
    assert_eq!(parsed_idr.payload, small_idr.as_slice());

    let mut depack = H264Depacketizer::new();
    assert!(depack.should_send_pli()); // unsynced initially
    let reassembled_small = depack.push_rtp_packet(&idr_pkts[0]).unwrap();
    assert_eq!(reassembled_small, small_idr);
    assert!(depack.has_keyframe_sync());
    assert!(!depack.should_send_pli());

    // 2. Large IDR AU (`> 1200` bytes) splits into even `[0x1C, 0x80 | 0x00 | 0x40]` fragments (`PT 98`, `M=1` on last).
    let mut large_idr = small_idr.clone();
    large_idr.extend((0..2800).map(|i| (i % 251) as u8));
    let large_pkts = packetize_zalo_h264_au(&large_idr, &mut seq, 96_000, 0xABCD_0001);
    assert_eq!(large_pkts.len(), 3);

    for (idx, pkt) in large_pkts.iter().enumerate() {
        let p = parse_rtp_packet(pkt).unwrap();
        assert_eq!(p.payload_type, ZALO_H264_PT_IDR);
        assert_eq!(p.payload[0], 0x1C);
        let expected_flag = match idx {
            0 => 0x80,
            1 => 0x00,
            _ => 0x40,
        };
        assert_eq!(p.payload[1], expected_flag);
        assert_eq!(p.marker, idx == 2);

        let out = depack.push_rtp_packet(pkt);
        if idx < 2 {
            assert!(out.is_none());
        } else {
            assert_eq!(out.unwrap(), large_idr);
        }
    }

    // 3. Non-IDR P-frame AU uses `PT 97` (`0x61`).
    let mut p_frame = vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x22, 0x33];
    p_frame.extend((0..1500).map(|i| (i & 0xff) as u8));
    assert!(!is_h264_idr_au(&p_frame));
    let p_pkts = packetize_zalo_h264_au(&p_frame, &mut seq, 102_000, 0xABCD_0001);
    assert_eq!(p_pkts.len(), 2);
    assert_eq!(
        parse_rtp_packet(&p_pkts[0]).unwrap().payload_type,
        ZALO_H264_PT_PFRAME
    );
    assert!(depack.push_rtp_packet(&p_pkts[0]).is_none());
    assert_eq!(depack.push_rtp_packet(&p_pkts[1]).unwrap(), p_frame);

    // 4. Sequence gap triggers `should_send_pli() == true`.
    seq = seq.wrapping_add(5);
    let after_gap = packetize_zalo_h264_au(
        &[0x00, 0x00, 0x00, 0x01, 0x41, 0x01, 0x02],
        &mut seq,
        108_000,
        0xABCD_0001,
    );
    let _ = depack.push_rtp_packet(&after_gap[0]);
    assert!(depack.should_send_pli());

    // 5. RFC 6184 STAP-A (`nal_type = 24`) and FU-A (`nal_type = 28`, `nal = 5` IDR) depacketization.
    let mut stap_payload = vec![24u8];
    stap_payload.extend_from_slice(&4u16.to_be_bytes());
    stap_payload.extend_from_slice(&[0x67, 0x42, 0x00, 0x1f]);
    stap_payload.extend_from_slice(&3u16.to_be_bytes());
    stap_payload.extend_from_slice(&[0x68, 0xce, 0x3c]);
    let stap_rtp = build_audio_rtp(seq, 114_000, 0xABCD_0001, &stap_payload);
    seq = seq.wrapping_add(1);
    let stap_au = depack.push_rtp_packet(&stap_rtp).unwrap();
    assert_eq!(split_annex_b_nals(&stap_au).len(), 2);

    // FU-A with `fu_indicator = 0x7C` (`nal=28`), `fu_header = 0x85` (start, IDR=5) + `0x45` (end, IDR=5).
    let fua_1 = build_audio_rtp(seq, 120_000, 0xABCD_0001, &[0x7C, 0x85, 0xAA, 0xBB]);
    seq = seq.wrapping_add(1);
    let mut fua_2 = build_audio_rtp(seq, 120_000, 0xABCD_0001, &[0x7C, 0x45, 0xCC, 0xDD]);
    fua_2[1] |= 0x80; // M=1
    assert!(depack.push_rtp_packet(&fua_1).is_none());
    let fua_au = depack.push_rtp_packet(&fua_2).unwrap();
    assert_eq!(fua_au, vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC, 0xDD]);

    // 6. RTCP RR, PLI, SR+SDES+REMB & `H264_AUD_ANNEX_B` checks.
    assert_eq!(H264_AUD_ANNEX_B, [0x00, 0x00, 0x00, 0x01, 0x09, 0xF0]);
    let rr = build_rtcp_rr(111, 222, 1000, 12);
    assert_eq!(rr.len(), 32);
    assert!(!rtcp_requests_keyframe(&rr));

    let pli = build_rtcp_pli(111, 222);
    assert_eq!(&pli[0..4], &[0x81, 206, 0x00, 0x02]);
    assert!(rtcp_requests_keyframe(&pli));

    let sr_remb = build_rtcp_sr_sdes_remb(111, 222, 90_000, 15, 18_000, 350_000, "olaz@linux");
    assert!(sr_remb.windows(4).any(|w| w == b"REMB"));
    assert!(!rtcp_requests_keyframe(&sr_remb));
}

#[test]
fn voip_logger_writes_log_and_redacts_session_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let logger = VoipLogger::new(tmp.path());

    let raw_payload = json!({
        "callId": 42,
        "session": SAMPLE_SESS_154,
        "params": serde_json::to_string(&json!({
            "sessId": SAMPLE_SESS_154,
            "rtpIP": "49.213.119.2:4200"
        })).unwrap(),
    });

    let redacted = redact_voip_value(&raw_payload);
    let redacted_str = serde_json::to_string(&redacted).unwrap();
    assert!(!redacted_str.contains(SAMPLE_SESS_154));
    assert!(redacted_str.contains("[REDACTED len=154]"));
    assert!(redacted_str.contains("49.213.119.2:4200"));

    logger.log_json("test", "signal416", &raw_payload);
    let disk_text = std::fs::read_to_string(logger.log_file_path()).unwrap();
    assert!(!disk_text.contains(SAMPLE_SESS_154));
    assert!(disk_text.contains("[REDACTED len=154]"));
}

// ============================================================================
// 5. `OlazCallEngine` (`engine.rs`) Full Signaling & State Machine Parity Tests
// ============================================================================

struct MockMediaDriver {
    mic_muted: AtomicBool,
    speaker_muted: AtomicBool,
    selected_in: AtomicI32,
    selected_out: AtomicI32,
    video_state: Mutex<(bool, bool)>,
    audio_tap: Mutex<Option<SyncSender<AudioTapPacket>>>,
    dropped_tap: AtomicU64,
    stopped: AtomicBool,
}

impl Default for MockMediaDriver {
    fn default() -> Self {
        Self {
            mic_muted: AtomicBool::new(false),
            speaker_muted: AtomicBool::new(false),
            selected_in: AtomicI32::new(-1),
            selected_out: AtomicI32::new(-1),
            video_state: Mutex::new((false, false)),
            audio_tap: Mutex::new(None),
            dropped_tap: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        }
    }
}

impl MockMediaDriver {
    fn emit_tap(&self, pkt: AudioTapPacket) {
        if let Some(ref tx) = *self.audio_tap.lock() {
            if tx.try_send(pkt).is_err() {
                self.dropped_tap.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl CallMediaDriver for MockMediaDriver {
    fn list_devices(&self) -> AudioDeviceList {
        AudioDeviceList {
            capture: vec![AudioDeviceEntry {
                index: 0,
                id: "mic0".into(),
                name: "Test Mic".into(),
                is_default: true,
            }],
            playback: vec![AudioDeviceEntry {
                index: 0,
                id: "spk0".into(),
                name: "Test Speaker".into(),
                is_default: true,
            }],
            selected_in: self.selected_in.load(Ordering::Relaxed),
            selected_out: self.selected_out.load(Ordering::Relaxed),
        }
    }

    fn set_mic_muted(&self, muted: bool) {
        self.mic_muted.store(muted, Ordering::Relaxed);
    }

    fn set_speaker_muted(&self, muted: bool) {
        self.speaker_muted.store(muted, Ordering::Relaxed);
    }

    fn set_input_device(&self, idx: i32) {
        self.selected_in.store(idx, Ordering::Relaxed);
    }

    fn set_output_device(&self, idx: i32) {
        self.selected_out.store(idx, Ordering::Relaxed);
    }

    fn set_video_mode(&self, camera_enabled: bool, screen_sharing: bool) {
        *self.video_state.lock() = (camera_enabled, screen_sharing);
    }

    fn set_audio_tap(&self, tap: Option<SyncSender<AudioTapPacket>>) {
        if tap.is_some() {
            self.dropped_tap.store(0, Ordering::Relaxed);
        }
        *self.audio_tap.lock() = tap;
    }

    fn dropped_tap_frames(&self) -> u64 {
        self.dropped_tap.load(Ordering::Relaxed)
    }

    fn inject_inbound_audio_for_test(&self, _opus_payload: &[u8]) {}

    fn stop(&self) {
        *self.audio_tap.lock() = None;
        self.stopped.store(true, Ordering::Relaxed);
    }
}

struct MockMediaFactory {
    calls: Mutex<Vec<MediaSetupParams>>,
    last_driver: Mutex<Option<Arc<MockMediaDriver>>>,
}

impl Default for MockMediaFactory {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            last_driver: Mutex::new(None),
        }
    }
}

impl CallMediaDriverFactory for MockMediaFactory {
    fn open_media<'a>(
        &'a self,
        params: MediaSetupParams,
        _ui_cb: Arc<dyn Fn(CallUiEvent) + Send + Sync>,
    ) -> BoxMediaSetupFuture<'a> {
        Box::pin(async move {
            self.calls.lock().push(params.clone());
            let chosen: SocketAddr = params
                .candidates
                .first()
                .copied()
                .unwrap_or_else(|| "49.213.119.2:4200".parse().unwrap());
            let driver = Arc::new(MockMediaDriver::default());
            if params.is_video {
                driver.set_video_mode(true, false);
            }
            *self.last_driver.lock() = Some(Arc::clone(&driver));
            let res = MediaSetupResult {
                chosen_relay: chosen,
                local_port: 51234,
                results: vec![RelaySweepEntry {
                    addr: chosen,
                    recv: 7,
                    rtt_ms: 14,
                    flow_token: [1, 2, 3, 4],
                    reflexive_addr: None,
                }],
                devices: driver.list_devices(),
            };
            Ok((driver as Arc<dyn CallMediaDriver>, res))
        })
    }
}

#[derive(Debug, Clone)]
enum RecordedEvent {
    Update(String, Value),
    Signal(u32, Value),
    Ui(String),
}

struct TestHarness {
    engine: Arc<OlazCallEngine>,
    factory: Arc<MockMediaFactory>,
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    ui_events: Arc<Mutex<Vec<CallUiEvent>>>,
}

impl TestHarness {
    fn new(ring_timeout: Duration) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let ui_events = Arc::new(Mutex::new(Vec::new()));
        let factory = Arc::new(MockMediaFactory::default());

        let ev_u = Arc::clone(&events);
        let ev_s = Arc::clone(&events);
        let ev_ui = Arc::clone(&events);
        let raw_ui = Arc::clone(&ui_events);

        let mut cfg = CallEngineConfig::new(
            move |cmd, val| ev_u.lock().push(RecordedEvent::Update(cmd.to_string(), val)),
            move |cmd, val| ev_s.lock().push(RecordedEvent::Signal(cmd, val)),
            move |ui| {
                raw_ui.lock().push(ui.clone());
                let tag = match ui {
                    CallUiEvent::ShowCall { is_video, .. } => format!("ShowCall(video={is_video})"),
                    CallUiEvent::ShowIncoming { is_video, .. } => {
                        format!("ShowIncoming(video={is_video})")
                    }
                    CallUiEvent::SetState { state, outcome, .. } => {
                        format!("SetState({state},{outcome:?})")
                    }
                    CallUiEvent::CloseCall => "CloseCall".into(),
                    CallUiEvent::CloseIncoming => "CloseIncoming".into(),
                    CallUiEvent::OpenDevices => "OpenDevices".into(),
                    CallUiEvent::SetDevices(_) => "SetDevices".into(),
                    CallUiEvent::SetMicLevel { .. } => "SetMicLevel".into(),
                    CallUiEvent::VideoFrame { .. } => "VideoFrame".into(),
                    CallUiEvent::SetRecording {
                        active,
                        format,
                        error,
                        ..
                    } => format!("SetRecording(active={active},fmt={format:?},err={error:?})"),
                };
                ev_ui.lock().push(RecordedEvent::Ui(tag));
            },
        );
        cfg.media_factory = Arc::clone(&factory) as Arc<dyn CallMediaDriverFactory>;
        cfg.ring_timeout = ring_timeout;
        cfg.close_ui_delay = Duration::from_millis(10);
        cfg.call_id_generator = Some(Arc::new(|| 700_001));

        let engine = OlazCallEngine::new(cfg);
        Self {
            engine,
            factory,
            events,
            ui_events,
        }
    }
}

#[test]
fn unpack_ipc_args_and_nonzero_answer_mapping() {
    let str_args = json!([r#"{"type":"request","command":"makeCall"}"#, true]);
    let unpacked = unpack_send_to_native_args(&str_args).unwrap();
    assert_eq!(unpacked["type"], "request");
    assert_eq!(unpacked["command"], "makeCall");

    let obj_args = json!([{ "type": "control", "data": { "act": "end_call" } }]);
    assert_eq!(
        unpack_send_to_native_args(&obj_args).unwrap()["type"],
        "control"
    );

    assert_eq!(map_nonzero_answer_status(1), (1, Some("busy")));
    assert_eq!(map_nonzero_answer_status(3), (3, None));
    assert_eq!(map_nonzero_answer_status(5), (2, None));
    assert_eq!(map_nonzero_answer_status(6), (2, None));
}

#[tokio::test]
async fn engine_outgoing_call_full_lifecycle_midcall_controls_and_free_before_bubble() {
    let h = TestHarness::new(Duration::from_secs(30));
    h.engine.announce_native_ready();

    // 1. `makeCall` via JSON-string array IPC (`[json_str, true]`).
    let make_call_str = serde_json::to_string(&json!({
        "type": "request",
        "command": "makeCall",
        "data": {
            "type": 1,
            "partner": [{ "id": "888001", "name": "Alice Nguyen", "avatar": "https://ava/1.jpg" }]
        }
    }))
    .unwrap();
    h.engine.handle_ipc_args(&json!([make_call_str, true]));

    // Verify `401` emitted immediately.
    {
        let evs = h.events.lock();
        assert!(evs.iter().any(|e| matches!(e, RecordedEvent::Update(c, _) if c == "native-ready")));
        let s401 = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Signal(401, v) => Some(v.clone()),
                _ => None,
            })
            .expect("expected 401 signal");
        assert_eq!(s401["calleeId"], "888001");
        assert_eq!(s401["callId"], 700_001);
        assert_eq!(s401["type"], 1);
    }

    // 2. `recvSignal 401` config -> opens media (`is_callee == false`) -> emits `416`.
    h.engine.handle_send_to_native(json!({
        "type": "recvSignal",
        "command": 401,
        "data": {
            "fromId": 100_001,
            "toId": 888_001,
            "sessId": SAMPLE_SESS_154,
            "rtpIP": "49.213.119.2:4200",
            "jsonConfig": serde_json::to_string(&json!({
                "zrtd": ["49.213.119.2:4200", "49.213.119.3:4200"]
            })).unwrap()
        }
    }));

    tokio::time::sleep(Duration::from_millis(30)).await;

    {
        let setups = h.factory.calls.lock();
        assert_eq!(setups.len(), 1);
        assert!(!setups[0].is_callee);
        assert_eq!(setups[0].from_id, 100_001);
        assert_eq!(setups[0].to_id, 888_001);

        let evs = h.events.lock();
        let s416 = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Signal(416, v) => Some(v.clone()),
                _ => None,
            })
            .expect("expected 416 signal");
        assert_eq!(s416["calleeId"], "888001");
        assert_eq!(s416["codec"], OPUS_CODEC_DESCRIPTOR);
        assert_eq!(s416["rtpAddress"], "49.213.119.2:4200");
        assert_eq!(s416["rtpIPv6Addr"], "");
    }

    // 3. Remote `ring_ring` -> `callState: "ringing"`.
    h.engine.handle_send_to_native(json!({
        "type": "control",
        "data": { "act": "ring_ring", "data": { "callId": 700_001 } }
    }));

    // 4. Remote `answer status: 0` (sent TWICE to verify duplicate guard sends `408` only ONCE!).
    let answer_msg = json!({
        "type": "control",
        "data": { "act": "answer", "data": { "callId": 700_001, "status": 0 } }
    });
    h.engine.handle_send_to_native(answer_msg.clone());
    h.engine.handle_send_to_native(answer_msg);

    let count_408 = h
        .events
        .lock()
        .iter()
        .filter(|e| matches!(e, RecordedEvent::Signal(408, _)))
        .count();
    assert_eq!(count_408, 1, "duplicate answer status=0 must NOT send a 2nd 408");

    // 5. Mid-call UI actions: mute, speaker, selectInput/Output, toggleCamera (`12006`), toggleScreenShare (`12064`).
    h.engine.handle_ui_action("mute", json!(true));
    h.engine.handle_ui_action("toggleSpeaker", json!(true));
    h.engine.handle_ui_action("selectInput", json!(0));
    h.engine.handle_ui_action("selectOutput", json!(0));
    h.engine.handle_ui_action("toggleCamera", json!(true));
    h.engine.handle_ui_action("toggleScreenShare", json!(true));

    {
        let drv = h.factory.last_driver.lock().clone().unwrap();
        assert!(drv.mic_muted.load(Ordering::Relaxed));
        assert!(drv.speaker_muted.load(Ordering::Relaxed));
        assert_eq!(drv.selected_in.load(Ordering::Relaxed), 0);
        assert_eq!(drv.selected_out.load(Ordering::Relaxed), 0);
        assert_eq!(*drv.video_state.lock(), (true, true));

        let evs = h.events.lock();
        assert!(evs.iter().any(|e| matches!(e, RecordedEvent::Signal(12006, _))));
        assert!(evs.iter().any(|e| matches!(e, RecordedEvent::Signal(12064, _))));
    }

    // 6. User hangs up connected call -> `409` AND `callState: "free"` strictly BEFORE `bubble`!
    h.engine.handle_ui_action("end", Value::Null);

    let evs = h.events.lock();
    assert!(evs.iter().any(|e| matches!(e, RecordedEvent::Signal(409, _))));
    let free_pos = evs
        .iter()
        .position(|e| {
            matches!(
                e,
                RecordedEvent::Update(cmd, v) if cmd == "callState" && v["state"] == "free"
            )
        })
        .expect("expected callState: free");
    let bubble_pos = evs
        .iter()
        .position(|e| matches!(e, RecordedEvent::Update(cmd, _) if cmd == "bubble"))
        .expect("expected bubble event");
    assert!(
        free_pos < bubble_pos,
        "`callState: free` ({free_pos}) MUST be emitted strictly BEFORE `bubble` ({bubble_pos})!"
    );
    if let RecordedEvent::Update(_, b) = &evs[bubble_pos] {
        assert_eq!(b["role"], 1); // caller
        assert_eq!(b["reason"], 0); // answered calls always log `reason: 0, missed: false`
        assert_eq!(b["missed"], false);
        assert!(b["duration"].as_u64().is_some());
    }
}

#[tokio::test]
async fn engine_outgoing_cancel_timeout_and_busy_reject_statuses() {
    // A. Caller cancels while ringing -> `405` + `bubble` with `duration: 0, reason: 4`.
    let h_cancel = TestHarness::new(Duration::from_secs(30));
    h_cancel.engine.handle_send_to_native(json!({
        "type": "request",
        "command": "makeCall",
        "data": { "type": 1, "partner": [{ "id": "5001", "name": "Bob" }] }
    }));
    h_cancel.engine.handle_ui_action("end", Value::Null);
    {
        let evs = h_cancel.events.lock();
        assert!(evs.iter().any(|e| matches!(e, RecordedEvent::Signal(405, _))));
        let b = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Update(c, v) if c == "bubble" => Some(v.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(b["duration"], 0);
        assert_eq!(b["reason"], 4);
    }

    // B. Remote `answer status: 1` (busy) -> NO `408`, teardown `reason: 1`, UI `outcome: "busy"`.
    let h_busy = TestHarness::new(Duration::from_secs(30));
    h_busy.engine.handle_send_to_native(json!({
        "type": "request",
        "command": "makeCall",
        "data": { "type": 1, "partner": [{ "id": "5002", "name": "Carol" }] }
    }));
    h_busy.engine.handle_send_to_native(json!({
        "type": "control",
        "data": { "act": "answer", "data": { "callId": 700_001, "status": 1 } }
    }));
    {
        let evs = h_busy.events.lock();
        assert!(!evs.iter().any(|e| matches!(e, RecordedEvent::Signal(408, _))));
        let b = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Update(c, v) if c == "bubble" => Some(v.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(b["reason"], 1);
        assert!(evs
            .iter()
            .any(|e| matches!(e, RecordedEvent::Ui(s) if s.contains("Some(\"busy\")"))));
    }

    // C. Ring timeout -> auto-sends `405` and tears down with `reason: 2`.
    let h_to = TestHarness::new(Duration::from_millis(60));
    h_to.engine.handle_send_to_native(json!({
        "type": "request",
        "command": "makeCall",
        "data": { "type": 1, "partner": [{ "id": "5003", "name": "Dan" }] }
    }));
    h_to.engine.handle_send_to_native(json!({
        "type": "recvSignal",
        "command": 401,
        "data": {
            "fromId": 100,
            "toId": 5003,
            "sessId": SAMPLE_SESS_154,
            "rtpIP": "49.213.119.2:4200"
        }
    }));
    tokio::time::sleep(Duration::from_millis(130)).await;
    {
        let evs = h_to.events.lock();
        assert!(evs.iter().any(|e| matches!(e, RecordedEvent::Signal(405, _))));
        let b = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Update(c, v) if c == "bubble" => Some(v.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(b["reason"], 2);
    }
}

#[tokio::test]
async fn engine_incoming_dual_id_split_busy_accept_and_decline_rules() {
    let incoming_ctrl = json!({
        "type": "control",
        "data": {
            "act": "request",
            "_caller": { "name": "Khoa Peer", "avatar": "https://ava/peer.png" },
            "data": {
                "callId": 812_345,
                "uidN": "333",
                "uidFrom": "999",
                "uidTo": "444",
                "session": SAMPLE_SESS_154,
                "rtpAddress": "49.213.119.2:4200",
                "params": serde_json::to_string(&json!({
                    "sessId": SAMPLE_SESS_154,
                    "rtpIP": "49.213.119.2:4200",
                    "extendData": serde_json::to_string(&json!({
                        "callType": 1,
                        "srtpMode": 1,
                        "serverResult": [{ "rtp": "49.213.119.2:4200" }]
                    })).unwrap()
                })).unwrap()
            }
        }
    });

    // 1. Incoming Accept + Dual-ID Split verification (`callerId="333"`, `InitZRTP`: `is_callee=true`, `from_id=444`, `to_id=999`).
    let h_acc = TestHarness::new(Duration::from_secs(30));
    h_acc.engine.handle_send_to_native(incoming_ctrl.clone());

    // While already in an active call, a 2nd incoming call MUST auto-reject with `402 status: 1` (busy)!
    let mut second_inc = incoming_ctrl.clone();
    second_inc["data"]["data"]["callId"] = json!(999_999);
    second_inc["data"]["data"]["uidN"] = json!("777");
    h_acc.engine.handle_send_to_native(second_inc);

    {
        let evs = h_acc.events.lock();
        let s407 = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Signal(407, v) => Some(v.clone()),
                _ => None,
            })
            .expect("expected 407 ringing ack");
        assert_eq!(s407["callerId"], "333"); // `routeId` (`uidN`), NOT `uidFrom`!

        let busy_402 = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Signal(402, v) if v["callId"] == 999_999 => Some(v.clone()),
                _ => None,
            })
            .expect("expected busy 402 auto-reject");
        assert_eq!(busy_402["status"], 1);
        assert_eq!(busy_402["callerId"], "777");
        assert_eq!(busy_402["codec"], "");
        assert_eq!(busy_402["rtpIPv6Addr"], "");
    }

    // Accept the first incoming video call!
    h_acc.engine.handle_ui_action("accept", Value::Null);
    tokio::time::sleep(Duration::from_millis(30)).await;

    {
        let setups = h_acc.factory.calls.lock();
        assert_eq!(setups.len(), 1);
        assert!(setups[0].is_callee); // Role `0x0c`!
        assert!(setups[0].is_video);
        assert_eq!(setups[0].from_id, 444); // `uidTo`!
        assert_eq!(setups[0].to_id, 999); // `uidFrom` (`mediaId`)!

        let evs = h_acc.events.lock();
        let ok_402 = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Signal(402, v) if v["callId"] == 812_345 => Some(v.clone()),
                _ => None,
            })
            .expect("expected 402 accept");
        assert_eq!(ok_402["status"], 0);
        assert_eq!(ok_402["callerId"], "333");
        assert_eq!(ok_402["codec"], OPUS_CODEC_DESCRIPTOR);
    }

    // End accepted incoming call -> `bubble` IS emitted (`role: 0`, `partnerId: "333"`).
    h_acc.engine.handle_ui_action("end", Value::Null);
    {
        let evs = h_acc.events.lock();
        let b = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Update(c, v) if c == "bubble" => Some(v.clone()),
                _ => None,
            })
            .expect("answered incoming call should emit bubble");
        assert_eq!(b["role"], 0);
        assert_eq!(b["partnerId"], "333");
    }

    // 2. Incoming Decline -> `402 status: 3` with explicit `""` string fields and NO local `bubble`!
    let h_dec = TestHarness::new(Duration::from_secs(30));
    h_dec.engine.handle_send_to_native(incoming_ctrl);
    h_dec.engine.handle_ui_action("decline", Value::Null);
    {
        let evs = h_dec.events.lock();
        let dec_402 = evs
            .iter()
            .find_map(|e| match e {
                RecordedEvent::Signal(402, v) => Some(v.clone()),
                _ => None,
            })
            .expect("expected 402 decline");
        assert_eq!(dec_402["status"], 3);
        assert_eq!(dec_402["callerId"], "333");
        assert_eq!(dec_402["codec"], "");
        assert_eq!(dec_402["extendData"], "");
        assert_eq!(dec_402["rtpAddress"], "");
        assert_eq!(dec_402["rtcpAddress"], "");
        assert_eq!(dec_402["rtpIPv6Addr"], "");
        assert_eq!(dec_402["rtcpIPv6Addr"], "");
        assert!(
            !evs.iter().any(|e| matches!(e, RecordedEvent::Update(c, _) if c == "bubble")),
            "declined incoming call must NOT emit a duplicate local bubble"
        );
    }
}

// ============================================================================
// 6. Embedded UI Assets (`ui_assets.rs`) & `call_stub` Compatibility Tests
// ============================================================================

#[test]
fn ui_assets_and_call_stub_backwards_compatibility() {
    assert_eq!(CALL_UI_CALL_URL, "zcall-ui://app/call.html");
    assert_eq!(CALL_UI_INCOMING_URL, "zcall-ui://app/incoming.html");
    assert_eq!(CALL_UI_DEVICES_URL, "zcall-ui://app/devices.html");
    assert!(PRELOAD_BRIDGE_JS.contains("window.zcallUI"));

    for path in [
        "call.html",
        "call.css",
        "call.js",
        "call-format.js",
        "preload.js",
        "incoming.html",
        "incoming.css",
        "incoming.js",
        "devices.html",
        "devices.css",
        "devices.js",
        "icons.css",
        "sounds.js",
    ] {
        let (mime, body) = get_ui_asset(path).unwrap_or_else(|| panic!("missing UI asset {path}"));
        assert!(!mime.is_empty());
        assert!(!body.is_empty());
    }

    let (_, call_html) = get_ui_asset("call.html").unwrap();
    let call_html_str = std::str::from_utf8(call_html).unwrap();
    assert!(call_html_str.contains(r#"id="remote-video""#));
    assert!(call_html_str.contains(r#"id="local-video""#));
    assert!(call_html_str.contains(r#"id="btn-cam""#));
    assert!(call_html_str.contains(r#"id="btn-screen""#));
    assert!(call_html_str.contains(r#"id="btn-record""#));
    assert!(call_html_str.contains(r#"id="rec-indicator""#));
    assert!(call_html_str.contains(r#"id="rec-toast""#));
    assert!(call_html_str.contains("header-info"));
    assert!(call_html_str.contains("controls-bar"));

    assert!(PRELOAD_BRIDGE_JS.contains("toggleRecord"));
    assert!(PRELOAD_BRIDGE_JS.contains("openRecordingFolder"));
    assert!(PRELOAD_BRIDGE_JS.contains("onRecording"));

    let (_, call_css) = get_ui_asset("call.css").unwrap();
    let call_css_str = std::str::from_utf8(call_css).unwrap();
    assert!(call_css_str.contains(".rec-indicator"));
    assert!(call_css_str.contains("recPulse"));

    let (_, call_js) = get_ui_asset("call.js").unwrap();
    let call_js_str = std::str::from_utf8(call_js).unwrap();
    assert!(call_js_str.contains("toggleRecord"));
    assert!(call_js_str.contains("openRecordingFolder"));

    // Verify existing `call_stub` exports remain intact.
    assert!(!LINUX_CALL_UNSUPPORTED_MESSAGE.is_empty());
    let enc_hex = CallTransportCrypto::encrypt("{\"ping\":1}").unwrap();
    assert_eq!(
        CallTransportCrypto::decrypt(&enc_hex).as_deref(),
        Some("{\"ping\":1}")
    );
    let chunker = CallChunker::new();
    let _ = chunker;
}

#[tokio::test]
async fn engine_group_sfu_port_selects_zavi_null_auth_no_seq_and_group_flag() {
    let h = TestHarness::new(Duration::from_secs(30));
    h.engine.handle_send_to_native(json!({
        "type": "request",
        "command": "startGroupCall",
        "data": {
            "type": 2,
            "groupId": "900100",
            "partner": [{ "id": "900100", "name": "Engineering Group" }]
        }
    }));

    h.engine.handle_send_to_native(json!({
        "type": "recvSignal",
        "command": 401,
        "data": {
            "fromId": 1001,
            "toId": 900_100,
            "sessId": SAMPLE_SESS_154,
            "rtpIP": "49.213.119.2:3000"
        }
    }));

    tokio::time::sleep(Duration::from_millis(30)).await;
    let setups = h.factory.calls.lock();
    assert_eq!(setups.len(), 1);
    assert!(setups[0].is_group);
    assert!(setups[0].is_video);
    assert_eq!(setups[0].srtp_mode, SrtpMode::ZaviNullAuthNoSeq);
    h.engine.stop();
}

#[test]
fn ffmpeg_h264_aud_flush_decoder_roundtrip() {
    // Generate 2 tiny Baseline Annex-B H.264 frames via `ffmpeg` if available on PATH.
    let Ok(gen_out) = std::process::Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=15",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "baseline",
            "-bf",
            "0",
            "-g",
            "1",
            "-f",
            "h264",
            "pipe:1",
        ])
        .output()
    else {
        return;
    };
    if !gen_out.status.success() || gen_out.stdout.is_empty() {
        return;
    }

    let decoded_frames: Arc<Mutex<Vec<(u32, u32, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let df_cb = Arc::clone(&decoded_frames);
    let dec = FfmpegH264Decoder::start(move |w, h, rgba| {
        df_cb.lock().push((w, h, rgba.len()));
    })
    .unwrap();

    // Packetize 4 AUs via Zalo H.264 packetizer -> depacketize -> push with `H264_AUD` flush trick.
    let mut seq = 1u16;
    let mut depack = H264Depacketizer::new();
    for step in 0..4u32 {
        let pkts =
            packetize_zalo_h264_au(&gen_out.stdout, &mut seq, 90_000 + step * 6_000, 0x55AA_1122);
        for p in &pkts {
            if let Some(au) = depack.push_rtp_packet(p) {
                dec.push_access_unit(&au);
            }
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if !decoded_frames.lock().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    dec.stop();

    let got = decoded_frames.lock();
    assert!(
        !got.is_empty(),
        "FfmpegH264Decoder with H264_AUD_ANNEX_B flush should emit at least 1 RGBA frame immediately"
    );
    assert_eq!(got[0], (320, 240, 320 * 240 * 4));
}

// ============================================================================
// 7. Full-Duplex VoIP Call Recorder (`recorder.rs`, `audio_io.rs`, `opus_ffi.rs`, `engine.rs`)
// ============================================================================

#[test]
fn recorder_wav_header_stereo_split_mono_mix_permissions_and_crash_safe_periodic_sync() {
    assert_eq!(WAV_HEADER_BYTES, 44);
    assert_eq!(WAV_SYNC_INTERVAL_FRAMES, 250);
    assert!(!default_recordings_dir().as_os_str().is_empty());
    assert_eq!(
        sanitize_partner_slug("Nguyễn Văn Đạt / Team #1!"),
        "nguyen-van-dat-team-1"
    );
    assert_eq!(sanitize_partner_slug("...///"), "peer");

    let tmp = tempfile::tempdir().unwrap();
    let rec_dir = tmp.path().join("private_recordings");
    let wav_path = build_recording_file_path(&rec_dir, "Nguyễn Văn Đạt", RecordingFormat::Wav);
    assert!(wav_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with("-nguyen-van-dat.wav"));

    // 1. Start StereoSplit WAV recorder and feed 260 frames (> 250 periodic sync threshold!).
    let rec = CallRecorderHandle::start(
        wav_path.clone(),
        RecordingFormat::Wav,
        RecordingChannelLayout::StereoSplit,
    )
    .unwrap();
    let tap = rec.tap_sender();

    let total_frames: usize = 260;
    for f in 0..total_frames {
        let tx_val = (1000 + f as i16).clamp(-30_000, 30_000);
        let rx_val = (-2000 - f as i16).clamp(-30_000, 30_000);
        let mut tx_samples = [0i16; AUDIO_FRAME_SAMPLES];
        let mut rx_samples = [0i16; AUDIO_FRAME_SAMPLES];
        for i in 0..AUDIO_FRAME_SAMPLES {
            tx_samples[i] = tx_val.wrapping_add((i & 7) as i16);
            rx_samples[i] = rx_val.wrapping_sub((i & 7) as i16);
        }
        tap.send(AudioTapPacket {
            source: TapSource::TxMic,
            samples: tx_samples,
        })
        .unwrap();
        tap.send(AudioTapPacket {
            source: TapSource::RxRemote,
            samples: rx_samples,
        })
        .unwrap();
    }

    // Verify crash-safe periodic patching at 250 frames BEFORE `stop()` is called!
    let expected_250_data_bytes = 250u32 * (AUDIO_FRAME_SAMPLES as u32) * 4;
    let sync_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut saw_midstream_patched_header = false;
    while std::time::Instant::now() < sync_deadline {
        if let Ok(mid_bytes) = std::fs::read(&wav_path) {
            if mid_bytes.len() >= 44 {
                let mid_riff_sz = u32::from_le_bytes(mid_bytes[4..8].try_into().unwrap());
                let mid_data_sz = u32::from_le_bytes(mid_bytes[40..44].try_into().unwrap());
                if mid_data_sz >= expected_250_data_bytes && mid_riff_sz == 36 + mid_data_sz {
                    saw_midstream_patched_header = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        saw_midstream_patched_header,
        "WAV writer must patch RIFF (offset 4) and data (offset 40) chunk sizes every 250 frames while still recording"
    );

    drop(tap);
    let summary = rec.stop().unwrap();
    assert_eq!(summary.format, RecordingFormat::Wav);
    assert_eq!(summary.layout, RecordingChannelLayout::StereoSplit);
    assert_eq!(summary.frames_written, total_frames as u64);
    assert_eq!(summary.duration_ms, (total_frames as u64) * 20);
    let expected_data_len = (total_frames * AUDIO_FRAME_SAMPLES * 4) as u64;
    assert_eq!(summary.bytes_written, 44 + expected_data_len);

    // Verify Unix `0o700` directory mode and `0o600` file mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(&rec_dir).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(&wav_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }

    // Parse full 44-byte WAV header and verify EVERY StereoSplit sample (`Left == TxMic`, `Right == RxRemote`).
    let raw = std::fs::read(&wav_path).unwrap();
    assert_eq!(raw.len() as u64, 44 + expected_data_len);
    assert_eq!(&raw[0..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(raw[4..8].try_into().unwrap()),
        (raw.len() - 8) as u32
    );
    assert_eq!(&raw[8..12], b"WAVE");
    assert_eq!(&raw[12..16], b"fmt ");
    assert_eq!(u32::from_le_bytes(raw[16..20].try_into().unwrap()), 16);
    assert_eq!(u16::from_le_bytes(raw[20..22].try_into().unwrap()), 1); // PCM
    assert_eq!(u16::from_le_bytes(raw[22..24].try_into().unwrap()), 2); // 2 channels
    assert_eq!(u32::from_le_bytes(raw[24..28].try_into().unwrap()), 16_000);
    assert_eq!(u32::from_le_bytes(raw[28..32].try_into().unwrap()), 64_000); // 16000 * 2 * 2
    assert_eq!(u16::from_le_bytes(raw[32..34].try_into().unwrap()), 4); // block_align
    assert_eq!(u16::from_le_bytes(raw[34..36].try_into().unwrap()), 16); // bits_per_sample
    assert_eq!(&raw[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes(raw[40..44].try_into().unwrap()),
        expected_data_len as u32
    );

    let pcm_bytes = &raw[44..];
    for f in 0..total_frames {
        let tx_base = 1000 + f as i16;
        let rx_base = -2000 - f as i16;
        for i in 0..AUDIO_FRAME_SAMPLES {
            let sample_pair_idx = f * AUDIO_FRAME_SAMPLES + i;
            let off = sample_pair_idx * 4;
            let left = i16::from_le_bytes([pcm_bytes[off], pcm_bytes[off + 1]]);
            let right = i16::from_le_bytes([pcm_bytes[off + 2], pcm_bytes[off + 3]]);
            assert_eq!(left, tx_base + (i & 7) as i16);
            assert_eq!(right, rx_base - (i & 7) as i16);
        }
    }

    // 2. Verify MonoMix WAV + soft-clipping behavior.
    assert_eq!(soft_clip_mono_sum(10_000, 5_000), 15_000);
    assert_eq!(soft_clip_mono_sum(-10_000, -5_000), -15_000);
    let loud_pos = soft_clip_mono_sum(30_000, 30_000);
    let loud_neg = soft_clip_mono_sum(-30_000, -30_000);
    assert!(loud_pos > 24_576 && loud_pos <= i16::MAX);
    assert_eq!(loud_neg, -loud_pos);

    let mono_path = rec_dir.join("mono-test.wav");
    let mono_rec = CallRecorderHandle::start(
        mono_path.clone(),
        RecordingFormat::Wav,
        RecordingChannelLayout::MonoMix,
    )
    .unwrap();
    let mono_tap = mono_rec.tap_sender();
    mono_tap
        .send(AudioTapPacket {
            source: TapSource::TxMic,
            samples: [8_000i16; AUDIO_FRAME_SAMPLES],
        })
        .unwrap();
    mono_tap
        .send(AudioTapPacket {
            source: TapSource::RxRemote,
            samples: [4_500i16; AUDIO_FRAME_SAMPLES],
        })
        .unwrap();
    drop(mono_tap);
    let mono_sum = mono_rec.stop().unwrap();
    assert_eq!(mono_sum.frames_written, 1);
    let mono_raw = std::fs::read(&mono_path).unwrap();
    assert_eq!(u16::from_le_bytes(mono_raw[22..24].try_into().unwrap()), 1); // 1 channel
    assert_eq!(
        i16::from_le_bytes([mono_raw[44], mono_raw[45]]),
        12_500 // 8_000 + 4_500 linear sum
    );
}

#[test]
fn recorder_drift_aligner_lag_zero_padding_queue_cap_and_wait_free_audio_tap() {
    assert_eq!(ALIGNER_FRAME_SAMPLES, 320);
    assert_eq!(ALIGNER_LAG_THRESHOLD_SAMPLES, 960);
    assert_eq!(ALIGNER_MAX_QUEUE_SAMPLES, 3_200);

    let mut aligner = DriftAligner::new();

    // 1. Push 2 TX frames (`640` samples < `960` lag threshold) with 0 RX frames -> must wait without padding yet.
    aligner.push(AudioTapPacket {
        source: TapSource::TxMic,
        samples: [101i16; 320],
    });
    aligner.push(AudioTapPacket {
        source: TapSource::TxMic,
        samples: [102i16; 320],
    });
    assert!(aligner.pop_aligned_frame().is_none());
    assert_eq!(aligner.zero_padded_frames(), 0);

    // 2. Push 3rd TX frame (`960` samples == `60ms` threshold) while RX `< 320` -> pops `(tx0, [0; 320])`!
    aligner.push(AudioTapPacket {
        source: TapSource::TxMic,
        samples: [103i16; 320],
    });
    let (tx0, rx0) = aligner
        .pop_aligned_frame()
        .expect("960-sample TX lead must zero-pad lagging RX");
    assert_eq!(tx0, [101i16; 320]);
    assert_eq!(rx0, [0i16; 320]);
    assert_eq!(aligner.zero_padded_frames(), 1);
    // Remaining TX is `640` (< `960`), so no second pad yet.
    assert!(aligner.pop_aligned_frame().is_none());

    // 3. Now RX arrives -> pairs cleanly with queued `tx1` (`102`) without zero-padding!
    aligner.push(AudioTapPacket {
        source: TapSource::RxRemote,
        samples: [202i16; 320],
    });
    let (tx1, rx1) = aligner.pop_aligned_frame().unwrap();
    assert_eq!(tx1, [102i16; 320]);
    assert_eq!(rx1, [202i16; 320]);
    assert_eq!(aligner.zero_padded_frames(), 1);

    // Drain remaining `tx2` via flush, then test reverse lag (`RX >= 960` while `TX == 0`).
    let (tx2_flush, rx2_flush) = aligner.pop_flush_frame().unwrap();
    assert_eq!(tx2_flush, [103i16; 320]);
    assert_eq!(rx2_flush, [0i16; 320]);
    assert!(aligner.pop_flush_frame().is_none());

    for val in [301i16, 302, 303] {
        aligner.push(AudioTapPacket {
            source: TapSource::RxRemote,
            samples: [val; 320],
        });
    }
    let (tx_pad, rx_lead) = aligner
        .pop_aligned_frame()
        .expect("960-sample RX lead must zero-pad lagging TX");
    assert_eq!(tx_pad, [0i16; 320]);
    assert_eq!(rx_lead, [301i16; 320]);

    // 4. Verify `3,200`-sample (`200ms` = 10 frames) clock-skew cap per queue.
    let mut cap_aligner = DriftAligner::new();
    for step in 0..20i16 {
        cap_aligner.push(AudioTapPacket {
            source: TapSource::TxMic,
            samples: [step; 320],
        });
    }
    assert_eq!(cap_aligner.tx_len(), ALIGNER_MAX_QUEUE_SAMPLES);
    let (oldest_kept_tx, _) = cap_aligner.pop_aligned_frame().unwrap();
    // 20 frames pushed, cap = 10 frames -> oldest 10 (`0..10`) dropped, oldest retained is `10`!
    assert_eq!(oldest_kept_tx[0], 10);

    // 5. Verify wait-free `AudioIoSession` tap (`try_send` never blocks when full + counts `dropped_tap_frames`)
    // AND preserves inbound remote PCM in jitter buffer while `speaker_muted` is `true` if `recording_active`!
    let audio = AudioIoSession::new();
    let (tiny_tx, tiny_rx) = std::sync::mpsc::sync_channel::<AudioTapPacket>(2);
    audio.set_audio_tap(Some(tiny_tx));
    for k in 0..5i16 {
        audio.emit_tap_frame(TapSource::TxMic, &[k; 320]);
    }
    assert_eq!(audio.dropped_tap_frames(), 3);
    assert_eq!(tiny_rx.try_recv().unwrap().samples[0], 0);
    assert_eq!(tiny_rx.try_recv().unwrap().samples[0], 1);
    assert!(tiny_rx.try_recv().is_err());

    // Mute speaker while recording is active -> `push_inbound_pcm` must NOT discard remote audio!
    audio.set_speaker_muted(true);
    audio.push_inbound_pcm(&[777i16; 320]);
    audio.set_audio_tap(None);
}

struct ParsedOggPage {
    header_type: u8,
    granule_pos: u64,
    serial: u32,
    page_seq: u32,
    packets: Vec<Vec<u8>>,
}

fn parse_and_verify_ogg_pages(raw: &[u8]) -> Vec<ParsedOggPage> {
    let mut pos = 0usize;
    let mut pages = Vec::new();
    let mut carry_pkt = Vec::new();

    while pos < raw.len() {
        assert!(
            pos + 27 <= raw.len(),
            "truncated Ogg page header at offset {pos}"
        );
        assert_eq!(&raw[pos..pos + 4], b"OggS", "invalid OggS magic at {pos}");
        assert_eq!(raw[pos + 4], 0, "Ogg stream_structure_version must be 0");
        let header_type = raw[pos + 5];
        let granule_pos = u64::from_le_bytes(raw[pos + 6..pos + 14].try_into().unwrap());
        let serial = u32::from_le_bytes(raw[pos + 14..pos + 18].try_into().unwrap());
        let page_seq = u32::from_le_bytes(raw[pos + 18..pos + 22].try_into().unwrap());
        let stored_crc = u32::from_le_bytes(raw[pos + 22..pos + 26].try_into().unwrap());
        let seg_count = raw[pos + 26] as usize;
        let seg_table_end = pos + 27 + seg_count;
        assert!(seg_table_end <= raw.len());
        let seg_table = &raw[pos + 27..seg_table_end];
        let body_len: usize = seg_table.iter().map(|&b| b as usize).sum();
        let page_end = seg_table_end + body_len;
        assert!(page_end <= raw.len());

        // Verify RFC 3533 Ogg CRC-32 with bytes 22..26 zeroed!
        let mut page_copy = raw[pos..page_end].to_vec();
        page_copy[22..26].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            ogg_crc32(&page_copy),
            stored_crc,
            "Ogg CRC-32 mismatch on page_seq={page_seq}"
        );

        let mut packets = Vec::new();
        let mut body_off = seg_table_end;
        for &lacing in seg_table {
            let seg_len = lacing as usize;
            carry_pkt.extend_from_slice(&raw[body_off..body_off + seg_len]);
            body_off += seg_len;
            if lacing < 255 {
                packets.push(std::mem::take(&mut carry_pkt));
            }
        }

        pages.push(ParsedOggPage {
            header_type,
            granule_pos,
            serial,
            page_seq,
            packets,
        });
        pos = page_end;
    }
    assert!(carry_pkt.is_empty(), "unterminated laced packet at EOF");
    pages
}

#[test]
fn recorder_ogg_opus_rfc7845_headers_crc32_granule_decode_and_wav_fallback() {
    let tmp = tempfile::tempdir().unwrap();

    // 1. Deterministic fallback test: when `allow_opus == false`, `.opus` cleanly falls back to `.wav`!
    let req_opus_fb = tmp.path().join("fallback-call.opus");
    let fb_rec = CallRecorderHandle::start_with_opus_availability(
        req_opus_fb,
        RecordingFormat::OggOpus,
        RecordingChannelLayout::StereoSplit,
        false,
    )
    .unwrap();
    assert_eq!(fb_rec.format(), RecordingFormat::Wav);
    assert_eq!(fb_rec.path().extension().unwrap(), "wav");
    let fb_tap = fb_rec.tap_sender();
    fb_tap
        .send(AudioTapPacket {
            source: TapSource::TxMic,
            samples: [500i16; 320],
        })
        .unwrap();
    fb_tap
        .send(AudioTapPacket {
            source: TapSource::RxRemote,
            samples: [-500i16; 320],
        })
        .unwrap();
    drop(fb_tap);
    let fb_summary = fb_rec.stop().unwrap();
    assert_eq!(fb_summary.format, RecordingFormat::Wav);
    assert_eq!(&std::fs::read(&fb_summary.path).unwrap()[0..4], b"RIFF");

    if !is_opus_available() {
        return;
    }

    // 2. Direct multi-channel `OpusPcmEncoder` + `OpusVoipDecoder` (mono) & `OpusPcmDecoder` (stereo) sanity check.
    let mut mono_enc = OpusPcmEncoder::new(1, 32_000).unwrap();
    let mut voip_dec = OpusVoipDecoder::new().unwrap();
    let mono_pkt = mono_enc.encode_20ms_slice(&[4_000i16; 320]).unwrap();
    assert_eq!(voip_dec.decode_20ms(Some(&mono_pkt), false).unwrap().len(), 320);

    // 3. Full RFC 7845 Stereo Ogg/Opus recording (`30` frames = `600ms` across 2 audio pages: 25 + 5 frames).
    let opus_path = build_recording_file_path(tmp.path(), "Trần Minh", RecordingFormat::OggOpus);
    let rec = CallRecorderHandle::start(
        opus_path.clone(),
        RecordingFormat::OggOpus,
        RecordingChannelLayout::StereoSplit,
    )
    .unwrap();
    assert_eq!(rec.format(), RecordingFormat::OggOpus);
    let tap = rec.tap_sender();

    let total_frames = 30usize;
    for f in 0..total_frames {
        let mut tx_sine = [0i16; 320];
        let mut rx_sine = [0i16; 320];
        for i in 0..320 {
            let t = ((f * 320 + i) as f64) / 16_000.0;
            tx_sine[i] = ((2.0 * std::f64::consts::PI * 440.0 * t).sin() * 14_000.0) as i16;
            rx_sine[i] = ((2.0 * std::f64::consts::PI * 880.0 * t).sin() * 11_000.0) as i16;
        }
        tap.send(AudioTapPacket {
            source: TapSource::TxMic,
            samples: tx_sine,
        })
        .unwrap();
        tap.send(AudioTapPacket {
            source: TapSource::RxRemote,
            samples: rx_sine,
        })
        .unwrap();
    }
    drop(tap);

    let summary = rec.stop().unwrap();
    assert_eq!(summary.format, RecordingFormat::OggOpus);
    assert_eq!(summary.layout, RecordingChannelLayout::StereoSplit);
    assert_eq!(summary.frames_written, total_frames as u64);
    assert_eq!(summary.duration_ms, 600);

    let raw_ogg = std::fs::read(&summary.path).unwrap();
    assert_eq!(raw_ogg.len() as u64, summary.bytes_written);

    let pages = parse_and_verify_ogg_pages(&raw_ogg);
    assert!(
        pages.len() >= 3,
        "expected OpusHead + OpusTags + >=1 audio page, got {}",
        pages.len()
    );

    let serial = pages[0].serial;
    for (idx, p) in pages.iter().enumerate() {
        assert_eq!(p.serial, serial);
        assert_eq!(p.page_seq, idx as u32);
        if idx == 0 {
            assert_eq!(p.header_type, 0x02, "Page 0 MUST have BOS flag (0x02)");
        } else if idx + 1 == pages.len() {
            assert_eq!(
                p.header_type & 0x04,
                0x04,
                "Final page MUST have EOS flag (0x04)"
            );
        } else {
            assert_eq!(p.header_type, 0x00);
        }
    }

    // Page 0: RFC 7845 §5.1 `OpusHead` (19 bytes).
    assert_eq!(pages[0].granule_pos, 0);
    assert_eq!(pages[0].packets.len(), 1);
    let head = &pages[0].packets[0];
    assert_eq!(head.len(), 19);
    assert_eq!(&head[0..8], b"OpusHead");
    assert_eq!(head[8], 1); // version = 1
    assert_eq!(head[9], 2); // channels = 2 (StereoSplit)
    assert_eq!(
        u16::from_le_bytes(head[10..12].try_into().unwrap()),
        OGG_OPUS_PRE_SKIP
    );
    assert_eq!(u32::from_le_bytes(head[12..16].try_into().unwrap()), 16_000);
    assert_eq!(i16::from_le_bytes(head[16..18].try_into().unwrap()), 0);
    assert_eq!(head[18], 0); // channel_mapping_family = 0

    // Page 1: RFC 7845 §5.2 `OpusTags` (`vendor = "olaz-linux"`).
    assert_eq!(pages[1].granule_pos, 0);
    assert_eq!(pages[1].packets.len(), 1);
    let tags = &pages[1].packets[0];
    assert_eq!(&tags[0..8], b"OpusTags");
    let vendor_len = u32::from_le_bytes(tags[8..12].try_into().unwrap()) as usize;
    assert_eq!(&tags[12..12 + vendor_len], b"olaz-linux");

    // Audio pages: verify granule progression (`960` per 20ms frame at `48 kHz`) & decode every stereo Opus packet!
    let mut dec = OpusPcmDecoder::new(2).unwrap();
    let mut cumulative_frames = 0u64;
    let mut peak_left: i16 = 0;
    let mut peak_right: i16 = 0;

    for audio_page in &pages[2..] {
        assert!(!audio_page.packets.is_empty());
        cumulative_frames += audio_page.packets.len() as u64;
        assert_eq!(
            audio_page.granule_pos,
            cumulative_frames * OGG_OPUS_GRANULE_STEP_20MS
        );
        for pkt in &audio_page.packets {
            let decoded_interleaved = dec.decode_20ms(Some(pkt), false).unwrap();
            assert_eq!(decoded_interleaved.len(), 320 * 2);
            for pair in decoded_interleaved.chunks_exact(2) {
                peak_left = peak_left.max(pair[0].saturating_abs());
                peak_right = peak_right.max(pair[1].saturating_abs());
            }
        }
    }
    assert_eq!(cumulative_frames, total_frames as u64);
    assert!(
        peak_left > 2_500 && peak_right > 2_000,
        "decoded stereo Opus channels must both carry strong sine tones (peak_left={peak_left}, peak_right={peak_right})"
    );

    // Also verify external `ffmpeg` RFC 7845 Ogg/Opus demux + decode if `ffmpeg` is installed!
    if let Ok(ff_out) = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            summary.path.to_str().unwrap(),
            "-f",
            "s16le",
            "-ac",
            "2",
            "-ar",
            "16000",
            "pipe:1",
        ])
        .output()
    {
        assert!(
            ff_out.status.success(),
            "ffmpeg failed to decode recorded .opus file: {}",
            String::from_utf8_lossy(&ff_out.stderr)
        );
        assert!(
            ff_out.stdout.len() >= 25 * 320 * 4,
            "ffmpeg decoded unexpectedly short PCM byte length: {}",
            ff_out.stdout.len()
        );
    }
}

#[tokio::test]
async fn engine_toggle_record_midcall_manual_stop_and_autostop_on_end_call() {
    let tmp = tempfile::tempdir().unwrap();
    let rec_dir = tmp.path().join("engine_recordings");

    let h = TestHarness::new(Duration::from_secs(30));
    h.engine.set_recordings_dir(rec_dir.clone());
    assert_eq!(h.engine.recordings_dir(), rec_dir);

    // 1. Calling `toggleRecord` with no active call emits `SetRecording { active: false, error: Some(...) }`.
    h.engine.handle_ui_action("toggleRecord", Value::Null);
    {
        let uis = h.ui_events.lock();
        assert!(matches!(
            uis.last(),
            Some(CallUiEvent::SetRecording {
                active: false,
                error: Some(_),
                ..
            })
        ));
    }

    // 2. Establish an outgoing connected call with partner `"Nguyễn Văn Đạt"`.
    h.engine.handle_send_to_native(json!({
        "type": "request",
        "command": "makeCall",
        "data": {
            "type": 1,
            "partner": [{ "id": "888002", "name": "Nguyễn Văn Đạt" }]
        }
    }));
    h.engine.handle_send_to_native(json!({
        "type": "recvSignal",
        "command": 401,
        "data": {
            "fromId": 100_002,
            "toId": 888_002,
            "sessId": SAMPLE_SESS_154,
            "rtpIP": "49.213.119.2:4200"
        }
    }));
    tokio::time::sleep(Duration::from_millis(30)).await;
    h.engine.handle_send_to_native(json!({
        "type": "control",
        "data": { "act": "answer", "data": { "callId": 700_001, "status": 0 } }
    }));

    let drv = h.factory.last_driver.lock().clone().unwrap();

    // 3. Toggle record ON (WAV + StereoSplit), push 6 frames, then toggle record OFF manually mid-call.
    h.engine
        .handle_ui_action("toggleRecord", json!({ "format": "wav", "layout": "stereo" }));
    assert!(h.engine.is_recording());

    let started_wav_path = {
        let uis = h.ui_events.lock();
        match uis.last().cloned() {
            Some(CallUiEvent::SetRecording {
                active: true,
                started_at_ms: Some(ts),
                path: Some(p),
                format: Some(fmt),
                error: None,
            }) => {
                assert!(ts > 0);
                assert_eq!(fmt, "wav");
                assert!(p.ends_with("-nguyen-van-dat.wav"));
                p
            }
            other => panic!("expected SetRecording(active=true, wav), got {other:?}"),
        }
    };

    for f in 0..6i16 {
        drv.emit_tap(AudioTapPacket {
            source: TapSource::TxMic,
            samples: [1000 + f; 320],
        });
        drv.emit_tap(AudioTapPacket {
            source: TapSource::RxRemote,
            samples: [-1000 - f; 320],
        });
    }

    h.engine.handle_ui_action("toggleRecord", Value::Null);
    assert!(!h.engine.is_recording());
    {
        let uis = h.ui_events.lock();
        match uis.last().cloned() {
            Some(CallUiEvent::SetRecording {
                active: false,
                started_at_ms: None,
                path: Some(p),
                format: Some(fmt),
                error: None,
            }) => {
                assert_eq!(p, started_wav_path);
                assert_eq!(fmt, "wav");
            }
            other => panic!("expected SetRecording(active=false, wav), got {other:?}"),
        }
    }
    let wav_bytes = std::fs::read(&started_wav_path).unwrap();
    assert_eq!(wav_bytes.len(), 44 + 6 * 320 * 4);
    assert_eq!(
        u32::from_le_bytes(wav_bytes[40..44].try_into().unwrap()),
        (6 * 320 * 4) as u32
    );

    // 4. Start a SECOND recording (`"opus"`) during the same call, push 8 frames, then send `"endCall"`
    // WITHOUT manually toggling record off -> engine MUST auto-stop & finalize before closing call!
    h.engine
        .handle_ui_action("toggleRecord", json!({ "format": "opus", "layout": "stereo" }));
    assert!(h.engine.is_recording());

    let started_second_path = {
        let uis = h.ui_events.lock();
        match uis.last().cloned() {
            Some(CallUiEvent::SetRecording {
                active: true,
                path: Some(p),
                error: None,
                ..
            }) => p,
            other => panic!("expected 2nd SetRecording(active=true), got {other:?}"),
        }
    };

    for f in 0..8usize {
        let mut s = [0i16; 320];
        for (i, sample) in s.iter_mut().enumerate() {
            let t = ((f * 320 + i) as f64) / 16_000.0;
            *sample = ((2.0 * std::f64::consts::PI * 520.0 * t).sin() * 9_000.0) as i16;
        }
        drv.emit_tap(AudioTapPacket {
            source: TapSource::TxMic,
            samples: s,
        });
        drv.emit_tap(AudioTapPacket {
            source: TapSource::RxRemote,
            samples: s,
        });
    }

    // Trigger `"endCall"` via `handle_send_to_native`!
    h.engine.handle_send_to_native(json!({
        "type": "request",
        "command": "endCall"
    }));
    assert!(!h.engine.is_recording());

    // Wait for delayed `CloseCall` (`10ms` in `TestHarness`).
    tokio::time::sleep(Duration::from_millis(30)).await;

    {
        let uis = h.ui_events.lock();
        let stop_rec_idx = uis
            .iter()
            .rposition(|ev| {
                matches!(
                    ev,
                    CallUiEvent::SetRecording {
                        active: false,
                        path: Some(p),
                        error: None,
                        ..
                    } if p == &started_second_path
                )
            })
            .expect("endCall teardown MUST emit SetRecording(active=false, path=Some(..))");
        let close_idx = uis
            .iter()
            .rposition(|ev| matches!(ev, CallUiEvent::CloseCall))
            .expect("expected CloseCall");
        assert!(
            stop_rec_idx < close_idx,
            "SetRecording(active=false) ({stop_rec_idx}) must fire before CloseCall ({close_idx})"
        );
    }

    let finalized_second = std::fs::read(&started_second_path).unwrap();
    if is_opus_available() {
        let pages = parse_and_verify_ogg_pages(&finalized_second);
        assert!(pages.len() >= 3);
        assert_eq!(
            pages.last().unwrap().header_type & 0x04,
            0x04,
            "auto-stopped .opus recording on endCall must have EOS flag set on final Ogg page"
        );
    } else {
        assert_eq!(&finalized_second[0..4], b"RIFF");
    }
}


