//! Self-contained embedded HTML/CSS/JS Call UI assets adapted from `zalo-linux/tools/zcall-ui` (MIT).
//!
//! Zero proprietary binary PNGs, fonts, or MP3s are stored in git:
//! - All call controls use crisp inline SVG icons (`icons.css`).
//! - Telephony call tones (`connecting`, `ringback`, `ringtone`, `busy`, `endcall`, `disconnect`)
//!   are synthesized cleanly with the WebAudio API (`sounds.js`), with optional `zalo-app://` MP3
//!   loading when available.
//! - `call.html` supports both 1-1 / group audio calls and full video / screen-share rendering
//!   via `<canvas id="remote-video">` and `<canvas id="local-video">` plus live mic level metering.

/// Custom URI scheme prefix served to the Call UI WebViews.
pub const CALL_UI_SCHEME: &str = "zcall-ui";
pub const CALL_UI_CALL_URL: &str = "zcall-ui://app/call.html";
pub const CALL_UI_INCOMING_URL: &str = "zcall-ui://app/incoming.html";
pub const CALL_UI_DEVICES_URL: &str = "zcall-ui://app/devices.html";

/// Preload bridge script injected at `UserScriptInjectionTime::Start` into every Call UI WebView.
pub const PRELOAD_BRIDGE_JS: &str = r#"(function () {
  if (window.zcallUI) return;
  var listeners = { partner: [], state: [], devices: [], micLevel: [], videoFrame: [], recording: [] };
  var latest = { partner: null, state: null, devices: null, micLevel: 0, recording: null };

  function postToRust(action, value) {
    try {
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.zcallUi) {
        window.webkit.messageHandlers.zcallUi.postMessage(JSON.stringify({
          action: String(action),
          value: value === undefined ? null : value
        }));
      }
    } catch (_e) {}
  }
  var post = postToRust;

  window.zcallUI = {
    onPartner: function (cb) {
      listeners.partner.push(cb);
      if (latest.partner) { try { cb(latest.partner); } catch (_e) {} }
    },
    onState: function (cb) {
      listeners.state.push(cb);
      if (latest.state) { try { cb(latest.state); } catch (_e) {} }
    },
    onDevices: function (cb) {
      listeners.devices.push(cb);
      if (latest.devices) { try { cb(latest.devices); } catch (_e) {} }
    },
    onMicLevel: function (cb) {
      listeners.micLevel.push(cb);
      try { cb(latest.micLevel); } catch (_e) {}
    },
    onVideoFrame: function (cb) {
      listeners.videoFrame.push(cb);
    },
    onRecording: function (cb) {
      listeners.recording.push(cb);
      if (latest.recording) { try { cb(latest.recording); } catch (_e) {} }
    },
    toggleRecord: (opts) => post("toggleRecord", opts),
    openRecordingFolder: (path) => post("openRecordingFolder", path),
    action: function (action, value) {
      postToRust(action, value);
    },
    __dispatch: function (channel, payload) {
      if (channel !== 'videoFrame') latest[channel] = payload;
      var list = listeners[channel] || [];
      for (var i = 0; i < list.length; i++) {
        try { list[i](payload); } catch (_e) {}
      }
    }
  };

  document.addEventListener('mousedown', function (ev) {
    if (ev.button !== 0) return;
    var t = ev.target;
    if (!t || !t.closest) return;
    if (t.closest('button, select, input, option, .no-drag, .wc, .dock-btn, .rbtn, #actions, #dock, .controls-bar, #rec-toast')) return;
    if (t.closest('#titlebar, .drag-region, #wrap')) {
      postToRust('windowDrag', {
        button: 1,
        screenX: Math.round(ev.screenX || 0),
        screenY: Math.round(ev.screenY || 0)
      });
    }
  }, true);
})();"#;

pub const ICONS_CSS: &str = r#"/* Clean inline SVG icons — zero binary PNG/TTF dependencies */
.nic, .zic {
  display: inline-block;
  width: 24px;
  height: 24px;
  background-size: contain;
  background-repeat: no-repeat;
  background-position: center;
  vertical-align: middle;
}
.nic { width: 26px; height: 26px; }
.nic-mic, .zic-mic {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ffffff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='9' y='2' width='6' height='11' rx='3'/><path d='M5 10a7 7 0 0 0 14 0'/><line x1='12' y1='17' x2='12' y2='22'/><line x1='8' y1='22' x2='16' y2='22'/></svg>");
}
.statbox .zic-mic {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%231a1a1a' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='9' y='2' width='6' height='11' rx='3'/><path d='M5 10a7 7 0 0 0 14 0'/><line x1='12' y1='17' x2='12' y2='22'/><line x1='8' y1='22' x2='16' y2='22'/></svg>");
}
.nic-mic-off, .zic-mic-off {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ff5a5f' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><line x1='2' y1='2' x2='22' y2='22'/><path d='M18.89 13.23A7.12 7.12 0 0 0 19 12v-2'/><path d='M5 10v2a7 7 0 0 0 12 5'/><path d='M15 9.34V5a3 3 0 0 0-5.68-1.33'/><path d='M9 9v3a3 3 0 0 0 5.12 2.12'/><line x1='12' y1='19' x2='12' y2='22'/></svg>");
}
.nic-speaker, .zic-speaker {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ffffff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><polygon points='11 5 6 9 2 9 2 15 6 15 11 19 11 5'/><path d='M15.54 8.46a5 5 0 0 1 0 7.07'/><path d='M19.07 4.93a10 10 0 0 1 0 14.14'/></svg>");
}
.statbox .zic-speaker {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%231a1a1a' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><polygon points='11 5 6 9 2 9 2 15 6 15 11 19 11 5'/><path d='M15.54 8.46a5 5 0 0 1 0 7.07'/></svg>");
}
.nic-speaker-off {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ff5a5f' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><polygon points='11 5 6 9 2 9 2 15 6 15 11 19 11 5'/><line x1='23' y1='9' x2='17' y2='15'/><line x1='17' y1='9' x2='23' y2='15'/></svg>");
}
.nic-cam {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ffffff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><polygon points='23 7 16 12 23 17 23 7'/><rect x='1' y='5' width='15' height='14' rx='2' ry='2'/></svg>");
}
.nic-cam-off {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%239aa0a6' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M16 16v1a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h2m5.66 0H14a2 2 0 0 1 2 2v3.34l1 1L23 7v10'/><line x1='1' y1='1' x2='23' y2='23'/></svg>");
}
.nic-screen {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ffffff' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='2' y='3' width='20' height='14' rx='2' ry='2'/><line x1='8' y1='21' x2='16' y2='21'/><line x1='12' y1='17' x2='12' y2='21'/><polyline points='9 11 12 8 15 11'/><line x1='12' y1='8' x2='12' y2='14'/></svg>");
}
.nic-more {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%23ffffff'><circle cx='5' cy='12' r='2'/><circle cx='12' cy='12' r='2'/><circle cx='19' cy='12' r='2'/></svg>");
}
.nic-endcall {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ffffff' stroke-width='2.2' stroke-linecap='round' stroke-linejoin='round'><path d='M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7 2 2 0 0 1 1.72 2v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67m-2.67-3.34a19.79 19.79 0 0 1-3.07-8.63A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91' transform='rotate(135 12 12)'/></svg>");
}
.nic-accept {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23ffffff' stroke-width='2.2' stroke-linecap='round' stroke-linejoin='round'><path d='M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92z'/></svg>");
}
.nic-record {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none'><circle cx='12' cy='12' r='9' stroke='%23ffffff' stroke-width='2'/><circle cx='12' cy='12' r='4.5' fill='%23ef4444'/></svg>");
}
.nic-record-stop {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none'><circle cx='12' cy='12' r='9' stroke='%23fca5a5' stroke-width='2'/><rect x='8.5' y='8.5' width='7' height='7' rx='1.4' fill='%23ffffff'/></svg>");
}
.nic-folder {
  background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%2334d399' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z'/></svg>");
}
"#;

pub const CALL_FORMAT_JS: &str = r#"(function (root, factory) {
  if (typeof module !== 'undefined' && module.exports) module.exports = factory();
  else root.zcallFormat = factory();
})(typeof self !== 'undefined' ? self : this, function () {
  function pad2(n) { return n < 10 ? '0' + n : '' + n; }
  function formatDuration(sec) {
    sec = Math.max(0, Math.floor(Number(sec) || 0));
    var h = Math.floor(sec / 3600);
    var m = Math.floor((sec % 3600) / 60);
    var s = sec % 60;
    return h > 0 ? (pad2(h) + ':' + pad2(m) + ':' + pad2(s)) : (pad2(m) + ':' + pad2(s));
  }
  function formatStatus(state, elapsedSec) {
    if (state === 'calling') return 'Đang kết nối...';
    if (state === 'ringing') return 'Đang đổ chuông...';
    if (state === 'connecting') return 'Đang kết nối âm thanh...';
    if (state === 'connected') return formatDuration(elapsedSec || 0);
    if (state === 'ended') return 'Cuộc gọi đã kết thúc';
    return '';
  }
  return { formatDuration: formatDuration, formatStatus: formatStatus };
});"#;

pub const SOUNDS_JS: &str = r#"// Synthesized WebAudio call tones (zero binary MP3s required; optional zalo-app:// MP3 override).
(function (root, factory) {
  if (typeof module !== 'undefined' && module.exports) module.exports = { createSounds: factory() };
  else root.createSounds = factory();
})(typeof self !== 'undefined' ? self : this, function () {
  return function createSounds(opts) {
    opts = opts || {};
    var ctx = null;
    var loopTimer = null;
    var activeNodes = [];

    function ensureCtx() {
      if (!ctx) {
        var AC = window.AudioContext || window.webkitAudioContext;
        if (AC) { try { ctx = new AC(); } catch (_e) {} }
      }
      if (ctx && ctx.state === 'suspended') { try { ctx.resume(); } catch (_e) {} }
      return ctx;
    }

    function stopNodes() {
      if (loopTimer) { clearInterval(loopTimer); loopTimer = null; }
      for (var i = 0; i < activeNodes.length; i++) {
        try { activeNodes[i].stop(); } catch (_e) {}
        try { activeNodes[i].disconnect(); } catch (_e) {}
      }
      activeNodes = [];
    }

    function playToneBurst(freqs, startOffset, durationSec, peakGain) {
      var c = ensureCtx();
      if (!c) return;
      var t0 = c.currentTime + (startOffset || 0);
      var g = c.createGain();
      g.gain.setValueAtTime(0.0001, t0);
      g.gain.linearRampToValueAtTime(peakGain || 0.08, t0 + 0.02);
      g.gain.exponentialRampToValueAtTime(0.0001, t0 + durationSec);
      g.connect(c.destination);
      for (var i = 0; i < freqs.length; i++) {
        var osc = c.createOscillator();
        osc.type = 'sine';
        osc.frequency.setValueAtTime(freqs[i], t0);
        osc.connect(g);
        osc.start(t0);
        osc.stop(t0 + durationSec + 0.02);
        activeNodes.push(osc);
      }
    }

    function startLoop(patternFn, intervalMs) {
      stopNodes();
      patternFn();
      loopTimer = setInterval(patternFn, intervalMs);
    }

    return {
      apply: function (state, outcome) {
        if (state === 'calling' || state === 'connecting') {
          startLoop(function () {
            playToneBurst([523.25], 0.0, 0.14, 0.06);
            playToneBurst([659.25], 0.16, 0.18, 0.06);
          }, 1800);
        } else if (state === 'ringing') {
          startLoop(function () {
            playToneBurst([440, 480], 0.0, 0.95, 0.07);
          }, 3200);
        } else if (state === 'ringing-incoming') {
          startLoop(function () {
            playToneBurst([523.25], 0.00, 0.18, 0.09);
            playToneBurst([659.25], 0.18, 0.18, 0.09);
            playToneBurst([783.99], 0.36, 0.18, 0.09);
            playToneBurst([1046.5], 0.54, 0.32, 0.10);
          }, 2000);
        } else if (state === 'connected') {
          stopNodes();
        } else if (state === 'ended') {
          stopNodes();
          if (outcome === 'busy') {
            playToneBurst([480, 620], 0.00, 0.28, 0.08);
            playToneBurst([480, 620], 0.45, 0.28, 0.08);
            playToneBurst([480, 620], 0.90, 0.28, 0.08);
          } else {
            playToneBurst([480], 0.00, 0.16, 0.08);
            playToneBurst([330], 0.18, 0.24, 0.08);
          }
        } else {
          stopNodes();
        }
      },
      stopAll: function () { stopNodes(); }
    };
  };
});"#;

pub const CALL_HTML: &str = r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <link rel="stylesheet" href="icons.css" />
  <link rel="stylesheet" href="call.css" />
  <title>Zalo Call</title>
</head>
<body>
  <div id="titlebar">
    <span class="tb-title" id="tb-title">Zalo Call</span>
    <span class="tb-spacer"></span>
    <div class="wc" id="wc-min" title="Thu nhỏ"><i class="wc-min-i"></i></div>
    <div class="wc" id="wc-max" title="Phóng to"><i class="wc-max-i"></i></div>
    <div class="wc wc-close" id="wc-close" title="Đóng"><i class="wc-x"></i></div>
  </div>

  <div id="stage" class="drag-region">
    <canvas id="remote-video" width="320" height="240"></canvas>
    <canvas id="local-video" width="240" height="180"></canvas>

    <div id="center" class="header-info">
      <div class="avatar-ring">
        <div class="avatar" id="avatar"><span id="avatar-initial">Z</span></div>
      </div>
      <div class="partner-name" id="partner-name">—</div>
      <div class="call-status" id="status">Đang kết nối...</div>
      <div class="rec-indicator" id="rec-indicator" hidden><span class="rec-dot">●</span> <span id="rec-time">REC 00:00</span></div>
      <div class="mic-bar-wrap" title="Mức thu micro">
        <div class="mic-bar-fill" id="mic-level-fill"></div>
      </div>
    </div>

    <div id="rec-toast" class="rec-toast no-drag" hidden title="Nhấn để mở thư mục chứa bản ghi">
      <span class="nic nic-folder"></span>
      <span id="rec-toast-text"></span>
    </div>

    <div id="dock" class="controls-bar no-drag">
      <button class="dock-btn" id="btn-spk" title="Loa"><span class="nic nic-speaker" id="ic-spk"></span></button>
      <button class="dock-btn" id="btn-mic" title="Micro"><span class="nic nic-mic" id="ic-mic"></span></button>
      <button class="dock-btn" id="btn-cam" title="Camera"><span class="nic nic-cam-off" id="ic-cam"></span></button>
      <button class="dock-btn" id="btn-screen" title="Chia sẻ màn hình"><span class="nic nic-screen" id="ic-screen"></span></button>
      <button class="dock-btn" id="btn-record" title="Ghi âm cuộc gọi (Chuột phải đổi WAV/Opus)"><span class="nic nic-record" id="ic-record"></span></button>
      <button class="dock-btn" id="btn-more" title="Thiết bị âm thanh"><span class="nic nic-more"></span></button>
      <button class="dock-btn dock-end" id="btn-end" title="Kết thúc"><span class="nic nic-endcall"></span></button>
    </div>
  </div>

  <script src="call-format.js"></script>
  <script src="sounds.js"></script>
  <script src="call.js"></script>
</body>
</html>"#;

pub const CALL_CSS: &str = r#"* { box-sizing: border-box; margin: 0; padding: 0; }
html, body { height: 100%; background: #1a1d21; color: #fff; overflow: hidden; user-select: none; -webkit-user-select: none; }
body { font-family: 'Inter', 'Segoe UI', 'Roboto', system-ui, sans-serif; display: flex; flex-direction: column; }
#titlebar {
  height: 34px; display: flex; align-items: center; padding-left: 14px;
  background: rgba(18, 20, 24, 0.92); border-bottom: 1px solid rgba(255,255,255,0.07);
  flex: 0 0 auto; z-index: 10;
}
.tb-title { font-size: 13px; font-weight: 600; color: #e5e7eb; }
.tb-spacer { flex: 1; }
.wc { width: 42px; height: 34px; display: flex; align-items: center; justify-content: center; cursor: pointer; color: #cbd5e1; }
.wc:hover { background: rgba(255,255,255,0.08); }
.wc-close:hover { background: #dc2626; color: #fff; }
.wc-min-i { width: 10px; height: 1px; background: currentColor; }
.wc-max-i { width: 9px; height: 9px; border: 1px solid currentColor; }
.wc-x { position: relative; width: 12px; height: 12px; }
.wc-x::before, .wc-x::after { content: ''; position: absolute; top: 50%; left: 0; width: 12px; height: 1.4px; background: currentColor; }
.wc-x::before { transform: rotate(45deg); }
.wc-x::after { transform: rotate(-45deg); }

#stage { position: relative; flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: space-between; padding: 48px 24px 34px; }
#remote-video {
  display: none; position: absolute; inset: 0; width: 100%; height: 100%;
  object-fit: cover; background: #0d0f12; z-index: 1;
}
#local-video {
  display: none; position: absolute; right: 16px; top: 16px; width: 148px; height: 111px;
  border-radius: 10px; border: 1.5px solid rgba(255,255,255,0.28); object-fit: cover;
  background: #111318; box-shadow: 0 8px 24px rgba(0,0,0,0.45); z-index: 4;
}
body.video-active #remote-video { display: block; }
body.local-video-active #local-video { display: block; }
body.video-active #center {
  z-index: 3; background: rgba(15, 18, 24, 0.45); padding: 12px 20px; border-radius: 14px;
  backdrop-filter: blur(6px); margin-top: 8px;
}
body.video-active.remote-has-frames .avatar-ring { display: none; }

#center { z-index: 3; display: flex; flex-direction: column; align-items: center; gap: 12px; margin-top: 32px; }
.avatar-ring { padding: 5px; border-radius: 50%; background: rgba(0, 104, 255, 0.18); }
.avatar {
  width: 116px; height: 116px; border-radius: 50%; background: #0068ff center/cover no-repeat;
  display: flex; align-items: center; justify-content: center; font-size: 40px; font-weight: 700; color: #fff;
}
.partner-name { font-size: 21px; font-weight: 600; color: #f9fafb; text-align: center; }
.call-status { font-size: 14px; color: #cbd5e1; min-height: 20px; }
[hidden] { display: none !important; }
.rec-indicator {
  display: inline-flex; align-items: center; gap: 6px; padding: 4px 11px; border-radius: 999px;
  font-size: 12px; font-weight: 700; letter-spacing: 0.4px; color: #fecaca;
  background: rgba(220, 38, 38, 0.24); border: 1px solid rgba(248, 113, 113, 0.55);
}
@keyframes recPulse { 0%, 100% { opacity: 1; transform: scale(1); } 50% { opacity: 0.3; transform: scale(0.85); } }
.rec-dot { color: #ef4444; font-size: 12px; line-height: 1; animation: recPulse 1.1s ease-in-out infinite; }
.mic-bar-wrap { width: 120px; height: 5px; border-radius: 3px; background: rgba(255,255,255,0.14); overflow: hidden; margin-top: 4px; }
.mic-bar-fill { width: 0%; height: 100%; background: #34d399; transition: width 75ms linear; }

.rec-toast {
  position: absolute; bottom: 98px; left: 50%; transform: translateX(-50%); z-index: 6;
  max-width: calc(100% - 32px); display: inline-flex; align-items: center; gap: 8px;
  padding: 8px 14px; border-radius: 10px; background: rgba(17, 24, 39, 0.94);
  border: 1px solid rgba(52, 211, 153, 0.45); color: #ecfdf5; font-size: 12.5px; cursor: pointer;
  box-shadow: 0 8px 20px rgba(0,0,0,0.45); white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.rec-toast:hover { background: rgba(31, 41, 55, 0.98); border-color: rgba(52, 211, 153, 0.75); }
.rec-toast.rec-toast-error { border-color: rgba(248, 113, 113, 0.6); color: #fecaca; cursor: default; }
.rec-toast .nic-folder { width: 18px; height: 18px; flex: 0 0 auto; }
#rec-toast-text { overflow: hidden; text-overflow: ellipsis; }

#dock {
  z-index: 5; display: flex; align-items: center; gap: 9px;
  background: rgba(24, 28, 34, 0.86); padding: 10px 14px; border-radius: 40px;
  border: 1px solid rgba(255,255,255,0.09); backdrop-filter: blur(8px);
}
.dock-btn {
  width: 43px; height: 43px; border-radius: 50%; border: none; cursor: pointer;
  background: rgba(255,255,255,0.12); display: inline-flex; align-items: center; justify-content: center;
  transition: background 120ms ease, transform 90ms ease, box-shadow 120ms ease;
}
.dock-btn:hover { background: rgba(255,255,255,0.2); }
.dock-btn:active { transform: scale(0.95); }
.dock-btn.active-accent { background: #2563eb; }
.dock-btn.muted { background: rgba(239, 68, 68, 0.22); }
.dock-btn.recording { background: rgba(239, 68, 68, 0.34); box-shadow: 0 0 0 2px rgba(248, 113, 113, 0.65); }
.dock-end { background: #ef4444; }
.dock-end:hover { background: #dc2626; }
"#;

pub const CALL_JS: &str = r#"(function () {
  var fmt = window.zcallFormat;
  var api = window.zcallUI;
  var $ = function (id) { return document.getElementById(id); };
  var sounds = window.createSounds ? window.createSounds() : { apply: function(){}, stopAll: function(){} };

  var state = 'calling', connectedAt = null, muted = false, spkOff = false;
  var camEnabled = false, screenSharing = false, tickIv = null;
  var recActive = false, recStartedAt = null, recTickIv = null, recToastTm = null;
  var recFormat = 'wav', lastSavedPath = null;

  function renderRecTimer() {
    var sec = (recActive && recStartedAt) ? (Date.now() - recStartedAt) / 1000 : 0;
    $('rec-time').textContent = 'REC ' + fmt.formatDuration(sec);
  }

  function showRecToast(text, savedPath, isError) {
    lastSavedPath = savedPath || null;
    var toast = $('rec-toast');
    $('rec-toast-text').textContent = text || '';
    toast.classList.toggle('rec-toast-error', !!isError);
    toast.hidden = false;
    if (recToastTm) clearTimeout(recToastTm);
    recToastTm = setTimeout(function () { toast.hidden = true; }, 8000);
  }

  function applyRecording(r) {
    if (!r) return;
    recActive = !!r.active;
    if (r.format === 'wav' || r.format === 'opus') recFormat = r.format;
    if (recTickIv) { clearInterval(recTickIv); recTickIv = null; }
    if (recActive) {
      $('rec-indicator').hidden = false;
      $('rec-toast').hidden = true;
      if (r.startedAtMs) {
        recStartedAt = r.startedAtMs;
        renderRecTimer();
        recTickIv = setInterval(renderRecTimer, 500);
      } else {
        recStartedAt = null;
        $('rec-time').textContent = 'REC CHỜ KẾT NỐI';
      }
    } else {
      recStartedAt = null;
      $('rec-indicator').hidden = true;
      if (r.error) {
        showRecToast('Lỗi ghi âm: ' + r.error, null, true);
      } else if (r.path) {
        var fname = String(r.path).split(/[\\/]/).pop() || String(r.path);
        showRecToast('Đã lưu: ' + fname + ' (mở thư mục)', r.path, false);
      }
    }
    $('btn-record').classList.toggle('recording', recActive);
    $('ic-record').className = 'nic ' + (recActive ? 'nic-record-stop' : 'nic-record');
  }

  function renderStatus() {
    var sec = (state === 'connected' && connectedAt) ? (Date.now() - connectedAt) / 1000 : 0;
    $('status').textContent = fmt.formatStatus(state, sec);
  }

  function applyPartner(p) {
    p = p || {};
    var name = p.name || 'Zalo Call';
    $('partner-name').textContent = name;
    $('tb-title').textContent = 'Zalo Call — ' + name;
    var av = $('avatar');
    if (p.avatar) {
      av.style.backgroundImage = 'url("' + p.avatar + '")';
      $('avatar-initial').textContent = '';
    } else {
      av.style.backgroundImage = '';
      $('avatar-initial').textContent = (name.trim().charAt(0) || 'Z').toUpperCase();
    }
  }

  function syncMediaButtons() {
    $('ic-cam').className = 'nic ' + (camEnabled ? 'nic-cam' : 'nic-cam-off');
    $('btn-cam').classList.toggle('active-accent', camEnabled);
    $('btn-screen').classList.toggle('active-accent', screenSharing);
    document.body.classList.toggle('local-video-active', camEnabled || screenSharing);
  }

  function applyState(s) {
    if (!s) return;
    state = s.state || state;
    if (s.connectedAt) connectedAt = s.connectedAt;
    if (typeof s.isVideo === 'boolean') document.body.classList.toggle('video-active', s.isVideo);
    if (typeof s.cameraEnabled === 'boolean') camEnabled = s.cameraEnabled;
    if (typeof s.screenSharing === 'boolean') screenSharing = s.screenSharing;
    syncMediaButtons();
    if (tickIv) { clearInterval(tickIv); tickIv = null; }
    if (state === 'connected') {
      if (!connectedAt) connectedAt = Date.now();
      tickIv = setInterval(renderStatus, 500);
    }
    sounds.apply(state, s.outcome);
    renderStatus();
  }

  function drawRgbaFrame(f) {
    if (!f || !f.rgbaBase64 || !f.width || !f.height) return;
    var canvas = $(f.isLocal ? 'local-video' : 'remote-video');
    if (!canvas) return;
    if (canvas.width !== f.width) canvas.width = f.width;
    if (canvas.height !== f.height) canvas.height = f.height;
    var raw = atob(f.rgbaBase64);
    var bytes = new Uint8ClampedArray(raw.length);
    for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    var ctx = canvas.getContext('2d');
    if (ctx && bytes.length === f.width * f.height * 4) {
      ctx.putImageData(new ImageData(bytes, f.width, f.height), 0, 0);
      document.body.classList.add('video-active');
      if (f.isLocal) document.body.classList.add('local-video-active');
      else document.body.classList.add('remote-has-frames');
    }
  }

  api.onPartner(applyPartner);
  api.onState(applyState);
  api.onMicLevel(function (permille) {
    var pct = Math.max(0, Math.min(100, Math.round((Number(permille) || 0) / 10)));
    $('mic-level-fill').style.width = (muted ? 0 : pct) + '%';
  });
  api.onVideoFrame(drawRgbaFrame);
  api.onRecording(applyRecording);

  $('btn-end').addEventListener('click', function () { sounds.stopAll(); api.action('end'); });
  $('btn-mic').addEventListener('click', function () {
    muted = !muted;
    $('ic-mic').className = 'nic ' + (muted ? 'nic-mic-off' : 'nic-mic');
    $('btn-mic').classList.toggle('muted', muted);
    if (muted) $('mic-level-fill').style.width = '0%';
    api.action('mute', muted);
  });
  $('btn-spk').addEventListener('click', function () {
    spkOff = !spkOff;
    $('ic-spk').className = 'nic ' + (spkOff ? 'nic-speaker-off' : 'nic-speaker');
    $('btn-spk').classList.toggle('muted', spkOff);
    api.action('toggleSpeaker', spkOff);
  });
  $('btn-cam').addEventListener('click', function () {
    camEnabled = !camEnabled;
    if (camEnabled) document.body.classList.add('video-active');
    syncMediaButtons();
    api.action('toggleCamera', camEnabled);
  });
  $('btn-screen').addEventListener('click', function () {
    screenSharing = !screenSharing;
    if (screenSharing) document.body.classList.add('video-active');
    syncMediaButtons();
    api.action('toggleScreenShare', screenSharing);
  });
  $('btn-record').addEventListener('click', function () {
    api.toggleRecord({ format: recFormat, layout: 'stereo' });
  });
  $('btn-record').addEventListener('contextmenu', function (ev) {
    ev.preventDefault();
    if (!recActive) {
      recFormat = (recFormat === 'wav') ? 'opus' : 'wav';
      showRecToast('Định dạng ghi âm: ' + recFormat.toUpperCase(), null, false);
    }
  });
  $('rec-toast').addEventListener('click', function () {
    if (lastSavedPath) {
      api.openRecordingFolder(lastSavedPath);
    }
  });
  $('btn-more').addEventListener('click', function () { api.action('openDevices'); });
  $('wc-min').addEventListener('click', function () { api.action('win', 'minimize'); });
  $('wc-max').addEventListener('click', function () { api.action('win', 'maximize'); });
  $('wc-close').addEventListener('click', function () { sounds.stopAll(); api.action('win', 'close'); });
  renderStatus();
})();"#;

pub const INCOMING_HTML: &str = r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <link rel="stylesheet" href="icons.css" />
  <link rel="stylesheet" href="incoming.css" />
  <title>Cuộc gọi đến</title>
</head>
<body>
  <div id="wrap" class="drag-region">
    <div id="wave"></div>
    <div id="avatar"><span id="avatar-initial">Z</span></div>
    <div id="name">—</div>
    <div id="sub">Cuộc gọi đến</div>
    <div id="actions" class="no-drag">
      <button id="btn-decline" class="rbtn rbtn-decline" title="Từ chối"><span class="nic nic-endcall"></span></button>
      <button id="btn-accept" class="rbtn rbtn-accept" title="Trả lời"><span class="nic nic-accept"></span></button>
    </div>
  </div>
  <script src="sounds.js"></script>
  <script src="incoming.js"></script>
</body>
</html>"#;

pub const INCOMING_CSS: &str = r#"* { margin: 0; padding: 0; box-sizing: border-box; }
html, body { height: 100%; background: #1a1d21; color: #fff; font-family: 'Inter', 'Roboto', 'Segoe UI', sans-serif; user-select: none; -webkit-user-select: none; overflow: hidden; }
#wrap { position: relative; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 14px; }
#avatar {
  width: 118px; height: 118px; border-radius: 50%; background: #0068ff center/cover no-repeat;
  z-index: 2; display: flex; align-items: center; justify-content: center; font-size: 42px; font-weight: 700;
}
#wave {
  position: absolute; top: calc(50% - 148px); width: 184px; height: 184px; border-radius: 50%;
  border: 2px solid rgba(0, 104, 255, 0.55); animation: pulse 1.6s ease-out infinite; z-index: 1;
}
@keyframes pulse { 0% { transform: scale(0.75); opacity: 0.85; } 100% { transform: scale(1.3); opacity: 0; } }
#name { font-size: 20px; font-weight: 600; z-index: 2; padding: 0 18px; text-align: center; }
#sub { font-size: 14px; color: #cbd5e1; z-index: 2; }
#actions { display: flex; gap: 60px; margin-top: 26px; z-index: 2; }
.rbtn {
  width: 64px; height: 64px; border: none; border-radius: 50%; cursor: pointer;
  display: inline-flex; align-items: center; justify-content: center;
  transition: transform 90ms ease, filter 120ms ease;
}
.rbtn:hover { filter: brightness(1.1); }
.rbtn:active { transform: scale(0.95); }
.rbtn-decline { background: #ef4444; }
.rbtn-accept { background: #22c55e; }
"#;

pub const INCOMING_JS: &str = r#"(function () {
  var $ = function (id) { return document.getElementById(id); };
  var api = window.zcallUI;
  var sounds = window.createSounds ? window.createSounds() : { apply: function(){}, stopAll: function(){} };

  api.onPartner(function (p) {
    p = p || {};
    var nm = p.name || '—';
    $('name').textContent = nm;
    if (p.avatar) {
      $('avatar').style.backgroundImage = 'url("' + p.avatar + '")';
      $('avatar-initial').textContent = '';
    } else {
      $('avatar').style.backgroundImage = '';
      $('avatar-initial').textContent = (nm.trim().charAt(0) || 'Z').toUpperCase();
    }
  });
  api.onState(function (s) {
    if (s && s.isVideo) $('sub').textContent = 'Cuộc gọi video đến';
    else $('sub').textContent = 'Cuộc gọi thoại đến';
    sounds.apply(s && s.state, s && s.outcome);
  });
  $('btn-accept').addEventListener('click', function () { sounds.stopAll(); api.action('accept'); });
  $('btn-decline').addEventListener('click', function () { sounds.stopAll(); api.action('decline'); });
})();"#;

pub const DEVICES_HTML: &str = r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <link rel="stylesheet" href="icons.css" />
  <link rel="stylesheet" href="devices.css" />
  <title>Tình trạng thiết bị</title>
</head>
<body>
  <div id="titlebar">
    <span class="tb-title" id="tb-title">Tình trạng thiết bị</span>
    <span class="tb-spacer"></span>
    <div class="wc" id="wc-min" title="Thu nhỏ"><i class="wc-min-i"></i></div>
    <div class="wc" id="wc-max" title="Phóng to"><i class="wc-max-i"></i></div>
    <div class="wc wc-close" id="wc-close" title="Đóng"><i class="wc-x"></i></div>
  </div>

  <div class="dlg-body no-drag">
    <div class="statbox"><span class="zic zic-mic sb-ic"></span><span class="sb-tx">Micro đang hoạt động</span><span class="ok-check">✓</span></div>
    <div class="dlg-label">Micro</div>
    <select class="dlg-select" id="sel-mic"></select>
    <div class="meter" id="mic-meter"></div>

    <div class="statbox" style="margin-top: 18px;"><span class="zic zic-speaker sb-ic"></span><span class="sb-tx">Loa/tai nghe đang hoạt động</span><span class="ok-check">✓</span></div>
    <div class="dlg-label">Loa/tai nghe</div>
    <select class="dlg-select" id="sel-spk"></select>

    <div class="dlg-label">Kiểm tra loa/tai nghe</div>
    <div class="test-row"><button class="test-play" id="test-play">▶</button><div class="test-bar"><div class="test-fill" id="test-fill"></div></div></div>

    <div class="dlg-label">Âm lượng</div>
    <input type="range" class="dlg-slider" id="vol" min="0" max="100" value="80" />
  </div>
  <div class="dlg-foot no-drag"><button class="dlg-cancel" id="dlg-cancel">Đóng</button></div>

  <script src="sounds.js"></script>
  <script src="devices.js"></script>
</body>
</html>"#;

pub const DEVICES_CSS: &str = r#":root {
  --ink: #1a1a1a;
  --line: #e1e4ea;
  --titlebar-bg: #f3f3f3;
  --close-hover: #c42b1c;
  --label-gray: #6b7280;
}
* { box-sizing: border-box; margin: 0; padding: 0; }
html, body { height: 100%; }
body {
  font-family: 'Inter', 'Segoe UI', 'Roboto', system-ui, sans-serif;
  color: var(--ink); background: #fff; overflow: hidden; user-select: none; -webkit-user-select: none;
  display: flex; flex-direction: column;
}
#titlebar {
  height: 32px; display: flex; align-items: center; padding-left: 14px;
  background: var(--titlebar-bg); border-bottom: 1px solid var(--line); flex: 0 0 auto;
}
.tb-title { font-size: 13px; font-weight: 600; }
.tb-spacer { flex: 1; }
.wc { width: 40px; height: 32px; display: flex; align-items: center; justify-content: center; cursor: pointer; color: #46556a; }
.wc:hover { background: rgba(0,0,0,.06); }
.wc-close:hover { background: var(--close-hover); color: #fff; }
.wc-min-i { width: 10px; height: 1px; background: currentColor; }
.wc-max-i { width: 9px; height: 9px; border: 1px solid currentColor; }
.wc-x { position: relative; width: 12px; height: 12px; }
.wc-x::before, .wc-x::after { content: ''; position: absolute; top: 50%; left: 0; width: 12px; height: 1px; background: currentColor; }
.wc-x::before { transform: rotate(45deg); }
.wc-x::after { transform: rotate(-45deg); }

.dlg-body { padding: 18px 20px; overflow-y: auto; flex: 1; }
.statbox {
  display: flex; align-items: center; gap: 8px; padding: 11px 12px;
  background: #edfaf1; border: 1px solid #bfe8cf; border-radius: 6px; font-size: 14px; font-weight: 600;
}
.statbox .sb-tx { flex: 1; }
.statbox .ok-check {
  width: 18px; height: 18px; border-radius: 50%; background: #1f9f64; color: #fff;
  font-size: 12px; display: inline-flex; align-items: center; justify-content: center;
}
.dlg-label { color: var(--label-gray); font-size: 12px; margin: 14px 0 6px; }
.dlg-select {
  width: 100%; height: 38px; padding: 0 10px; font-size: 13px; color: var(--ink);
  border: 1px solid #e0e0e0; border-radius: 6px; background: #fff; cursor: pointer;
}
.meter { display: flex; gap: 6px; margin-top: 12px; }
.meter .dot { flex: 1; max-width: 11px; height: 11px; border-radius: 50%; background: #d9d9d9; transition: background 60ms linear; }
.meter .dot.on { background: #2f80ed; }
.test-row { display: flex; align-items: center; gap: 12px; }
.test-play {
  width: 32px; height: 32px; border-radius: 50%; border: none; cursor: pointer;
  background: #eaf3ff; color: #2f80ed; font-size: 12px; display: inline-flex; align-items: center; justify-content: center;
}
.test-bar { flex: 1; height: 5px; background: #e6eefb; border-radius: 3px; overflow: hidden; }
.test-fill { width: 0; height: 100%; background: #2f80ed; }
.dlg-slider { width: 100%; accent-color: #2f80ed; }
.dlg-foot { padding: 14px 20px 18px; display: flex; justify-content: flex-end; flex: 0 0 auto; }
.dlg-cancel {
  min-width: 80px; height: 36px; padding: 0 18px; font-size: 13px; cursor: pointer;
  background: #f0f0f0; color: var(--ink); border: 1px solid #e0e0e0; border-radius: 6px;
}
.dlg-cancel:hover { background: #e6e6e6; }
"#;

pub const DEVICES_JS: &str = r#"(function () {
  var $ = function (id) { return document.getElementById(id); };
  var api = window.zcallUI;
  var devices = { capture: [], playback: [] }, selIn = -1, selOut = -1;

  function buildMeter() {
    var m = $('mic-meter');
    if (m.children.length) return;
    for (var i = 0; i < 20; i++) {
      var d = document.createElement('span');
      d.className = 'dot';
      m.appendChild(d);
    }
  }

  function updateMeter(permille) {
    var dots = $('mic-meter').children;
    var lit = Math.round(((Number(permille) || 0) / 1000) * dots.length);
    for (var i = 0; i < dots.length; i++) {
      dots[i].className = 'dot' + (i < lit ? ' on' : '');
    }
  }

  function fillSelect(sel, list, cur) {
    sel.innerHTML = '';
    var od = document.createElement('option');
    od.value = -1;
    od.textContent = 'Thiết bị mặc định';
    if (cur === -1) od.selected = true;
    sel.appendChild(od);
    (list || []).forEach(function (dev) {
      var o = document.createElement('option');
      o.value = dev.index;
      o.textContent = dev.name;
      if (cur === dev.index) o.selected = true;
      sel.appendChild(o);
    });
  }

  function render() {
    fillSelect($('sel-mic'), devices.capture, selIn);
    fillSelect($('sel-spk'), devices.playback, selOut);
  }

  buildMeter();
  render();

  api.onDevices(function (d) {
    devices = d || { capture: [], playback: [] };
    if (d && typeof d.selectedIn === 'number') selIn = d.selectedIn;
    if (d && typeof d.selectedOut === 'number') selOut = d.selectedOut;
    render();
  });
  api.onMicLevel(updateMeter);

  $('sel-mic').addEventListener('change', function () { selIn = parseInt(this.value, 10); api.action('selectInput', selIn); });
  $('sel-spk').addEventListener('change', function () { selOut = parseInt(this.value, 10); api.action('selectOutput', selOut); });
  $('test-play').addEventListener('click', function () {
    var f = $('test-fill');
    f.style.transition = 'none';
    f.style.width = '0';
    setTimeout(function () { f.style.transition = 'width 0.9s linear'; f.style.width = '100%'; }, 20);
    if (window.createSounds) {
      var s = window.createSounds();
      s.apply('calling');
      setTimeout(function () { s.stopAll(); }, 900);
    }
  });
  $('dlg-cancel').addEventListener('click', function () { api.action('devwin', 'close'); });
  $('wc-close').addEventListener('click', function () { api.action('devwin', 'close'); });
  $('wc-min').addEventListener('click', function () { api.action('devwin', 'minimize'); });
  $('wc-max').addEventListener('click', function () { api.action('devwin', 'maximize'); });
})();"#;

/// Looks up an embedded Call UI asset by URI path (e.g. `"/call.html"` or `"call.css"`),
/// returning `(mime_type, bytes)`.
pub fn get_ui_asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    let clean = path
        .trim_start_matches('/')
        .split('?')
        .next()
        .unwrap_or("")
        .trim_start_matches("app/");
    match clean {
        "" | "call.html" => Some(("text/html; charset=utf-8", CALL_HTML.as_bytes())),
        "call.css" => Some(("text/css; charset=utf-8", CALL_CSS.as_bytes())),
        "call.js" => Some(("application/javascript; charset=utf-8", CALL_JS.as_bytes())),
        "call-format.js" => Some((
            "application/javascript; charset=utf-8",
            CALL_FORMAT_JS.as_bytes(),
        )),
        "sounds.js" => Some(("application/javascript; charset=utf-8", SOUNDS_JS.as_bytes())),
        "icons.css" => Some(("text/css; charset=utf-8", ICONS_CSS.as_bytes())),
        "incoming.html" => Some(("text/html; charset=utf-8", INCOMING_HTML.as_bytes())),
        "incoming.css" => Some(("text/css; charset=utf-8", INCOMING_CSS.as_bytes())),
        "incoming.js" => Some((
            "application/javascript; charset=utf-8",
            INCOMING_JS.as_bytes(),
        )),
        "devices.html" => Some(("text/html; charset=utf-8", DEVICES_HTML.as_bytes())),
        "devices.css" => Some(("text/css; charset=utf-8", DEVICES_CSS.as_bytes())),
        "devices.js" => Some((
            "application/javascript; charset=utf-8",
            DEVICES_JS.as_bytes(),
        )),
        "preload.js" => Some((
            "application/javascript; charset=utf-8",
            PRELOAD_BRIDGE_JS.as_bytes(),
        )),
        _ => None,
    }
}
