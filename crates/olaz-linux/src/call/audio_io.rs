//! Full-duplex Linux VoIP audio I/O (`16,000 Hz` mono `S16LE`, `20ms` = `320` samples).
//!
//! Primary backend uses `dlopen("libpulse-simple.so.0")` + `dlopen("libpulse.so.0")`
//! (`PA_STREAM_RECORD` with `fragsize = 640`, `PA_STREAM_PLAYBACK` with `tlength = 1920`),
//! automatically falling back to `cpal` (with multi-rate linear resampling and mono downmix/upmix)
//! when PulseAudio/PipeWire-Pulse is unavailable.

use crate::call::opus_ffi::{OpusVoipDecoder, OpusVoipEncoder, OPUS_VOIP_FRAME_SAMPLES};
use crate::call::voip_log::voip_log;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use libc::{c_char, c_int, c_void, size_t};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Native VoIP sample rate (`16,000 Hz`).
pub const AUDIO_SAMPLE_RATE_HZ: u32 = 16_000;
/// Samples per 20ms VoIP frame (`320`).
pub const AUDIO_FRAME_SAMPLES: usize = OPUS_VOIP_FRAME_SAMPLES;
/// Bytes per 20ms `S16LE` mono frame (`640` bytes).
pub const PULSE_RECORD_FRAGSIZE_BYTES: u32 = 640;
/// Playback target buffer (`1,920` bytes = `60ms` @ `16,000 Hz` mono `S16LE`).
pub const PULSE_PLAYBACK_TLENGTH_BYTES: u32 = 1_920;
/// Maximum speaker jitter-buffer depth (`6,400` samples = `400ms` @ `16,000 Hz` mono).
pub const MAX_PLAYBACK_JITTER_SAMPLES: usize = 6_400;
/// Default software microphone gain (`2.0x`), clamped to `i16`.
pub const DEFAULT_MIC_GAIN: f32 = 2.0;

const PA_SAMPLE_S16LE: c_int = 3;
const PA_STREAM_PLAYBACK: c_int = 1;
const PA_STREAM_RECORD: c_int = 2;

#[repr(C)]
struct PaSimpleOpaque {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PaSampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PaBufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

type PaSimpleNewFn = unsafe extern "C" fn(
    server: *const c_char,
    name: *const c_char,
    dir: c_int,
    dev: *const c_char,
    stream_name: *const c_char,
    ss: *const PaSampleSpec,
    map: *const c_void,
    attr: *const PaBufferAttr,
    error: *mut c_int,
) -> *mut PaSimpleOpaque;

type PaSimpleReadFn = unsafe extern "C" fn(
    s: *mut PaSimpleOpaque,
    data: *mut c_void,
    bytes: size_t,
    error: *mut c_int,
) -> c_int;

type PaSimpleWriteFn = unsafe extern "C" fn(
    s: *mut PaSimpleOpaque,
    data: *const c_void,
    bytes: size_t,
    error: *mut c_int,
) -> c_int;

type PaSimpleFreeFn = unsafe extern "C" fn(s: *mut PaSimpleOpaque);
type PaStrerrorFn = unsafe extern "C" fn(error: c_int) -> *const c_char;

struct PulseSimpleLib {
    _simple_handle: *mut c_void,
    _pulse_handle: *mut c_void,
    simple_new: PaSimpleNewFn,
    simple_read: PaSimpleReadFn,
    simple_write: PaSimpleWriteFn,
    simple_free: PaSimpleFreeFn,
    strerror: Option<PaStrerrorFn>,
}

// SAFETY: Immutable function pointers loaded once from `libpulse-simple.so.0` / `libpulse.so.0`.
unsafe impl Send for PulseSimpleLib {}
unsafe impl Sync for PulseSimpleLib {}

static PULSE_LIB: OnceLock<Result<PulseSimpleLib, String>> = OnceLock::new();

unsafe fn load_sym<T: Copy>(handle: *mut c_void, name: &'static str) -> Result<T, String> {
    let c_name = CString::new(name).map_err(|e| e.to_string())?;
    let _ = libc::dlerror();
    let sym = libc::dlsym(handle, c_name.as_ptr());
    let err = libc::dlerror();
    if !err.is_null() || sym.is_null() {
        let msg = if !err.is_null() {
            CStr::from_ptr(err).to_string_lossy().into_owned()
        } else {
            "null symbol".to_string()
        };
        return Err(format!("dlsym({name}) failed: {msg}"));
    }
    Ok(std::mem::transmute_copy(&sym))
}

fn get_pulse_lib() -> Result<&'static PulseSimpleLib, String> {
    PULSE_LIB
        .get_or_init(|| unsafe {
            let mut simple_handle = ptr::null_mut();
            let mut last_err = String::new();
            for soname in ["libpulse-simple.so.0", "libpulse-simple.so"] {
                if let Ok(cs) = CString::new(soname) {
                    let _ = libc::dlerror();
                    let h = libc::dlopen(cs.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                    if !h.is_null() {
                        simple_handle = h;
                        break;
                    }
                    let ep = libc::dlerror();
                    if !ep.is_null() {
                        last_err = CStr::from_ptr(ep).to_string_lossy().into_owned();
                    }
                }
            }
            if simple_handle.is_null() {
                return Err(format!("dlopen(libpulse-simple.so.0) failed: {last_err}"));
            }

            let mut pulse_handle = ptr::null_mut();
            for soname in ["libpulse.so.0", "libpulse.so"] {
                if let Ok(cs) = CString::new(soname) {
                    let h = libc::dlopen(cs.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                    if !h.is_null() {
                        pulse_handle = h;
                        break;
                    }
                }
            }

            let strerror = if !pulse_handle.is_null() {
                load_sym::<PaStrerrorFn>(pulse_handle, "pa_strerror").ok()
            } else {
                None
            };

            let res = (|| -> Result<PulseSimpleLib, String> {
                Ok(PulseSimpleLib {
                    _simple_handle: simple_handle,
                    _pulse_handle: pulse_handle,
                    simple_new: load_sym(simple_handle, "pa_simple_new")?,
                    simple_read: load_sym(simple_handle, "pa_simple_read")?,
                    simple_write: load_sym(simple_handle, "pa_simple_write")?,
                    simple_free: load_sym(simple_handle, "pa_simple_free")?,
                    strerror,
                })
            })();

            if res.is_err() {
                let _ = libc::dlclose(simple_handle);
                if !pulse_handle.is_null() {
                    let _ = libc::dlclose(pulse_handle);
                }
            }
            res
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Returns `true` if `libpulse-simple.so.0` was dynamically loaded via `dlopen`.
pub fn is_pulse_simple_available() -> bool {
    get_pulse_lib().is_ok()
}

fn pulse_err_str(lib: &PulseSimpleLib, err: c_int) -> String {
    if let Some(f) = lib.strerror {
        unsafe {
            let p = f(err);
            if !p.is_null() {
                return CStr::from_ptr(p).to_string_lossy().into_owned();
            }
        }
    }
    format!("PulseAudio error {err}")
}

/// Single capture or playback device entry exposed to the Call Devices UI (`devices.js`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceEntry {
    pub index: i32,
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Enumerated capture + playback devices plus current selection indices (`-1` = system default).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceList {
    pub capture: Vec<AudioDeviceEntry>,
    pub playback: Vec<AudioDeviceEntry>,
    pub selected_in: i32,
    pub selected_out: i32,
}

/// Returns `true` when an audio source/sink is a loopback `.monitor`, `Monitor of ...`,
/// dummy/null sink, or IR camera pseudo-device that should be hidden from VoIP selection.
pub fn is_ignored_audio_device(id: &str, display_name: &str) -> bool {
    let id_l = id.trim().to_ascii_lowercase();
    let name_l = display_name.trim().to_ascii_lowercase();
    if id_l.is_empty() && name_l.is_empty() {
        return true;
    }
    if id_l.ends_with(".monitor")
        || id_l.contains(".monitor")
        || name_l.starts_with("monitor of ")
        || name_l.contains("monitor of ")
        || id_l == "auto_null"
        || name_l == "dummy output"
        || name_l.contains("infrared")
        || name_l.contains("ir camera")
    {
        return true;
    }
    false
}

/// Applies software microphone gain (clamped to `i16::MIN..=i16::MAX`) in-place and returns
/// the resulting signal level in permille (`0..=1000`).
pub fn apply_mic_gain_and_level(samples: &mut [i16], gain: f32) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    let g = if gain.is_finite() && gain >= 0.0 {
        gain
    } else {
        1.0
    };
    let mut sum_sq: f64 = 0.0;
    for s in samples.iter_mut() {
        let scaled = (*s as f32 * g).round();
        let clamped = scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        *s = clamped;
        let norm = (clamped as f64) / 32768.0;
        sum_sq += norm * norm;
    }
    let rms = (sum_sq / (samples.len() as f64)).sqrt();
    // Map RMS (~0..0.5 speech peak) cleanly onto 0..=1000 permille.
    ((rms * 2200.0).round() as u32).min(1000)
}

/// Linearly resamples mono `i16` PCM from `src_rate` to `dst_rate`.
pub fn resample_mono_i16(input: &[i16], src_rate: u32, dst_rate: u32) -> Vec<i16> {
    if input.is_empty() || src_rate == 0 || dst_rate == 0 {
        return Vec::new();
    }
    if src_rate == dst_rate {
        return input.to_vec();
    }
    let out_len = (((input.len() as u64) * (dst_rate as u64)) / (src_rate as u64)).max(1) as usize;
    let mut out = Vec::with_capacity(out_len);
    let ratio = (src_rate as f64) / (dst_rate as f64);
    for i in 0..out_len {
        let src_pos = (i as f64) * ratio;
        let idx0 = (src_pos.floor() as usize).min(input.len() - 1);
        let idx1 = (idx0 + 1).min(input.len() - 1);
        let frac = src_pos - (idx0 as f64);
        let s0 = input[idx0] as f64;
        let s1 = input[idx1] as f64;
        let interp = (s0 + (s1 - s0) * frac)
            .round()
            .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        out.push(interp);
    }
    out
}

/// Downmixes interleaved multi-channel PCM into mono `i16`.
pub fn downmix_to_mono_i16(interleaved: &[i16], channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let frames = interleaved.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc: i32 = 0;
        for c in 0..channels {
            acc += interleaved[f * channels + c] as i32;
        }
        mono.push((acc / (channels as i32)) as i16);
    }
    mono
}

/// Bounded speaker playback jitter buffer enforcing the `400ms` (`6,400` samples @ 16 kHz) overrun cap.
#[derive(Debug)]
pub struct PlaybackJitterBuffer {
    samples: VecDeque<i16>,
    max_samples: usize,
}

impl Default for PlaybackJitterBuffer {
    fn default() -> Self {
        Self::new(MAX_PLAYBACK_JITTER_SAMPLES)
    }
}

impl PlaybackJitterBuffer {
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples: max_samples.max(AUDIO_FRAME_SAMPLES),
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Appends decoded `16 kHz` mono PCM samples, dropping oldest samples if queued latency
    /// would exceed `max_samples` (`6,400` samples = `400ms`).
    pub fn push_pcm(&mut self, pcm: &[i16]) {
        if pcm.len() >= self.max_samples {
            self.samples.clear();
            let tail = &pcm[pcm.len() - self.max_samples..];
            self.samples.extend(tail.iter().copied());
            return;
        }
        let projected = self.samples.len() + pcm.len();
        if projected > self.max_samples {
            let drop_n = projected - self.max_samples;
            let actual_drop = drop_n.min(self.samples.len());
            self.samples.drain(..actual_drop);
        }
        self.samples.extend(pcm.iter().copied());
    }

    /// Pops exactly `out.len()` samples into `out`, zero-filling on underrun.
    pub fn pop_into(&mut self, out: &mut [i16]) {
        for slot in out.iter_mut() {
            *slot = self.samples.pop_front().unwrap_or(0);
        }
    }
}

/// Enumerates filtered capture & playback audio devices via PulseAudio (`pactl`) with `cpal` fallback.
pub fn enumerate_audio_devices() -> AudioDeviceList {
    if let Some(pa_list) = enumerate_pactl_devices() {
        if !pa_list.capture.is_empty() || !pa_list.playback.is_empty() {
            return pa_list;
        }
    }
    enumerate_cpal_devices()
}

fn enumerate_pactl_devices() -> Option<AudioDeviceList> {
    let default_source = Command::new("pactl")
        .arg("get-default-source")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let default_sink = Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let parse_json_kind = |kind: &str, default_id: &str| -> Option<Vec<AudioDeviceEntry>> {
        let out = Command::new("pactl")
            .args(["--format=json", "list", kind])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let arr: Vec<Value> = serde_json::from_slice(&out.stdout).ok()?;
        let mut list = Vec::new();
        for item in arr {
            let id = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let desc = item
                .get("description")
                .and_then(Value::as_str)
                .or_else(|| {
                    item.get("properties")
                        .and_then(|p| p.get("device.description"))
                        .and_then(Value::as_str)
                })
                .unwrap_or(&id)
                .to_string();
            if is_ignored_audio_device(&id, &desc) {
                continue;
            }
            let idx = list.len() as i32;
            let is_default = !default_id.is_empty() && id == default_id;
            list.push(AudioDeviceEntry {
                index: idx,
                id,
                name: desc,
                is_default,
            });
        }
        Some(list)
    };

    let capture = parse_json_kind("sources", &default_source)?;
    let playback = parse_json_kind("sinks", &default_sink)?;
    Some(AudioDeviceList {
        capture,
        playback,
        selected_in: -1,
        selected_out: -1,
    })
}

fn enumerate_cpal_devices() -> AudioDeviceList {
    let host = cpal::default_host();
    let default_in = host.default_input_device().and_then(|d| d.name().ok());
    let default_out = host.default_output_device().and_then(|d| d.name().ok());

    let mut capture = Vec::new();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                if is_ignored_audio_device(&name, &name) {
                    continue;
                }
                let idx = capture.len() as i32;
                let is_default = default_in.as_deref() == Some(name.as_str());
                capture.push(AudioDeviceEntry {
                    index: idx,
                    id: name.clone(),
                    name,
                    is_default,
                });
            }
        }
    }

    let mut playback = Vec::new();
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                if is_ignored_audio_device(&name, &name) {
                    continue;
                }
                let idx = playback.len() as i32;
                let is_default = default_out.as_deref() == Some(name.as_str());
                playback.push(AudioDeviceEntry {
                    index: idx,
                    id: name.clone(),
                    name,
                    is_default,
                });
            }
        }
    }

    AudioDeviceList {
        capture,
        playback,
        selected_in: -1,
        selected_out: -1,
    }
}

/// Shared state controlled by `AudioIoSession` and read by capture/playback threads.
struct SharedAudioControl {
    running: AtomicBool,
    mic_muted: AtomicBool,
    speaker_muted: AtomicBool,
    mic_gain_bits: AtomicU32,
    mic_level_permille: AtomicU32,
    selected_in: AtomicI32,
    selected_out: AtomicI32,
    in_epoch: AtomicU64,
    out_epoch: AtomicU64,
    devices: Mutex<AudioDeviceList>,
    jitter: Mutex<PlaybackJitterBuffer>,
    decoder: Mutex<Option<OpusVoipDecoder>>,
}

/// Full-duplex VoIP audio engine managing Opus encoding/decoding, PulseAudio (`pa_simple`) streams,
/// automatic `cpal` fallback, live device switching, mute/speaker gating, and `400ms` jitter capping.
pub struct AudioIoSession {
    ctrl: Arc<SharedAudioControl>,
    cap_thread: Mutex<Option<JoinHandle<()>>>,
    play_thread: Mutex<Option<JoinHandle<()>>>,
}

impl Default for AudioIoSession {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioIoSession {
    /// Creates a new full-duplex audio controller (not yet streaming until `start` is called).
    pub fn new() -> Self {
        let devices = enumerate_audio_devices();
        let decoder = OpusVoipDecoder::new().ok();
        Self {
            ctrl: Arc::new(SharedAudioControl {
                running: AtomicBool::new(false),
                mic_muted: AtomicBool::new(false),
                speaker_muted: AtomicBool::new(false),
                mic_gain_bits: AtomicU32::new(DEFAULT_MIC_GAIN.to_bits()),
                mic_level_permille: AtomicU32::new(0),
                selected_in: AtomicI32::new(-1),
                selected_out: AtomicI32::new(-1),
                in_epoch: AtomicU64::new(0),
                out_epoch: AtomicU64::new(0),
                devices: Mutex::new(devices),
                jitter: Mutex::new(PlaybackJitterBuffer::new(MAX_PLAYBACK_JITTER_SAMPLES)),
                decoder: Mutex::new(decoder),
            }),
            cap_thread: Mutex::new(None),
            play_thread: Mutex::new(None),
        }
    }

    /// Enumerates current capture & playback devices (refreshing cached device IDs).
    pub fn list_devices(&self) -> AudioDeviceList {
        let mut fresh = enumerate_audio_devices();
        fresh.selected_in = self.ctrl.selected_in.load(Ordering::Relaxed);
        fresh.selected_out = self.ctrl.selected_out.load(Ordering::Relaxed);
        *self.ctrl.devices.lock() = fresh.clone();
        fresh
    }

    /// Switches microphone input device live (`-1` = system default).
    pub fn set_input_device(&self, idx: i32) {
        self.ctrl.selected_in.store(idx, Ordering::SeqCst);
        self.ctrl.devices.lock().selected_in = idx;
        self.ctrl.in_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Switches speaker/headphone output device live (`-1` = system default).
    pub fn set_output_device(&self, idx: i32) {
        self.ctrl.selected_out.store(idx, Ordering::SeqCst);
        self.ctrl.devices.lock().selected_out = idx;
        self.ctrl.out_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Mutes/unmutes microphone capture. When muted, mic frames are dropped before Opus encode.
    pub fn set_mic_muted(&self, muted: bool) {
        self.ctrl.mic_muted.store(muted, Ordering::SeqCst);
        if muted {
            self.ctrl.mic_level_permille.store(0, Ordering::Relaxed);
        }
    }

    /// Returns whether microphone capture is muted.
    pub fn is_mic_muted(&self) -> bool {
        self.ctrl.mic_muted.load(Ordering::Relaxed)
    }

    /// Mutes/unmutes speaker playback.
    pub fn set_speaker_muted(&self, muted: bool) {
        self.ctrl.speaker_muted.store(muted, Ordering::SeqCst);
        if muted {
            self.ctrl.jitter.lock().clear();
        }
    }

    /// Returns whether speaker playback is muted.
    pub fn is_speaker_muted(&self) -> bool {
        self.ctrl.speaker_muted.load(Ordering::Relaxed)
    }

    /// Sets software microphone gain (`default = 2.0`), clamped to `i16` on capture.
    pub fn set_mic_gain(&self, gain: f32) {
        self.ctrl.mic_gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Current software microphone gain.
    pub fn mic_gain(&self) -> f32 {
        f32::from_bits(self.ctrl.mic_gain_bits.load(Ordering::Relaxed))
    }

    /// Live microphone signal level in permille (`0..=1000`).
    pub fn mic_level_permille(&self) -> u32 {
        self.ctrl.mic_level_permille.load(Ordering::Relaxed)
    }

    /// Decodes an inbound 20ms Opus packet and queues it into the `400ms`-capped playback buffer
    /// (unless speaker output is muted).
    pub fn push_inbound_opus(&self, opus_packet: &[u8]) {
        if self.ctrl.speaker_muted.load(Ordering::Relaxed) {
            return;
        }
        let mut dec_guard = self.ctrl.decoder.lock();
        if dec_guard.is_none() {
            *dec_guard = OpusVoipDecoder::new().ok();
        }
        if let Some(ref mut dec) = *dec_guard {
            if let Ok(pcm) = dec.decode_20ms(Some(opus_packet), false) {
                self.ctrl.jitter.lock().push_pcm(&pcm);
            }
        }
    }

    /// Pushes pre-decoded `16,000 Hz` mono PCM directly into the `400ms`-capped playback buffer.
    pub fn push_inbound_pcm(&self, pcm_16k: &[i16]) {
        if self.ctrl.speaker_muted.load(Ordering::Relaxed) {
            return;
        }
        self.ctrl.jitter.lock().push_pcm(pcm_16k);
    }

    /// Current number of queued `16 kHz` samples in the playback jitter buffer.
    pub fn queued_playback_samples(&self) -> usize {
        self.ctrl.jitter.lock().len()
    }

    /// Starts full-duplex capture + playback workers, invoking `on_encoded_opus` for every
    /// unmuted 20ms Opus frame (`320` samples @ `16 kHz`).
    pub fn start<F>(&self, on_encoded_opus: F)
    where
        F: Fn(Vec<u8>) + Send + Sync + 'static,
    {
        if self.ctrl.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let cb = Arc::new(on_encoded_opus);

        let ctrl_cap = Arc::clone(&self.ctrl);
        let cb_cap = Arc::clone(&cb);
        let cap_handle = thread::Builder::new()
            .name("olaz-voip-capture".to_string())
            .spawn(move || run_capture_loop(ctrl_cap, cb_cap))
            .ok();
        *self.cap_thread.lock() = cap_handle;

        let ctrl_play = Arc::clone(&self.ctrl);
        let play_handle = thread::Builder::new()
            .name("olaz-voip-playback".to_string())
            .spawn(move || run_playback_loop(ctrl_play))
            .ok();
        *self.play_thread.lock() = play_handle;
    }

    /// Stops capture & playback threads cleanly.
    pub fn stop(&self) {
        if !self.ctrl.running.swap(false, Ordering::SeqCst) {
            return;
        }
        self.ctrl.mic_level_permille.store(0, Ordering::Relaxed);
        if let Some(h) = self.cap_thread.lock().take() {
            let _ = h.join();
        }
        if let Some(h) = self.play_thread.lock().take() {
            let _ = h.join();
        }
        self.ctrl.jitter.lock().clear();
    }
}

impl Drop for AudioIoSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn selected_device_id(ctrl: &SharedAudioControl, is_capture: bool) -> Option<String> {
    let idx = if is_capture {
        ctrl.selected_in.load(Ordering::Relaxed)
    } else {
        ctrl.selected_out.load(Ordering::Relaxed)
    };
    if idx < 0 {
        return None;
    }
    let guard = ctrl.devices.lock();
    let list = if is_capture {
        &guard.capture
    } else {
        &guard.playback
    };
    list.iter()
        .find(|d| d.index == idx)
        .map(|d| d.id.clone())
}

fn process_captured_320(
    ctrl: &SharedAudioControl,
    enc: &mut OpusVoipEncoder,
    frame_320: &mut [i16; AUDIO_FRAME_SAMPLES],
    cb: &Arc<dyn Fn(Vec<u8>) + Send + Sync>,
) {
    if ctrl.mic_muted.load(Ordering::Relaxed) {
        // Muted: drop mic PCM before encode (matches ZaloCall / zaudio.cpp).
        ctrl.mic_level_permille.store(0, Ordering::Relaxed);
        return;
    }
    let gain = f32::from_bits(ctrl.mic_gain_bits.load(Ordering::Relaxed));
    let level = apply_mic_gain_and_level(frame_320, gain);
    ctrl.mic_level_permille.store(level, Ordering::Relaxed);
    if let Ok(pkt) = enc.encode_20ms(frame_320) {
        cb(pkt);
    }
}

fn run_capture_loop(ctrl: Arc<SharedAudioControl>, cb: Arc<dyn Fn(Vec<u8>) + Send + Sync>) {
    let Ok(mut enc) = OpusVoipEncoder::new() else {
        voip_log("audio", "capture disabled: OpusVoipEncoder unavailable");
        return;
    };

    while ctrl.running.load(Ordering::Relaxed) {
        let epoch = ctrl.in_epoch.load(Ordering::SeqCst);
        let dev_id = selected_device_id(&ctrl, true);

        if let Ok(lib) = get_pulse_lib() {
            if run_pulse_capture_once(lib, &ctrl, &mut enc, dev_id.as_deref(), epoch, &cb) {
                continue;
            }
        }

        // Automatic fallback to `cpal` when PulseAudio is unavailable or stream open failed.
        run_cpal_capture_once(&ctrl, &mut enc, dev_id.as_deref(), epoch, &cb);
        if ctrl.running.load(Ordering::Relaxed) && ctrl.in_epoch.load(Ordering::SeqCst) == epoch {
            thread::sleep(Duration::from_millis(150));
        }
    }
}

fn run_pulse_capture_once(
    lib: &'static PulseSimpleLib,
    ctrl: &Arc<SharedAudioControl>,
    enc: &mut OpusVoipEncoder,
    dev_id: Option<&str>,
    epoch: u64,
    cb: &Arc<dyn Fn(Vec<u8>) + Send + Sync>,
) -> bool {
    let app_name = CString::new("Olaz").unwrap();
    let stream_name = CString::new("VoIP Microphone").unwrap();
    let dev_c = dev_id.and_then(|s| CString::new(s).ok());
    let ss = PaSampleSpec {
        format: PA_SAMPLE_S16LE,
        rate: AUDIO_SAMPLE_RATE_HZ,
        channels: 1,
    };
    let attr = PaBufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: PULSE_RECORD_FRAGSIZE_BYTES,
    };
    let mut err: c_int = 0;
    let handle = unsafe {
        (lib.simple_new)(
            ptr::null(),
            app_name.as_ptr(),
            PA_STREAM_RECORD,
            dev_c.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            stream_name.as_ptr(),
            &ss,
            ptr::null(),
            &attr,
            &mut err,
        )
    };
    if handle.is_null() {
        voip_log(
            "audio",
            &format!(
                "pa_simple_new(RECORD) failed ({}): falling back to cpal",
                pulse_err_str(lib, err)
            ),
        );
        return false;
    }

    voip_log(
        "audio",
        &format!(
            "PulseAudio capture opened (16kHz mono S16LE, fragsize=640, dev={})",
            dev_id.unwrap_or("default")
        ),
    );

    let mut pcm_buf = [0i16; AUDIO_FRAME_SAMPLES];
    while ctrl.running.load(Ordering::Relaxed) && ctrl.in_epoch.load(Ordering::SeqCst) == epoch {
        let mut rerr: c_int = 0;
        let rc = unsafe {
            (lib.simple_read)(
                handle,
                pcm_buf.as_mut_ptr() as *mut c_void,
                PULSE_RECORD_FRAGSIZE_BYTES as size_t,
                &mut rerr,
            )
        };
        if rc < 0 {
            voip_log(
                "audio",
                &format!("pa_simple_read error: {}", pulse_err_str(lib, rerr)),
            );
            break;
        }
        process_captured_320(ctrl, enc, &mut pcm_buf, cb);
    }

    unsafe { (lib.simple_free)(handle) };
    ctrl.in_epoch.load(Ordering::SeqCst) != epoch
}

fn run_cpal_capture_once(
    ctrl: &Arc<SharedAudioControl>,
    enc: &mut OpusVoipEncoder,
    dev_id: Option<&str>,
    epoch: u64,
    cb: &Arc<dyn Fn(Vec<u8>) + Send + Sync>,
) {
    let host = cpal::default_host();
    let device = dev_id
        .and_then(|want| {
            host.input_devices()
                .ok()?
                .find(|d| d.name().ok().as_deref() == Some(want))
        })
        .or_else(|| host.default_input_device());
    let Some(device) = device else {
        return;
    };
    let Ok(supported) = device.default_input_config() else {
        return;
    };
    let sample_format = supported.sample_format();
    let cfg: cpal::StreamConfig = supported.into();
    let in_rate = cfg.sample_rate.0;
    let in_channels = (cfg.channels as usize).max(1);

    let pcm_queue: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::with_capacity(3200)));
    let q_cb = Arc::clone(&pcm_queue);
    let ctrl_muted = Arc::clone(ctrl);

    let push_interleaved = move |mono_native: Vec<i16>| {
        if ctrl_muted.mic_muted.load(Ordering::Relaxed) {
            q_cb.lock().clear();
            return;
        }
        let resampled = resample_mono_i16(&mono_native, in_rate, AUDIO_SAMPLE_RATE_HZ);
        let mut g = q_cb.lock();
        if g.len() > MAX_PLAYBACK_JITTER_SAMPLES {
            g.clear();
        }
        g.extend(resampled);
    };

    let err_fn = |err| {
        voip_log("audio", &format!("cpal capture stream error: {err}"));
    };

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_input_stream(
            &cfg,
            move |data: &[i16], _| {
                push_interleaved(downmix_to_mono_i16(data, in_channels));
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &cfg,
            move |data: &[u16], _| {
                let signed: Vec<i16> = data
                    .iter()
                    .map(|&u| (u as i32 - 32768).clamp(-32768, 32767) as i16)
                    .collect();
                push_interleaved(downmix_to_mono_i16(&signed, in_channels));
            },
            err_fn,
            None,
        ),
        _ => device.build_input_stream(
            &cfg,
            move |data: &[f32], _| {
                let signed: Vec<i16> = data
                    .iter()
                    .map(|&f| (f * 32767.0).round().clamp(-32768.0, 32767.0) as i16)
                    .collect();
                push_interleaved(downmix_to_mono_i16(&signed, in_channels));
            },
            err_fn,
            None,
        ),
    };

    let Ok(stream) = stream else {
        return;
    };
    if stream.play().is_err() {
        return;
    }

    voip_log(
        "audio",
        &format!("cpal capture fallback active ({in_rate}Hz ch={in_channels} -> 16kHz mono)"),
    );

    let mut frame = [0i16; AUDIO_FRAME_SAMPLES];
    while ctrl.running.load(Ordering::Relaxed) && ctrl.in_epoch.load(Ordering::SeqCst) == epoch {
        if ctrl.mic_muted.load(Ordering::Relaxed) {
            pcm_queue.lock().clear();
            ctrl.mic_level_permille.store(0, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        let has_frame = {
            let mut q = pcm_queue.lock();
            if q.len() >= AUDIO_FRAME_SAMPLES {
                for slot in frame.iter_mut() {
                    *slot = q.pop_front().unwrap_or(0);
                }
                true
            } else {
                false
            }
        };
        if has_frame {
            process_captured_320(ctrl, enc, &mut frame, cb);
        } else {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn run_playback_loop(ctrl: Arc<SharedAudioControl>) {
    while ctrl.running.load(Ordering::Relaxed) {
        let epoch = ctrl.out_epoch.load(Ordering::SeqCst);
        let dev_id = selected_device_id(&ctrl, false);

        if let Ok(lib) = get_pulse_lib() {
            if run_pulse_playback_once(lib, &ctrl, dev_id.as_deref(), epoch) {
                continue;
            }
        }

        run_cpal_playback_once(&ctrl, dev_id.as_deref(), epoch);
        if ctrl.running.load(Ordering::Relaxed) && ctrl.out_epoch.load(Ordering::SeqCst) == epoch {
            thread::sleep(Duration::from_millis(150));
        }
    }
}

fn run_pulse_playback_once(
    lib: &'static PulseSimpleLib,
    ctrl: &Arc<SharedAudioControl>,
    dev_id: Option<&str>,
    epoch: u64,
) -> bool {
    let app_name = CString::new("Olaz").unwrap();
    let stream_name = CString::new("VoIP Speaker").unwrap();
    let dev_c = dev_id.and_then(|s| CString::new(s).ok());
    let ss = PaSampleSpec {
        format: PA_SAMPLE_S16LE,
        rate: AUDIO_SAMPLE_RATE_HZ,
        channels: 1,
    };
    let attr = PaBufferAttr {
        maxlength: u32::MAX,
        tlength: PULSE_PLAYBACK_TLENGTH_BYTES,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: u32::MAX,
    };
    let mut err: c_int = 0;
    let handle = unsafe {
        (lib.simple_new)(
            ptr::null(),
            app_name.as_ptr(),
            PA_STREAM_PLAYBACK,
            dev_c.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            stream_name.as_ptr(),
            &ss,
            ptr::null(),
            &attr,
            &mut err,
        )
    };
    if handle.is_null() {
        voip_log(
            "audio",
            &format!(
                "pa_simple_new(PLAYBACK) failed ({}): falling back to cpal",
                pulse_err_str(lib, err)
            ),
        );
        return false;
    }

    voip_log(
        "audio",
        &format!(
            "PulseAudio playback opened (16kHz mono S16LE, tlength=1920, dev={})",
            dev_id.unwrap_or("default")
        ),
    );

    let mut pcm_buf = [0i16; AUDIO_FRAME_SAMPLES];
    while ctrl.running.load(Ordering::Relaxed) && ctrl.out_epoch.load(Ordering::SeqCst) == epoch {
        if ctrl.speaker_muted.load(Ordering::Relaxed) {
            pcm_buf.fill(0);
        } else {
            ctrl.jitter.lock().pop_into(&mut pcm_buf);
        }
        let mut werr: c_int = 0;
        let rc = unsafe {
            (lib.simple_write)(
                handle,
                pcm_buf.as_ptr() as *const c_void,
                PULSE_RECORD_FRAGSIZE_BYTES as size_t,
                &mut werr,
            )
        };
        if rc < 0 {
            voip_log(
                "audio",
                &format!("pa_simple_write error: {}", pulse_err_str(lib, werr)),
            );
            break;
        }
    }

    unsafe { (lib.simple_free)(handle) };
    ctrl.out_epoch.load(Ordering::SeqCst) != epoch
}

fn run_cpal_playback_once(ctrl: &Arc<SharedAudioControl>, dev_id: Option<&str>, epoch: u64) {
    let host = cpal::default_host();
    let device = dev_id
        .and_then(|want| {
            host.output_devices()
                .ok()?
                .find(|d| d.name().ok().as_deref() == Some(want))
        })
        .or_else(|| host.default_output_device());
    let Some(device) = device else {
        return;
    };
    let Ok(supported) = device.default_output_config() else {
        return;
    };
    let sample_format = supported.sample_format();
    let cfg: cpal::StreamConfig = supported.into();
    let out_rate = cfg.sample_rate.0;
    let out_channels = (cfg.channels as usize).max(1);

    let resampled_out: Arc<Mutex<VecDeque<i16>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(4096)));
    let q_out = Arc::clone(&resampled_out);
    let ctrl_cb = Arc::clone(ctrl);

    let pull_mono = move |frames: usize| -> Vec<i16> {
        if ctrl_cb.speaker_muted.load(Ordering::Relaxed) {
            q_out.lock().clear();
            return vec![0i16; frames];
        }
        let mut q = q_out.lock();
        while q.len() < frames {
            let mut src_chunk = [0i16; AUDIO_FRAME_SAMPLES];
            ctrl_cb.jitter.lock().pop_into(&mut src_chunk);
            let up = resample_mono_i16(&src_chunk, AUDIO_SAMPLE_RATE_HZ, out_rate);
            if up.is_empty() {
                break;
            }
            q.extend(up);
        }
        let mut out = Vec::with_capacity(frames);
        for _ in 0..frames {
            out.push(q.pop_front().unwrap_or(0));
        }
        out
    };

    let err_fn = |err| {
        voip_log("audio", &format!("cpal playback stream error: {err}"));
    };

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_output_stream(
            &cfg,
            move |out: &mut [i16], _| {
                let frames = out.len() / out_channels;
                let mono = pull_mono(frames);
                for (f, &s) in mono.iter().enumerate() {
                    for c in 0..out_channels {
                        out[f * out_channels + c] = s;
                    }
                }
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            &cfg,
            move |out: &mut [u16], _| {
                let frames = out.len() / out_channels;
                let mono = pull_mono(frames);
                for (f, &s) in mono.iter().enumerate() {
                    let u = (s as i32 + 32768).clamp(0, 65535) as u16;
                    for c in 0..out_channels {
                        out[f * out_channels + c] = u;
                    }
                }
            },
            err_fn,
            None,
        ),
        _ => device.build_output_stream(
            &cfg,
            move |out: &mut [f32], _| {
                let frames = out.len() / out_channels;
                let mono = pull_mono(frames);
                for (f, &s) in mono.iter().enumerate() {
                    let v = (s as f32) / 32768.0;
                    for c in 0..out_channels {
                        out[f * out_channels + c] = v;
                    }
                }
            },
            err_fn,
            None,
        ),
    };

    let Ok(stream) = stream else {
        return;
    };
    if stream.play().is_err() {
        return;
    }

    voip_log(
        "audio",
        &format!("cpal playback fallback active (16kHz -> {out_rate}Hz ch={out_channels})"),
    );

    while ctrl.running.load(Ordering::Relaxed) && ctrl.out_epoch.load(Ordering::SeqCst) == epoch {
        thread::sleep(Duration::from_millis(25));
    }
}
