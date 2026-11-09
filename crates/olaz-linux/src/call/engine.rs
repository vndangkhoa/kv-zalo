//! `OlazCallEngine`: async/thread-safe VoIP coordinator unifying `zalo-linux`'s `main-engine.js`
//! signaling state machine with `ctdozlite`'s pure-Rust SRTP, H.264 video, screen-share, and Group SFU media.

use crate::call::audio_io::{AudioDeviceList, AudioIoSession};
use crate::call::srtp::SrtpMode;
use crate::call::video_io::{
    build_rtcp_pli, build_rtcp_rr, build_rtcp_sr_sdes_remb, packetize_zalo_h264_au,
    rtcp_requests_keyframe, FfmpegH264Decoder, FfmpegVideoCapture, H264Depacketizer,
    VideoCaptureMode, VIDEO_TS_STEP_15FPS,
};
use crate::call::voip_log::voip_log_json;
use crate::call::zrtc::{
    parse_relay_endpoint, InboundZrtcEvent, RelaySweepEntry, ZrtcMediaSession, ZrtcSweepConfig,
    DEFAULT_RELAY_PORT,
};
use parking_lot::Mutex;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::net::{IpAddr, SocketAddr, UdpSocket as StdUdpSocket};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::AbortHandle;

/// Negotiated Opus 16 kHz mono 20ms SDP/JSON descriptor sent in `416` and `402`.
pub const OPUS_CODEC_DESCRIPTOR: &str =
    r#"[{"name":"opus/16000/1","payload":112,"frmPtime":20,"dynamicFptime":0}]"#;

/// Default outgoing/incoming unanswered ring timeout (`60,000 ms`).
pub const DEFAULT_RING_TIMEOUT: Duration = Duration::from_secs(60);

/// Display profile for the remote call peer / group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallPartnerInfo {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
}

/// UI events emitted from `OlazCallEngine` to `CallUiController`.
#[derive(Debug, Clone)]
pub enum CallUiEvent {
    ShowCall {
        partner: CallPartnerInfo,
        is_video: bool,
        is_group: bool,
    },
    ShowIncoming {
        partner: CallPartnerInfo,
        is_video: bool,
        is_group: bool,
    },
    OpenDevices,
    SetState {
        state: String,
        connected_at: Option<u64>,
        outcome: Option<String>,
        is_video: bool,
        camera_enabled: bool,
        screen_sharing: bool,
    },
    SetDevices(AudioDeviceList),
    SetMicLevel {
        permille: u32,
    },
    VideoFrame {
        is_local: bool,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    CloseCall,
    CloseIncoming,
}

/// Parameters passed when opening media relays (`InitZRTP` + audio/video streams).
#[derive(Debug, Clone)]
pub struct MediaSetupParams {
    pub candidates: Vec<SocketAddr>,
    pub prefer_endpoint: Option<String>,
    pub from_id: u32,
    pub to_id: u32,
    pub call_id: u32,
    pub sess_id: String,
    pub is_callee: bool,
    pub is_video: bool,
    pub is_group: bool,
    pub srtp_mode: SrtpMode,
}

/// Result of media setup returned to `OlazCallEngine` for `416` / `402` signaling.
#[derive(Debug, Clone)]
pub struct MediaSetupResult {
    pub chosen_relay: SocketAddr,
    pub local_port: u16,
    pub results: Vec<RelaySweepEntry>,
    pub devices: AudioDeviceList,
}

/// Abstract media driver trait so `OlazCallEngine` runs against the full live ZRTC/SRTP/Opus/H.264
/// stack in production while supporting fast deterministic unit tests without hardware audio devices.
pub trait CallMediaDriver: Send + Sync {
    fn list_devices(&self) -> AudioDeviceList;
    fn set_mic_muted(&self, muted: bool);
    fn set_speaker_muted(&self, muted: bool);
    fn set_input_device(&self, idx: i32);
    fn set_output_device(&self, idx: i32);
    fn set_video_mode(&self, camera_enabled: bool, screen_sharing: bool);
    fn inject_inbound_audio_for_test(&self, opus_payload: &[u8]);
    fn stop(&self);
}

/// Boxed async media setup future.
pub type BoxMediaSetupFuture<'a> = Pin<
    Box<dyn Future<Output = Result<(Arc<dyn CallMediaDriver>, MediaSetupResult), String>> + Send + 'a>,
>;

/// Factory creating a `CallMediaDriver` per call.
pub trait CallMediaDriverFactory: Send + Sync {
    fn open_media<'a>(
        &'a self,
        params: MediaSetupParams,
        ui_cb: Arc<dyn Fn(CallUiEvent) + Send + Sync>,
    ) -> BoxMediaSetupFuture<'a>;
}

/// Production media driver running `ZrtcMediaSession` + `AudioIoSession` + `FfmpegVideoCapture` + `FfmpegH264Decoder`.
pub struct LiveCallMediaDriver {
    zrtc: Arc<ZrtcMediaSession>,
    audio: Arc<AudioIoSession>,
    video_cap: Mutex<Option<FfmpegVideoCapture>>,
    video_dec: Mutex<Option<FfmpegH264Decoder>>,
    closed: Arc<AtomicBool>,
    video_seq: Arc<AtomicU32>,
    video_ts: Arc<AtomicU32>,
    video_pkts: Arc<AtomicU32>,
    video_octets: Arc<AtomicU64>,
    ui_cb: Arc<dyn Fn(CallUiEvent) + Send + Sync>,
    rt_handle: tokio::runtime::Handle,
    bg_tasks: Mutex<Vec<AbortHandle>>,
}

impl LiveCallMediaDriver {
    fn ensure_decoder(&self) {
        let mut dec_guard = self.video_dec.lock();
        if dec_guard.is_none() {
            let ui_cb = Arc::clone(&self.ui_cb);
            if let Ok(dec) = FfmpegH264Decoder::start(move |w, h, rgba| {
                (ui_cb)(CallUiEvent::VideoFrame {
                    is_local: false,
                    width: w,
                    height: h,
                    rgba,
                });
            }) {
                *dec_guard = Some(dec);
            }
        }
    }

    fn restart_capture(&self, mode: Option<VideoCaptureMode>) {
        if let Some(prev) = self.video_cap.lock().take() {
            prev.stop();
        }
        let Some(mode) = mode else {
            return;
        };
        let zrtc = Arc::clone(&self.zrtc);
        let closed = Arc::clone(&self.closed);
        let seq_atom = Arc::clone(&self.video_seq);
        let ts_atom = Arc::clone(&self.video_ts);
        let pkt_atom = Arc::clone(&self.video_pkts);
        let oct_atom = Arc::clone(&self.video_octets);
        let rt = self.rt_handle.clone();
        let ui_cb = Arc::clone(&self.ui_cb);

        let cap_res = FfmpegVideoCapture::start(
            mode,
            move |au| {
                if closed.load(Ordering::Relaxed) {
                    return;
                }
                let ts = ts_atom.fetch_add(VIDEO_TS_STEP_15FPS, Ordering::Relaxed);
                let mut seq = (seq_atom.load(Ordering::Relaxed) & 0xffff) as u16;
                let pkts = packetize_zalo_h264_au(&au, &mut seq, ts, zrtc.ssrc());
                seq_atom.store(seq as u32, Ordering::Relaxed);
                pkt_atom.fetch_add(pkts.len() as u32, Ordering::Relaxed);
                let total_bytes: u64 = pkts.iter().map(|p| p.len() as u64).sum();
                oct_atom.fetch_add(total_bytes, Ordering::Relaxed);

                let z = Arc::clone(&zrtc);
                rt.spawn(async move {
                    let _ = z.send_video_rtp_packets(&pkts).await;
                });
            },
            move |w, h, rgba| {
                (ui_cb)(CallUiEvent::VideoFrame {
                    is_local: true,
                    width: w,
                    height: h,
                    rgba,
                });
            },
        );
        if let Ok(cap) = cap_res {
            *self.video_cap.lock() = Some(cap);
        }
    }
}

impl CallMediaDriver for LiveCallMediaDriver {
    fn list_devices(&self) -> AudioDeviceList {
        self.audio.list_devices()
    }

    fn set_mic_muted(&self, muted: bool) {
        self.audio.set_mic_muted(muted);
    }

    fn set_speaker_muted(&self, muted: bool) {
        self.audio.set_speaker_muted(muted);
    }

    fn set_input_device(&self, idx: i32) {
        self.audio.set_input_device(idx);
    }

    fn set_output_device(&self, idx: i32) {
        self.audio.set_output_device(idx);
    }

    fn set_video_mode(&self, camera_enabled: bool, screen_sharing: bool) {
        if screen_sharing {
            self.ensure_decoder();
            self.restart_capture(Some(VideoCaptureMode::ScreenShare));
        } else if camera_enabled {
            self.ensure_decoder();
            self.restart_capture(Some(VideoCaptureMode::Camera));
        } else {
            self.restart_capture(None);
        }
    }

    fn inject_inbound_audio_for_test(&self, opus_payload: &[u8]) {
        self.audio.push_inbound_opus(opus_payload);
    }

    fn stop(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        for h in self.bg_tasks.lock().drain(..) {
            h.abort();
        }
        if let Some(cap) = self.video_cap.lock().take() {
            cap.stop();
        }
        if let Some(dec) = self.video_dec.lock().take() {
            dec.stop();
        }
        self.audio.stop();
        self.zrtc.close();
    }
}

/// Default production `CallMediaDriverFactory`.
#[derive(Default)]
pub struct LiveCallMediaFactory;

impl CallMediaDriverFactory for LiveCallMediaFactory {
    fn open_media<'a>(
        &'a self,
        params: MediaSetupParams,
        ui_cb: Arc<dyn Fn(CallUiEvent) + Send + Sync>,
    ) -> BoxMediaSetupFuture<'a> {
        Box::pin(async move {
        let mut sweep_cfg = ZrtcSweepConfig::new_one_to_one(
            params.candidates,
            params.prefer_endpoint.as_deref(),
            params.from_id,
            params.to_id,
            params.call_id,
            params.sess_id,
            params.is_callee,
        );
        sweep_cfg.is_group = params.is_group;

        let (zrtc, outcome, mut rx_ev) =
            ZrtcMediaSession::bind_and_sweep(sweep_cfg, params.srtp_mode)
                .await
                .map_err(|e| e.to_string())?;

        let audio = Arc::new(AudioIoSession::new());
        let devices = audio.list_devices();
        let closed = Arc::new(AtomicBool::new(false));
        let rt_handle = tokio::runtime::Handle::current();

        let driver = Arc::new(LiveCallMediaDriver {
            zrtc: Arc::clone(&zrtc),
            audio: Arc::clone(&audio),
            video_cap: Mutex::new(None),
            video_dec: Mutex::new(None),
            closed: Arc::clone(&closed),
            video_seq: Arc::new(AtomicU32::new(1)),
            video_ts: Arc::new(AtomicU32::new(VIDEO_TS_STEP_15FPS)),
            video_pkts: Arc::new(AtomicU32::new(0)),
            video_octets: Arc::new(AtomicU64::new(0)),
            ui_cb: Arc::clone(&ui_cb),
            rt_handle: rt_handle.clone(),
            bg_tasks: Mutex::new(Vec::new()),
        });

        // Start microphone capture -> outbound ZRTC Audio RTP (`0x03`).
        let zrtc_tx = Arc::clone(&zrtc);
        let rt_tx = rt_handle.clone();
        audio.start(move |opus_frame| {
            let z = Arc::clone(&zrtc_tx);
            rt_tx.spawn(async move {
                let _ = z.send_audio_opus(&opus_frame).await;
            });
        });

        if params.is_video {
            driver.set_video_mode(true, false);
        }

        // Inbound ZRTC event loop (Audio -> Opus decode/playback, Video -> H.264 depacketize/decode + PLI).
        let drv_rx = Arc::clone(&driver);
        let remote_audio_ssrc = Arc::new(AtomicU32::new(params.to_id));
        let remote_audio_seq = Arc::new(AtomicU32::new(0));
        let remote_video_ssrc = Arc::new(AtomicU32::new(params.to_id));
        let need_pli_flag = Arc::new(AtomicBool::new(params.is_video));

        let rem_a_ssrc = Arc::clone(&remote_audio_ssrc);
        let rem_a_seq = Arc::clone(&remote_audio_seq);
        let rem_v_ssrc = Arc::clone(&remote_video_ssrc);
        let need_pli_rx = Arc::clone(&need_pli_flag);

        let rx_task = tokio::spawn(async move {
            let mut depack = H264Depacketizer::new();
            while let Some(ev) = rx_ev.recv().await {
                if drv_rx.closed.load(Ordering::Relaxed) {
                    break;
                }
                match ev {
                    InboundZrtcEvent::AudioRtp {
                        seq,
                        ssrc,
                        opus_payload,
                        ..
                    } => {
                        rem_a_ssrc.store(ssrc, Ordering::Relaxed);
                        rem_a_seq.store(seq as u32, Ordering::Relaxed);
                        drv_rx.audio.push_inbound_opus(&opus_payload);
                    }
                    InboundZrtcEvent::VideoRtp { rtp_packet, .. } => {
                        drv_rx.ensure_decoder();
                        if let Some(au) = depack.push_rtp_packet(&rtp_packet) {
                            if let Some( dec ) = drv_rx.video_dec.lock().as_ref() {
                                dec.push_access_unit(&au);
                            }
                        }
                        if let Some(v_ssrc) = depack.remote_ssrc() {
                            rem_v_ssrc.store(v_ssrc, Ordering::Relaxed);
                        }
                        need_pli_rx.store(depack.should_send_pli(), Ordering::Relaxed);
                    }
                    InboundZrtcEvent::VideoRtcp { rtcp_packet, .. } => {
                        if rtcp_requests_keyframe(&rtcp_packet) {
                            if let Some(ref cap) = *drv_rx.video_cap.lock() {
                                cap.request_keyframe();
                            }
                        }
                    }
                    InboundZrtcEvent::AudioRtcp { .. }
                    | InboundZrtcEvent::RelayRetargeted { .. } => {}
                }
            }
        });

        // Periodic RTCP (`500ms` PLI when unsynced/gapped + `2s` Audio RR & Video SR+REMB) + `150ms` mic level meter.
        let drv_tick = Arc::clone(&driver);
        let ui_tick = Arc::clone(&ui_cb);
        let tick_task = tokio::spawn(async move {
            let mut step: u64 = 0;
            let mut interval = tokio::time::interval(Duration::from_millis(125));
            while !drv_tick.closed.load(Ordering::Relaxed) {
                interval.tick().await;
                step += 1;
                let lvl = drv_tick.audio.mic_level_permille();
                (ui_tick)(CallUiEvent::SetMicLevel { permille: lvl });

                // Every 500ms: send Video RTCP PLI while awaiting first SPS/IDR or after packet gap.
                if step % 4 == 0 && need_pli_flag.load(Ordering::Relaxed) {
                    let pli = build_rtcp_pli(
                        drv_tick.zrtc.ssrc(),
                        remote_video_ssrc.load(Ordering::Relaxed),
                    );
                    let _ = drv_tick.zrtc.send_video_rtcp(&pli).await;
                }

                // Every 2000ms: send Audio RR (`0x05`) and (if video active) Video SR+SDES+REMB (`0x0F`).
                if step % 16 == 0 {
                    let rr = build_rtcp_rr(
                        drv_tick.zrtc.ssrc(),
                        remote_audio_ssrc.load(Ordering::Relaxed),
                        remote_audio_seq.load(Ordering::Relaxed),
                        0,
                    );
                    let _ = drv_tick.zrtc.send_audio_rtcp(&rr).await;

                    if drv_tick.video_cap.lock().is_some() || drv_tick.video_dec.lock().is_some() {
                        let sr = build_rtcp_sr_sdes_remb(
                            drv_tick.zrtc.ssrc(),
                            remote_video_ssrc.load(Ordering::Relaxed),
                            drv_tick.video_ts.load(Ordering::Relaxed),
                            drv_tick.video_pkts.load(Ordering::Relaxed),
                            drv_tick.video_octets.load(Ordering::Relaxed) as u32,
                            350_000,
                            "olaz@linux",
                        );
                        let _ = drv_tick.zrtc.send_video_rtcp(&sr).await;
                    }
                }
            }
        });

        driver
            .bg_tasks
            .lock()
            .extend([rx_task.abort_handle(), tick_task.abort_handle()]);

        let setup_res = MediaSetupResult {
            chosen_relay: outcome.chosen_relay,
            local_port: outcome.local_addr.port(),
            results: outcome.results,
            devices,
        };
        Ok((driver as Arc<dyn CallMediaDriver>, setup_res))
        })
    }
}

/// Configuration & callback hooks for `OlazCallEngine`.
pub struct CallEngineConfig {
    pub emit_call_update: Arc<dyn Fn(&str, Value) + Send + Sync>,
    pub emit_call_signal: Arc<dyn Fn(u32, Value) + Send + Sync>,
    pub emit_ui_event: Arc<dyn Fn(CallUiEvent) + Send + Sync>,
    pub media_factory: Arc<dyn CallMediaDriverFactory>,
    pub ring_timeout: Duration,
    pub close_ui_delay: Duration,
    pub call_id_generator: Option<Arc<dyn Fn() -> i32 + Send + Sync>>,
}

impl CallEngineConfig {
    pub fn new<FU, FS, FUi>(
        emit_call_update: FU,
        emit_call_signal: FS,
        emit_ui_event: FUi,
    ) -> Self
    where
        FU: Fn(&str, Value) + Send + Sync + 'static,
        FS: Fn(u32, Value) + Send + Sync + 'static,
        FUi: Fn(CallUiEvent) + Send + Sync + 'static,
    {
        Self {
            emit_call_update: Arc::new(emit_call_update),
            emit_call_signal: Arc::new(emit_call_signal),
            emit_ui_event: Arc::new(emit_ui_event),
            media_factory: Arc::new(LiveCallMediaFactory),
            ring_timeout: DEFAULT_RING_TIMEOUT,
            close_ui_delay: Duration::from_millis(900),
            call_id_generator: None,
        }
    }
}

struct ActiveCallState {
    call_id: i32,
    /// Routing UID (`d.uidN ?? d.uidFrom ?? d.fromId` on incoming; `calleeId` on outgoing).
    route_id: String,
    /// Media peer UID (`d.uidFrom ?? d.uidN` on incoming; `cfg.toId` on outgoing) for `InitZRTP` `to_id`.
    media_peer_id: u32,
    /// Local media UID (`d.uidTo` on incoming; `cfg.fromId` on outgoing) for `InitZRTP` `from_id`.
    local_media_id: u32,
    partner: CallPartnerInfo,
    sess_id: String,
    incoming: bool,
    answered: bool,
    connected_at_ms: Option<u64>,
    is_video: bool,
    is_group: bool,
    camera_enabled: bool,
    screen_sharing: bool,
    speaker_muted: bool,
    candidates: Vec<SocketAddr>,
    prefer_endpoint: Option<String>,
    srtp_mode: SrtpMode,
    media: Option<Arc<dyn CallMediaDriver>>,
    ring_timer: Option<AbortHandle>,
}

/// Thread-safe VoIP call signaling & media coordinator.
pub struct OlazCallEngine {
    cfg: CallEngineConfig,
    active: Mutex<Option<ActiveCallState>>,
}

impl OlazCallEngine {
    /// Creates a new `OlazCallEngine` wrapped in `Arc`.
    pub fn new(cfg: CallEngineConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            active: Mutex::new(None),
        })
    }

    /// Emits `("native-ready", {})` to notify the Zalo web renderer that native calling is active.
    pub fn announce_native_ready(&self) {
        (self.cfg.emit_call_update)("native-ready", json!({}));
    }

    /// Returns `true` if a call is currently dialing, ringing, or connected.
    pub fn has_active_call(&self) -> bool {
        self.active.lock().is_some()
    }

    /// Returns the active media driver (if media has been opened for the current call).
    pub fn active_media_driver(&self) -> Option<Arc<dyn CallMediaDriver>> {
        self.active.lock().as_ref().and_then(|c| c.media.clone())
    }

    /// Unpacks `call-send-to-native` IPC arguments whether delivered as `[json_string, optional_bool]`,
    /// `[json_object]`, bare `json_string`, or bare `json_object`, and dispatches into the engine.
    pub fn handle_ipc_args(self: &Arc<Self>, raw_args: &Value) {
        if let Some(msg) = unpack_send_to_native_args(raw_args) {
            self.handle_send_to_native(msg);
        }
    }

    /// Handles a normalized `call-send-to-native` payload object.
    pub fn handle_send_to_native(self: &Arc<Self>, mut msg: Value) {
        if let Some(obj) = msg.as_object_mut() {
            obj.remove("_optional");
        }
        voip_log_json("engine.in", "handleSendToNative", &msg);

        let msg_type = msg.get("type").and_then(Value::as_str).unwrap_or("");
        match msg_type {
            "request" => {
                let cmd = msg.get("command").and_then(Value::as_str).unwrap_or("");
                let data = msg.get("data").cloned().unwrap_or_else(|| json!({}));
                match cmd {
                    "makeCall" | "startGroupCall" | "joinGroupCall" => {
                        let force_group = cmd != "makeCall";
                        self.start_outgoing_call(data, force_group);
                    }
                    "endCall" => {
                        self.handle_ui_action("end", Value::Null);
                    }
                    _ => {}
                }
            }
            "recvSignal" => {
                let cmd = value_to_u32(msg.get("command")).unwrap_or(0);
                let data = parse_json_if_str(msg.get("data").cloned().unwrap_or_else(|| json!({})));
                match cmd {
                    401 => self.on_outgoing_config(data),
                    407 => self.on_remote_ringing(data.get("callId")),
                    405 | 409 => self.on_remote_terminate(data.get("callId")),
                    12006 => self.on_remote_video_conf(&data),
                    12064 => self.on_remote_screen_share(&data),
                    _ => {}
                }
            }
            "control" => {
                let ctrl = parse_json_if_str(msg.get("data").cloned().unwrap_or_else(|| json!({})));
                let act = ctrl.get("act").and_then(Value::as_str).unwrap_or("");
                match act {
                    "request" | "group_request" => self.on_incoming_request(&ctrl),
                    "ring_ring" => {
                        let cid = ctrl.get("data").and_then(|d| d.get("callId"));
                        self.on_remote_ringing(cid);
                    }
                    "answer" => {
                        let d = ctrl.get("data").cloned().unwrap_or_else(|| json!({}));
                        self.on_outgoing_answer(&d);
                    }
                    "end_call" | "cancel" => {
                        let cid = ctrl.get("data").and_then(|d| d.get("callId"));
                        self.on_remote_terminate(cid);
                    }
                    "conf" => {
                        let d = ctrl.get("data").cloned().unwrap_or_else(|| json!({}));
                        self.on_remote_video_conf(&d);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Handles user actions originating from the Call / Incoming / Devices UI windows.
    pub fn handle_ui_action(self: &Arc<Self>, action: &str, value: Value) {
        match action {
            "end" | "hangup" => self.user_hangup_or_cancel(),
            "accept" => self.user_accept_incoming(),
            "decline" => self.user_decline_incoming(),
            "mute" => {
                let muted = value.as_bool().unwrap_or(false);
                if let Some(m) = self.active_media_driver() {
                    m.set_mic_muted(muted);
                }
            }
            "toggleSpeaker" => {
                let muted = value.as_bool().unwrap_or(false);
                let mut guard = self.active.lock();
                if let Some(ref mut c) = *guard {
                    c.speaker_muted = muted;
                    if let Some(ref m) = c.media {
                        m.set_speaker_muted(muted);
                    }
                }
            }
            "toggleCamera" | "toggleVideo" => self.user_toggle_camera(value.as_bool()),
            "toggleScreenShare" | "shareScreen" => self.user_toggle_screen_share(value.as_bool()),
            "selectInput" => {
                if let Some(idx) = value_to_i32(Some(&value)) {
                    if let Some(m) = self.active_media_driver() {
                        m.set_input_device(idx);
                        (self.cfg.emit_ui_event)(CallUiEvent::SetDevices(m.list_devices()));
                    }
                }
            }
            "selectOutput" => {
                if let Some(idx) = value_to_i32(Some(&value)) {
                    if let Some(m) = self.active_media_driver() {
                        m.set_output_device(idx);
                        (self.cfg.emit_ui_event)(CallUiEvent::SetDevices(m.list_devices()));
                    }
                }
            }
            "openDevices" => {
                let devs = self
                    .active_media_driver()
                    .map(|m| m.list_devices())
                    .unwrap_or_else(crate::call::audio_io::enumerate_audio_devices);
                (self.cfg.emit_ui_event)(CallUiEvent::SetDevices(devs));
                (self.cfg.emit_ui_event)(CallUiEvent::OpenDevices);
            }
            _ => {}
        }
    }

    /// Stops any active call immediately (e.g. on app shutdown).
    pub fn stop(&self) {
        let should_teardown = self.active.lock().is_some();
        if should_teardown {
            self.teardown(0, None);
        }
    }

    fn next_call_id(&self) -> i32 {
        if let Some(ref gen_fn) = self.cfg.call_id_generator {
            return gen_fn();
        }
        rand::thread_rng().gen_range(1..0x7fff_ffff)
    }

    fn emit_update(&self, cmd: &str, data: Value) {
        voip_log_json("engine.update", cmd, &data);
        (self.cfg.emit_call_update)(cmd, data);
    }

    fn emit_signal(&self, cmd: u32, data: Value) {
        voip_log_json("engine.signal", &cmd.to_string(), &data);
        (self.cfg.emit_call_signal)(cmd, data);
    }

    fn start_outgoing_call(self: &Arc<Self>, data: Value, force_group: bool) {
        if self.has_active_call() {
            return;
        }
        let first_partner = data
            .get("partner")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or_else(|| json!({}));

        let route_id = value_to_string(first_partner.get("id"))
            .or_else(|| value_to_string(data.get("calleeId")))
            .or_else(|| value_to_string(data.get("groupId")))
            .unwrap_or_default();
        if route_id.is_empty() {
            return;
        }

        let name = first_partner
            .get("name")
            .or_else(|| first_partner.get("dName"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| route_id.clone());
        let avatar = first_partner
            .get("avatar")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);

        let raw_call_type = value_to_i32(data.get("type")).unwrap_or(1);
        let is_video = matches!(raw_call_type, 2 | 3)
            || value_to_i32(data.get("callType")) == Some(1)
            || data
                .get("isVideo")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let is_group = force_group
            || data
                .get("isGroup")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || data.get("groupId").is_some();

        let call_id = self.next_call_id();
        let partner = CallPartnerInfo {
            id: route_id.clone(),
            name,
            avatar,
        };

        *self.active.lock() = Some(ActiveCallState {
            call_id,
            route_id: route_id.clone(),
            media_peer_id: route_id.parse::<u32>().unwrap_or(0),
            local_media_id: 0,
            partner: partner.clone(),
            sess_id: String::new(),
            incoming: false,
            answered: false,
            connected_at_ms: None,
            is_video,
            is_group,
            camera_enabled: is_video,
            screen_sharing: false,
            speaker_muted: false,
            candidates: Vec::new(),
            prefer_endpoint: None,
            srtp_mode: SrtpMode::AesCm128HmacSha1_80,
            media: None,
            ring_timer: None,
        });

        self.emit_update("callState", json!({ "state": "calling", "callId": call_id }));
        self.emit_signal(
            401,
            json!({
                "calleeId": route_id,
                "callId": call_id,
                "type": if is_video { raw_call_type.max(2) } else { 1 },
                "subType": 1,
                "networkType": 4,
                "version": 1,
                "params": "",
            }),
        );
        (self.cfg.emit_ui_event)(CallUiEvent::ShowCall {
            partner,
            is_video,
            is_group,
        });
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: "calling".to_string(),
            connected_at: None,
            outcome: None,
            is_video,
            camera_enabled: is_video,
            screen_sharing: false,
        });
    }

    fn on_outgoing_config(self: &Arc<Self>, raw_cfg: Value) {
        let cfg = if let Some(inner) = raw_cfg.get("data").filter(|v| v.is_object() || v.is_string())
        {
            let parsed = parse_json_if_str(inner.clone());
            if parsed.get("sessId").is_some() {
                parsed
            } else {
                raw_cfg
            }
        } else {
            raw_cfg
        };

        let sess_id = cfg
            .get("sessId")
            .or_else(|| cfg.get("session"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if sess_id.len() < 30 {
            return;
        }

        let mut candidates = extract_candidate_relays(&cfg);
        let prefer_ep = cfg
            .get("rtpIP")
            .or_else(|| cfg.get("rtpAddress"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if let Some(ref ep) = prefer_ep {
            if let Some(sa) = parse_relay_endpoint(ep, DEFAULT_RELAY_PORT) {
                if !candidates.contains(&sa) {
                    candidates.insert(0, sa);
                }
            }
        }

        let is_sfu_port = candidates
            .iter()
            .any(|c| c.port() == 3000 || c.port() == 3500);
        let srtp_mode = if let Some(code) = cfg.get("srtpMode").and_then(Value::as_i64) {
            SrtpMode::from_wire_code(code)
        } else if is_sfu_port {
            SrtpMode::ZaviNullAuthNoSeq
        } else {
            SrtpMode::AesCm128HmacSha1_80
        };

        let (params, callee_id, call_id, is_video) = {
            let mut guard = self.active.lock();
            let Some(ref mut c) = *guard else {
                return;
            };
            if c.incoming {
                return;
            }
            c.sess_id = sess_id.clone();
            if let Some(fid) = value_to_u32(cfg.get("fromId")) {
                c.local_media_id = fid;
            }
            if let Some(tid) = value_to_u32(cfg.get("toId")) {
                c.media_peer_id = tid;
            }
            if is_sfu_port {
                c.is_group = true;
            }
            c.candidates = candidates.clone();
            c.prefer_endpoint = prefer_ep.clone();
            c.srtp_mode = srtp_mode;

            (
                MediaSetupParams {
                    candidates,
                    prefer_endpoint: prefer_ep,
                    from_id: c.local_media_id,
                    to_id: c.media_peer_id,
                    call_id: c.call_id as u32,
                    sess_id,
                    is_callee: false,
                    is_video: c.is_video,
                    is_group: c.is_group,
                    srtp_mode,
                },
                c.route_id.clone(),
                c.call_id,
                c.is_video,
            )
        };

        let eng = Arc::clone(self);
        tokio::spawn(async move {
            let ui_cb = Arc::clone(&eng.cfg.emit_ui_event);
            let Ok((driver, setup_res)) = eng.cfg.media_factory.open_media(params.clone(), ui_cb).await
            else {
                eng.teardown(2, None);
                return;
            };

            let extend_data = build_extend_data_json(
                &setup_res.chosen_relay,
                setup_res.local_port,
                &setup_res.results,
                is_video,
                params.srtp_mode,
            );
            let chosen_str = setup_res.chosen_relay.to_string();

            {
                let mut guard = eng.active.lock();
                let Some(ref mut c) = *guard else {
                    driver.stop();
                    return;
                };
                if c.call_id != call_id {
                    driver.stop();
                    return;
                }
                if c.speaker_muted {
                    driver.set_speaker_muted(true);
                }
                c.media = Some(Arc::clone(&driver));
                c.ring_timer = Some(eng.spawn_ring_timeout(call_id, true));
            }

            (eng.cfg.emit_ui_event)(CallUiEvent::SetDevices(setup_res.devices));
            eng.emit_signal(
                416,
                json!({
                    "calleeId": callee_id,
                    "callId": call_id,
                    "codec": OPUS_CODEC_DESCRIPTOR,
                    "extendData": extend_data,
                    "session": params.sess_id,
                    "rtpAddress": chosen_str,
                    "rtcpAddress": chosen_str,
                    "rtpIPv6Addr": "",
                    "rtcpIPv6Addr": "",
                    "status": 0,
                    "version": 1,
                }),
            );
        });
    }

    fn spawn_ring_timeout(self: &Arc<Self>, call_id: i32, send_405_on_expire: bool) -> AbortHandle {
        let eng = Arc::clone(self);
        let wait = self.cfg.ring_timeout;
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let cancel_payload = {
                let guard = eng.active.lock();
                let Some(ref c) = *guard else {
                    return;
                };
                if c.call_id != call_id || c.answered {
                    return;
                }
                if send_405_on_expire && !c.incoming {
                    Some(json!({
                        "calleeId": c.route_id,
                        "callId": c.call_id,
                        "reason": 1,
                        "session": c.sess_id,
                        "status": 0,
                    }))
                } else {
                    None
                }
            };
            if let Some(p405) = cancel_payload {
                eng.emit_signal(405, p405);
            }
            eng.teardown(2, None);
        })
        .abort_handle()
    }

    fn on_remote_ringing(&self, call_id_val: Option<&Value>) {
        let state_info = {
            let guard = self.active.lock();
            let Some(ref c) = *guard else {
                return;
            };
            if let Some(cid) = value_to_i32(call_id_val) {
                if cid != c.call_id {
                    return;
                }
            }
            if c.answered || c.incoming {
                return;
            }
            (c.call_id, c.is_video, c.camera_enabled, c.screen_sharing)
        };
        self.emit_update(
            "callState",
            json!({ "state": "ringing", "callId": state_info.0 }),
        );
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: "ringing".to_string(),
            connected_at: None,
            outcome: None,
            is_video: state_info.1,
            camera_enabled: state_info.2,
            screen_sharing: state_info.3,
        });
    }

    fn on_outgoing_answer(&self, d: &Value) {
        let status = value_to_i64(d.get("status")).unwrap_or(0);
        if status != 0 {
            let (reason, outcome) = map_nonzero_answer_status(status);
            self.teardown(reason, outcome.map(ToOwned::to_owned));
            return;
        }

        let connected_info = {
            let mut guard = self.active.lock();
            let Some(ref mut c) = *guard else {
                return;
            };
            if let Some(cid) = value_to_i32(d.get("callId")) {
                if cid != c.call_id {
                    return;
                }
            }
            // Duplicate `answer status=0` guard: ignore once connected so no 2nd `408` is sent!
            if c.answered {
                return;
            }
            c.answered = true;
            if let Some(t) = c.ring_timer.take() {
                t.abort();
            }
            let now_ms = current_epoch_ms();
            c.connected_at_ms = Some(now_ms);
            (
                c.route_id.clone(),
                c.call_id,
                c.sess_id.clone(),
                now_ms,
                c.is_video,
                c.camera_enabled,
                c.screen_sharing,
            )
        };

        self.emit_signal(
            408,
            json!({
                "calleeId": connected_info.0,
                "callId": connected_info.1,
                "session": connected_info.2,
                "status": 0,
            }),
        );
        self.emit_update(
            "callState",
            json!({ "state": "connected", "callId": connected_info.1 }),
        );
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: "connected".to_string(),
            connected_at: Some(connected_info.3),
            outcome: None,
            is_video: connected_info.4,
            camera_enabled: connected_info.5,
            screen_sharing: connected_info.6,
        });
    }

    fn on_incoming_request(self: &Arc<Self>, ctrl: &Value) {
        let d = ctrl.get("data").cloned().unwrap_or_else(|| json!({}));
        let Some(call_id) = value_to_i32(d.get("callId")) else {
            return;
        };

        // Strict Dual-ID Split:
        // - `route_id = d.uidN ?? d.uidFrom ?? d.fromId` -> used for 407, 402, 409, and bubble.partnerId.
        // - `media_id = d.uidFrom ?? d.uidN ?? d.fromId` -> used for InitZRTP `to_id`.
        // - `from_id  = d.uidTo`                         -> used for InitZRTP `from_id` (`is_callee = true`, 0x0c).
        let route_id = value_to_string(d.get("uidN"))
            .or_else(|| value_to_string(d.get("uidFrom")))
            .or_else(|| value_to_string(d.get("fromId")))
            .unwrap_or_default();
        if route_id.is_empty() {
            return;
        }
        let media_id = value_to_u32(d.get("uidFrom"))
            .or_else(|| value_to_u32(d.get("uidN")))
            .or_else(|| value_to_u32(d.get("fromId")))
            .unwrap_or(0);
        let from_id = value_to_u32(d.get("uidTo")).unwrap_or(0);

        let params = parse_json_if_str(d.get("params").cloned().unwrap_or_else(|| json!({})));
        let ext = parse_json_if_str(
            params
                .get("extendData")
                .cloned()
                .unwrap_or_else(|| json!({})),
        );

        let sess_id = d
            .get("session")
            .and_then(Value::as_str)
            .or_else(|| params.get("sessId").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();

        let in_call_elsewhere = ctrl
            .get("inCallStatus")
            .and_then(|v| match v {
                Value::Bool(b) => Some(*b),
                Value::String(s) => Some(!s.trim().is_empty()),
                _ => None,
            })
            .unwrap_or(false);

        // Busy auto-reject (`402 status: 1`) when already in a call or `ctrl.inCallStatus` is truthy.
        if self.has_active_call() || in_call_elsewhere {
            self.emit_signal(
                402,
                build_callee_402_payload(&route_id, call_id, &sess_id, 1, None),
            );
            return;
        }

        let is_video = value_to_i32(ext.get("callType")) == Some(1)
            || value_to_i32(d.get("callType")) == Some(1)
            || matches!(value_to_i32(d.get("type")), Some(2 | 3));
        let is_group = ctrl.get("act").and_then(Value::as_str) == Some("group_request")
            || d.get("groupId").is_some();

        let prefer_ep = d
            .get("rtpAddress")
            .and_then(Value::as_str)
            .or_else(|| params.get("rtpIP").and_then(Value::as_str))
            .map(ToOwned::to_owned);

        let mut candidates = Vec::new();
        for raw_ep in [
            d.get("rtpAddress").and_then(Value::as_str),
            params.get("rtpIP").and_then(Value::as_str),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(sa) = parse_relay_endpoint(raw_ep, DEFAULT_RELAY_PORT) {
                if !candidates.contains(&sa) {
                    candidates.push(sa);
                }
            }
        }
        for key in ["serverResult", "serverAddr"] {
            if let Some(arr) = ext.get(key).and_then(Value::as_array) {
                for item in arr {
                    if let Some(rtp_str) = item
                        .get("rtp")
                        .or_else(|| item.get("rtpaddr"))
                        .and_then(Value::as_str)
                    {
                        if let Some(sa) = parse_relay_endpoint(rtp_str, DEFAULT_RELAY_PORT) {
                            if !candidates.contains(&sa) {
                                candidates.push(sa);
                            }
                        }
                    }
                }
            }
        }

        let is_sfu_port = candidates
            .iter()
            .any(|c| c.port() == 3000 || c.port() == 3500);
        let srtp_mode = if let Some(code) = ext.get("srtpMode").and_then(Value::as_i64) {
            SrtpMode::from_wire_code(code)
        } else if is_sfu_port {
            SrtpMode::ZaviNullAuthNoSeq
        } else {
            SrtpMode::AesCm128HmacSha1_80
        };

        let caller_meta = ctrl.get("_caller").cloned().unwrap_or_else(|| json!({}));
        let caller_name = caller_meta
            .get("name")
            .or_else(|| caller_meta.get("dName"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| route_id.clone());
        let caller_avatar = caller_meta
            .get("avatar")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let partner = CallPartnerInfo {
            id: route_id.clone(),
            name: caller_name,
            avatar: caller_avatar,
        };

        let ring_timer = self.spawn_ring_timeout(call_id, false);
        *self.active.lock() = Some(ActiveCallState {
            call_id,
            route_id: route_id.clone(),
            media_peer_id: media_id,
            local_media_id: from_id,
            partner: partner.clone(),
            sess_id: sess_id.clone(),
            incoming: true,
            answered: false,
            connected_at_ms: None,
            is_video,
            is_group: is_group || is_sfu_port,
            camera_enabled: is_video,
            screen_sharing: false,
            speaker_muted: false,
            candidates,
            prefer_endpoint: prefer_ep,
            srtp_mode,
            media: None,
            ring_timer: Some(ring_timer),
        });

        self.emit_signal(
            407,
            json!({
                "callerId": route_id,
                "callId": call_id,
                "session": sess_id,
                "status": 0,
            }),
        );
        self.emit_update(
            "callState",
            json!({ "state": "ringing-incoming", "callId": call_id }),
        );
        (self.cfg.emit_ui_event)(CallUiEvent::ShowIncoming {
            partner,
            is_video,
            is_group,
        });
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: "ringing-incoming".to_string(),
            connected_at: None,
            outcome: None,
            is_video,
            camera_enabled: is_video,
            screen_sharing: false,
        });
    }

    fn user_accept_incoming(self: &Arc<Self>) {
        let (params, route_id, call_id, partner, is_video, is_group) = {
            let mut guard = self.active.lock();
            let Some(ref mut c) = *guard else {
                return;
            };
            if !c.incoming || c.answered {
                return;
            }
            c.answered = true;
            if let Some(t) = c.ring_timer.take() {
                t.abort();
            }
            (
                MediaSetupParams {
                    candidates: c.candidates.clone(),
                    prefer_endpoint: c.prefer_endpoint.clone(),
                    from_id: c.local_media_id,
                    to_id: c.media_peer_id,
                    call_id: c.call_id as u32,
                    sess_id: c.sess_id.clone(),
                    is_callee: true, // Role byte `0x0c`!
                    is_video: c.is_video,
                    is_group: c.is_group,
                    srtp_mode: c.srtp_mode,
                },
                c.route_id.clone(),
                c.call_id,
                c.partner.clone(),
                c.is_video,
                c.is_group,
            )
        };

        (self.cfg.emit_ui_event)(CallUiEvent::CloseIncoming);
        (self.cfg.emit_ui_event)(CallUiEvent::ShowCall {
            partner,
            is_video,
            is_group,
        });
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: "connecting".to_string(),
            connected_at: None,
            outcome: None,
            is_video,
            camera_enabled: is_video,
            screen_sharing: false,
        });

        let eng = Arc::clone(self);
        tokio::spawn(async move {
            let ui_cb = Arc::clone(&eng.cfg.emit_ui_event);
            let Ok((driver, setup_res)) = eng.cfg.media_factory.open_media(params.clone(), ui_cb).await
            else {
                eng.teardown(2, None);
                return;
            };

            let extend_data = build_extend_data_json(
                &setup_res.chosen_relay,
                setup_res.local_port,
                &setup_res.results,
                is_video,
                params.srtp_mode,
            );
            let chosen_str = setup_res.chosen_relay.to_string();
            let now_ms = current_epoch_ms();

            {
                let mut guard = eng.active.lock();
                let Some(ref mut c) = *guard else {
                    driver.stop();
                    return;
                };
                if c.call_id != call_id {
                    driver.stop();
                    return;
                }
                if c.speaker_muted {
                    driver.set_speaker_muted(true);
                }
                c.media = Some(Arc::clone(&driver));
                c.connected_at_ms = Some(now_ms);
            }

            eng.emit_signal(
                402,
                build_callee_402_payload(
                    &route_id,
                    call_id,
                    &params.sess_id,
                    0,
                    Some((&chosen_str, &extend_data)),
                ),
            );
            (eng.cfg.emit_ui_event)(CallUiEvent::SetDevices(setup_res.devices));
            (eng.cfg.emit_ui_event)(CallUiEvent::SetState {
                state: "connected".to_string(),
                connected_at: Some(now_ms),
                outcome: None,
                is_video,
                camera_enabled: is_video,
                screen_sharing: false,
            });
            eng.emit_update(
                "callState",
                json!({ "state": "connected", "callId": call_id }),
            );
        });
    }

    fn user_decline_incoming(&self) {
        let decline_info = {
            let guard = self.active.lock();
            let Some(ref c) = *guard else {
                return;
            };
            if !c.incoming || c.answered {
                return;
            }
            (c.route_id.clone(), c.call_id, c.sess_id.clone())
        };
        // Explicit `""` strings on every empty field (NEVER `null`) so Zalo server accepts `402 status: 3`!
        self.emit_signal(
            402,
            build_callee_402_payload(
                &decline_info.0,
                decline_info.1,
                &decline_info.2,
                3,
                None,
            ),
        );
        self.teardown(3, None);
    }

    fn user_hangup_or_cancel(&self) {
        let state = {
            let guard = self.active.lock();
            let Some(ref c) = *guard else {
                return;
            };
            (
                c.incoming,
                c.answered,
                c.route_id.clone(),
                c.call_id,
                c.sess_id.clone(),
            )
        };
        let (incoming, answered, route_id, call_id, sess_id) = state;
        if incoming && !answered {
            self.user_decline_incoming();
            return;
        }
        if answered {
            self.emit_signal(
                409,
                json!({
                    "partnerId": route_id,
                    "callId": call_id,
                    "duration": 0,
                    "reason": 0,
                    "session": sess_id,
                    "status": 0,
                }),
            );
            self.teardown(0, None);
        } else {
            self.emit_signal(
                405,
                json!({
                    "calleeId": route_id,
                    "callId": call_id,
                    "reason": 1,
                    "session": sess_id,
                    "status": 0,
                }),
            );
            self.teardown(4, None);
        }
    }

    fn user_toggle_camera(&self, explicit: Option<bool>) {
        let update = {
            let mut guard = self.active.lock();
            let Some(ref mut c) = *guard else {
                return;
            };
            let next = explicit.unwrap_or(!c.camera_enabled);
            c.camera_enabled = next;
            if next {
                c.is_video = true;
            }
            if let Some(ref m) = c.media {
                m.set_video_mode(c.camera_enabled, c.screen_sharing);
            }
            (
                c.call_id,
                c.route_id.clone(),
                c.sess_id.clone(),
                c.camera_enabled,
                c.screen_sharing,
                c.is_video,
                c.answered,
                c.connected_at_ms,
            )
        };

        self.emit_signal(
            12006,
            json!({
                "callId": update.0,
                "partnerId": update.1,
                "video": if update.3 { 1 } else { 0 },
                "session": update.2,
                "status": 0,
            }),
        );
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: if update.6 {
                "connected".to_string()
            } else {
                "calling".to_string()
            },
            connected_at: update.7,
            outcome: None,
            is_video: update.5,
            camera_enabled: update.3,
            screen_sharing: update.4,
        });
    }

    fn user_toggle_screen_share(&self, explicit: Option<bool>) {
        let update = {
            let mut guard = self.active.lock();
            let Some(ref mut c) = *guard else {
                return;
            };
            let next = explicit.unwrap_or(!c.screen_sharing);
            c.screen_sharing = next;
            if next {
                c.is_video = true;
            }
            if let Some(ref m) = c.media {
                m.set_video_mode(c.camera_enabled, c.screen_sharing);
            }
            (
                c.call_id,
                c.route_id.clone(),
                c.sess_id.clone(),
                c.camera_enabled,
                c.screen_sharing,
                c.is_video,
                c.answered,
                c.connected_at_ms,
            )
        };

        self.emit_signal(
            12064,
            json!({
                "callId": update.0,
                "partnerId": update.1,
                "shareScreen": if update.4 { 1 } else { 0 },
                "session": update.2,
                "status": 0,
            }),
        );
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: if update.6 {
                "connected".to_string()
            } else {
                "calling".to_string()
            },
            connected_at: update.7,
            outcome: None,
            is_video: update.5,
            camera_enabled: update.3,
            screen_sharing: update.4,
        });
    }

    fn on_remote_video_conf(&self, d: &Value) {
        let remote_video_on = value_to_i32(d.get("video")).unwrap_or(1) != 0;
        let snapshot = {
            let mut guard = self.active.lock();
            let Some(ref mut c) = *guard else {
                return;
            };
            if remote_video_on {
                c.is_video = true;
            }
            (
                c.answered,
                c.connected_at_ms,
                c.is_video,
                c.camera_enabled,
                c.screen_sharing,
            )
        };
        (self.cfg.emit_ui_event)(CallUiEvent::SetState {
            state: if snapshot.0 {
                "connected".to_string()
            } else {
                "calling".to_string()
            },
            connected_at: snapshot.1,
            outcome: None,
            is_video: snapshot.2,
            camera_enabled: snapshot.3,
            screen_sharing: snapshot.4,
        });
    }

    fn on_remote_screen_share(&self, d: &Value) {
        let remote_share = value_to_i32(d.get("shareScreen")).unwrap_or(1) != 0;
        if remote_share {
            self.on_remote_video_conf(&json!({ "video": 1 }));
        }
    }

    fn on_remote_terminate(&self, call_id_val: Option<&Value>) {
        let reason = {
            let guard = self.active.lock();
            let Some(ref c) = *guard else {
                return;
            };
            if let Some(cid) = value_to_i32(call_id_val) {
                if cid != c.call_id {
                    return;
                }
            }
            if c.answered { 0 } else { 2 }
        };
        self.teardown(reason, None);
    }

    /// Strict teardown sequence:
    /// 1. Stop timers & media streams and transition UI to `"ended"` / close windows.
    /// 2. Emit `callState: "free"` **BEFORE** `bubble` so chat header `callRunning` resets first.
    /// 3. Emit `bubble` ONLY when `!incoming || answered` (preventing double-logged missed incoming calls).
    fn teardown(&self, reason: i32, outcome: Option<String>) {
        let Some(mut c) = self.active.lock().take() else {
            return;
        };
        if let Some(t) = c.ring_timer.take() {
            t.abort();
        }
        if let Some(m) = c.media.take() {
            m.stop();
        }

        if c.incoming && !c.answered {
            (self.cfg.emit_ui_event)(CallUiEvent::CloseIncoming);
        } else {
            if c.incoming {
                (self.cfg.emit_ui_event)(CallUiEvent::CloseIncoming);
            }
            (self.cfg.emit_ui_event)(CallUiEvent::SetState {
                state: "ended".to_string(),
                connected_at: c.connected_at_ms,
                outcome,
                is_video: c.is_video,
                camera_enabled: c.camera_enabled,
                screen_sharing: c.screen_sharing,
            });
            let ui_cb = Arc::clone(&self.cfg.emit_ui_event);
            let delay = self.cfg.close_ui_delay;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    (ui_cb)(CallUiEvent::CloseCall);
                });
            } else {
                (ui_cb)(CallUiEvent::CloseCall);
            }
        }

        // MUST emit `callState: "free"` strictly BEFORE `bubble`!
        self.emit_update(
            "callState",
            json!({ "state": "free", "callId": c.call_id }),
        );

        if !c.incoming || c.answered {
            let duration_sec = match (c.answered, c.connected_at_ms) {
                (true, Some(t0)) => current_epoch_ms().saturating_sub(t0) / 1000,
                _ => 0,
            };
            self.emit_update(
                "bubble",
                json!({
                    "partnerId": c.route_id,
                    "role": if c.incoming { 0 } else { 1 },
                    "duration": duration_sec,
                    "callId": c.call_id,
                    "missed": !c.answered,
                    "reason": if c.answered { 0 } else { reason },
                    "calltype": if c.is_video { 1 } else { 0 },
                }),
            );
        }
    }
}

/// Maps non-zero `control { act: "answer", data: { status } }` codes to `(bubble_reason, ui_outcome)`.
pub fn map_nonzero_answer_status(status: i64) -> (i32, Option<&'static str>) {
    match status {
        1 => (1, Some("busy")),
        3 => (3, None),
        5 | 6 => (2, None),
        _ => (2, None),
    }
}

/// Unpacks `call-send-to-native` IPC argument variations:
/// - `[json_string, optional_bool]`
/// - `[json_object]` or `[json_object, optional_bool]`
/// - `json_string`
/// - `json_object`
pub fn unpack_send_to_native_args(raw: &Value) -> Option<Value> {
    match raw {
        Value::Object(_) => Some(raw.clone()),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(|v| v.is_object()),
        Value::Array(items) => {
            let first = items.first()?;
            unpack_send_to_native_args(first)
        }
        _ => None,
    }
}

fn build_callee_402_payload(
    route_id: &str,
    call_id: i32,
    sess_id: &str,
    status: i32,
    media_fields: Option<(&str, &str)>,
) -> Value {
    let (codec, extend_data, rtp_addr) = match media_fields {
        Some((chosen, ext)) => (OPUS_CODEC_DESCRIPTOR, ext, chosen),
        None => ("", "", ""),
    };
    json!({
        "callerId": route_id,
        "callId": call_id,
        "codec": codec,
        "extendData": extend_data,
        "rtpAddress": rtp_addr,
        "rtcpAddress": rtp_addr,
        "rtpIPv6Addr": "",
        "rtcpIPv6Addr": "",
        "session": sess_id,
        "status": status,
        "version": 1,
    })
}

fn build_extend_data_json(
    chosen_relay: &SocketAddr,
    local_port: u16,
    sweep_results: &[RelaySweepEntry],
    is_video: bool,
    srtp_mode: SrtpMode,
) -> String {
    let local_ip = detect_lan_ipv4().unwrap_or_else(|| "127.0.0.1".to_string());
    let lan_ep = format!("{local_ip}:{local_port}");
    let chosen_str = chosen_relay.to_string();

    let server_result: Vec<Value> = if sweep_results.is_empty() {
        vec![json!({
            "rtcp": chosen_str,
            "rtp": chosen_str,
            "rtt": 20,
            "recv": 1,
        })]
    } else {
        sweep_results
            .iter()
            .map(|r| {
                let ep = r.addr.to_string();
                json!({
                    "rtcp": ep,
                    "rtp": ep,
                    "rtt": r.rtt_ms,
                    "recv": r.recv,
                })
            })
            .collect()
    };

    let video_codecs = if is_video {
        json!([
            { "name": "h264", "payload": 97 },
            { "name": "h264", "payload": 98 }
        ])
    } else {
        json!([{ "name": "h264", "payload": 97 }])
    };

    let payload = json!({
        "callType": if is_video { 1 } else { 0 },
        "gccAudio": 1,
        "gccMode": 1,
        "maxFT": 60,
        "newZrtc": 1,
        "p2p": {
            "local": [{ "ip": lan_ep, "ipType": 1 }],
            "public": [{ "ip": lan_ep, "ipType": 1 }],
        },
        "packetMode": 2,
        "platform": 2,
        "serverAddr": [{
            "ipType": 1,
            "rtcp": chosen_str,
            "rtp": chosen_str,
        }],
        "serverResult": server_result,
        "spTcp": 1,
        "srtcp": 0,
        "srtpMode": srtp_mode.to_wire_code(),
        "supportCallBusy": 1,
        "supportHevcDecode": 1,
        "tpType": 0,
        "video": { "codec": video_codecs },
    });
    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn extract_candidate_relays(cfg: &Value) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    if let Some(arr) = cfg.get("servers").and_then(Value::as_array) {
        for item in arr {
            let ep_opt = item
                .as_str()
                .or_else(|| item.get("rtpaddr").and_then(Value::as_str))
                .or_else(|| item.get("rtp").and_then(Value::as_str));
            if let Some(ep) = ep_opt {
                if let Some(sa) = parse_relay_endpoint(ep, DEFAULT_RELAY_PORT) {
                    if !out.contains(&sa) {
                        out.push(sa);
                    }
                }
            }
        }
    }
    out
}

fn detect_lan_ipv4() -> Option<String> {
    let sock = StdUdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
        _ => None,
    }
}

fn parse_json_if_str(v: Value) -> Value {
    if let Value::String(ref s) = v {
        serde_json::from_str::<Value>(s).unwrap_or(v)
    } else {
        v
    }
}

fn value_to_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn value_to_u32(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::Number(n) => n.as_u64().and_then(|u| u32::try_from(u).ok()),
        Value::String(s) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn value_to_i32(v: Option<&Value>) -> Option<i32> {
    match v? {
        Value::Number(n) => n.as_i64().and_then(|i| i32::try_from(i).ok()),
        Value::String(s) => s.trim().parse::<i32>().ok(),
        _ => None,
    }
}

fn value_to_i64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
