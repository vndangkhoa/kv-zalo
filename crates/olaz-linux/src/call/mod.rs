//! Pure-Rust Linux VoIP Audio, Video & Group Call Engine (`SRTP`, `ZRTC`, `Opus`, `PulseAudio/cpal`, `H.264`).

pub mod audio_io;
pub mod engine;
pub mod opus_ffi;
pub mod recorder;
pub mod srtp;
pub mod ui_assets;
pub mod video_io;
pub mod voip_log;
pub mod zrtc;

pub use audio_io::{
    apply_mic_gain_and_level, downmix_to_mono_i16, enumerate_audio_devices,
    is_ignored_audio_device, is_pulse_simple_available, resample_mono_i16, AudioDeviceEntry,
    AudioDeviceList, AudioIoSession, AudioTapPacket, PlaybackJitterBuffer, TapSource,
    AUDIO_FRAME_SAMPLES, AUDIO_SAMPLE_RATE_HZ, DEFAULT_MIC_GAIN, MAX_PLAYBACK_JITTER_SAMPLES,
};
pub use engine::{
    map_nonzero_answer_status, unpack_send_to_native_args, BoxMediaSetupFuture, CallEngineConfig,
    CallMediaDriver, CallMediaDriverFactory, CallPartnerInfo, CallUiEvent, LiveCallMediaDriver,
    LiveCallMediaFactory, MediaSetupParams, MediaSetupResult, OlazCallEngine,
    OPUS_CODEC_DESCRIPTOR,
};
pub use opus_ffi::{
    is_opus_available, OpusError, OpusPcmDecoder, OpusPcmEncoder, OpusVoipDecoder,
    OpusVoipEncoder, OPUS_APPLICATION_AUDIO, OPUS_RECORDING_DEFAULT_BITRATE, OPUS_VOIP_CHANNELS,
    OPUS_VOIP_DEFAULT_BITRATE, OPUS_VOIP_FRAME_SAMPLES, OPUS_VOIP_SAMPLE_RATE,
};
pub use recorder::{
    build_recording_file_path, default_recordings_dir, ogg_crc32, sanitize_partner_slug,
    soft_clip_mono_sum, CallRecorderHandle, DriftAligner, RecordingChannelLayout, RecordingFormat,
    RecordingSummary, ALIGNER_FRAME_SAMPLES, ALIGNER_LAG_THRESHOLD_SAMPLES,
    ALIGNER_MAX_QUEUE_SAMPLES, OGG_OPUS_GRANULE_STEP_20MS, OGG_OPUS_PRE_SKIP,
    RECORDER_CHANNEL_CAPACITY, WAV_HEADER_BYTES, WAV_SYNC_INTERVAL_FRAMES,
};
pub use srtp::{
    build_srtp_iv, build_zavi_no_seq_iv, extract_master_material, rtp_header_layout, SrtpContext,
    SrtpError, SrtpMode, SrtpSessionKeys,
};
pub use ui_assets::{
    get_ui_asset, CALL_UI_CALL_URL, CALL_UI_DEVICES_URL, CALL_UI_INCOMING_URL, CALL_UI_SCHEME,
    PRELOAD_BRIDGE_JS,
};
pub use video_io::{
    build_rtcp_pli, build_rtcp_rr, build_rtcp_sr_sdes_remb, find_v4l2_camera_device,
    is_h264_idr_au, packetize_zalo_h264_au, rtcp_requests_keyframe, split_annex_b_nals,
    FfmpegH264Decoder, FfmpegVideoCapture, H264Depacketizer, VideoCaptureMode, H264_AUD_ANNEX_B,
    ZALO_H264_PT_IDR, ZALO_H264_PT_PFRAME,
};
pub use voip_log::{
    hex_preview, init_global_voip_log, redact_secret_str, redact_voip_value, voip_log,
    voip_log_json, VoipLogger,
};
pub use zrtc::{
    build_audio_rtp, build_group_init_zrtp_request, build_group_init_zrtp_v6_request,
    build_init_zrtp_request, build_probe, parse_init_zrtp_response, parse_relay_endpoint,
    parse_rtp_packet, unwrap_zrtc, wrap_group_zrtc, wrap_zrtc, InboundZrtcEvent,
    InitZrtpResponse, ParsedRtpPacket, ReflexiveAddress, RelaySweepEntry, UnwrappedZrtc,
    ZrtcError, ZrtcMediaSession, ZrtcPacketKind, ZrtcSweepConfig, ZrtcSweepOutcome,
    DEFAULT_RELAY_PORT,
};
