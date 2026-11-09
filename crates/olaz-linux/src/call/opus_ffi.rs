//! Dynamic `dlopen` FFI wrapper for system `libopus.so.0` (fallback `libopus.so`).
//! Configures `16,000 Hz` mono `OPUS_APPLICATION_VOIP (2048)` at `24,000 bps`
//! with safe RAII `OpusVoipEncoder` and `OpusVoipDecoder` (`320` `i16` samples = 20ms).

use libc::{c_char, c_int, c_void};
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::OnceLock;
use thiserror::Error;

/// Standard Zalo VoIP Opus sample rate (`16,000 Hz`).
pub const OPUS_VOIP_SAMPLE_RATE: i32 = 16_000;
/// Standard Zalo VoIP Opus channel count (`1` = mono).
pub const OPUS_VOIP_CHANNELS: i32 = 1;
/// Samples per 20ms frame at `16,000 Hz` mono (`320` `i16` samples).
pub const OPUS_VOIP_FRAME_SAMPLES: usize = 320;
/// `OPUS_OK` return code.
pub const OPUS_OK: c_int = 0;
/// `OPUS_APPLICATION_VOIP` (`2048`).
pub const OPUS_APPLICATION_VOIP: c_int = 2048;
/// `OPUS_SET_BITRATE_REQUEST` (`4002`).
pub const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
/// Default VoIP target bitrate (`24,000 bps`).
pub const OPUS_VOIP_DEFAULT_BITRATE: c_int = 24_000;

#[repr(C)]
struct OpusEncoderOpaque {
    _private: [u8; 0],
}

#[repr(C)]
struct OpusDecoderOpaque {
    _private: [u8; 0],
}

type OpusEncoderCreateFn =
    unsafe extern "C" fn(fs: i32, channels: c_int, application: c_int, error: *mut c_int) -> *mut OpusEncoderOpaque;
type OpusEncoderCtlFn =
    unsafe extern "C" fn(st: *mut OpusEncoderOpaque, request: c_int, ...) -> c_int;
type OpusEncodeFn = unsafe extern "C" fn(
    st: *mut OpusEncoderOpaque,
    pcm: *const i16,
    frame_size: c_int,
    data: *mut u8,
    max_data_bytes: i32,
) -> i32;
type OpusEncoderDestroyFn = unsafe extern "C" fn(st: *mut OpusEncoderOpaque);

type OpusDecoderCreateFn =
    unsafe extern "C" fn(fs: i32, channels: c_int, error: *mut c_int) -> *mut OpusDecoderOpaque;
type OpusDecodeFn = unsafe extern "C" fn(
    st: *mut OpusDecoderOpaque,
    data: *const u8,
    len: i32,
    pcm: *mut i16,
    frame_size: c_int,
    decode_fec: c_int,
) -> c_int;
type OpusDecoderDestroyFn = unsafe extern "C" fn(st: *mut OpusDecoderOpaque);
type OpusStrerrorFn = unsafe extern "C" fn(error: c_int) -> *const c_char;

struct OpusLib {
    _handle: *mut c_void,
    encoder_create: OpusEncoderCreateFn,
    encoder_ctl: OpusEncoderCtlFn,
    encode: OpusEncodeFn,
    encoder_destroy: OpusEncoderDestroyFn,
    decoder_create: OpusDecoderCreateFn,
    decode: OpusDecodeFn,
    decoder_destroy: OpusDecoderDestroyFn,
    strerror: OpusStrerrorFn,
}

// SAFETY: `OpusLib` holds immutable function pointers loaded once from `libopus.so.0`.
unsafe impl Send for OpusLib {}
unsafe impl Sync for OpusLib {}

static OPUS_LIB: OnceLock<Result<OpusLib, String>> = OnceLock::new();

/// Errors returned when loading `libopus` or encoding/decoding Opus VoIP frames.
#[derive(Debug, Error, Clone)]
pub enum OpusError {
    #[error("libopus unavailable: {0}")]
    LibraryUnavailable(String),
    #[error("Opus API `{func}` failed ({code}): {message}")]
    CodecError {
        func: &'static str,
        code: i32,
        message: String,
    },
    #[error("expected {expected} PCM samples for 20ms frame, got {actual}")]
    InvalidFrameSamples { expected: usize, actual: usize },
}

unsafe fn load_symbol<T: Copy>(handle: *mut c_void, name: &'static str) -> Result<T, String> {
    let c_name = CString::new(name).map_err(|e| e.to_string())?;
    // Clear any stale dlerror state before dlsym.
    let _ = libc::dlerror();
    let sym = libc::dlsym(handle, c_name.as_ptr());
    let err_ptr = libc::dlerror();
    if !err_ptr.is_null() || sym.is_null() {
        let detail = if !err_ptr.is_null() {
            CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
        } else {
            "null symbol".to_string()
        };
        return Err(format!("missing symbol `{name}`: {detail}"));
    }
    Ok(std::mem::transmute_copy(&sym))
}

fn get_opus_lib() -> Result<&'static OpusLib, OpusError> {
    OPUS_LIB
        .get_or_init(|| unsafe {
            let candidates = ["libopus.so.0", "libopus.so"];
            let mut handle: *mut c_void = ptr::null_mut();
            let mut last_err = String::new();
            for soname in candidates {
                let Ok(c_soname) = CString::new(soname) else {
                    continue;
                };
                let _ = libc::dlerror();
                let h = libc::dlopen(c_soname.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                if !h.is_null() {
                    handle = h;
                    break;
                }
                let e = libc::dlerror();
                if !e.is_null() {
                    last_err = CStr::from_ptr(e).to_string_lossy().into_owned();
                }
            }
            if handle.is_null() {
                return Err(format!(
                    "dlopen(libopus.so.0 / libopus.so) failed: {last_err}"
                ));
            }

            let load_res = (|| -> Result<OpusLib, String> {
                Ok(OpusLib {
                    _handle: handle,
                    encoder_create: load_symbol(handle, "opus_encoder_create")?,
                    encoder_ctl: load_symbol(handle, "opus_encoder_ctl")?,
                    encode: load_symbol(handle, "opus_encode")?,
                    encoder_destroy: load_symbol(handle, "opus_encoder_destroy")?,
                    decoder_create: load_symbol(handle, "opus_decoder_create")?,
                    decode: load_symbol(handle, "opus_decode")?,
                    decoder_destroy: load_symbol(handle, "opus_decoder_destroy")?,
                    strerror: load_symbol(handle, "opus_strerror")?,
                })
            })();

            if load_res.is_err() {
                let _ = libc::dlclose(handle);
            }
            load_res
        })
        .as_ref()
        .map_err(|e| OpusError::LibraryUnavailable(e.clone()))
}

fn opus_err_string(lib: &OpusLib, code: c_int) -> String {
    unsafe {
        let p = (lib.strerror)(code);
        if p.is_null() {
            format!("opus error {code}")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Returns `true` if `libopus.so.0` (or `libopus.so`) is dynamically loadable on this system.
pub fn is_opus_available() -> bool {
    get_opus_lib().is_ok()
}

/// Safe RAII wrapper around `OpusEncoder*` configured for Zalo VoIP (`16,000 Hz`, mono, `24 kbps`).
pub struct OpusVoipEncoder {
    lib: &'static OpusLib,
    ptr: *mut OpusEncoderOpaque,
    out_scratch: Vec<u8>,
}

// SAFETY: Each `OpusVoipEncoder` owns an independent `OpusEncoder*` state mutated only via `&mut self`.
unsafe impl Send for OpusVoipEncoder {}

impl OpusVoipEncoder {
    /// Creates a `16,000 Hz` mono `OPUS_APPLICATION_VOIP` encoder at `24,000 bps`.
    pub fn new() -> Result<Self, OpusError> {
        Self::with_bitrate(OPUS_VOIP_DEFAULT_BITRATE)
    }

    /// Creates a `16,000 Hz` mono `OPUS_APPLICATION_VOIP` encoder with custom target `bitrate_bps`.
    pub fn with_bitrate(bitrate_bps: i32) -> Result<Self, OpusError> {
        let lib = get_opus_lib()?;
        let mut err: c_int = 0;
        let ptr = unsafe {
            (lib.encoder_create)(
                OPUS_VOIP_SAMPLE_RATE,
                OPUS_VOIP_CHANNELS,
                OPUS_APPLICATION_VOIP,
                &mut err,
            )
        };
        if err != OPUS_OK || ptr.is_null() {
            return Err(OpusError::CodecError {
                func: "opus_encoder_create",
                code: err,
                message: opus_err_string(lib, err),
            });
        }
        let ctl_rc = unsafe { (lib.encoder_ctl)(ptr, OPUS_SET_BITRATE_REQUEST, bitrate_bps) };
        if ctl_rc != OPUS_OK {
            unsafe { (lib.encoder_destroy)(ptr) };
            return Err(OpusError::CodecError {
                func: "opus_encoder_ctl(OPUS_SET_BITRATE_REQUEST)",
                code: ctl_rc,
                message: opus_err_string(lib, ctl_rc),
            });
        }
        Ok(Self {
            lib,
            ptr,
            out_scratch: vec![0u8; 1500],
        })
    }

    /// Encodes a 20ms frame (`320` `i16` mono samples at 16 kHz) into an Opus packet.
    pub fn encode_20ms(&mut self, pcm_320: &[i16]) -> Result<Vec<u8>, OpusError> {
        if pcm_320.len() != OPUS_VOIP_FRAME_SAMPLES {
            return Err(OpusError::InvalidFrameSamples {
                expected: OPUS_VOIP_FRAME_SAMPLES,
                actual: pcm_320.len(),
            });
        }
        let n = unsafe {
            (self.lib.encode)(
                self.ptr,
                pcm_320.as_ptr(),
                OPUS_VOIP_FRAME_SAMPLES as c_int,
                self.out_scratch.as_mut_ptr(),
                self.out_scratch.len() as i32,
            )
        };
        if n < 0 {
            return Err(OpusError::CodecError {
                func: "opus_encode",
                code: n,
                message: opus_err_string(self.lib, n),
            });
        }
        Ok(self.out_scratch[..(n as usize)].to_vec())
    }
}

impl Drop for OpusVoipEncoder {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.lib.encoder_destroy)(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

/// Safe RAII wrapper around `OpusDecoder*` configured for Zalo VoIP (`16,000 Hz`, mono).
pub struct OpusVoipDecoder {
    lib: &'static OpusLib,
    ptr: *mut OpusDecoderOpaque,
}

// SAFETY: Each `OpusVoipDecoder` owns an independent `OpusDecoder*` state mutated only via `&mut self`.
unsafe impl Send for OpusVoipDecoder {}

impl OpusVoipDecoder {
    /// Creates a `16,000 Hz` mono Opus decoder.
    pub fn new() -> Result<Self, OpusError> {
        let lib = get_opus_lib()?;
        let mut err: c_int = 0;
        let ptr =
            unsafe { (lib.decoder_create)(OPUS_VOIP_SAMPLE_RATE, OPUS_VOIP_CHANNELS, &mut err) };
        if err != OPUS_OK || ptr.is_null() {
            return Err(OpusError::CodecError {
                func: "opus_decoder_create",
                code: err,
                message: opus_err_string(lib, err),
            });
        }
        Ok(Self { lib, ptr })
    }

    /// Decodes an Opus packet (or synthesizes Packet Loss Concealment when `packet` is `None`)
    /// into `320` `i16` mono PCM samples (20ms at 16 kHz).
    pub fn decode_20ms(
        &mut self,
        packet: Option<&[u8]>,
        fec: bool,
    ) -> Result<[i16; OPUS_VOIP_FRAME_SAMPLES], OpusError> {
        let mut pcm = [0i16; OPUS_VOIP_FRAME_SAMPLES];
        let (data_ptr, data_len) = match packet {
            Some(b) if !b.is_empty() => (b.as_ptr(), b.len() as i32),
            _ => (ptr::null(), 0),
        };
        let n = unsafe {
            (self.lib.decode)(
                self.ptr,
                data_ptr,
                data_len,
                pcm.as_mut_ptr(),
                OPUS_VOIP_FRAME_SAMPLES as c_int,
                if fec { 1 } else { 0 },
            )
        };
        if n < 0 {
            return Err(OpusError::CodecError {
                func: "opus_decode",
                code: n,
                message: opus_err_string(self.lib, n),
            });
        }
        let decoded = (n as usize).min(OPUS_VOIP_FRAME_SAMPLES);
        for sample in &mut pcm[decoded..] {
            *sample = 0;
        }
        Ok(pcm)
    }
}

impl Drop for OpusVoipDecoder {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.lib.decoder_destroy)(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}
