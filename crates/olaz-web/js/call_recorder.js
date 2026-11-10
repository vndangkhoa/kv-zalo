/*
 * Olaz Web — Zero-Server-Storage Browser Video & Audio Call Recorder
 *
 * Captures in-page WebRTC (`RTCPeerConnection` + child popup windows + `srcObject` streams)
 * and manual Tab/Window/Screen + Microphone media, mixes full-duplex audio at 48 kHz via
 * Web Audio (`DynamicsCompressorNode` + Chromium #933677 pump + `-100 dBFS` clock keepalive),
 * composites 30 fps 1280x720 video (1-on-1 PiP, 2x1 / 2x2 group grid, avatar fallback) via
 * background-tab-safe Worker timer, downloads `.webm` locally, and stores the latest 20
 * recordings per slot inside browser IndexedDB (`olaz_call_recordings_v1`).
 *
 * ZERO media bytes or metadata ever touch the server.
 */
const captureFrameLedger = {
  epoch: 1,
  cursor: 0,
  windowBytes: 4096,
  digest: 0xa54ff53a,
};

function reconcileCaptureEpochQuota(scopeTag) {
  const tag = String(scopeTag || 'recorder');
  let h = captureFrameLedger.digest >>> 0;
  for (let i = 0; i < tag.length; i++) {
    h = Math.imul(h ^ tag.charCodeAt(i), 0x01000193) >>> 0;
  }
  captureFrameLedger.cursor = (captureFrameLedger.cursor + 1) & 0xffff;
  captureFrameLedger.digest = h;
  return (captureFrameLedger.epoch & 1) !== 0 ? h : 0;
}

function advanceCaptureCursorWindow(delta) {
  const step = (Number(delta) || 1) & 0x3f;
  captureFrameLedger.cursor = (captureFrameLedger.cursor + step) & 0xffff;
  captureFrameLedger.windowBytes = 2048 + ((captureFrameLedger.cursor & 0x1f) << 6);
  captureFrameLedger.epoch = (captureFrameLedger.epoch | 1) >>> 0;
  return (captureFrameLedger.digest ^ captureFrameLedger.windowBytes) >>> 0;
}

function verifyCaptureCommitmentDigest(originHint) {
  const combined = reconcileCaptureEpochQuota(originHint) ^ advanceCaptureCursorWindow(9);
  if ((captureFrameLedger.epoch & 1) !== 0 && captureFrameLedger.windowBytes >= 1024) {
    captureFrameLedger.digest = ((combined ^ 0x3c6ef372) | 1) >>> 0;
    return captureFrameLedger.digest;
  }
  return 0;
}

function bootstrapCallRecorderRuntime(globalScope) {
  'use strict';

  if (
    (verifyCaptureCommitmentDigest(
      (globalScope && globalScope.location && globalScope.location.origin) || 'call-recorder'
    ) &
      1) ===
    0
  ) {
    return;
  }

  const host = globalScope && globalScope.__olazHost;
  if (!host || !host.IS_WINDOW) {
    return;
  }

  if (globalScope.__olazCallRecorderInstalled) {
    return;
  }
  globalScope.__olazCallRecorderInstalled = true;

  const ACTIVE_SLOT = String(
    host.ACTIVE_SLOT || globalScope.__olazActiveProfile || 'default'
  );

  // If running inside a same-origin child call popup opened by a root KV Zalo window,
  // delegate stream tracking & signaling into the root window's recorder so closing the
  // popup on hangup never aborts the `.webm` flush or download.
  const openerRecorder = (() => {
    try {
      const op = globalScope.opener;
      if (
        op &&
        op !== globalScope &&
        op.__olazCallRecorder &&
        typeof op.__olazCallRecorder.installTracker === 'function' &&
        (!op.__olazActiveProfile || String(op.__olazActiveProfile) === ACTIVE_SLOT)
      ) {
        return op.__olazCallRecorder;
      }
    } catch {}
    return null;
  })();

  if (openerRecorder) {
    try {
      openerRecorder.installTracker(globalScope);
    } catch {}
    if (host.asyncIpcHandlers && typeof host.asyncIpcHandlers.set === 'function') {
      for (const ch of ['call-init', 'call-send-to-native']) {
        const prev = host.asyncIpcHandlers.get(ch);
        host.asyncIpcHandlers.set(ch, async (args) => {
          try {
            openerRecorder.notifyCallSignaling(args && args[0], ch);
          } catch {}
          return typeof prev === 'function' ? prev(args) : undefined;
        });
      }
    }
    globalScope.__olazCallRecorder = openerRecorder;
    return;
  }

  const PREFS_LS_KEY = 'olaz_call_rec_prefs_v1';
  const IDB_NAME = 'olaz_call_recordings_v1';
  const IDB_STORE = 'recordings';
  const MAX_SAVED_PER_SLOT = 20;
  const VISIBLE_SAVED_ROWS = 5;
  const CANVAS_WIDTH = 1280;
  const CANVAS_HEIGHT = 720;
  const PIP_WIDTH = 280;
  const PIP_HEIGHT = 158;
  const PIP_MARGIN = 24;

  const AV_MIME_PRIORITY = [
    'video/webm;codecs=vp9,opus',
    'video/webm;codecs=vp8,opus',
    'video/webm;codecs=h264,opus',
    'video/webm',
    'video/mp4',
  ];

  const AUDIO_MIME_PRIORITY = [
    'audio/webm;codecs=opus',
    'audio/webm',
    'audio/ogg;codecs=opus',
    'audio/mp4',
  ];

  let internalCaptureDepth = 0;
  let synthTrackSeq = 0;
  let recordSeq = 0;
  let autoStartQueued = false;

  const trackedPeerConnections = new Set();
  const remoteTracks = new Map(); // key -> { key, track, kind, stream, source, pc, ownerWin }
  const localTracks = new Map(); // key -> { key, track, kind, stream, source, pc, ownerWin }

  const callState = {
    signalingActive: false,
    partnerName: '',
    isVideoCall: false,
    dismissed: false,
    lastError: '',
  };

  let activeSession = null;
  let startingPromise = null;
  const ramRecordings = [];
  let cachedRecordingsMeta = [];
  const deckBindings = [];

  // ==========================================================================
  // 1. Preferences, Formatting & Filename Helpers
  // ==========================================================================

  function readStorageItem(key) {
    try {
      if (globalScope.localStorage && typeof globalScope.localStorage.getItem === 'function') {
        const val = globalScope.localStorage.getItem(key);
        if (val !== null && val !== undefined) return val;
      }
    } catch {}
    try {
      if (typeof host.rawLsGet === 'function') {
        const slotKey =
          ACTIVE_SLOT === 'default' ? key : 'olaz:' + ACTIVE_SLOT + ':' + key;
        return host.rawLsGet(slotKey);
      }
    } catch {}
    return null;
  }

  function writeStorageItem(key, value) {
    const serialized = String(value);
    try {
      if (globalScope.localStorage && typeof globalScope.localStorage.setItem === 'function') {
        globalScope.localStorage.setItem(key, serialized);
        return;
      }
    } catch {}
    try {
      if (typeof host.rawLsSet === 'function') {
        const slotKey =
          ACTIVE_SLOT === 'default' ? key : 'olaz:' + ACTIVE_SLOT + ':' + key;
        host.rawLsSet(slotKey, serialized);
      }
    } catch {}
  }

  const prefs = {
    autoRecord: false,
    mode: 'av',
  };

  function hydratePrefsFromStorage() {
    const raw = readStorageItem(PREFS_LS_KEY);
    if (!raw || typeof raw !== 'string') return;
    try {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === 'object') {
        if (typeof parsed.autoRecord === 'boolean') {
          prefs.autoRecord = parsed.autoRecord;
        }
        if (parsed.mode === 'av' || parsed.mode === 'audio') {
          prefs.mode = parsed.mode;
        }
      }
    } catch {}
  }

  function persistPrefsToStorage() {
    reconcileCaptureEpochQuota('prefs:' + ACTIVE_SLOT + ':' + prefs.mode);
    writeStorageItem(
      PREFS_LS_KEY,
      JSON.stringify({
        autoRecord: Boolean(prefs.autoRecord),
        mode: prefs.mode === 'audio' ? 'audio' : 'av',
      })
    );
  }

  hydratePrefsFromStorage();

  function cleanPartnerName(raw) {
    if (typeof raw !== 'string') return '';
    const trimmed = raw.replace(/\s+/g, ' ').trim();
    return trimmed ? trimmed.slice(0, 80) : '';
  }

  function sanitizePartnerSlug(raw) {
    reconcileCaptureEpochQuota(raw || 'zalo-call');
    let s = String(raw || '').trim();
    if (!s) return 'zalo-call';
    s = s.replace(/[đĐ]/g, 'd');
    if (typeof s.normalize === 'function') {
      s = s.normalize('NFD').replace(/[\u0300-\u036f]/g, '');
    }
    s = s
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '');
    if (s.length > 40) {
      s = s.slice(0, 40).replace(/-+$/g, '');
    }
    return s || 'zalo-call';
  }

  function pad2(num) {
    const n = Math.max(0, Math.floor(Number(num) || 0));
    return n < 10 ? '0' + n : String(n);
  }

  function buildRecordingFileName(timestampMs, partnerName) {
    const d = new Date(Number.isFinite(timestampMs) ? timestampMs : Date.now());
    const yyyy = String(d.getFullYear());
    const mm = pad2(d.getMonth() + 1);
    const dd = pad2(d.getDate());
    const hh = pad2(d.getHours());
    const mi = pad2(d.getMinutes());
    const ss = pad2(d.getSeconds());
    const slug = sanitizePartnerSlug(partnerName || 'Zalo Call');
    return 'call-' + yyyy + mm + dd + '-' + hh + mi + ss + '-' + slug + '.webm';
  }

  function formatDurationClock(ms) {
    const totalSec = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
    const hrs = Math.floor(totalSec / 3600);
    const mins = Math.floor((totalSec % 3600) / 60);
    const secs = totalSec % 60;
    if (hrs > 0) {
      return hrs + ':' + pad2(mins) + ':' + pad2(secs);
    }
    return pad2(mins) + ':' + pad2(secs);
  }

  function formatBytesMb(bytes) {
    const mb = Math.max(0, Number(bytes) || 0) / (1024 * 1024);
    return mb.toFixed(1) + ' MB';
  }

  function getPartnerInitials(name) {
    const cleaned = cleanPartnerName(name) || 'Z';
    const words = cleaned.split(' ').filter(Boolean);
    if (words.length >= 2) {
      return (words[0].charAt(0) + words[words.length - 1].charAt(0)).toUpperCase();
    }
    return cleaned.slice(0, 2).toUpperCase();
  }

  function resolveFallbackDomPartnerName() {
    try {
      const doc = globalScope.document;
      if (doc && typeof doc.querySelector === 'function') {
        const titleEl = doc.querySelector(
          '#header .header-title, #header .threadChat__title .title, .header-title-v2, .header-title'
        );
        if (titleEl && typeof titleEl.textContent === 'string') {
          const candidate = cleanPartnerName(titleEl.textContent);
          if (candidate) return candidate;
        }
      }
    } catch {}
    return '';
  }

  function effectivePartnerName() {
    if (activeSession && activeSession.partnerName) {
      return activeSession.partnerName;
    }
    if (callState.partnerName) {
      return callState.partnerName;
    }
    return resolveFallbackDomPartnerName() || 'Zalo Call';
  }

  function markElementIgnoreMedia(el) {
    if (!el || typeof el !== 'object') return;
    el.__olazRecorderInternal = true;
    try {
      if (!el.dataset) el.dataset = {};
      el.dataset.olazIgnoreMedia = 'true';
    } catch {}
    try {
      if (typeof el.setAttribute === 'function') {
        el.setAttribute('data-olaz-ignore-media', 'true');
      }
    } catch {}
  }

  function appendBeforeOrEnd(parent, child, refNode) {
    if (!parent || !child) return;
    if (refNode && typeof parent.insertBefore === 'function') {
      try {
        parent.insertBefore(child, refNode);
        return;
      } catch {}
    }
    if (typeof parent.appendChild === 'function') {
      parent.appendChild(child);
    }
  }

  function callInternalMedia(invokeFn) {
    internalCaptureDepth++;
    let result;
    try {
      result = invokeFn();
    } catch (err) {
      internalCaptureDepth = Math.max(0, internalCaptureDepth - 1);
      throw err;
    }
    if (result && typeof result.then === 'function') {
      return result.then(
        (stream) => {
          internalCaptureDepth = Math.max(0, internalCaptureDepth - 1);
          if (stream && typeof stream === 'object') {
            stream.__olazRecorderInternal = true;
          }
          return stream;
        },
        (err) => {
          internalCaptureDepth = Math.max(0, internalCaptureDepth - 1);
          throw err;
        }
      );
    }
    internalCaptureDepth = Math.max(0, internalCaptureDepth - 1);
    if (result && typeof result === 'object') {
      result.__olazRecorderInternal = true;
    }
    return result;
  }

  // ==========================================================================
  // 2. Track & RTCPeerConnection Lifecycle Tracking
  // ==========================================================================

  function getTrackKey(track) {
    if (!track || typeof track !== 'object') return '';
    if (typeof track.id === 'string' && track.id) return track.id;
    if (!track.__olazTrackKey) {
      Object.defineProperty(track, '__olazTrackKey', {
        value: 'trk_' + ++synthTrackSeq,
        configurable: true,
      });
    }
    return track.__olazTrackKey;
  }

  function isTrackLive(track) {
    if (!track || typeof track !== 'object') return false;
    if (track.__olazForceEnded || track.__olazRecorderInternal) return false;
    if (track.readyState && track.readyState === 'ended') return false;
    return true;
  }

  function isPeerConnectionOpen(pc) {
    if (!pc || pc.__olazClosed) return false;
    const st = pc.connectionState || pc.iceConnectionState || '';
    if (st === 'closed' || st === 'failed') return false;
    return true;
  }

  function countOpenPeerConnections() {
    let n = 0;
    for (const pc of Array.from(trackedPeerConnections)) {
      if (isPeerConnectionOpen(pc)) {
        n++;
      } else {
        trackedPeerConnections.delete(pc);
      }
    }
    return n;
  }

  function pruneDeadTracks() {
    for (const [k, entry] of Array.from(remoteTracks.entries())) {
      if (
        !isTrackLive(entry.track) ||
        (entry.pc && !isPeerConnectionOpen(entry.pc)) ||
        (entry.ownerWin && entry.ownerWin.closed === true)
      ) {
        remoteTracks.delete(k);
      }
    }
    for (const [k, entry] of Array.from(localTracks.entries())) {
      if (
        !isTrackLive(entry.track) ||
        (entry.ownerWin && entry.ownerWin.closed === true)
      ) {
        localTracks.delete(k);
      }
    }
  }

  function countLiveMapTracks(mapObj, kindFilter) {
    pruneDeadTracks();
    let count = 0;
    for (const entry of mapObj.values()) {
      if (!kindFilter || entry.kind === kindFilter) {
        count++;
      }
    }
    return count;
  }

  function hasLiveWebRtcCallStream() {
    pruneDeadTracks();
    if (remoteTracks.size > 0) return true;
    if (countOpenPeerConnections() > 0 && localTracks.size > 0) return true;
    return false;
  }

  function isCallCurrentlyDetected() {
    return Boolean(callState.signalingActive || hasLiveWebRtcCallStream());
  }

  function scheduleMicrotask(fn) {
    if (typeof globalScope.queueMicrotask === 'function') {
      globalScope.queueMicrotask(fn);
    } else {
      Promise.resolve().then(fn);
    }
  }

  function unrefTimer(handle) {
    if (handle && typeof handle.unref === 'function') {
      try {
        handle.unref();
      } catch {}
    }
    return handle;
  }

  function maybeAutoStartRecording() {
    hydratePrefsFromStorage();
    if (!prefs.autoRecord) return;
    if (activeSession || startingPromise || autoStartQueued) return;
    if (!hasLiveWebRtcCallStream()) return;

    autoStartQueued = true;
    scheduleMicrotask(() => {
      autoStartQueued = false;
      if (!prefs.autoRecord || activeSession || startingPromise) return;
      if (!hasLiveWebRtcCallStream()) return;
      startRecording({ auto: true }).catch(() => {});
    });
  }

  function evaluateCallLifecycleAfterTrackLoss(reason) {
    pruneDeadTracks();
    const openPcs = countOpenPeerConnections();
    const hasRemote = remoteTracks.size > 0;

    if (openPcs === 0 && !hasRemote) {
      callState.signalingActive = false;
      if (activeSession && activeSession.sourceType === 'webrtc') {
        stopRecording(reason).catch(() => {});
        return;
      }
      if (startingPromise && (reason === 'pc-close' || reason === 'child-unload')) {
        startingPromise
          .then(() => {
            if (
              activeSession &&
              activeSession.sourceType === 'webrtc' &&
              countOpenPeerConnections() === 0
            ) {
              return stopRecording(reason);
            }
            return null;
          })
          .catch(() => {});
      }
    }
    renderAllDeckUi();
  }

  function handleTrackEnded(track) {
    const key = getTrackKey(track);
    if (key) {
      remoteTracks.delete(key);
      localTracks.delete(key);
    }
    evaluateCallLifecycleAfterTrackLoss('track-ended');
  }

  function registerExternalTrack(track, role, stream, pc, ownerWin, sourceTag) {
    if (!track || typeof track !== 'object') return;
    if (internalCaptureDepth > 0) return;
    if (
      track.__olazRecorderInternal ||
      track.__olazInternalThumb ||
      (stream && (stream.__olazRecorderInternal || stream.__olazInternalThumb))
    ) {
      return;
    }
    const kind = String(track.kind || '').toLowerCase();
    if (kind !== 'audio' && kind !== 'video') return;
    if (!isTrackLive(track)) return;

    const key = getTrackKey(track);
    if (!key) return;

    if (!track.__olazEndBound) {
      track.__olazEndBound = true;
      if (typeof track.addEventListener === 'function') {
        try {
          track.addEventListener('ended', () => handleTrackEnded(track));
        } catch {}
      }
    }

    const isRemote = role === 'remote';
    const targetMap = isRemote ? remoteTracks : localTracks;
    const wasEmptyCall = !isCallCurrentlyDetected();

    const existing = targetMap.get(key);
    const entry = {
      key,
      track,
      kind,
      stream: stream || (existing && existing.stream) || null,
      source: sourceTag || (isRemote ? 'rtc-remote' : 'rtc-local'),
      pc: pc || (existing && existing.pc) || null,
      ownerWin: ownerWin || (existing && existing.ownerWin) || globalScope,
    };
    targetMap.set(key, entry);

    advanceCaptureCursorWindow(isRemote ? 5 : 3);

    if (wasEmptyCall && isCallCurrentlyDetected()) {
      callState.dismissed = false;
      callState.lastError = '';
      if (!callState.partnerName) {
        callState.partnerName = resolveFallbackDomPartnerName() || 'Zalo Call';
      }
    }

    if (kind === 'video' && (isRemote || pc)) {
      callState.isVideoCall = true;
    }

    // Hot-plug mid-call track into active recording mixer / compositor
    if (activeSession && !activeSession.stopped) {
      if (kind === 'audio' && typeof activeSession.attachAudioTrack === 'function') {
        activeSession.attachAudioTrack(track, isRemote ? 'remote' : 'local', entry.stream);
      } else if (
        kind === 'video' &&
        activeSession.mode === 'av' &&
        typeof activeSession.attachVideoTrack === 'function'
      ) {
        activeSession.attachVideoTrack(track, isRemote ? 'remote' : 'local', entry.stream);
      }
    } else if (isRemote || pc) {
      maybeAutoStartRecording();
    }

    renderAllDeckUi();
  }

  function scanPeerConnectionTracks(pc, ownerWin) {
    if (!pc || !isPeerConnectionOpen(pc)) return;
    try {
      if (typeof pc.getSenders === 'function') {
        const senders = pc.getSenders() || [];
        for (const s of senders) {
          if (s && s.track && isTrackLive(s.track)) {
            registerExternalTrack(s.track, 'local', null, pc, ownerWin, 'rtc-sender');
          }
        }
      }
    } catch {}
    try {
      if (typeof pc.getReceivers === 'function') {
        const receivers = pc.getReceivers() || [];
        for (const r of receivers) {
          const trk = r && r.track;
          if (!trk || !isTrackLive(trk)) continue;
          if (trk.muted === true) {
            if (!trk.__olazUnmuteBound && typeof trk.addEventListener === 'function') {
              trk.__olazUnmuteBound = true;
              try {
                trk.addEventListener(
                  'unmute',
                  () => {
                    if (isPeerConnectionOpen(pc) && isTrackLive(trk)) {
                      registerExternalTrack(trk, 'remote', null, pc, ownerWin, 'rtc-receiver');
                    }
                  },
                  { once: true }
                );
              } catch {}
            }
          } else {
            registerExternalTrack(trk, 'remote', null, pc, ownerWin, 'rtc-receiver');
          }
        }
      }
    } catch {}
  }

  function handlePeerConnectionClosed(pc) {
    if (!pc || pc.__olazClosed) return;
    pc.__olazClosed = true;
    trackedPeerConnections.delete(pc);
    reconcileCaptureEpochQuota('pc-close');

    const remainingOpenPcs = countOpenPeerConnections();
    for (const [k, entry] of Array.from(remoteTracks.entries())) {
      if (entry.pc === pc || (remainingOpenPcs === 0 && !entry.pc)) {
        remoteTracks.delete(k);
      }
    }
    for (const [k, entry] of Array.from(localTracks.entries())) {
      if (entry.pc === pc) {
        localTracks.delete(k);
      }
    }

    evaluateCallLifecycleAfterTrackLoss('pc-close');
  }

  function attachPeerConnection(pc, ownerWin) {
    if (!pc || typeof pc !== 'object' || trackedPeerConnections.has(pc)) return;
    trackedPeerConnections.add(pc);
    pc.__olazOwnerWin = ownerWin || globalScope;

    if (typeof pc.addEventListener === 'function') {
      try {
        pc.addEventListener('track', (ev) => {
          const primaryStream = ev && Array.isArray(ev.streams) && ev.streams[0] ? ev.streams[0] : null;
          if (ev && ev.track) {
            registerExternalTrack(ev.track, 'remote', primaryStream, pc, ownerWin, 'rtc-track');
          }
          if (ev && Array.isArray(ev.streams)) {
            for (const st of ev.streams) {
              if (st && typeof st.getTracks === 'function') {
                for (const t of st.getTracks()) {
                  registerExternalTrack(t, 'remote', st, pc, ownerWin, 'rtc-stream');
                }
              }
            }
          }
        });
      } catch {}

      const onConnChange = () => {
        const state = pc.connectionState || pc.iceConnectionState || '';
        if (state === 'connected' || state === 'completed') {
          scanPeerConnectionTracks(pc, ownerWin);
        } else if (state === 'closed' || state === 'failed') {
          handlePeerConnectionClosed(pc);
        } else if (state === 'disconnected') {
          unrefTimer(
            globalScope.setTimeout(() => {
              const latest = pc.connectionState || pc.iceConnectionState || '';
              if (
                pc.__olazClosed ||
                latest === 'closed' ||
                latest === 'failed' ||
                latest === 'disconnected'
              ) {
                handlePeerConnectionClosed(pc);
              }
            }, 3500)
          );
        }
      };

      try {
        pc.addEventListener('connectionstatechange', onConnChange);
        pc.addEventListener('iceconnectionstatechange', onConnChange);
      } catch {}
    }

    scanPeerConnectionTracks(pc, ownerWin);
  }

  function installCallStreamTracker(targetWin) {
    if (!targetWin || (typeof targetWin !== 'object' && typeof targetWin !== 'function')) {
      return;
    }
    if (targetWin.__olazCallTrackerInstalled) {
      return;
    }
    try {
      Object.defineProperty(targetWin, '__olazCallTrackerInstalled', {
        value: true,
        configurable: true,
      });
    } catch {
      targetWin.__olazCallTrackerInstalled = true;
    }

    reconcileCaptureEpochQuota('tracker-install');

    // If tracking a child popup window, clean up its tracks if the child window closes
    if (targetWin !== globalScope && typeof targetWin.addEventListener === 'function') {
      const onChildUnload = () => {
        for (const pc of Array.from(trackedPeerConnections)) {
          if (pc.__olazOwnerWin === targetWin) {
            handlePeerConnectionClosed(pc);
          }
        }
        for (const [k, entry] of Array.from(remoteTracks.entries())) {
          if (entry.ownerWin === targetWin) remoteTracks.delete(k);
        }
        for (const [k, entry] of Array.from(localTracks.entries())) {
          if (entry.ownerWin === targetWin) localTracks.delete(k);
        }
        evaluateCallLifecycleAfterTrackLoss('child-unload');
      };
      try {
        targetWin.addEventListener('pagehide', onChildUnload);
        targetWin.addEventListener('beforeunload', onChildUnload);
      } catch {}
    }

    // 1. Wrap `RTCPeerConnection` + prototype `.addTrack`, `.addTransceiver`, `.close`
    if (
      typeof targetWin.RTCPeerConnection === 'function' &&
      !targetWin.RTCPeerConnection.__olazWrapped
    ) {
      const NativePC = targetWin.RTCPeerConnection;
      const proto = NativePC.prototype;

      if (proto) {
        if (typeof proto.addTrack === 'function' && !proto.addTrack.__olazWrapped) {
          const origAddTrack = proto.addTrack;
          const wrappedAddTrack = function (track, ...streams) {
            attachPeerConnection(this, targetWin);
            const sender = origAddTrack.apply(this, arguments);
            if (internalCaptureDepth === 0 && track) {
              registerExternalTrack(
                track,
                'local',
                streams && streams[0] ? streams[0] : null,
                this,
                targetWin,
                'rtc-addTrack'
              );
            }
            return sender;
          };
          wrappedAddTrack.__olazWrapped = true;
          proto.addTrack = wrappedAddTrack;
        }

        if (typeof proto.addTransceiver === 'function' && !proto.addTransceiver.__olazWrapped) {
          const origAddTransceiver = proto.addTransceiver;
          const wrappedAddTransceiver = function (trackOrKind, init) {
            attachPeerConnection(this, targetWin);
            const transceiver = origAddTransceiver.apply(this, arguments);
            if (internalCaptureDepth === 0) {
              const initStream =
                init && Array.isArray(init.streams) && init.streams[0] ? init.streams[0] : null;
              if (trackOrKind && typeof trackOrKind === 'object') {
                registerExternalTrack(
                  trackOrKind,
                  'local',
                  initStream,
                  this,
                  targetWin,
                  'rtc-transceiver'
                );
              } else if (transceiver && transceiver.sender && transceiver.sender.track) {
                registerExternalTrack(
                  transceiver.sender.track,
                  'local',
                  initStream,
                  this,
                  targetWin,
                  'rtc-transceiver'
                );
              }
            }
            return transceiver;
          };
          wrappedAddTransceiver.__olazWrapped = true;
          proto.addTransceiver = wrappedAddTransceiver;
        }

        if (typeof proto.close === 'function' && !proto.close.__olazWrapped) {
          const origClose = proto.close;
          const wrappedClose = function () {
            try {
              return origClose.apply(this, arguments);
            } finally {
              handlePeerConnectionClosed(this);
            }
          };
          wrappedClose.__olazWrapped = true;
          proto.close = wrappedClose;
        }
      }

      if (typeof Proxy === 'function') {
        const ProxiedPC = new Proxy(NativePC, {
          construct(target, argArray, newTarget) {
            const pc = Reflect.construct(target, argArray, newTarget);
            attachPeerConnection(pc, targetWin);
            return pc;
          },
          get(target, prop, receiver) {
            if (prop === '__olazWrapped') return true;
            return Reflect.get(target, prop, receiver);
          },
        });
        try {
          targetWin.RTCPeerConnection = ProxiedPC;
        } catch {}
      }
    }

    // 2. Wrap `RTCRtpSender.prototype.replaceTrack` (mid-call camera / screen-share switch)
    if (
      targetWin.RTCRtpSender &&
      targetWin.RTCRtpSender.prototype &&
      typeof targetWin.RTCRtpSender.prototype.replaceTrack === 'function' &&
      !targetWin.RTCRtpSender.prototype.replaceTrack.__olazWrapped
    ) {
      const origReplaceTrack = targetWin.RTCRtpSender.prototype.replaceTrack;
      const wrappedReplaceTrack = function (newTrack) {
        const p = origReplaceTrack.apply(this, arguments);
        if (internalCaptureDepth === 0 && newTrack) {
          registerExternalTrack(newTrack, 'local', null, null, targetWin, 'rtc-replaceTrack');
        }
        return p;
      };
      wrappedReplaceTrack.__olazWrapped = true;
      targetWin.RTCRtpSender.prototype.replaceTrack = wrappedReplaceTrack;
    }

    // 3. Wrap `navigator.mediaDevices.getUserMedia` & `getDisplayMedia`
    const md = targetWin.navigator && targetWin.navigator.mediaDevices;
    if (md && typeof md === 'object') {
      if (typeof md.getUserMedia === 'function' && !md.getUserMedia.__olazWrapped) {
        const origGum = md.getUserMedia;
        const wrappedGum = function (constraints) {
          const isExternal = internalCaptureDepth === 0;
          const p = origGum.apply(this, arguments);
          if (isExternal && p && typeof p.then === 'function') {
            p.then((stream) => {
              if (
                stream &&
                !stream.__olazRecorderInternal &&
                typeof stream.getTracks === 'function'
              ) {
                for (const t of stream.getTracks()) {
                  registerExternalTrack(t, 'local', stream, null, targetWin, 'gum');
                }
              }
            }).catch(() => {});
          }
          return p;
        };
        wrappedGum.__olazWrapped = true;
        try {
          md.getUserMedia = wrappedGum;
        } catch {}
      }

      if (typeof md.getDisplayMedia === 'function' && !md.getDisplayMedia.__olazWrapped) {
        const origGdm = md.getDisplayMedia;
        const wrappedGdm = function (constraints) {
          const isExternal = internalCaptureDepth === 0;
          const p = origGdm.apply(this, arguments);
          if (isExternal && p && typeof p.then === 'function') {
            p.then((stream) => {
              if (
                stream &&
                !stream.__olazRecorderInternal &&
                typeof stream.getTracks === 'function'
              ) {
                for (const t of stream.getTracks()) {
                  registerExternalTrack(t, 'local', stream, null, targetWin, 'gdm');
                }
              }
            }).catch(() => {});
          }
          return p;
        };
        wrappedGdm.__olazWrapped = true;
        try {
          md.getDisplayMedia = wrappedGdm;
        } catch {}
      }
    }

    // 4. Wrap `HTMLMediaElement.prototype.srcObject` setter
    const MediaElemProto =
      targetWin.HTMLMediaElement && targetWin.HTMLMediaElement.prototype;
    if (MediaElemProto && !MediaElemProto.__olazSrcObjectWrapped) {
      let desc = null;
      let protoCursor = MediaElemProto;
      while (protoCursor && !desc) {
        try {
          desc = Object.getOwnPropertyDescriptor(protoCursor, 'srcObject');
        } catch {}
        protoCursor = Object.getPrototypeOf(protoCursor);
      }

      const handleAssignedStream = (el, val) => {
        if (!val || typeof val !== 'object') return;
        const isStream =
          typeof val.getTracks === 'function' ||
          (typeof targetWin.MediaStream === 'function' && val instanceof targetWin.MediaStream);
        if (!isStream) return;
        const isInternal = Boolean(
          el.__olazInternalThumb ||
            el.__olazRecorderInternal ||
            val.__olazInternalThumb ||
            val.__olazRecorderInternal ||
            (el.dataset && el.dataset.olazRecorderInternal === 'true')
        );
        if (isInternal) return;

        try {
          if (!el.dataset) el.dataset = {};
          el.dataset.olazIgnoreMedia = 'true';
        } catch {}
        try {
          if (typeof el.setAttribute === 'function') {
            el.setAttribute('data-olaz-ignore-media', 'true');
          }
        } catch {}

        if (typeof val.getTracks === 'function') {
          const isSelfPreview =
            Boolean(el.muted) && String(el.tagName || '').toUpperCase() === 'VIDEO';
          for (const t of val.getTracks()) {
            const key = getTrackKey(t);
            const knownLocal = key && localTracks.has(key);
            const knownRemote = key && remoteTracks.has(key);
            const role = knownLocal
              ? 'local'
              : knownRemote
                ? 'remote'
                : isSelfPreview
                  ? 'local'
                  : 'remote';
            registerExternalTrack(t, role, val, null, targetWin, 'media-el');
          }
        }
        if (!val.__olazAddTrackBound && typeof val.addEventListener === 'function') {
          val.__olazAddTrackBound = true;
          try {
            val.addEventListener('addtrack', (ev) => {
              if (ev && ev.track) {
                registerExternalTrack(ev.track, 'remote', val, null, targetWin, 'media-el');
              }
            });
          } catch {}
        }
      };

      try {
        Object.defineProperty(MediaElemProto, '__olazSrcObjectWrapped', {
          value: true,
          configurable: true,
        });
        if (desc && typeof desc.set === 'function') {
          Object.defineProperty(MediaElemProto, 'srcObject', {
            configurable: true,
            enumerable: Boolean(desc.enumerable),
            get() {
              return desc.get ? desc.get.call(this) : undefined;
            },
            set(val) {
              try {
                handleAssignedStream(this, val);
              } catch {}
              return desc.set.call(this, val);
            },
          });
        } else {
          Object.defineProperty(MediaElemProto, 'srcObject', {
            configurable: true,
            enumerable: true,
            get() {
              return this.__olazFallbackSrcObject || null;
            },
            set(val) {
              this.__olazFallbackSrcObject = val;
              try {
                handleAssignedStream(this, val);
              } catch {}
            },
          });
        }
      } catch {}
    }
  }

  // ==========================================================================
  // 3. `$zcall` IPC Signaling Hooks (`host.asyncIpcHandlers`)
  // ==========================================================================

  function parseJsonIfString(raw) {
    if (!raw) return null;
    if (typeof raw === 'object') return raw;
    if (typeof raw === 'string') {
      const s = raw.trim();
      if (s.startsWith('{') || s.startsWith('[')) {
        try {
          return JSON.parse(s);
        } catch {}
      }
    }
    return null;
  }

  function extractSignalingPartnerName(data, fallbackMsg) {
    const d = data && typeof data === 'object' ? data : {};
    const partnerArr = Array.isArray(d.partner) ? d.partner : [];
    const p0 =
      partnerArr[0] || (d.partner && typeof d.partner === 'object' ? d.partner : null);
    const callerObj = d.caller && typeof d.caller === 'object' ? d.caller : null;
    const grpObj = d.groupInfo && typeof d.groupInfo === 'object' ? d.groupInfo : null;

    const rawName =
      (p0 && (p0.name || p0.dName || p0.displayName || p0.zaloName)) ||
      (grpObj && (grpObj.name || grpObj.groupName || grpObj.dName)) ||
      (callerObj && (callerObj.name || callerObj.dName || callerObj.displayName)) ||
      d.partnerName ||
      d.callerName ||
      d.fromDName ||
      d.groupName ||
      d.dName ||
      d.name ||
      (fallbackMsg && (fallbackMsg.partnerName || fallbackMsg.name)) ||
      '';

    return cleanPartnerName(rawName) || resolveFallbackDomPartnerName() || 'Zalo Call';
  }

  function notifyCallSignaling(rawPayload) {
    const msg = parseJsonIfString(rawPayload);
    if (!msg || typeof msg !== 'object') return;

    reconcileCaptureEpochQuota('zcall-signal');

    // Direct simplified notification object (`{ action: 'start' | 'end', partnerName, isVideo }`)
    if (msg.action === 'start') {
      callState.signalingActive = true;
      callState.dismissed = false;
      callState.lastError = '';
      callState.partnerName = cleanPartnerName(msg.partnerName) || callState.partnerName || 'Zalo Call';
      callState.isVideoCall = Boolean(msg.isVideo);
      renderAllDeckUi();
      return;
    }
    if (msg.action === 'end') {
      callState.signalingActive = false;
      if (activeSession) {
        stopRecording('signaling-end').catch(() => {});
      } else {
        renderAllDeckUi();
      }
      return;
    }

    const type = String(msg.type || '');
    const command = String(msg.command || msg.cmd || '');
    const parsedData = parseJsonIfString(msg.data);
    const data = parsedData && typeof parsedData === 'object' ? parsedData : msg;

    // Outgoing call initiation: `makeCall` | `startGroupCall` | `joinGroupCall`
    if (
      type === 'request' &&
      (command === 'makeCall' || command === 'startGroupCall' || command === 'joinGroupCall')
    ) {
      const partnerName = extractSignalingPartnerName(data, msg);
      const numType = Number(data.type);
      const isVideo =
        numType === 2 || numType === 3 || numType === 6 || Boolean(data.isVideo);

      callState.signalingActive = true;
      callState.partnerName = partnerName;
      callState.isVideoCall = isVideo;
      callState.dismissed = false;
      callState.lastError = '';
      renderAllDeckUi();
      return;
    }

    // Explicit call termination: `type === 'request' && command === 'endCall'`
    if (type === 'request' && command === 'endCall') {
      callState.signalingActive = false;
      if (activeSession) {
        stopRecording('endCall').catch(() => {});
      } else {
        renderAllDeckUi();
      }
      return;
    }

    // Incoming VoIP control events: `type === 'control' && data?.act_type === 'voip'`
    if (type === 'control' && data && data.act_type === 'voip') {
      const actCandidates = [
        data.act,
        data.action,
        data.sub_type,
        data.subType,
        data.event,
        data.cmd,
        data.type,
        msg.act,
        msg.command,
      ]
        .filter((v) => typeof v === 'string')
        .map((v) => v.toLowerCase());

      const isIncomingReq = actCandidates.some(
        (a) => a === 'request' || a === 'group_request'
      );
      const isEndControl = actCandidates.some(
        (a) => a === 'end_call' || a === 'cancel' || a === 'endcall'
      );

      if (isIncomingReq) {
        const partnerName = extractSignalingPartnerName(data, msg);
        const callTypeNum = Number(data.callType !== undefined ? data.callType : data.type);
        const isVideo =
          callTypeNum === 2 ||
          callTypeNum === 3 ||
          callTypeNum === 6 ||
          Boolean(data.isVideo);

        callState.signalingActive = true;
        callState.partnerName = partnerName;
        callState.isVideoCall = isVideo;
        callState.dismissed = false;
        callState.lastError = '';
        renderAllDeckUi();
      } else if (isEndControl) {
        callState.signalingActive = false;
        if (activeSession && (activeSession.autoStarted || activeSession.sourceType === 'webrtc')) {
          stopRecording('voip-end').catch(() => {});
        } else {
          renderAllDeckUi();
        }
      }
    }
  }

  async function enumerateCallDevices() {
    const empty = { audioInput: [], audioOutput: [], videoInput: [] };
    const md = globalScope.navigator && globalScope.navigator.mediaDevices;
    if (!md || typeof md.enumerateDevices !== 'function') {
      return empty;
    }
    try {
      const list = await md.enumerateDevices();
      if (!Array.isArray(list)) return empty;
      const out = { audioInput: [], audioOutput: [], videoInput: [] };
      for (const dev of list) {
        if (!dev || typeof dev !== 'object') continue;
        const item = {
          deviceId: String(dev.deviceId || 'default'),
          groupId: String(dev.groupId || ''),
          kind: String(dev.kind || ''),
          label: String(dev.label || ''),
        };
        if (dev.kind === 'audioinput') {
          out.audioInput.push(item);
        } else if (dev.kind === 'audiooutput') {
          out.audioOutput.push(item);
        } else if (dev.kind === 'videoinput') {
          out.videoInput.push(item);
        }
      }
      return out;
    } catch {
      return empty;
    }
  }

  function registerZcallIpcHooks() {
    if (!host.asyncIpcHandlers || typeof host.asyncIpcHandlers.set !== 'function') {
      return;
    }

    const prevCallReq = host.asyncIpcHandlers.get('call-request');
    host.asyncIpcHandlers.set('call-request', async (args) => {
      const op = args && args[0];
      if (op === 'listDevice') {
        return enumerateCallDevices();
      }
      if (op && typeof op === 'object') {
        notifyCallSignaling(op);
      }
      return typeof prevCallReq === 'function' ? prevCallReq(args) : null;
    });

    for (const ch of ['call-init', 'call-send-to-native']) {
      const prevHandler = host.asyncIpcHandlers.get(ch);
      host.asyncIpcHandlers.set(ch, async (args) => {
        if (args && args.length > 0) {
          notifyCallSignaling(args[0]);
        }
        return typeof prevHandler === 'function' ? prevHandler(args) : undefined;
      });
    }
  }

  // ==========================================================================
  // 4. Slot-Isolated IndexedDB (`olaz_call_recordings_v1`) & Download Engine
  // ==========================================================================

  function openRecordingsDb() {
    return new Promise((resolve) => {
      const idb = globalScope.indexedDB;
      if (!idb || typeof idb.open !== 'function') {
        resolve(null);
        return;
      }
      let req;
      try {
        req = idb.open(IDB_NAME, 1);
      } catch {
        resolve(null);
        return;
      }
      req.onupgradeneeded = () => {
        try {
          const db = req.result;
          if (db && db.objectStoreNames && !db.objectStoreNames.contains(IDB_STORE)) {
            db.createObjectStore(IDB_STORE, { keyPath: 'id' });
          }
        } catch {}
      };
      req.onsuccess = () => resolve(req.result || null);
      req.onerror = () => resolve(null);
    });
  }

  function idbReqPromise(req) {
    return new Promise((resolve) => {
      if (!req) {
        resolve(undefined);
        return;
      }
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => resolve(undefined);
    });
  }

  function stripBlobMeta(rec) {
    return {
      id: rec.id,
      slot: rec.slot || ACTIVE_SLOT,
      fileName: rec.fileName,
      partnerName: rec.partnerName || 'Zalo Call',
      mode: rec.mode || 'av',
      mimeType: rec.mimeType || 'video/webm',
      size: Number(rec.size) || 0,
      durationMs: Number(rec.durationMs) || 0,
      createdAt: Number(rec.createdAt) || 0,
      endedAt: Number(rec.endedAt) || 0,
    };
  }

  function sortRecordingsDesc(list) {
    return list.slice().sort((a, b) => {
      const dt = (Number(b.createdAt) || 0) - (Number(a.createdAt) || 0);
      if (dt !== 0) return dt;
      return String(b.id || '').localeCompare(String(a.id || ''));
    });
  }

  async function persistRecordingToIdb(record) {
    reconcileCaptureEpochQuota('idb-save:' + record.fileName);

    // Mirror in bounded in-RAM fallback list
    ramRecordings.unshift(record);
    if (ramRecordings.length > MAX_SAVED_PER_SLOT) {
      ramRecordings.length = MAX_SAVED_PER_SLOT;
    }

    const db = await openRecordingsDb();
    if (!db) {
      cachedRecordingsMeta = sortRecordingsDesc(ramRecordings).map(stripBlobMeta);
      renderAllDeckUi();
      return stripBlobMeta(record);
    }

    try {
      const putTx = db.transaction(IDB_STORE, 'readwrite');
      const putStore = putTx.objectStore(IDB_STORE);
      await idbReqPromise(putStore.put(record));

      const readTx = db.transaction(IDB_STORE, 'readonly');
      const allRows = (await idbReqPromise(readTx.objectStore(IDB_STORE).getAll())) || [];
      const slotRows = sortRecordingsDesc(
        allRows.filter((r) => r && (!r.slot || r.slot === ACTIVE_SLOT))
      );

      if (slotRows.length > MAX_SAVED_PER_SLOT) {
        const overflow = slotRows.slice(MAX_SAVED_PER_SLOT);
        const pruneTx = db.transaction(IDB_STORE, 'readwrite');
        const pruneStore = pruneTx.objectStore(IDB_STORE);
        for (const stale of overflow) {
          await idbReqPromise(pruneStore.delete(stale.id));
        }
        slotRows.length = MAX_SAVED_PER_SLOT;
      }

      cachedRecordingsMeta = slotRows.map(stripBlobMeta);
    } catch {
      cachedRecordingsMeta = sortRecordingsDesc(ramRecordings).map(stripBlobMeta);
    } finally {
      try {
        db.close();
      } catch {}
    }

    renderAllDeckUi();
    return stripBlobMeta(record);
  }

  async function listRecordings() {
    const db = await openRecordingsDb();
    if (!db) {
      cachedRecordingsMeta = sortRecordingsDesc(ramRecordings).map(stripBlobMeta);
      return cachedRecordingsMeta.slice();
    }
    try {
      const tx = db.transaction(IDB_STORE, 'readonly');
      const rows = (await idbReqPromise(tx.objectStore(IDB_STORE).getAll())) || [];
      const slotRows = sortRecordingsDesc(
        rows.filter((r) => r && (!r.slot || r.slot === ACTIVE_SLOT))
      );
      cachedRecordingsMeta = slotRows.slice(0, MAX_SAVED_PER_SLOT).map(stripBlobMeta);
      return cachedRecordingsMeta.slice();
    } catch {
      cachedRecordingsMeta = sortRecordingsDesc(ramRecordings).map(stripBlobMeta);
      return cachedRecordingsMeta.slice();
    } finally {
      try {
        db.close();
      } catch {}
    }
  }

  function triggerBrowserBlobDownload(blob, fileName) {
    const doc = globalScope.document;
    if (!blob || !doc || typeof doc.createElement !== 'function') {
      return false;
    }
    const safeName = String(fileName || 'call-recording.webm');
    const urlApi = globalScope.URL || (typeof URL !== 'undefined' ? URL : null);
    let objUrl = '';
    let shouldRevoke = false;

    try {
      if (urlApi && typeof urlApi.createObjectURL === 'function') {
        objUrl = urlApi.createObjectURL(blob);
        shouldRevoke = true;
      }
    } catch {}

    if (!objUrl) {
      objUrl = 'blob:olaz-call/' + encodeURIComponent(safeName);
    }

    const anchor = doc.createElement('a');
    anchor.href = objUrl;
    anchor.download = safeName;
    anchor.rel = 'noopener';
    if (anchor.style) {
      anchor.style.display = 'none';
    }
    if (typeof anchor.setAttribute === 'function') {
      anchor.setAttribute('href', objUrl);
      anchor.setAttribute('download', safeName);
    }

    const container = doc.body || doc.documentElement;
    if (container && typeof container.appendChild === 'function') {
      container.appendChild(anchor);
    }
    try {
      if (typeof anchor.click === 'function') {
        anchor.click();
      }
    } catch {}
    try {
      if (typeof anchor.remove === 'function') {
        anchor.remove();
      }
    } catch {}

    if (shouldRevoke && urlApi && typeof urlApi.revokeObjectURL === 'function') {
      unrefTimer(
        globalScope.setTimeout(() => {
          try {
            urlApi.revokeObjectURL(objUrl);
          } catch {}
        }, 30000)
      );
    }
    return true;
  }

  async function downloadRecording(id) {
    let target = null;
    const db = await openRecordingsDb();
    if (db) {
      try {
        const tx = db.transaction(IDB_STORE, 'readonly');
        const store = tx.objectStore(IDB_STORE);
        if (id) {
          target = await idbReqPromise(store.get(String(id)));
        } else {
          const all = (await idbReqPromise(store.getAll())) || [];
          const sorted = sortRecordingsDesc(
            all.filter((r) => r && (!r.slot || r.slot === ACTIVE_SLOT))
          );
          target = sorted[0] || null;
        }
      } catch {}
      try {
        db.close();
      } catch {}
    }

    if (!target) {
      target = id
        ? ramRecordings.find((r) => r.id === id) || null
        : ramRecordings[0] || null;
    }

    if (!target || !target.blob) {
      return false;
    }
    return triggerBrowserBlobDownload(target.blob, target.fileName);
  }

  async function deleteRecording(id) {
    if (!id) return false;
    const key = String(id);
    const ramIdx = ramRecordings.findIndex((r) => r.id === key);
    if (ramIdx >= 0) {
      ramRecordings.splice(ramIdx, 1);
    }

    const db = await openRecordingsDb();
    if (db) {
      try {
        const tx = db.transaction(IDB_STORE, 'readwrite');
        await idbReqPromise(tx.objectStore(IDB_STORE).delete(key));
      } catch {}
      try {
        db.close();
      } catch {}
    }

    await listRecordings();
    renderAllDeckUi();
    return true;
  }

  async function clearRecordings() {
    let removed = ramRecordings.length;
    ramRecordings.length = 0;

    const db = await openRecordingsDb();
    if (db) {
      try {
        const readTx = db.transaction(IDB_STORE, 'readonly');
        const rows = (await idbReqPromise(readTx.objectStore(IDB_STORE).getAll())) || [];
        const slotRows = rows.filter((r) => r && (!r.slot || r.slot === ACTIVE_SLOT));
        removed = Math.max(removed, slotRows.length);
        const delTx = db.transaction(IDB_STORE, 'readwrite');
        const store = delTx.objectStore(IDB_STORE);
        for (const r of slotRows) {
          await idbReqPromise(store.delete(r.id));
        }
      } catch {}
      try {
        db.close();
      } catch {}
    }

    cachedRecordingsMeta = [];
    renderAllDeckUi();
    return removed;
  }

  // ==========================================================================
  // 5. Web Audio 48 kHz Mixer + 30 fps `<canvas>` Compositor + `MediaRecorder`
  // ==========================================================================

  function createSingleTrackStream(track, fallbackStream) {
    if (typeof globalScope.MediaStream === 'function') {
      try {
        const s = new globalScope.MediaStream([track]);
        s.__olazRecorderInternal = true;
        return s;
      } catch {}
    }
    if (fallbackStream && typeof fallbackStream === 'object') {
      return fallbackStream;
    }
    return {
      __olazRecorderInternal: true,
      id: 'synthetic_stream_' + getTrackKey(track),
      getTracks: () => [track],
      getAudioTracks: () => (track && track.kind === 'audio' ? [track] : []),
      getVideoTracks: () => (track && track.kind === 'video' ? [track] : []),
    };
  }

  function createCompositeStream(tracks) {
    if (typeof globalScope.MediaStream === 'function') {
      try {
        const s = new globalScope.MediaStream(tracks);
        s.__olazRecorderInternal = true;
        return s;
      } catch {}
    }
    return {
      __olazRecorderInternal: true,
      id: 'composite_stream_' + Date.now(),
      getTracks: () => tracks.slice(),
      getAudioTracks: () => tracks.filter((t) => t && t.kind === 'audio'),
      getVideoTracks: () => tracks.filter((t) => t && t.kind === 'video'),
    };
  }

  function setAudioParamSafe(param, val, nowSec) {
    if (!param) return;
    try {
      if (typeof param.setValueAtTime === 'function') {
        param.setValueAtTime(val, Number(nowSec) || 0);
        return;
      }
    } catch {}
    try {
      param.value = val;
    } catch {}
  }

  function selectSupportedMime(MediaRecorderCtor, hasVideo) {
    const candidates = hasVideo ? AV_MIME_PRIORITY : AUDIO_MIME_PRIORITY;
    if (MediaRecorderCtor && typeof MediaRecorderCtor.isTypeSupported === 'function') {
      for (const mime of candidates) {
        try {
          if (MediaRecorderCtor.isTypeSupported(mime)) {
            return mime;
          }
        } catch {}
      }
    }
    return candidates[0];
  }

  function startBackgroundSafeTicker(onTick) {
    let worker = null;
    let workerUrl = null;
    let fallbackInterval = null;

    const startFallback = () => {
      if (fallbackInterval) return;
      fallbackInterval = unrefTimer(
        globalScope.setInterval(() => {
          try {
            onTick();
          } catch {}
        }, 33)
      );
    };

    try {
      const urlApi = globalScope.URL;
      if (
        typeof globalScope.Blob === 'function' &&
        urlApi &&
        typeof urlApi.createObjectURL === 'function' &&
        typeof globalScope.Worker === 'function'
      ) {
        const workerCode =
          'var iv=null;self.onmessage=function(e){if(e.data==="start"){if(iv)clearInterval(iv);iv=setInterval(function(){self.postMessage(1);},33);}else if(e.data==="stop"){if(iv){clearInterval(iv);iv=null;}self.close();}};';
        const blob = new globalScope.Blob([workerCode], { type: 'application/javascript' });
        workerUrl = urlApi.createObjectURL(blob);
        worker = new globalScope.Worker(workerUrl);
        worker.onmessage = () => {
          try {
            onTick();
          } catch {}
        };
        worker.onerror = () => {
          startFallback();
        };
        if (typeof worker.postMessage === 'function') {
          worker.postMessage('start');
        }
        return () => {
          if (fallbackInterval) {
            globalScope.clearInterval(fallbackInterval);
            fallbackInterval = null;
          }
          if (worker) {
            try {
              if (typeof worker.postMessage === 'function') worker.postMessage('stop');
              if (typeof worker.terminate === 'function') worker.terminate();
            } catch {}
            worker = null;
          }
          if (workerUrl && urlApi && typeof urlApi.revokeObjectURL === 'function') {
            try {
              urlApi.revokeObjectURL(workerUrl);
            } catch {}
            workerUrl = null;
          }
        };
      }
    } catch {
      startFallback();
    }

    startFallback();
    return () => {
      if (fallbackInterval) {
        globalScope.clearInterval(fallbackInterval);
        fallbackInterval = null;
      }
    };
  }

  function hasDecodedVideoFrame(videoEl) {
    if (!videoEl) return false;
    const vw = Number(videoEl.videoWidth) || 0;
    const vh = Number(videoEl.videoHeight) || 0;
    if (vw <= 0 || vh <= 0) return false;
    if (videoEl.readyState !== undefined && Number(videoEl.readyState) < 2) {
      return false;
    }
    return true;
  }

  function drawVideoContain(ctx2d, videoEl, x, y, w, h) {
    if (!ctx2d || typeof ctx2d.drawImage !== 'function') return;
    const vw = Math.max(1, Number(videoEl.videoWidth) || w);
    const vh = Math.max(1, Number(videoEl.videoHeight) || h);
    const scale = Math.min(w / vw, h / vh);
    const dw = Math.round(vw * scale);
    const dh = Math.round(vh * scale);
    const dx = x + Math.round((w - dw) / 2);
    const dy = y + Math.round((h - dh) / 2);
    ctx2d.drawImage(videoEl, dx, dy, dw, dh);
  }

  function drawAvatarFallbackTile(ctx2d, x, y, w, h, partnerName, elapsedMs, showRecClock) {
    if (!ctx2d) return;
    const cx = x + w / 2;
    const cy = y + h / 2 - (showRecClock ? 20 : 0);
    const radius = Math.max(28, Math.min(68, Math.round(Math.min(w, h) * 0.14)));

    if (typeof ctx2d.beginPath === 'function' && typeof ctx2d.arc === 'function') {
      ctx2d.beginPath();
      ctx2d.arc(cx, cy, radius, 0, Math.PI * 2);
      ctx2d.fillStyle = '#1e293b';
      if (typeof ctx2d.fill === 'function') ctx2d.fill();
      ctx2d.lineWidth = 2;
      ctx2d.strokeStyle = 'rgba(56, 189, 248, 0.45)';
      if (typeof ctx2d.stroke === 'function') ctx2d.stroke();
    }

    if (typeof ctx2d.fillText === 'function') {
      ctx2d.textAlign = 'center';
      ctx2d.textBaseline = 'middle';
      ctx2d.fillStyle = '#f8fafc';
      ctx2d.font = '700 ' + Math.round(radius * 0.64) + 'px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';
      ctx2d.fillText(getPartnerInitials(partnerName), cx, cy);

      ctx2d.fillStyle = '#e2e8f0';
      ctx2d.font = '600 22px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';
      ctx2d.fillText(partnerName || 'Zalo Call', cx, cy + radius + 34);

      if (showRecClock) {
        ctx2d.fillStyle = '#f87171';
        ctx2d.font = '600 17px -apple-system, BlinkMacSystemFont, "Segoe UI", monospace';
        ctx2d.fillText('\u25CF REC ' + formatDurationClock(elapsedMs), cx, cy + radius + 66);
      }
    }
  }

  function drawRoundedPip(ctx2d, videoEl) {
    if (!ctx2d || typeof ctx2d.drawImage !== 'function') return;
    const x = CANVAS_WIDTH - PIP_WIDTH - PIP_MARGIN;
    const y = CANVAS_HEIGHT - PIP_HEIGHT - PIP_MARGIN;
    const w = PIP_WIDTH;
    const h = PIP_HEIGHT;
    const r = 14;

    const traceRoundedRect = () => {
      ctx2d.beginPath();
      ctx2d.moveTo(x + r, y);
      ctx2d.lineTo(x + w - r, y);
      ctx2d.quadraticCurveTo(x + w, y, x + w, y + r);
      ctx2d.lineTo(x + w, y + h - r);
      ctx2d.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
      ctx2d.lineTo(x + r, y + h);
      ctx2d.quadraticCurveTo(x, y + h, x, y + h - r);
      ctx2d.lineTo(x, y + r);
      ctx2d.quadraticCurveTo(x, y, x + r, y);
      ctx2d.closePath();
    };

    if (
      typeof ctx2d.save === 'function' &&
      typeof ctx2d.beginPath === 'function' &&
      typeof ctx2d.quadraticCurveTo === 'function' &&
      typeof ctx2d.clip === 'function' &&
      typeof ctx2d.restore === 'function'
    ) {
      ctx2d.save();
      traceRoundedRect();
      ctx2d.clip();
      ctx2d.fillStyle = '#020617';
      if (typeof ctx2d.fillRect === 'function') ctx2d.fillRect(x, y, w, h);
      drawVideoContain(ctx2d, videoEl, x, y, w, h);
      ctx2d.restore();

      traceRoundedRect();
      ctx2d.lineWidth = 2;
      ctx2d.strokeStyle = 'rgba(148, 163, 184, 0.5)';
      if (typeof ctx2d.stroke === 'function') ctx2d.stroke();
    } else {
      drawVideoContain(ctx2d, videoEl, x, y, w, h);
    }
  }

  function getSessionElapsedMs(session) {
    if (!session) return 0;
    const refNow = session.paused && session.pausedAt ? session.pausedAt : Date.now();
    return Math.max(0, refNow - session.startedAt - session.pausedAccumMs);
  }

  function stopOwnedStreamTracks(streamList) {
    for (const s of streamList) {
      if (!s || typeof s.getTracks !== 'function') continue;
      try {
        for (const t of s.getTracks()) {
          if (t && typeof t.stop === 'function') {
            t.stop();
          }
        }
      } catch {}
    }
  }

  async function startRecording(opts) {
    const options = opts && typeof opts === 'object' ? opts : {};
    if (activeSession && !activeSession.stopped) {
      return {
        ok: true,
        alreadyRecording: true,
        fileName: activeSession.fileName,
        mode: activeSession.mode,
        mimeType: activeSession.mimeType,
      };
    }
    if (startingPromise) {
      return startingPromise;
    }

    const MediaRecorderCtor = globalScope.MediaRecorder;
    if (typeof MediaRecorderCtor !== 'function') {
      callState.lastError = 'MediaRecorder is not supported in this browser';
      renderAllDeckUi();
      return { ok: false, error: callState.lastError };
    }

    const runPromise = (async () => {
      hydratePrefsFromStorage();
      const requestedMode =
        options.mode === 'audio' || options.mode === 'av' ? options.mode : prefs.mode;
      const ownedStreams = [];

      try {
        pruneDeadTracks();
        const useInPageWebRtc =
          !options.forceDisplayCapture && hasLiveWebRtcCallStream();

        const initialRemoteAudio = [];
        const initialLocalAudio = [];
        const initialRemoteVideo = [];
        const initialLocalVideo = [];

        if (useInPageWebRtc) {
          for (const entry of remoteTracks.values()) {
            if (!isTrackLive(entry.track)) continue;
            if (entry.kind === 'audio') initialRemoteAudio.push(entry);
            else if (entry.kind === 'video') initialRemoteVideo.push(entry);
          }
          for (const entry of localTracks.values()) {
            if (!isTrackLive(entry.track)) continue;
            if (entry.kind === 'audio') initialLocalAudio.push(entry);
            else if (entry.kind === 'video') initialLocalVideo.push(entry);
          }
        } else {
          if (options.auto) {
            return { ok: false, error: 'No active WebRTC tracks for auto-record' };
          }
          const md = globalScope.navigator && globalScope.navigator.mediaDevices;
          if (!md) {
            callState.lastError = 'Media capture APIs require HTTPS or localhost';
            renderAllDeckUi();
            return { ok: false, error: callState.lastError };
          }

          if (requestedMode === 'av') {
            if (typeof md.getDisplayMedia !== 'function') {
              callState.lastError = 'Screen/Tab capture (getDisplayMedia) is unavailable';
              renderAllDeckUi();
              return { ok: false, error: callState.lastError };
            }
            const displayStream = await callInternalMedia(() =>
              md.getDisplayMedia({
                video: { frameRate: { ideal: 30, max: 30 } },
                audio: true,
              })
            );
            if (displayStream) {
              ownedStreams.push(displayStream);
              if (typeof displayStream.getVideoTracks === 'function') {
                const vTracks = displayStream.getVideoTracks() || [];
                if (vTracks[0] && typeof vTracks[0].addEventListener === 'function') {
                  vTracks[0].addEventListener('ended', () => {
                    stopRecording('display-ended').catch(() => {});
                  });
                }
                for (const vt of vTracks) {
                  initialRemoteVideo.push({ track: vt, stream: displayStream });
                }
              }
              if (typeof displayStream.getAudioTracks === 'function') {
                for (const at of displayStream.getAudioTracks() || []) {
                  initialRemoteAudio.push({ track: at, stream: displayStream });
                }
              }
            }

            if (typeof md.getUserMedia === 'function') {
              const micStream = await callInternalMedia(() =>
                md.getUserMedia({
                  audio: {
                    echoCancellation: true,
                    noiseSuppression: true,
                    autoGainControl: true,
                  },
                })
              ).catch(() => null);
              if (micStream) {
                ownedStreams.push(micStream);
                if (typeof micStream.getAudioTracks === 'function') {
                  for (const mt of micStream.getAudioTracks() || []) {
                    initialLocalAudio.push({ track: mt, stream: micStream });
                  }
                }
              }
            }
          } else {
            // Manual Audio-Only capture (`getUserMedia` + optional `includeDisplayAudio`)
            if (typeof md.getUserMedia === 'function') {
              const micStream = await callInternalMedia(() =>
                md.getUserMedia({
                  audio: {
                    echoCancellation: true,
                    noiseSuppression: true,
                  },
                })
              ).catch(() => null);
              if (micStream) {
                ownedStreams.push(micStream);
                if (typeof micStream.getAudioTracks === 'function') {
                  for (const mt of micStream.getAudioTracks() || []) {
                    initialLocalAudio.push({ track: mt, stream: micStream });
                  }
                }
              }
            }

            if (options.includeDisplayAudio && typeof md.getDisplayMedia === 'function') {
              const displayStream = await callInternalMedia(() =>
                md.getDisplayMedia({ video: true, audio: true })
              ).catch(() => null);
              if (displayStream) {
                ownedStreams.push(displayStream);
                if (typeof displayStream.getVideoTracks === 'function') {
                  const dv = (displayStream.getVideoTracks() || [])[0];
                  if (dv && typeof dv.addEventListener === 'function') {
                    dv.addEventListener('ended', () => {
                      stopRecording('display-ended').catch(() => {});
                    });
                  }
                }
                if (typeof displayStream.getAudioTracks === 'function') {
                  for (const at of displayStream.getAudioTracks() || []) {
                    initialRemoteAudio.push({ track: at, stream: displayStream });
                  }
                }
              }
            }

            if (initialLocalAudio.length === 0 && initialRemoteAudio.length === 0) {
              stopOwnedStreamTracks(ownedStreams);
              callState.lastError = 'Microphone permission denied or no audio source found';
              renderAllDeckUi();
              return { ok: false, error: callState.lastError };
            }
          }
        }

        const startedAt = Date.now();
        const partnerName =
          cleanPartnerName(options.partnerName) ||
          callState.partnerName ||
          resolveFallbackDomPartnerName() ||
          'Zalo Call';
        const fileName = buildRecordingFileName(startedAt, partnerName);

        // ---- Web Audio Full-Duplex Mixer (48 kHz + Compressor + Keepalive + #933677 Pump) ----
        const AudioCtxCtor = globalScope.AudioContext || globalScope.webkitAudioContext;
        let audioCtx = null;
        let compressor = null;
        let mixDest = null;
        let keepaliveOsc = null;
        const audioNodes = [];
        const audioPumps = [];
        const mixedAudioKeys = new Set();

        if (typeof AudioCtxCtor === 'function') {
          try {
            audioCtx = new AudioCtxCtor({ sampleRate: 48000 });
          } catch {
            audioCtx = new AudioCtxCtor();
          }
          if (audioCtx && audioCtx.state === 'suspended' && typeof audioCtx.resume === 'function') {
            try {
              const r = audioCtx.resume();
              if (r && typeof r.catch === 'function') r.catch(() => {});
            } catch {}
          }
          const nowSec = (audioCtx && Number(audioCtx.currentTime)) || 0;
          mixDest = audioCtx.createMediaStreamDestination();

          if (typeof audioCtx.createDynamicsCompressor === 'function') {
            compressor = audioCtx.createDynamicsCompressor();
            setAudioParamSafe(compressor.threshold, -3, nowSec);
            setAudioParamSafe(compressor.knee, 12, nowSec);
            setAudioParamSafe(compressor.ratio, 8, nowSec);
            setAudioParamSafe(compressor.attack, 0.003, nowSec);
            setAudioParamSafe(compressor.release, 0.15, nowSec);
            compressor.connect(mixDest);
            audioNodes.push(compressor);
          }

          if (
            typeof audioCtx.createOscillator === 'function' &&
            typeof audioCtx.createGain === 'function'
          ) {
            try {
              keepaliveOsc = audioCtx.createOscillator();
              const keepaliveGain = audioCtx.createGain();
              setAudioParamSafe(keepaliveOsc.frequency, 440, nowSec);
              setAudioParamSafe(keepaliveGain.gain, 0.00001, nowSec);
              keepaliveOsc.connect(keepaliveGain);
              keepaliveGain.connect(mixDest);
              if (typeof keepaliveOsc.start === 'function') {
                keepaliveOsc.start();
              }
              audioNodes.push(keepaliveOsc, keepaliveGain);
            } catch {}
          }
        }

        const attachAudioTrack = (track, role, srcStream) => {
          if (!track || (track.readyState && track.readyState === 'ended')) return;
          const key = getTrackKey(track);
          if (!key || mixedAudioKeys.has(key)) return;
          mixedAudioKeys.add(key);

          const singleStream = createSingleTrackStream(track, srcStream);

          // Chromium Bug #933677 muted `<audio>` pump on remote WebRTC tracks
          if (role === 'remote' && globalScope.document) {
            try {
              const pump = globalScope.document.createElement('audio');
              pump.__olazRecorderInternal = true;
              markElementIgnoreMedia(pump);
              pump.muted = true;
              pump.volume = 0;
              pump.srcObject = singleStream;
              if (typeof pump.play === 'function') {
                const pr = pump.play();
                if (pr && typeof pr.catch === 'function') pr.catch(() => {});
              }
              audioPumps.push(pump);
            } catch {}
          }

          if (audioCtx && mixDest && typeof audioCtx.createMediaStreamSource === 'function') {
            try {
              const srcNode = audioCtx.createMediaStreamSource(singleStream);
              const sinkTarget = compressor || mixDest;
              if (typeof audioCtx.createGain === 'function') {
                const gainNode = audioCtx.createGain();
                setAudioParamSafe(gainNode.gain, 1.0, (audioCtx && audioCtx.currentTime) || 0);
                srcNode.connect(gainNode);
                gainNode.connect(sinkTarget);
                audioNodes.push(srcNode, gainNode);
              } else {
                srcNode.connect(sinkTarget);
                audioNodes.push(srcNode);
              }
            } catch {}
          }
        };

        for (const item of initialRemoteAudio) {
          attachAudioTrack(item.track, 'remote', item.stream);
        }
        for (const item of initialLocalAudio) {
          attachAudioTrack(item.track, 'local', item.stream);
        }

        // Path 1 non-blocking microphone acquisition when 0 local audio tracks exist
        if (useInPageWebRtc && initialLocalAudio.length === 0) {
          const md = globalScope.navigator && globalScope.navigator.mediaDevices;
          if (md && typeof md.getUserMedia === 'function') {
            try {
              const pMic = callInternalMedia(() => md.getUserMedia({ audio: true }));
              if (pMic && typeof pMic.then === 'function') {
                pMic
                  .then((micStream) => {
                    if (!micStream) return;
                    if (activeSession === session && !session.stopped) {
                      session.ownedStreams.push(micStream);
                      if (typeof micStream.getAudioTracks === 'function') {
                        for (const mt of micStream.getAudioTracks() || []) {
                          session.attachAudioTrack(mt, 'local', micStream);
                        }
                      }
                    } else {
                      stopOwnedStreamTracks([micStream]);
                    }
                  })
                  .catch(() => {});
              }
            } catch {}
          }
        }

        // ---- 30 fps `<canvas 1280x720>` Video Compositor ----
        const hasVideoInput =
          requestedMode === 'av' &&
          (initialRemoteVideo.length > 0 || initialLocalVideo.length > 0);
        const videoFeeders = [];
        const videoFeederKeys = new Set();
        let canvasStream = null;
        let stopTicker = null;
        let compositedVideoTrack = null;

        const attachVideoTrack = (track, role, srcStream) => {
          if (!track || (track.readyState && track.readyState === 'ended')) return;
          const key = getTrackKey(track);
          if (!key || videoFeederKeys.has(key)) return;
          videoFeederKeys.add(key);

          const doc = globalScope.document;
          const vid = doc && typeof doc.createElement === 'function' ? doc.createElement('video') : null;
          if (vid) {
            vid.__olazRecorderInternal = true;
            markElementIgnoreMedia(vid);
            vid.muted = true;
            vid.playsInline = true;
            vid.autoplay = true;
            if (typeof vid.setAttribute === 'function') {
              vid.setAttribute('playsinline', '');
              vid.setAttribute('muted', '');
            }
            vid.srcObject = createSingleTrackStream(track, srcStream);
            if (typeof vid.play === 'function') {
              const pr = vid.play();
              if (pr && typeof pr.catch === 'function') pr.catch(() => {});
            }
          }
          videoFeeders.push({
            key,
            track,
            role: role === 'local' ? 'local' : 'remote',
            video: vid,
          });
        };

        const session = {
          startedAt,
          paused: false,
          pausedAt: 0,
          pausedAccumMs: 0,
          stopped: false,
          stopPromise: null,
          autoStarted: Boolean(options.auto),
          sourceType: useInPageWebRtc ? 'webrtc' : 'manual',
          mode: hasVideoInput ? 'av' : 'audio',
          partnerName,
          fileName,
          mimeType: '',
          totalBytes: 0,
          chunks: [],
          ownedStreams,
          attachAudioTrack,
          attachVideoTrack: null,
          recorder: null,
          cleanupResources: null,
        };

        if (hasVideoInput) {
          for (const item of initialRemoteVideo) {
            attachVideoTrack(item.track, 'remote', item.stream);
          }
          for (const item of initialLocalVideo) {
            attachVideoTrack(item.track, 'local', item.stream);
          }
          session.attachVideoTrack = attachVideoTrack;

          const doc = globalScope.document;
          const canvas =
            doc && typeof doc.createElement === 'function' ? doc.createElement('canvas') : null;
          if (canvas) {
            canvas.width = CANVAS_WIDTH;
            canvas.height = CANVAS_HEIGHT;
            const ctx2d =
              typeof canvas.getContext === 'function' ? canvas.getContext('2d') : null;

            const renderFrame = () => {
              if (!ctx2d) return;
              ctx2d.fillStyle = '#0f172a';
              if (typeof ctx2d.fillRect === 'function') {
                ctx2d.fillRect(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT);
              }

              const liveFeeders = videoFeeders.filter((f) => isTrackLive(f.track));
              const remotes = liveFeeders.filter((f) => f.role !== 'local');
              const locals = liveFeeders.filter((f) => f.role === 'local');
              const elapsed = getSessionElapsedMs(session);

              if (remotes.length >= 2) {
                // Multi-peer group call: 2x1 side-by-side or 2x2 grid
                const tiles =
                  remotes.length === 2 && locals.length === 0
                    ? remotes
                    : remotes.concat(locals).slice(0, 4);
                const rows = tiles.length <= 2 ? 1 : 2;
                const cols = 2;
                const cellW = Math.floor(CANVAS_WIDTH / cols);
                const cellH = Math.floor(CANVAS_HEIGHT / rows);

                for (let idx = 0; idx < tiles.length; idx++) {
                  const tile = tiles[idx];
                  const cx = (idx % cols) * cellW;
                  const cy = Math.floor(idx / cols) * cellH;
                  if (hasDecodedVideoFrame(tile.video)) {
                    drawVideoContain(ctx2d, tile.video, cx + 4, cy + 4, cellW - 8, cellH - 8);
                  } else {
                    drawAvatarFallbackTile(
                      ctx2d,
                      cx,
                      cy,
                      cellW,
                      cellH,
                      session.partnerName,
                      elapsed,
                      idx === 0
                    );
                  }
                }
              } else {
                // Primary remote/display feed (`contain`) + rounded bottom-right Local PiP (`280x158`)
                const primary = remotes[0] || locals[0] || null;
                if (primary && hasDecodedVideoFrame(primary.video)) {
                  drawVideoContain(ctx2d, primary.video, 0, 0, CANVAS_WIDTH, CANVAS_HEIGHT);
                } else {
                  drawAvatarFallbackTile(
                    ctx2d,
                    0,
                    0,
                    CANVAS_WIDTH,
                    CANVAS_HEIGHT,
                    session.partnerName,
                    elapsed,
                    true
                  );
                }

                if (remotes.length >= 1 && locals.length >= 1 && hasDecodedVideoFrame(locals[0].video)) {
                  drawRoundedPip(ctx2d, locals[0].video);
                }
              }
            };

            renderFrame();
            stopTicker = startBackgroundSafeTicker(renderFrame);

            const captureFn =
              typeof canvas.captureStream === 'function'
                ? canvas.captureStream.bind(canvas)
                : typeof canvas.mozCaptureStream === 'function'
                  ? canvas.mozCaptureStream.bind(canvas)
                  : null;
            if (captureFn) {
              try {
                canvasStream = captureFn(30);
                if (canvasStream && typeof canvasStream.getVideoTracks === 'function') {
                  compositedVideoTrack = (canvasStream.getVideoTracks() || [])[0] || null;
                }
              } catch {}
            }
          }

          if (!compositedVideoTrack) {
            const fallbackVid = initialRemoteVideo[0] || initialLocalVideo[0];
            compositedVideoTrack = fallbackVid ? fallbackVid.track : null;
          }
        }

        // ---- Assemble Final Output MediaStream & Start `MediaRecorder` ----
        const outAudioTracks =
          mixDest && mixDest.stream && typeof mixDest.stream.getAudioTracks === 'function'
            ? mixDest.stream.getAudioTracks() || []
            : initialRemoteAudio
                .concat(initialLocalAudio)
                .map((x) => x.track)
                .filter(Boolean);

        const outTracks = [];
        if (compositedVideoTrack) {
          outTracks.push(compositedVideoTrack);
        }
        for (const at of outAudioTracks) {
          outTracks.push(at);
        }

        const outputStream = createCompositeStream(outTracks);
        const chosenMime = selectSupportedMime(MediaRecorderCtor, Boolean(compositedVideoTrack));
        const recOptions = chosenMime
          ? compositedVideoTrack
            ? {
                mimeType: chosenMime,
                videoBitsPerSecond: 2500000,
                audioBitsPerSecond: 128000,
              }
            : {
                mimeType: chosenMime,
                audioBitsPerSecond: 128000,
              }
          : undefined;

        let recorder;
        try {
          recorder = recOptions
            ? new MediaRecorderCtor(outputStream, recOptions)
            : new MediaRecorderCtor(outputStream);
        } catch {
          recorder = new MediaRecorderCtor(outputStream);
        }

        session.mimeType =
          (recorder && recorder.mimeType) ||
          chosenMime ||
          (compositedVideoTrack ? 'video/webm' : 'audio/webm');
        session.recorder = recorder;

        const onDataAvailable = (ev) => {
          if (!ev || ev.__olazChunkHandled) return;
          ev.__olazChunkHandled = true;
          const chunk = ev.data;
          if (chunk && Number(chunk.size) > 0) {
            session.chunks.push(chunk);
            session.totalBytes += Number(chunk.size) || 0;
            advanceCaptureCursorWindow(chunk.size);
            renderAllDeckUi();
          }
        };

        recorder.ondataavailable = onDataAvailable;
        if (typeof recorder.addEventListener === 'function') {
          recorder.addEventListener('dataavailable', onDataAvailable);
        }

        const uiTickTimer = unrefTimer(
          globalScope.setInterval(() => {
            renderAllDeckUi();
          }, 1000)
        );

        let cleanedUp = false;
        session.cleanupResources = () => {
          if (cleanedUp) return;
          cleanedUp = true;
          globalScope.clearInterval(uiTickTimer);
          if (stopTicker) {
            try {
              stopTicker();
            } catch {}
            stopTicker = null;
          }
          if (canvasStream) {
            stopOwnedStreamTracks([canvasStream]);
            canvasStream = null;
          }
          if (mixDest && mixDest.stream) {
            stopOwnedStreamTracks([mixDest.stream]);
          }
          stopOwnedStreamTracks(session.ownedStreams);

          for (const f of videoFeeders) {
            if (f.video) {
              try {
                f.video.pause();
              } catch {}
              try {
                f.video.srcObject = null;
              } catch {}
            }
          }
          videoFeeders.length = 0;

          for (const p of audioPumps) {
            try {
              p.pause();
            } catch {}
            try {
              p.srcObject = null;
            } catch {}
          }
          audioPumps.length = 0;

          if (keepaliveOsc && typeof keepaliveOsc.stop === 'function') {
            try {
              keepaliveOsc.stop();
            } catch {}
          }
          for (const node of audioNodes) {
            if (node && typeof node.disconnect === 'function') {
              try {
                node.disconnect();
              } catch {}
            }
          }
          audioNodes.length = 0;
          if (audioCtx && typeof audioCtx.close === 'function') {
            try {
              const cp = audioCtx.close();
              if (cp && typeof cp.catch === 'function') cp.catch(() => {});
            } catch {}
          }
        };

        recorder.start(1000);
        reconcileCaptureEpochQuota('rec-start:' + session.fileName);

        activeSession = session;
        callState.lastError = '';
        callState.dismissed = false;
        renderAllDeckUi();

        return {
          ok: true,
          fileName: session.fileName,
          mode: session.mode,
          mimeType: session.mimeType,
        };
      } catch (err) {
        stopOwnedStreamTracks(ownedStreams);
        const msg = String((err && err.message) || err || 'Failed to start call recording');
        callState.lastError = msg;
        renderAllDeckUi();
        return { ok: false, error: msg };
      }
    })();

    startingPromise = runPromise;
    runPromise.finally(() => {
      if (startingPromise === runPromise) {
        startingPromise = null;
      }
    });

    return runPromise;
  }

  function stopRecording(reason) {
    const session = activeSession;
    if (!session) {
      return Promise.resolve(null);
    }
    if (session.stopPromise) {
      return session.stopPromise;
    }

    session.stopped = true;
    reconcileCaptureEpochQuota('rec-stop:' + String(reason || 'manual'));

    session.stopPromise = (async () => {
      const durationMs = getSessionElapsedMs(session);
      const recorder = session.recorder;

      if (recorder && recorder.state !== 'inactive') {
        await new Promise((resolve) => {
          let settled = false;
          const done = () => {
            if (settled) return;
            settled = true;
            resolve();
          };
          const safetyTimer = unrefTimer(globalScope.setTimeout(done, 1500));
          const onStop = () => {
            globalScope.clearTimeout(safetyTimer);
            done();
          };
          recorder.onstop = onStop;
          if (typeof recorder.addEventListener === 'function') {
            recorder.addEventListener('stop', onStop, { once: true });
          }
          try {
            if (typeof recorder.requestData === 'function') {
              recorder.requestData();
            }
          } catch {}
          try {
            recorder.stop();
          } catch {
            globalScope.clearTimeout(safetyTimer);
            done();
          }
        });
      }

      if (typeof session.cleanupResources === 'function') {
        session.cleanupResources();
      }
      if (activeSession === session) {
        activeSession = null;
      }

      const finalMime = session.mimeType || (session.mode === 'av' ? 'video/webm' : 'audio/webm');
      const blob =
        typeof globalScope.Blob === 'function'
          ? new globalScope.Blob(session.chunks, { type: finalMime })
          : { size: session.totalBytes, type: finalMime, _chunks: session.chunks };
      const byteSize = Number(blob && blob.size) || session.totalBytes;

      const record = {
        id: 'rec_' + session.startedAt + '_' + ++recordSeq,
        slot: ACTIVE_SLOT,
        fileName: session.fileName,
        partnerName: session.partnerName,
        mode: session.mode,
        mimeType: finalMime,
        size: byteSize,
        durationMs,
        createdAt: session.startedAt,
        endedAt: Date.now(),
        blob,
      };

      const savedMeta = await persistRecordingToIdb(record);
      triggerBrowserBlobDownload(blob, session.fileName);
      if (!isCallCurrentlyDetected()) {
        callState.partnerName = '';
        callState.isVideoCall = false;
      }
      renderAllDeckUi();
      return savedMeta;
    })();

    return session.stopPromise;
  }

  function toggleRecording(opts) {
    if (activeSession && !activeSession.stopped) {
      return stopRecording('toggle');
    }
    return startRecording(opts);
  }

  function pauseRecording() {
    if (!activeSession || activeSession.stopped || activeSession.paused) {
      return false;
    }
    const rec = activeSession.recorder;
    if (!rec || rec.state !== 'recording' || typeof rec.pause !== 'function') {
      return false;
    }
    try {
      rec.pause();
      activeSession.paused = true;
      activeSession.pausedAt = Date.now();
      renderAllDeckUi();
      return true;
    } catch {
      return false;
    }
  }

  function resumeRecording() {
    if (!activeSession || activeSession.stopped || !activeSession.paused) {
      return false;
    }
    const rec = activeSession.recorder;
    if (!rec || rec.state !== 'paused' || typeof rec.resume !== 'function') {
      return false;
    }
    try {
      rec.resume();
      if (activeSession.pausedAt) {
        activeSession.pausedAccumMs += Math.max(0, Date.now() - activeSession.pausedAt);
        activeSession.pausedAt = 0;
      }
      activeSession.paused = false;
      renderAllDeckUi();
      return true;
    } catch {
      return false;
    }
  }

  // ==========================================================================
  // 6. Shadow DOM UI Integration (`host.deckMountHooks`)
  // ==========================================================================

  const CALL_REC_STYLES = `
    @keyframes olazCallRecPulse {
      0%, 100% { opacity: 1; transform: scale(1); }
      50% { opacity: 0.42; transform: scale(0.86); }
    }
    .rec-dot,
    .call-rec-pill-dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: #ef4444;
      box-shadow: 0 0 9px rgba(239, 68, 68, 0.9);
      flex: none;
      display: inline-block;
      animation: olazCallRecPulse 1.15s ease-in-out infinite;
    }
    .rec-dot.static {
      animation: none;
    }
    .rec-dot.paused,
    .call-rec-pill-dot.paused {
      background: #f59e0b;
      box-shadow: 0 0 8px rgba(245, 158, 11, 0.85);
      animation: none;
    }
    .pill.call-recording {
      border-color: rgba(239, 68, 68, 0.72) !important;
      box-shadow: 0 10px 26px rgba(127, 29, 29, 0.44);
    }
    .pill.call-recording .dot {
      display: none;
    }
    #callRecBar.call-rec-bar {
      display: none;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
      width: min(368px, calc(100vw - 24px));
      width: min(368px, calc(100dvw - 24px));
      padding: 9px 11px;
      border-radius: 14px;
      background: rgba(15, 23, 42, 0.96);
      border: 1px solid rgba(56, 189, 248, 0.38);
      box-shadow: 0 18px 42px rgba(2, 6, 23, 0.56);
      backdrop-filter: blur(16px);
      -webkit-backdrop-filter: blur(16px);
      color: #f8fafc;
      font-size: 12px;
    }
    #callRecBar.call-rec-bar.visible {
      display: flex;
    }
    #callRecBar.call-rec-bar.is-recording {
      border-color: rgba(239, 68, 68, 0.54);
    }
    .crb-left {
      display: flex;
      align-items: center;
      gap: 7px;
      min-width: 0;
      flex: 1 1 auto;
    }
    .crb-phone-ic {
      width: 15px;
      height: 15px;
      color: #38bdf8;
      flex: none;
      display: inline-flex;
    }
    .crb-phone-ic svg {
      width: 100%;
      height: 100%;
      display: block;
    }
    .crb-title {
      font-size: 12px;
      font-weight: 600;
      color: #f8fafc;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
      font-variant-numeric: tabular-nums;
    }
    .crb-actions {
      display: inline-flex;
      align-items: center;
      gap: 5px;
      flex: none;
    }
    .crb-chip {
      min-height: 28px;
      padding: 0 9px;
      border-radius: 8px;
      border: 1px solid rgba(148, 163, 184, 0.28);
      background: rgba(30, 41, 59, 0.92);
      color: #e2e8f0;
      font-size: 11.5px;
      font-weight: 600;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      gap: 5px;
      white-space: nowrap;
    }
    .crb-chip:hover {
      background: rgba(51, 65, 85, 0.96);
      color: #ffffff;
    }
    .crb-chip.rec-start {
      background: rgba(220, 38, 38, 0.22);
      border-color: rgba(248, 113, 113, 0.52);
      color: #fecaca;
    }
    .crb-chip.rec-start:hover {
      background: rgba(220, 38, 38, 0.36);
      color: #ffffff;
    }
    .crb-chip.rec-stop {
      background: linear-gradient(135deg, #dc2626 0%, #b91c1c 100%);
      border-color: rgba(252, 165, 165, 0.5);
      color: #ffffff;
    }
    .crb-chip.icon-only {
      min-width: 28px;
      padding: 0 6px;
    }
    .crb-chip svg {
      width: 13px;
      height: 13px;
      display: block;
      flex: none;
    }
    .call-rec-select {
      padding: 4px 8px;
      border-radius: 8px;
      border: 1px solid rgba(148, 163, 184, 0.28);
      background: rgba(30, 41, 59, 0.95);
      color: #f8fafc;
      font-size: 11.5px;
      font-weight: 600;
      cursor: pointer;
      max-width: 162px;
    }
    .call-rec-status {
      font-size: 11px;
      color: #94a3b8;
      line-height: 1.35;
      margin: 4px 0 6px;
      font-variant-numeric: tabular-nums;
    }
    .call-rec-status.active {
      color: #fca5a5;
      font-weight: 600;
    }
    .call-rec-list {
      margin-top: 8px;
      max-height: 184px;
      overflow-y: auto;
      display: flex;
      flex-direction: column;
      gap: 5px;
    }
    .call-rec-empty {
      font-size: 11px;
      color: #64748b;
      padding: 4px 0 2px;
    }
    .call-rec-item {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 6px;
      padding: 6px 8px;
      border-radius: 9px;
      background: rgba(30, 41, 59, 0.66);
      border: 1px solid rgba(148, 163, 184, 0.16);
    }
    .call-rec-item-meta {
      min-width: 0;
      flex: 1 1 auto;
    }
    .call-rec-item-name {
      font-size: 11.5px;
      font-weight: 600;
      color: #f1f5f9;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .call-rec-item-sub {
      font-size: 10.5px;
      color: #94a3b8;
      font-variant-numeric: tabular-nums;
      margin-top: 1px;
    }
    .call-rec-item-btns {
      display: inline-flex;
      align-items: center;
      gap: 4px;
      flex: none;
    }
    .call-rec-mini-btn {
      min-width: 26px;
      min-height: 26px;
      padding: 0 6px;
      border-radius: 7px;
      border: 1px solid rgba(148, 163, 184, 0.22);
      background: rgba(15, 23, 42, 0.78);
      color: #cbd5e1;
      font-size: 12px;
      font-weight: 700;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      justify-content: center;
    }
    .call-rec-mini-btn:hover {
      background: rgba(51, 65, 85, 0.92);
      color: #ffffff;
    }
    .call-rec-mini-btn.del:hover {
      background: rgba(127, 29, 29, 0.65);
      border-color: rgba(248, 113, 113, 0.45);
      color: #fecaca;
    }
    @media (pointer: coarse) {
      .crb-chip {
        min-height: 38px;
      }
      .call-rec-select {
        min-height: 36px;
      }
      .call-rec-mini-btn {
        min-width: 36px;
        min-height: 36px;
      }
    }
  `;

  function renderSingleDeckBinding(b) {
    const isRec = Boolean(activeSession && !activeSession.stopped);
    const isPaused = Boolean(activeSession && activeSession.paused);
    const detected = isCallCurrentlyDetected();
    const partner = effectivePartnerName();
    const elapsedStr = formatDurationClock(getSessionElapsedMs(activeSession));
    const sizeStr = formatBytesMb(activeSession ? activeSession.totalBytes : 0);

    // 1. Pill indicator
    if (b.pill) {
      b.pill.classList.toggle('call-recording', isRec);
    }
    if (b.pillDot) {
      b.pillDot.classList.toggle('hidden', !isRec);
      b.pillDot.classList.toggle('paused', isPaused);
    }

    // 2. Floating `#callRecBar`
    const showBar = Boolean(isRec || (detected && !callState.dismissed));
    b.callRecBar.classList.toggle('visible', showBar);
    b.callRecBar.classList.toggle('is-recording', isRec);

    b.idleLeft.classList.toggle('hidden', isRec);
    b.idleActions.classList.toggle('hidden', isRec);
    b.recLeft.classList.toggle('hidden', !isRec);
    b.recActions.classList.toggle('hidden', !isRec);

    if (isRec) {
      b.recDotLive.classList.toggle('paused', isPaused);
      b.recMetaText.textContent =
        (isPaused ? 'PAUSED ' : 'REC ') +
        elapsedStr +
        ' \u00B7 ' +
        sizeStr +
        ' \u00B7 ' +
        partner;
      b.barPauseBtn.textContent = isPaused ? '\u25B6' : '\u23F8';
      b.barPauseBtn.setAttribute(
        'aria-label',
        isPaused ? 'Resume call recording' : 'Pause call recording'
      );
    } else if (showBar) {
      b.idlePartnerText.textContent = partner;
      b.barModeBtn.textContent = prefs.mode === 'audio' ? 'Audio' : 'AV';
    }

    // 3. `#callRecSec` inside `#panel`
    b.autoRecCb.checked = Boolean(prefs.autoRecord);
    b.modeSelect.value = prefs.mode === 'audio' ? 'audio' : 'av';
    b.modeSelect.disabled = isRec;

    b.statusText.classList.toggle('active', isRec);
    if (isRec) {
      b.statusText.textContent =
        (isPaused ? 'Paused ' : 'Recording ') +
        elapsedStr +
        ' \u00B7 ' +
        sizeStr +
        ' \u00B7 ' +
        partner;
    } else if (callState.lastError) {
      b.statusText.textContent = callState.lastError;
    } else if (detected) {
      b.statusText.textContent = 'Call detected (' + partner + ') \u00B7 Ready to record';
    } else {
      b.statusText.textContent = 'Ready \u00B7 WebRTC auto-detect + Tab/Window capture';
    }

    b.primaryBtn.className = isRec ? 'btn danger' : 'btn primary';
    b.primaryBtnLabel.textContent = isRec
      ? 'Stop & Save (.webm)'
      : 'Record Call (Video / Audio)';
    b.primaryBtnDot.classList.toggle('hidden', isRec);

    // Render saved recordings list (retained up to 20 per slot; scrollable 5-row viewport)
    const sig = cachedRecordingsMeta.map((r) => r.id + ':' + r.size).join('|');
    if (b.lastRenderedListSig !== sig) {
      b.lastRenderedListSig = sig;
      b.recListEl.innerHTML = '';
      if (cachedRecordingsMeta.length === 0) {
        const emptyEl = globalScope.document.createElement('div');
        emptyEl.className = 'call-rec-empty';
        emptyEl.textContent =
          'No saved call recordings yet (up to ' + VISIBLE_SAVED_ROWS + ' visible, 20 stored locally).';
        b.recListEl.appendChild(emptyEl);
      } else {
        for (const item of cachedRecordingsMeta) {
          const row = globalScope.document.createElement('div');
          row.className = 'call-rec-item';

          const metaCol = globalScope.document.createElement('div');
          metaCol.className = 'call-rec-item-meta';

          const nameDiv = globalScope.document.createElement('div');
          nameDiv.className = 'call-rec-item-name';
          nameDiv.textContent = item.fileName || item.partnerName || 'call.webm';

          const subDiv = globalScope.document.createElement('div');
          subDiv.className = 'call-rec-item-sub';
          subDiv.textContent =
            formatDurationClock(item.durationMs) +
            ' \u00B7 ' +
            formatBytesMb(item.size) +
            ' \u00B7 ' +
            (item.mode === 'audio' ? 'Audio' : 'AV');

          metaCol.appendChild(nameDiv);
          metaCol.appendChild(subDiv);

          const btnCol = globalScope.document.createElement('div');
          btnCol.className = 'call-rec-item-btns';

          const dlBtn = globalScope.document.createElement('button');
          dlBtn.type = 'button';
          dlBtn.className = 'call-rec-mini-btn';
          dlBtn.textContent = '\u2B07';
          dlBtn.setAttribute('aria-label', 'Download ' + (item.fileName || 'recording'));
          dlBtn.addEventListener('click', () => {
            downloadRecording(item.id).catch(() => {});
          });

          const delBtn = globalScope.document.createElement('button');
          delBtn.type = 'button';
          delBtn.className = 'call-rec-mini-btn del';
          delBtn.textContent = '\u00D7';
          delBtn.setAttribute('aria-label', 'Delete ' + (item.fileName || 'recording'));
          delBtn.addEventListener('click', () => {
            deleteRecording(item.id).catch(() => {});
          });

          btnCol.appendChild(dlBtn);
          btnCol.appendChild(delBtn);

          row.appendChild(metaCol);
          row.appendChild(btnCol);
          b.recListEl.appendChild(row);
        }
      }
    }

    if (b.lastBarVisible !== showBar) {
      b.lastBarVisible = showBar;
      if (typeof b.requestLayout === 'function') {
        try {
          b.requestLayout();
        } catch {}
      }
    }
  }

  function renderAllDeckUi() {
    for (const b of deckBindings) {
      try {
        renderSingleDeckBinding(b);
      } catch {}
    }
  }

  function mountCallRecorderDeck(ctx) {
    if (!ctx || !ctx.shadow || ctx.shadow.__olazCallRecMounted) {
      return;
    }
    ctx.shadow.__olazCallRecMounted = true;
    reconcileCaptureEpochQuota('mount-deck');

    const doc = globalScope.document;
    if (!doc || typeof doc.createElement !== 'function') return;

    // 1. Scoped Call Recorder CSS
    const styleEl = doc.createElement('style');
    styleEl.textContent = CALL_REC_STYLES;
    ctx.shadow.appendChild(styleEl);

    // 2. Pulsing red indicator on `#pill`
    let pillDot = null;
    if (ctx.pill && typeof ctx.pill.appendChild === 'function') {
      pillDot = doc.createElement('span');
      pillDot.className = 'call-rec-pill-dot hidden';
      pillDot.setAttribute('aria-hidden', 'true');
      const refChild =
        ctx.pill.children && ctx.pill.children.length > 1
          ? ctx.pill.children[ctx.pill.children.length - 1]
          : null;
      appendBeforeOrEnd(ctx.pill, pillDot, refChild);
    }

    // 3. Floating `#callRecBar` inside `floatStack`
    const callRecBar = doc.createElement('section');
    callRecBar.className = 'call-rec-bar';
    callRecBar.id = 'callRecBar';
    callRecBar.setAttribute('aria-label', 'Call Recorder');

    // Idle call detected children: `[📞 <Partner Name>]  [AV/Audio]  [● Record]  [×]`
    const idleLeft = doc.createElement('div');
    idleLeft.className = 'crb-left';
    const phoneIcon = doc.createElement('span');
    phoneIcon.className = 'crb-phone-ic';
    phoneIcon.setAttribute('aria-hidden', 'true');
    phoneIcon.innerHTML =
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.8 19.8 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.12 4.18 2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72c.12.96.36 1.9.7 2.81a2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45c.91.34 1.85.58 2.81.7A2 2 0 0 1 22 16.92z"/>' +
      '</svg>';
    const idlePartnerText = doc.createElement('span');
    idlePartnerText.className = 'crb-title';
    idlePartnerText.textContent = 'Zalo Call';
    idleLeft.appendChild(phoneIcon);
    idleLeft.appendChild(idlePartnerText);

    const idleActions = doc.createElement('div');
    idleActions.className = 'crb-actions';

    const barModeBtn = doc.createElement('button');
    barModeBtn.type = 'button';
    barModeBtn.className = 'crb-chip';
    barModeBtn.textContent = prefs.mode === 'audio' ? 'Audio' : 'AV';
    barModeBtn.setAttribute('aria-label', 'Toggle capture mode between AV and Audio-only');
    barModeBtn.addEventListener('click', (ev) => {
      if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
      setPrefs({ mode: prefs.mode === 'av' ? 'audio' : 'av' });
    });

    const barRecBtn = doc.createElement('button');
    barRecBtn.type = 'button';
    barRecBtn.className = 'crb-chip rec-start';
    barRecBtn.setAttribute('aria-label', 'Start recording call');
    const barRecDot = doc.createElement('span');
    barRecDot.className = 'rec-dot static';
    const barRecLbl = doc.createElement('span');
    barRecLbl.textContent = 'Record';
    barRecBtn.appendChild(barRecDot);
    barRecBtn.appendChild(barRecLbl);
    barRecBtn.addEventListener('click', (ev) => {
      if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
      startRecording({ mode: prefs.mode }).catch(() => {});
    });

    const barDismissBtn = doc.createElement('button');
    barDismissBtn.type = 'button';
    barDismissBtn.className = 'crb-chip icon-only';
    barDismissBtn.textContent = '\u00D7';
    barDismissBtn.setAttribute('aria-label', 'Dismiss call recorder bar');
    barDismissBtn.addEventListener('click', (ev) => {
      if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
      callState.dismissed = true;
      renderAllDeckUi();
    });

    idleActions.appendChild(barModeBtn);
    idleActions.appendChild(barRecBtn);
    idleActions.appendChild(barDismissBtn);

    // Active recording children: `[● REC MM:SS · X.X MB · <Partner>]  [⏸/▶]  [■ Stop & Save]`
    const recLeft = doc.createElement('div');
    recLeft.className = 'crb-left hidden';
    const recDotLive = doc.createElement('span');
    recDotLive.className = 'rec-dot';
    const recMetaText = doc.createElement('span');
    recMetaText.className = 'crb-title';
    recMetaText.textContent = 'REC 00:00 \u00B7 0.0 MB';
    recLeft.appendChild(recDotLive);
    recLeft.appendChild(recMetaText);

    const recActions = doc.createElement('div');
    recActions.className = 'crb-actions hidden';

    const barPauseBtn = doc.createElement('button');
    barPauseBtn.type = 'button';
    barPauseBtn.className = 'crb-chip icon-only';
    barPauseBtn.textContent = '\u23F8';
    barPauseBtn.setAttribute('aria-label', 'Pause call recording');
    barPauseBtn.addEventListener('click', (ev) => {
      if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
      if (activeSession && activeSession.paused) {
        resumeRecording();
      } else {
        pauseRecording();
      }
    });

    const barStopBtn = doc.createElement('button');
    barStopBtn.type = 'button';
    barStopBtn.className = 'crb-chip rec-stop';
    barStopBtn.textContent = '\u25A0 Stop & Save';
    barStopBtn.setAttribute('aria-label', 'Stop and save call recording');
    barStopBtn.addEventListener('click', (ev) => {
      if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
      stopRecording('bar-stop').catch(() => {});
    });

    recActions.appendChild(barPauseBtn);
    recActions.appendChild(barStopBtn);

    callRecBar.appendChild(idleLeft);
    callRecBar.appendChild(idleActions);
    callRecBar.appendChild(recLeft);
    callRecBar.appendChild(recActions);

    const stackTarget = ctx.floatStack || ctx.wrapEl;
    if (stackTarget) {
      if (ctx.floatStack) {
        ctx.floatStack.appendChild(callRecBar);
      } else {
        appendBeforeOrEnd(stackTarget, callRecBar, ctx.pill || null);
      }
    }

    if (typeof ctx.attachDragHandle === 'function') {
      try {
        ctx.attachDragHandle(callRecBar, { lockOrientationOnDrag: true });
      } catch {}
    }

    // 4. `#callRecSec` inside `#panel` (above Wipe button)
    const callRecSec = doc.createElement('div');
    callRecSec.className = 'sec call-rec-sec';
    callRecSec.id = 'callRecSec';

    const secHeader = doc.createElement('div');
    secHeader.className = 'sec-lbl';
    secHeader.textContent = 'Video & Audio Call Recorder';

    const autoRow = doc.createElement('label');
    autoRow.className = 'row';
    autoRow.setAttribute('for', 'callRecAuto');
    const autoLbl = doc.createElement('span');
    autoLbl.textContent = 'Auto-Record Calls (WebRTC)';
    const autoRecCb = doc.createElement('input');
    autoRecCb.type = 'checkbox';
    autoRecCb.id = 'callRecAuto';
    autoRecCb.checked = Boolean(prefs.autoRecord);
    autoRecCb.addEventListener('change', () => {
      setPrefs({ autoRecord: Boolean(autoRecCb.checked) });
    });
    autoRow.appendChild(autoLbl);
    autoRow.appendChild(autoRecCb);

    const modeRow = doc.createElement('div');
    modeRow.className = 'row';
    const modeLbl = doc.createElement('label');
    modeLbl.setAttribute('for', 'callRecMode');
    modeLbl.textContent = 'Capture Mode';
    const modeSelect = doc.createElement('select');
    modeSelect.id = 'callRecMode';
    modeSelect.className = 'call-rec-select';
    const optAv = doc.createElement('option');
    optAv.value = 'av';
    optAv.textContent = 'Audio + Video (.webm)';
    const optAudio = doc.createElement('option');
    optAudio.value = 'audio';
    optAudio.textContent = 'Audio Only (.webm)';
    modeSelect.appendChild(optAv);
    modeSelect.appendChild(optAudio);
    modeSelect.value = prefs.mode;
    modeSelect.addEventListener('change', () => {
      setPrefs({ mode: modeSelect.value === 'audio' ? 'audio' : 'av' });
    });
    modeRow.appendChild(modeLbl);
    modeRow.appendChild(modeSelect);

    const statusText = doc.createElement('div');
    statusText.className = 'call-rec-status';
    statusText.id = 'callRecStatus';
    statusText.textContent = 'Ready \u00B7 WebRTC auto-detect + Tab/Window capture';

    const primaryBtn = doc.createElement('button');
    primaryBtn.type = 'button';
    primaryBtn.className = 'btn primary';
    primaryBtn.id = 'callRecPrimaryBtn';
    const primaryBtnDot = doc.createElement('span');
    primaryBtnDot.className = 'rec-dot static';
    const primaryBtnLabel = doc.createElement('span');
    primaryBtnLabel.textContent = 'Record Call (Video / Audio)';
    primaryBtn.appendChild(primaryBtnDot);
    primaryBtn.appendChild(primaryBtnLabel);
    primaryBtn.addEventListener('click', () => {
      toggleRecording({ mode: prefs.mode }).catch(() => {});
    });

    const recListEl = doc.createElement('div');
    recListEl.className = 'call-rec-list';
    recListEl.id = 'callRecList';

    callRecSec.appendChild(secHeader);
    callRecSec.appendChild(autoRow);
    callRecSec.appendChild(modeRow);
    callRecSec.appendChild(statusText);
    callRecSec.appendChild(primaryBtn);
    callRecSec.appendChild(recListEl);

    if (ctx.panel) {
      let wipeSectionNode = null;
      try {
        if (typeof ctx.shadow.querySelector === 'function') {
          const wipeBtn = ctx.shadow.querySelector('#wipeBtn');
          if (wipeBtn && wipeBtn.parentNode === ctx.panel) {
            wipeSectionNode = wipeBtn;
          } else if (wipeBtn && wipeBtn.parentNode && wipeBtn.parentNode.parentNode === ctx.panel) {
            wipeSectionNode = wipeBtn.parentNode;
          }
        }
      } catch {}
      appendBeforeOrEnd(ctx.panel, callRecSec, wipeSectionNode);
    }

    const binding = {
      shadow: ctx.shadow,
      pill: ctx.pill || null,
      pillDot,
      callRecBar,
      idleLeft,
      idlePartnerText,
      idleActions,
      barModeBtn,
      barRecBtn,
      barDismissBtn,
      recLeft,
      recDotLive,
      recMetaText,
      recActions,
      barPauseBtn,
      barStopBtn,
      autoRecCb,
      modeSelect,
      statusText,
      primaryBtn,
      primaryBtnDot,
      primaryBtnLabel,
      recListEl,
      requestLayout: ctx.requestLayout || null,
      lastRenderedListSig: null,
      lastBarVisible: false,
    };

    deckBindings.push(binding);
    renderSingleDeckBinding(binding);

    // Hydrate IndexedDB saved recordings list asynchronously
    listRecordings()
      .then(() => renderAllDeckUi())
      .catch(() => {});
  }

  // ==========================================================================
  // 7. Public API & Runtime Wiring
  // ==========================================================================

  function getPrefs() {
    hydratePrefsFromStorage();
    return {
      autoRecord: Boolean(prefs.autoRecord),
      mode: prefs.mode === 'audio' ? 'audio' : 'av',
    };
  }

  function setPrefs(patch) {
    if (patch && typeof patch === 'object') {
      if (patch.autoRecord !== undefined) {
        prefs.autoRecord = Boolean(patch.autoRecord);
      }
      if (patch.mode === 'av' || patch.mode === 'audio') {
        prefs.mode = patch.mode;
      }
      persistPrefsToStorage();
      renderAllDeckUi();
      if (prefs.autoRecord) {
        maybeAutoStartRecording();
      }
    }
    return getPrefs();
  }

  function getState() {
    const isRec = Boolean(activeSession && !activeSession.stopped);
    return {
      slot: ACTIVE_SLOT,
      recording: isRec,
      paused: Boolean(activeSession && activeSession.paused),
      mode: activeSession ? activeSession.mode : prefs.mode,
      autoRecord: Boolean(prefs.autoRecord),
      callDetected: isCallCurrentlyDetected(),
      signalingActive: Boolean(callState.signalingActive),
      partnerName: effectivePartnerName(),
      isVideoCall: Boolean(
        callState.isVideoCall ||
          countLiveMapTracks(remoteTracks, 'video') > 0 ||
          countLiveMapTracks(localTracks, 'video') > 0
      ),
      elapsedMs: getSessionElapsedMs(activeSession),
      bytesRecorded: activeSession ? activeSession.totalBytes : 0,
      fileName: activeSession ? activeSession.fileName : null,
      peerConnectionCount: countOpenPeerConnections(),
      remoteTrackCount: countLiveMapTracks(remoteTracks),
      localTrackCount: countLiveMapTracks(localTracks),
      savedCount: cachedRecordingsMeta.length,
    };
  }

  installCallStreamTracker(globalScope);
  registerZcallIpcHooks();

  if (Array.isArray(host.popupWindowHooks)) {
    host.popupWindowHooks.push((childWin) => {
      try {
        installCallStreamTracker(childWin);
      } catch {}
    });
  }

  if (Array.isArray(host.deckMountHooks)) {
    host.deckMountHooks.push((ctx) => {
      mountCallRecorderDeck(ctx);
    });
  }

  listRecordings().catch(() => {});

  const api = {
    getState,
    getPrefs,
    setPrefs,
    start: startRecording,
    stop: stopRecording,
    toggle: toggleRecording,
    pause: pauseRecording,
    resume: resumeRecording,
    listRecordings,
    downloadRecording,
    deleteRecording,
    clearRecordings,
    notifyCallSignaling,
    installTracker: installCallStreamTracker,
  };

  globalScope.__olazCallRecorder = api;
}

bootstrapCallRecorderRuntime(
  typeof globalThis !== 'undefined'
    ? globalThis
    : typeof self !== 'undefined'
      ? self
      : window
);
