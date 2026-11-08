// Olaz Linux — Embedded Frontend Application & WebKit2GTK Bridge
// Implements the Rust <-> UI bridge contract, Omarchy live desktop theming,
// native <dialog> workflows, and all 5 Vietnamese Telex/VNI IME guards.

import { icon } from './icons.js';
import { createDemoState, DEMO_STICKERS } from './demo.js';
import {
  ZALO_REACTIONS,
  foldVietnamese,
  escapeHtml,
  resolveMediaUrl,
  unwrapExternalMediaUrl,
  formatBytes,
  formatShortTime,
  formatDateLabel,
  formatDuration,
  renderAvatar,
  renderLifecycleScreen,
  normalizeMessageKind,
  renderSafeVideoCard,
  renderMessageBubbleContent,
  renderDeliveryStatus,
} from './components.js';

/* ==========================================================================
   1. Reactive Store & Local UI State
   ========================================================================== */

function hasNativeBridge() {
  return (
    typeof window.webkit?.messageHandlers?.olaz_ui?.postMessage === 'function' ||
    typeof window.webkit?.messageHandlers?.__olaz_ui_raw?.postMessage === 'function'
  );
}

const isExplicitDemo =
  new URLSearchParams(window.location.search).get('demo') === '1' ||
  (window.location.protocol !== 'olaz-ui:' && !hasNativeBridge());

function createCleanNativeState() {
  return {
    phase: 'starting',
    capabilities: { gstAudioSink: true },
    runtimeProgress: null,
    login: {
      qr: null,
      invalid: false,
      text: '',
      avatar: null,
      waitingConfirm: null,
      kickoutNotice: null,
      error: null,
    },
    user: null,
    conversations: [],
    convExtras: { inboxTitle: 'Tin nhắn', archivedEnabled: false, labels: [], strangerIds: [], otherIds: [] },
    convActions: { hidden: { enabled: false, hasPin: false, ids: [] }, flags: {} },
    activeConvId: null,
    messagesByConv: {},
    typingByConv: {},
    pinsByConv: {},
    contacts: { friends: [], groups: [], received: [], sent: [], pendingRequestCount: 0, search: { status: 'idle' } },
    cloud: { plan: null, types: null, conversations: [], settings: {} },
    sharedMediaByConv: {},
    transfer: null,
    stickers: { categories: [], items: [], suggestions: [] },
    polls: {},
    settings: {
      theme: 'system',
      language: 'vi',
      notifications: true,
      sound: true,
      soundVolume: 80,
      enterToSend: true,
      stickerSuggest: true,
      autoDownloadMb: 25,
      omarchySync: true,
    },
    toast: null,
    incomingCall: null,
  };
}

// NEVER fall back to createDemoState() on olaz-ui:// unless ?demo=1 is explicitly requested
const store = isExplicitDemo
  ? {
      capabilities: { gstAudioSink: true },
      ...createDemoState(),
    }
  : createCleanNativeState();

window.__olazStore = store;

const ui = {
  destination: 'inbox', // inbox | unread | other | strangers | hidden | myCloud | contacts | zcloud
  contactsTab: 'friends', // friends | groups | requests | search
  labelFilter: '',
  convSearch: '',
  railMode: 'auto', // 'auto' | 'expanded' | 'collapsed'
  railCollapsed: false,
  showInspector: false,
  inspectorTab: 'images', // images | files | links
  mobilePane: 'list', // list | stage (start on list so <=720px Hyprland tiles never show an empty stage)
  unlockedHidden: false,
  demoPin: '1234',

  // Composer state & all 5 Vietnamese Telex/VNI IME guard variables
  composerConvId: null,
  replyTo: null, // { id, senderName, text }
  mentions: [], // [{ uid, pos, len, isAll, text }]
  mentionPopup: { open: false, query: '', startPos: 0, candidates: [], activeIndex: 0 },
  pendingPasteBase64: null,
  pendingForwardMsgIds: [],
  activePollId: null,
  hiddenPinContext: { mode: 'unlock', convId: null }, // unlock | hide_conv | set | change | reset
  qsActiveIndex: 0,
  qsItems: [],
  loadingOlderConvId: null,
  lastTypingSentAt: 0,
};

function isEnglishLang() {
  return String(store.settings?.language || 'vi').toLowerCase().startsWith('en');
}

function isNarrowRailViewport() {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia('(max-width: 1060px)').matches
    : false;
}

function isRailCollapsed() {
  if (ui.railMode === 'collapsed') return true;
  if (ui.railMode === 'expanded') return false;
  return isNarrowRailViewport();
}

let lastContactsLoadAt = 0;
let lastZcloudLoadAt = 0;

function requestContactsLoad({ force = false } = {}) {
  const now = Date.now();
  if (!force && now - lastContactsLoadAt < 1500) return;
  lastContactsLoadAt = now;
  window.olazInvoke('load_contacts').catch(() => {});
}

function requestZcloudLoad({ force = false } = {}) {
  const now = Date.now();
  if (!force && now - lastZcloudLoadAt < 1500) return;
  lastZcloudLoadAt = now;
  window.olazInvoke('load_zcloud').catch(() => {});
}

function canUseInlineGstAudio() {
  return !(store.capabilities && store.capabilities.gstAudioSink === false);
}

// Mandatory Vietnamese Telex/VNI IME Guards (fcitx5-bamboo / ibus-bamboo)
let imeComposing = false;
let deferredDraft = null;
let stickerSuggestTimer = null;

// Single lazy Audio controller — created ONLY on user click when gstAudioSink !== false
const activeVoice = {
  msgId: null,
  src: '',
  audio: null,
  playing: false,
  currentTime: 0,
  duration: 0,
};

function getVoiceStateForMessage(msgId) {
  if (!msgId || activeVoice.msgId !== msgId) return null;
  const dur = activeVoice.duration > 0 ? activeVoice.duration : 0;
  const progressPct =
    dur > 0 ? Math.min(100, Math.max(0, (activeVoice.currentTime / dur) * 100)) : 0;
  return {
    playing: activeVoice.playing,
    currentTime: activeVoice.currentTime,
    duration: dur,
    progressPct,
  };
}

function syncVoiceDom(msgId) {
  if (!msgId || typeof CSS === 'undefined' || typeof CSS.escape !== 'function') return;
  const escaped = CSS.escape(String(msgId));
  const root = document.querySelector(`[data-voice-msg="${escaped}"]`);
  if (!root) return;

  const vs = getVoiceStateForMessage(msgId);
  const isPlaying = Boolean(vs?.playing);
  const progressPct = vs ? vs.progressPct : 0;

  root.classList.toggle('is-playing', isPlaying);
  root.setAttribute('data-voice-playing', isPlaying ? 'true' : 'false');

  const btn = root.querySelector('[data-action="toggle-voice"]');
  const fallbackDur = Number(btn?.getAttribute('data-duration')) || 0;
  if (btn) {
    btn.innerHTML = icon(isPlaying ? 'pause' : 'play');
  }

  const wave = root.querySelector(`[data-voice-waveform="${escaped}"]`);
  if (wave) {
    wave.style.setProperty('--voice-progress', `${progressPct.toFixed(1)}%`);
    const bars = wave.querySelectorAll('.voice-bar');
    const count = bars.length || 1;
    bars.forEach((bar, idx) => {
      const threshold = ((idx + 0.5) / count) * 100;
      bar.classList.toggle('is-played', progressPct >= threshold);
    });
  }

  const label = root.querySelector(`[data-voice-label="${escaped}"]`);
  if (label) {
    const shown =
      vs && (isPlaying || vs.currentTime > 0)
        ? vs.currentTime
        : vs?.duration || fallbackDur;
    label.textContent = formatDuration(shown);
  }
}

function stopActiveVoiceAudio() {
  const prevMsgId = activeVoice.msgId;
  const audio = activeVoice.audio;
  activeVoice.msgId = null;
  activeVoice.src = '';
  activeVoice.audio = null;
  activeVoice.playing = false;
  activeVoice.currentTime = 0;
  activeVoice.duration = 0;
  if (audio) {
    try {
      audio.onloadedmetadata = null;
      audio.ontimeupdate = null;
      audio.onplay = null;
      audio.onpause = null;
      audio.onended = null;
      audio.onerror = null;
      audio.pause();
      audio.removeAttribute('src');
    } catch {
      // Ignore cleanup errors
    }
  }
  if (prevMsgId) {
    syncVoiceDom(prevMsgId);
  }
}

async function toggleVoicePlayback(actionEl) {
  const msgId = actionEl.getAttribute('data-msg-id') || '';
  const voiceSrc = actionEl.getAttribute('data-voice-src') || '';
  const fallbackDur = Number(actionEl.getAttribute('data-duration')) || 0;

  if (!voiceSrc) {
    if (store.activeConvId && msgId) {
      await window.olazInvoke('resolve_media', {
        convId: store.activeConvId,
        msgId,
        kind: 'voice',
      });
    }
    return;
  }

  // Guard against WebKit2GTK MediaPlayerPrivateGStreamer::createAudioSink() SIGABRT
  if (!canUseInlineGstAudio()) {
    const externalUrl = unwrapExternalMediaUrl(voiceSrc);
    showToast(
      isEnglishLang()
        ? 'Inline audio requires gst-plugins-good — opening in external player…'
        : 'Phát âm thanh trực tiếp cần gói gst-plugins-good — đang mở bằng trình phát ngoài…'
    );
    if (externalUrl) {
      await window.olazInvoke('open_media_external', { url: externalUrl });
    }
    return;
  }

  if (activeVoice.msgId === msgId && activeVoice.audio) {
    if (activeVoice.audio.paused) {
      try {
        await activeVoice.audio.play();
      } catch {
        activeVoice.playing = false;
        syncVoiceDom(msgId);
      }
    } else {
      activeVoice.audio.pause();
    }
    return;
  }

  stopActiveVoiceAudio();

  const audio = new Audio();
  audio.preload = 'auto';
  audio.src = voiceSrc;

  activeVoice.msgId = msgId;
  activeVoice.src = voiceSrc;
  activeVoice.audio = audio;
  activeVoice.playing = true;
  activeVoice.currentTime = 0;
  activeVoice.duration = fallbackDur;
  syncVoiceDom(msgId);

  audio.onloadedmetadata = () => {
    if (activeVoice.audio !== audio) return;
    if (Number.isFinite(audio.duration) && audio.duration > 0) {
      activeVoice.duration = audio.duration;
      syncVoiceDom(msgId);
    }
  };

  audio.onplay = () => {
    if (activeVoice.audio !== audio) return;
    activeVoice.playing = true;
    syncVoiceDom(msgId);
  };

  audio.onpause = () => {
    if (activeVoice.audio !== audio) return;
    activeVoice.playing = false;
    syncVoiceDom(msgId);
  };

  audio.ontimeupdate = () => {
    if (activeVoice.audio !== audio) return;
    activeVoice.currentTime = Number(audio.currentTime) || 0;
    if (Number.isFinite(audio.duration) && audio.duration > 0) {
      activeVoice.duration = audio.duration;
    }
    syncVoiceDom(msgId);
  };

  audio.onended = () => {
    if (activeVoice.audio !== audio) return;
    activeVoice.playing = false;
    activeVoice.currentTime = 0;
    syncVoiceDom(msgId);
  };

  audio.onerror = () => {
    if (activeVoice.audio !== audio) return;
    stopActiveVoiceAudio();
    showToast(
      isEnglishLang()
        ? 'Unable to play voice message inline.'
        : 'Không thể phát trực tiếp tin nhắn thoại.'
    );
  };

  try {
    await audio.play();
  } catch {
    if (activeVoice.audio === audio) {
      stopActiveVoiceAudio();
    }
  }
}

function seekVoicePlayback(actionEl, clientX) {
  const msgId = actionEl.getAttribute('data-msg-id') || '';
  if (!msgId || activeVoice.msgId !== msgId || !activeVoice.audio || !canUseInlineGstAudio()) {
    return;
  }
  const rect = actionEl.getBoundingClientRect();
  if (rect.width <= 0) return;
  const dur =
    Number.isFinite(activeVoice.audio.duration) && activeVoice.audio.duration > 0
      ? activeVoice.audio.duration
      : activeVoice.duration;
  if (!(dur > 0)) return;
  const ratio = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
  activeVoice.audio.currentTime = ratio * dur;
  activeVoice.currentTime = activeVoice.audio.currentTime;
  syncVoiceDom(msgId);
}

/* ==========================================================================
   2. Outbound & Inbound Rust <-> UI Bridge
   ========================================================================== */

let invokeSeq = 0;
const pendingReplies = new Map();

window.__olazReply = function __olazReply(id, ok, dataOrErr) {
  const entry = pendingReplies.get(id);
  if (!entry) return;
  pendingReplies.delete(id);
  clearTimeout(entry.timer);
  if (ok) {
    entry.resolve(dataOrErr);
  } else {
    const message =
      typeof dataOrErr === 'string'
        ? dataOrErr
        : dataOrErr?.message || dataOrErr?.error || 'Thao tác không thành công';
    entry.reject(new Error(message));
  }
};

window.olazInvoke = async function olazInvoke(cmd, args = {}) {
  if (cmd === 'select_conversation' && args?.convId) {
    ui.mobilePane = 'stage';
    syncShellClasses();
  }

  // Route to demo responder ONLY when explicit demo mode is active
  if (isExplicitDemo) {
    return handleDemoCommand(cmd, args);
  }

  const mh = window.webkit?.messageHandlers;
  const olazHandler = mh?.olaz_ui;
  const rawHandler = mh?.__olaz_ui_raw;
  const hasOlaz = typeof olazHandler?.postMessage === 'function';
  const hasRaw = typeof rawHandler?.postMessage === 'function';

  if (!hasOlaz && !hasRaw) {
    const msg = isEnglishLang()
      ? 'Native bridge unavailable — please restart Olaz'
      : 'Bridge chưa sẵn sàng — vui lòng khởi động lại Olaz';
    if (cmd !== 'get_initial_state') {
      showToast(msg);
    }
    const err = new Error(msg);
    err._toasted = true;
    throw err;
  }

  const id = `olaz_${++invokeSeq}_${Date.now()}`;
  const payload = { id, cmd, args };

  let postResult;
  const callbackPromise = new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      if (pendingReplies.has(id)) {
        pendingReplies.delete(id);
        reject(
          new Error(
            isEnglishLang()
              ? `Timed out waiting for ${cmd}`
              : `Quá thời gian phản hồi cho lệnh ${cmd}`
          )
        );
      }
    }, 30000);
    pendingReplies.set(id, { resolve, reject, timer });
  });

  try {
    postResult = hasOlaz
      ? olazHandler.postMessage(payload)
      : rawHandler.postMessage(JSON.stringify(payload));
  } catch (err) {
    const entry = pendingReplies.get(id);
    if (entry) {
      clearTimeout(entry.timer);
      pendingReplies.delete(id);
    }
    throw err;
  }

  // Support WebKit2GTK register_script_message_handler_with_reply Promise return directly
  if (postResult && typeof postResult.then === 'function') {
    const entry = pendingReplies.get(id);
    if (entry) {
      clearTimeout(entry.timer);
      pendingReplies.delete(id);
    }
    const direct = await postResult;
    if (direct && typeof direct === 'object' && 'ok' in direct) {
      if (direct.ok) return direct.data ?? direct.result ?? direct;
      throw new Error(direct.error || 'Thao tác không thành công');
    }
    return direct;
  }

  return callbackPromise;
};

/**
 * Inbound state snapshot or partial patch from Rust (`ui_window.rs`).
 * Exposed on both `window.__olazApplyState` and `window.__olazPushState`.
 */
function applyIncomingState(snapshotOrPatch) {
  if (!snapshotOrPatch || typeof snapshotOrPatch !== 'object') return;

  const p = snapshotOrPatch;
  const prevPhase = store.phase;
  const prevConvId = store.activeConvId;

  if (p.capabilities && typeof p.capabilities === 'object') {
    const gstVal = p.capabilities.gstAudioSink ?? p.capabilities.gst_audio_sink;
    store.capabilities = {
      ...store.capabilities,
      ...p.capabilities,
      ...(typeof gstVal === 'boolean' ? { gstAudioSink: gstVal } : {}),
    };
    if (!canUseInlineGstAudio()) {
      stopActiveVoiceAudio();
    }
  }

  if (p.phase !== undefined) store.phase = p.phase;
  if (p.runtimeProgress !== undefined || p.runtime_progress !== undefined) {
    store.runtimeProgress = p.runtimeProgress ?? p.runtime_progress;
  }
  if (p.login !== undefined) {
    if (p.login && typeof p.login === 'object') {
      const rawConfirm = p.login.waitingConfirm ?? p.login.waiting_confirm ?? null;
      store.login = {
        qr: p.login.qr ?? null,
        invalid: Boolean(p.login.invalid),
        text: p.login.text ?? '',
        avatar: p.login.avatar ?? null,
        waitingConfirm:
          rawConfirm && typeof rawConfirm === 'object'
            ? {
                name: rawConfirm.name ?? '',
                avatar: rawConfirm.avatar ?? null,
              }
            : null,
        kickoutNotice: p.login.kickoutNotice ?? p.login.kickout_notice ?? null,
        error: p.login.error ?? null,
      };
    } else {
      store.login = p.login;
    }
  }
  if (p.user !== undefined) store.user = p.user;
  if (Array.isArray(p.conversations)) store.conversations = p.conversations;
  if (p.convExtras || p.conv_extras) {
    store.convExtras = { ...store.convExtras, ...(p.convExtras || p.conv_extras) };
  }
  if (p.convActions || p.conv_actions) {
    store.convActions = { ...store.convActions, ...(p.convActions || p.conv_actions) };
  }
  if (p.activeConvId !== undefined || p.active_conv_id !== undefined) {
    store.activeConvId = p.activeConvId ?? p.active_conv_id;
  }
  if (p.messagesByConv || p.messages_by_conv) {
    const incomingMap = p.messagesByConv || p.messages_by_conv;
    for (const [cid, val] of Object.entries(incomingMap)) {
      store.messagesByConv[cid] = Array.isArray(val) ? { items: val, hasMore: false } : val;
    }
  }
  if (p.typingByConv || p.typing_by_conv) {
    store.typingByConv = { ...store.typingByConv, ...(p.typingByConv || p.typing_by_conv) };
  }
  if (p.pinsByConv || p.pins_by_conv) {
    store.pinsByConv = { ...store.pinsByConv, ...(p.pinsByConv || p.pins_by_conv) };
  }
  if (p.contacts) {
    store.contacts = { ...store.contacts, ...p.contacts };
    const createGroupDlg = document.getElementById('create-group-dialog');
    if (createGroupDlg?.open) {
      const filterVal = document.getElementById('group-member-filter')?.value || '';
      renderGroupMemberChecklist(filterVal);
    }
  }
  if (p.cloud) store.cloud = { ...store.cloud, ...p.cloud };
  if (p.sharedMediaByConv || p.shared_media_by_conv) {
    store.sharedMediaByConv = {
      ...store.sharedMediaByConv,
      ...(p.sharedMediaByConv || p.shared_media_by_conv),
    };
  }
  if ('transfer' in p) store.transfer = p.transfer;
  if (p.stickers) store.stickers = { ...store.stickers, ...p.stickers };
  if (p.polls) store.polls = { ...store.polls, ...p.polls };
  if (p.settings) {
    store.settings = { ...store.settings, ...p.settings };
    applyUserThemePref(store.settings.theme);
    applyUserLanguagePref(store.settings.language);
  }
  if ('incomingCall' in p || 'incoming_call' in p) {
    store.incomingCall = p.incomingCall ?? p.incoming_call ?? null;
  }

  // Guard 2: If a failed draft arrives while Vietnamese IME is composing, stash in deferredDraft!
  const incomingDraft = p.failedDraft ?? p.failed_draft;
  if (incomingDraft && incomingDraft.text) {
    if (imeComposing) {
      deferredDraft = deferredDraft
        ? { ...deferredDraft, text: `${deferredDraft.text}\n${incomingDraft.text}` }
        : incomingDraft;
    } else {
      applyFailedDraftToComposer(incomingDraft);
    }
  }

  if (p.toast) {
    showToast(p.toast.text || String(p.toast), p.toast.undo || null);
  }

  if (store.phase !== 'ready') {
    stopActiveVoiceAudio();
  }

  if (store.phase === 'login' && prevPhase !== 'login') {
    ui.unlockedHidden = false;
  }

  if (
    !store.activeConvId &&
    ui.mobilePane === 'stage' &&
    ui.destination !== 'contacts' &&
    ui.destination !== 'zcloud'
  ) {
    ui.mobilePane = 'list';
  }

  renderRoot({ phaseChanged: prevPhase !== store.phase, convChanged: prevConvId !== store.activeConvId });
}

window.__olazApplyState = applyIncomingState;
window.__olazPushState = applyIncomingState;

/**
 * Inbound Omarchy / desktop theme hook (`window.__olazApplyDesktopTheme(theme)`).
 */
window.__olazApplyDesktopTheme = function __olazApplyDesktopTheme(theme) {
  if (!theme || typeof theme !== 'object') return;
  const root = document.documentElement;
  const isOmarchy = Boolean(theme.is_omarchy ?? theme.isOmarchy);
  const mode = theme.mode || 'system';
  const radiusPx = theme.radius_px ?? theme.radiusPx;

  root.dataset.omarchy = isOmarchy ? 'true' : 'false';
  if (mode === 'dark' || mode === 'light') {
    root.dataset.theme = mode;
  } else {
    delete root.dataset.theme;
  }

  const setVar = (name, val) => {
    if (val !== undefined && val !== null && val !== '') {
      root.style.setProperty(name, String(val));
    }
  };

  setVar('--omarchy-bg', theme.background);
  setVar('--omarchy-darker-bg', theme.darker_background ?? theme.darkerBackground);
  setVar('--omarchy-lighter-bg', theme.lighter_background ?? theme.lighterBackground);
  setVar('--omarchy-fg', theme.foreground);
  setVar('--omarchy-muted', theme.muted);
  setVar('--omarchy-selection', theme.selection);
  setVar('--omarchy-red', theme.red);
  setVar('--accent', theme.accent);
  setVar('--accent-fg', theme.accent_fg ?? theme.accentFg);

  if (radiusPx !== undefined && radiusPx !== null && Number.isFinite(Number(radiusPx))) {
    root.style.setProperty('--radius', `${Math.max(0, Number(radiusPx))}px`);
  }

  if (store.settings?.language) {
    applyUserLanguagePref(store.settings.language);
  } else if (theme.language) {
    root.lang = String(theme.language);
  }
  applyUserThemePref(store.settings?.theme || 'system');
};

function applyUserThemePref(mode) {
  const root = document.documentElement;
  if (root.dataset.omarchy === 'true' && store.settings?.omarchySync !== false) return;
  if (mode === 'dark' || mode === 'light') {
    root.dataset.theme = mode;
  } else {
    delete root.dataset.theme;
  }
}

function applyUserLanguagePref(lang) {
  const clean = String(lang || 'vi').toLowerCase();
  document.documentElement.lang = clean.startsWith('en') ? 'en' : 'vi';
}

/* ==========================================================================
   3. Standalone Demo Command Responder (active when ?demo=1 or non-olaz-ui)
   ========================================================================== */

async function handleDemoCommand(cmd, args = {}) {
  const now = Date.now();
  switch (cmd) {
    case 'get_initial_state':
      return store;

    case 'retry_runtime_install':
      store.phase = 'login';
      renderRoot({ phaseChanged: true });
      return { ok: true };

    case 'refresh_qr':
      if (!store.login || typeof store.login !== 'object') {
        store.login = {
          qr: null,
          invalid: false,
          text: '',
          avatar: null,
          waitingConfirm: null,
          kickoutNotice: null,
          error: null,
        };
      }
      store.login.invalid = false;
      store.login.waitingConfirm = null;
      store.login.error = null;
      showToast(isEnglishLang() ? 'Refreshed sign-in QR code.' : 'Đã làm mới mã QR đăng nhập.');
      renderRoot({});
      return { ok: true };

    case 'open_media_external':
      if (args.url && typeof window.open === 'function') {
        window.open(args.url, '_blank', 'noopener,noreferrer');
      }
      showToast(
        isEnglishLang()
          ? 'Opened media in external player.'
          : 'Đã mở phương tiện bằng trình phát ngoài.'
      );
      return { ok: true };

    case 'logout':
      store.phase = 'login';
      ui.unlockedHidden = false;
      closeAllDialogs();
      renderRoot({ phaseChanged: true });
      return { ok: true };

    case 'toggle_debug_webview':
      showToast('Đã chuyển trạng thái hiển thị WebView nền.');
      return { ok: true };

    case 'transfer_start_manual':
      store.transfer = {
        phase: 'offer',
        officialTitle: 'Đồng bộ tin nhắn từ điện thoại',
        officialDescription: 'Xác nhận trên ứng dụng Zalo điện thoại để chuyển lịch sử trò chuyện sang máy tính Linux.',
      };
      renderRoot({});
      return { ok: true };

    case 'transfer_accept':
      store.transfer = {
        phase: 'transferring',
        stage: 'messages',
        progress: 0.64,
        receivedMessages: 1420,
      };
      renderRoot({});
      setTimeout(() => {
        if (store.transfer?.phase === 'transferring') {
          store.transfer = null;
          showToast('Đã đồng bộ xong 1.420 tin nhắn từ điện thoại.');
          renderRoot({});
        }
      }, 1600);
      return { ok: true };

    case 'transfer_reject':
    case 'transfer_cancel':
      store.transfer = null;
      renderRoot({});
      return { ok: true };

    case 'select_conversation': {
      const convId = args.convId;
      store.activeConvId = convId;
      const conv = store.conversations.find((c) => c.id === convId);
      if (conv) conv.unread = 0;
      ui.mobilePane = 'stage';
      renderRoot({ convChanged: true });
      return { ok: true };
    }

    case 'load_older': {
      const convId = args.convId;
      const bucket = store.messagesByConv[convId] || { items: [], hasMore: false };
      const earliest = bucket.items[0]?.date || now - 7200 * 1000;
      const olderBatch = [
        {
          id: `older_${now}_1`,
          conversationId: convId,
          senderId: 'u-minh-anh',
          senderName: 'Minh Anh',
          isMine: false,
          kind: { type: 'text' },
          text: 'Tin nhắn lịch sử trước đó: Đã kiểm tra cuộn mượt và giữ nguyên vị trí khung nhìn khi tải thêm.',
          date: earliest - 5400 * 1000,
          status: 'seen',
          reactions: [],
        },
        {
          id: `older_${now}_2`,
          conversationId: convId,
          senderId: 'u-self',
          senderName: store.user?.name || 'Bạn',
          isMine: true,
          kind: { type: 'text' },
          text: 'Chuẩn rồi, thanh cuộn không bị giật về đầu trang.',
          date: earliest - 4800 * 1000,
          status: 'seen',
          reactions: [],
        },
      ];
      bucket.items = [...olderBatch, ...bucket.items];
      bucket.hasMore = false;
      store.messagesByConv[convId] = bucket;
      return { ok: true, count: olderBatch.length };
    }

    case 'conv_action': {
      const conv = store.conversations.find((c) => c.id === args.convId);
      if (!conv) return { ok: false };
      if (args.action === 'pin') conv.pinned = true;
      if (args.action === 'unpin') conv.pinned = false;
      if (args.action === 'mark_read') conv.unread = 0;
      if (args.action === 'mark_unread') conv.unread = Math.max(1, conv.unread || 1);
      if (args.action === 'mute') conv.isMuted = true;
      if (args.action === 'unmute') conv.isMuted = false;
      if (args.action === 'hide') {
        const ids = new Set(store.convActions.hidden?.ids || []);
        ids.add(conv.id);
        store.convActions.hidden = { enabled: true, hasPin: true, ids: [...ids] };
        showToast('Đã ẩn cuộc trò chuyện vào mục bảo mật PIN.');
      }
      if (args.action === 'unhide') {
        store.convActions.hidden.ids = (store.convActions.hidden?.ids || []).filter((x) => x !== conv.id);
        showToast('Đã bỏ ẩn cuộc trò chuyện.');
      }
      renderRoot({});
      return { ok: true };
    }

    case 'set_conv_label': {
      const conv = store.conversations.find((c) => c.id === args.convId);
      if (conv) conv.labelId = args.labelId || '';
      renderRoot({});
      return { ok: true };
    }

    case 'unlock_hidden_pin': {
      if (String(args.pin) !== ui.demoPin) {
        throw new Error('Mã PIN chưa chính xác (mã thử nghiệm: 1234).');
      }
      ui.unlockedHidden = true;
      ui.destination = 'hidden';
      renderRoot({});
      return { ok: true };
    }

    case 'manage_pin': {
      if (args.op === 'change') {
        if (String(args.pin) !== ui.demoPin) throw new Error('Mã PIN hiện tại không đúng.');
        ui.demoPin = String(args.newPin || '1234');
        showToast('Đã đổi mã PIN trò chuyện ẩn.');
      } else if (args.op === 'set') {
        ui.demoPin = String(args.newPin || args.pin || '1234');
        store.convActions.hidden.hasPin = true;
        showToast('Đã thiết lập mã PIN trò chuyện ẩn.');
      } else if (args.op === 'reset') {
        ui.demoPin = '1234';
        showToast('Đã đặt lại mã PIN mặc định.');
      }
      renderRoot({});
      return { ok: true };
    }

    case 'send_text': {
      const { convId, text, mentions, replyTo } = args;
      appendDemoMessage(convId, {
        id: `msg_${now}`,
        conversationId: convId,
        senderId: store.user?.id || 'u-self',
        senderName: store.user?.name || 'Bạn',
        isMine: true,
        kind: { type: 'text' },
        text,
        mentions: mentions || [],
        quote: replyTo ? { senderName: replyTo.senderName, text: replyTo.text } : null,
        date: now,
        status: 'seen',
        reactions: [],
      });
      return { ok: true };
    }

    case 'paste_clipboard_image': {
      const dataUrl = `data:image/png;base64,${args.base64Png}`;
      appendDemoMessage(args.convId, {
        id: `msg_img_${now}`,
        conversationId: args.convId,
        senderId: store.user?.id || 'u-self',
        senderName: store.user?.name || 'Bạn',
        isMine: true,
        kind: { type: 'photo', url: dataUrl, width: 640, height: 400 },
        text: '[Hình ảnh]',
        date: now,
        status: 'seen',
        reactions: [],
      });
      return { ok: true };
    }

    case 'pick_and_send_media':
    case 'pick_and_send_files':
    case 'send_dropped_paths': {
      const names = Array.isArray(args.paths) && args.paths.length > 0
        ? args.paths.map((p) => String(p).split('/').pop())
        : [cmd === 'pick_and_send_media' ? 'anh-chup-man-hinh.png' : 'tai-lieu-dinh-kem.pdf'];
      for (const name of names) {
        appendDemoMessage(args.convId, {
          id: `msg_file_${now}_${Math.random().toString(36).slice(2, 6)}`,
          conversationId: args.convId,
          senderId: store.user?.id || 'u-self',
          senderName: store.user?.name || 'Bạn',
          isMine: true,
          kind: { type: 'file', name, size: 324800, path: `/tmp/${name}` },
          text: `[Tệp] ${name}`,
          date: now,
          status: 'seen',
          reactions: [],
        });
      }
      return { ok: true };
    }

    case 'capture_screenshot': {
      const targetId = args.toMyCloud ? store.convExtras.sendToMeId || args.convId : args.convId;
      showToast('Đã gọi XDG Screenshot Portal và gửi ảnh chụp màn hình.');
      appendDemoMessage(targetId, {
        id: `msg_shot_${now}`,
        conversationId: targetId,
        senderId: store.user?.id || 'u-self',
        senderName: store.user?.name || 'Bạn',
        isMine: true,
        kind: { type: 'file', name: `wayland-screenshot-${now}.png`, size: 418200, path: `/tmp/shot.png` },
        text: '[Ảnh chụp màn hình]',
        date: now,
        status: 'seen',
        reactions: [],
      });
      return { ok: true };
    }

    case 'send_sticker': {
      const found = (store.stickers.items || DEMO_STICKERS).find((s) => s.id === args.stickerId) || DEMO_STICKERS[0];
      appendDemoMessage(args.convId, {
        id: `msg_stk_${now}`,
        conversationId: args.convId,
        senderId: store.user?.id || 'u-self',
        senderName: store.user?.name || 'Bạn',
        isMine: true,
        kind: { type: 'sticker', url: found.url },
        text: '[Sticker]',
        date: now,
        status: 'seen',
        reactions: [],
      });
      store.stickers.suggestions = [];
      renderRoot({});
      return { ok: true };
    }

    case 'query_sticker_suggestions': {
      const kw = foldVietnamese(args.keyword || '').trim();
      if (!kw || !store.settings.stickerSuggest) {
        store.stickers.suggestions = [];
      } else {
        store.stickers.suggestions = DEMO_STICKERS.filter((s) => s.keyword.includes(kw) || kw.includes(s.keyword.split(' ')[0]));
      }
      renderComposerPopups();
      return { ok: true, items: store.stickers.suggestions };
    }

    case 'send_typing':
      return { ok: true };

    case 'react_message': {
      const bucket = store.messagesByConv[args.convId];
      const msg = bucket?.items?.find((m) => m.id === args.msgId);
      if (msg) {
        msg.reactions = msg.reactions || [];
        const existing = msg.reactions.find((r) => r.emoji === args.reaction);
        if (existing) {
          if (!existing.includesMe) {
            existing.count += 1;
            existing.includesMe = true;
          }
        } else {
          msg.reactions.push({ emoji: args.reaction, count: 1, includesMe: true, names: ['Bạn'], people: 1 });
        }
        renderStageTimeline({ preserveScroll: true });
      }
      return { ok: true };
    }

    case 'recall_message': {
      const msg = store.messagesByConv[args.convId]?.items?.find((m) => m.id === args.msgId);
      if (msg) {
        msg.isRecalled = true;
        msg.kind = { type: 'recalled' };
        renderStageTimeline({ preserveScroll: true });
      }
      return { ok: true };
    }

    case 'delete_message_for_me': {
      const bucket = store.messagesByConv[args.convId];
      if (bucket?.items) {
        const idx = bucket.items.findIndex((m) => m.id === args.msgId);
        if (idx >= 0) {
          const [removed] = bucket.items.splice(idx, 1);
          ui._lastDeleted = { convId: args.convId, index: idx, msg: removed };
          renderStageTimeline({ preserveScroll: true });
          showToast('Đã xóa tin nhắn ở phía bạn.', { convId: args.convId, msgId: args.msgId });
        }
      }
      return { ok: true };
    }

    case 'undo_delete': {
      if (ui._lastDeleted && ui._lastDeleted.msg) {
        const { convId, index, msg } = ui._lastDeleted;
        const bucket = store.messagesByConv[convId];
        if (bucket?.items) {
          bucket.items.splice(Math.min(index, bucket.items.length), 0, msg);
          ui._lastDeleted = null;
          renderStageTimeline({ preserveScroll: true });
          showToast('Đã khôi phục tin nhắn.');
        }
      }
      return { ok: true };
    }

    case 'forward_messages': {
      const srcBucket = store.messagesByConv[args.fromConvId];
      const msgs = (srcBucket?.items || []).filter((m) => args.msgIds.includes(m.id));
      for (const targetId of args.targetConvIds || []) {
        for (const m of msgs) {
          appendDemoMessage(targetId, {
            ...structuredClone(m),
            id: `fwd_${now}_${Math.random().toString(36).slice(2, 6)}`,
            conversationId: targetId,
            senderId: store.user?.id || 'u-self',
            senderName: store.user?.name || 'Bạn',
            isMine: true,
            date: now,
            status: 'sent',
            reactions: [],
          });
        }
      }
      showToast(`Đã chuyển tiếp đến ${(args.targetConvIds || []).length} cuộc trò chuyện.`);
      return { ok: true };
    }

    case 'pin_message': {
      const msg = store.messagesByConv[args.convId]?.items?.find((m) => m.id === args.msgId);
      if (msg) {
        store.pinsByConv[args.convId] = store.pinsByConv[args.convId] || [];
        store.pinsByConv[args.convId].unshift({
          ref: `pin_${now}`,
          msgId: msg.id,
          kind: normalizeMessageKind(msg).type,
          title: msg.text || 'Tin nhắn đã ghim',
          sender: msg.senderName,
        });
        showToast('Đã ghim tin nhắn lên đầu hội thoại.');
        renderStageHeader();
      }
      return { ok: true };
    }

    case 'unpin_message': {
      const list = store.pinsByConv[args.convId] || [];
      store.pinsByConv[args.convId] = list.filter((p) => p.ref !== args.ref && p.msgId !== args.msgId);
      showToast('Đã bỏ ghim tin nhắn.');
      renderStageHeader();
      return { ok: true };
    }

    case 'star_message':
      showToast(args.starred ? 'Đã đánh dấu sao tin nhắn.' : 'Đã bỏ đánh dấu sao.');
      return { ok: true };

    case 'resolve_media':
      showToast('Đang tải và giải mã tệp phương tiện…');
      return { ok: true };

    case 'open_downloaded_file':
      showToast(args.revealOnly ? `Đã mở thư mục chứa ${args.path}` : `Đã mở tệp ${args.path}`);
      return { ok: true };

    case 'load_contacts':
    case 'load_zcloud':
    case 'load_shared_media':
      return { ok: true };

    case 'friend_action': {
      if (args.action === 'accept' || args.action === 'decline') {
        store.contacts.received = (store.contacts.received || []).filter((r) => r.id !== args.userId);
        store.contacts.pendingRequestCount = store.contacts.received.length;
        showToast(args.action === 'accept' ? 'Đã đồng ý kết bạn.' : 'Đã từ chối lời mời kết bạn.');
      } else if (args.action === 'cancel') {
        store.contacts.sent = (store.contacts.sent || []).filter((r) => r.id !== args.userId);
        showToast('Đã thu hồi lời mời kết bạn.');
      } else if (args.action === 'send') {
        showToast('Đã gửi lời mời kết bạn.');
      }
      renderRoot({});
      return { ok: true };
    }

    case 'search_phone': {
      const phone = String(args.phone || '').trim();
      store.contacts.search = phone
        ? {
            status: 'found',
            user: {
              id: 'u-phone-demo',
              name: 'Trần Nhật Minh',
              zaloName: `Số điện thoại: ${phone}`,
              bio: 'Kỹ sư hệ thống Linux & Rust',
              isFriend: false,
            },
          }
        : { status: 'idle', user: null };
      renderRoot({});
      return { ok: true };
    }

    case 'create_group': {
      const newId = `conv_grp_${now}`;
      store.conversations.unshift({
        id: newId,
        name: args.name,
        isGroup: true,
        members: (args.memberIds?.length || 0) + 1,
        pinned: false,
        isMuted: false,
        unread: 0,
        preview: { text: 'Nhóm vừa được tạo', ts: now, fromMe: true },
      });
      store.messagesByConv[newId] = {
        hasMore: false,
        items: [
          {
            id: `sys_${now}`,
            conversationId: newId,
            senderId: 'sys',
            senderName: 'Hệ thống',
            isMine: false,
            kind: { type: 'system' },
            text: `Bạn đã tạo nhóm "${args.name}"`,
            date: now,
            status: 'seen',
            reactions: [],
          },
        ],
      };
      store.activeConvId = newId;
      ui.destination = 'inbox';
      ui.mobilePane = 'stage';
      renderRoot({ convChanged: true });
      showToast(`Đã tạo nhóm "${args.name}".`);
      return { ok: true };
    }

    case 'create_poll': {
      const pollId = `poll_${now}`;
      store.polls[pollId] = {
        id: pollId,
        groupId: args.convId,
        question: args.question,
        creatorName: store.user?.name || 'Bạn',
        created: now,
        closed: false,
        multi: Boolean(args.multi),
        allowAdd: Boolean(args.allowAdd),
        anonymous: Boolean(args.anonymous),
        voters: 0,
        options: (args.options || []).map((text, idx) => ({
          id: `opt_${now}_${idx}`,
          content: text,
          votes: 0,
          voted: false,
        })),
      };
      appendDemoMessage(args.convId, {
        id: `msg_poll_${now}`,
        conversationId: args.convId,
        senderId: store.user?.id || 'u-self',
        senderName: store.user?.name || 'Bạn',
        isMine: true,
        kind: { type: 'poll', pollId, question: args.question },
        text: `[Bình chọn] ${args.question}`,
        date: now,
        status: 'seen',
        reactions: [],
      });
      return { ok: true };
    }

    case 'vote_poll': {
      const poll = store.polls[args.pollId];
      if (poll) {
        const chosen = new Set(args.votedOptionIds || []);
        for (const opt of poll.options) {
          const wasVoted = Boolean(opt.voted);
          const nowVoted = chosen.has(opt.id);
          if (!wasVoted && nowVoted) opt.votes += 1;
          if (wasVoted && !nowVoted) opt.votes = Math.max(0, opt.votes - 1);
          opt.voted = nowVoted;
        }
        for (const extra of args.newOptions || []) {
          if (extra.content) {
            poll.options.push({
              id: `opt_new_${Date.now()}`,
              content: extra.content,
              votes: extra.voted ? 1 : 0,
              voted: Boolean(extra.voted),
            });
          }
        }
        poll.voters = Math.max(1, poll.voters);
        renderStageTimeline({ preserveScroll: true });
        showToast('Đã cập nhật lựa chọn bình chọn.');
      }
      return { ok: true };
    }

    case 'update_settings': {
      store.settings = { ...store.settings, ...args };
      applyUserThemePref(store.settings.theme);
      applyUserLanguagePref(store.settings.language);
      renderRoot({});
      showToast(isEnglishLang() ? 'Settings saved.' : 'Đã lưu thiết lập.');
      return { ok: true };
    }

    default:
      return { ok: true };
  }
}

function appendDemoMessage(convId, msg) {
  if (!convId) return;
  const bucket = store.messagesByConv[convId] || { items: [], hasMore: false };
  bucket.items.push(msg);
  store.messagesByConv[convId] = bucket;

  const conv = store.conversations.find((c) => c.id === convId);
  if (conv) {
    conv.preview = { text: msg.text || '[Tin nhắn]', ts: msg.date, fromMe: true };
  }
  renderListBody();
  if (store.activeConvId === convId) {
    renderStageTimeline({ scrollToBottom: true });
  }
}

/* ==========================================================================
   4. Targeted Zone Rendering (Preserves Focus, Scroll & Vietnamese IME!)
   ========================================================================== */

let mountedPhase = null;

function renderRoot({ phaseChanged = false, convChanged = false } = {}) {
  const appEl = document.getElementById('app');
  if (!appEl) return;

  renderIncomingCallBanner();

  if (store.phase !== 'ready') {
    mountedPhase = store.phase;
    appEl.innerHTML = renderLifecycleScreen(store, hasNativeBridge());
    return;
  }

  if (mountedPhase !== 'ready' || phaseChanged || !document.getElementById('shell-grid')) {
    mountedPhase = 'ready';
    const isEn = isEnglishLang();
    appEl.innerHTML = `
      <div id="shell-grid" class="shell-grid" data-mobile-pane="${escapeHtml(ui.mobilePane)}">
        <nav id="rail-zone" class="nav-rail" aria-label="${isEn ? 'Primary navigation' : 'Điều hướng chính'}"></nav>
        <section id="list-zone" class="list-column" aria-label="${isEn ? 'Conversation & directory list' : 'Danh sách'}">
          <div id="list-header-zone" class="column-header"></div>
          <div id="list-transfer-zone"></div>
          <div id="list-body-zone" class="list-body scroller"></div>
        </section>
        <main id="stage-zone" class="stage-column">
          <div id="stage-header-zone"></div>
          <div id="stage-timeline-zone" class="message-scroller scroller"></div>
          <div id="stage-typing-zone" class="typing-strip" aria-live="polite"></div>
          <div id="stage-composer-zone"></div>
          <div id="drop-overlay" class="drop-overlay" hidden>
            ${icon('download')}
            <strong>${isEn ? 'Drop files or images here to send' : 'Thả tệp hoặc hình ảnh vào đây để gửi ngay'}</strong>
            <span class="dialog-subtitle">${isEn ? 'Supports sending multiple files directly from Linux filesystem paths' : 'Hỗ trợ gửi nhiều tệp trực tiếp qua đường dẫn hệ thống Linux'}</span>
          </div>
        </main>
        <aside id="inspector-zone" class="inspector-column scroller" hidden aria-label="${isEn ? 'Conversation details' : 'Thông tin hội thoại'}"></aside>
      </div>
    `;
    mountListHeader();
    bindStageDragAndDrop();
  }

  syncShellClasses();
  renderRail();
  updateListHeaderVisibility();
  renderTransferBanner();
  renderListBody();

  if (ui.destination === 'contacts' || ui.destination === 'zcloud') {
    renderWorkspaceStage();
  } else {
    renderChatStage({ convChanged });
  }

  renderInspector();
}

function syncShellClasses() {
  const shell = document.getElementById('shell-grid');
  if (!shell) return;
  ui.railCollapsed = isRailCollapsed();
  shell.classList.toggle('rail-collapsed', ui.railMode === 'collapsed');
  shell.classList.toggle('rail-expanded', ui.railMode === 'expanded');
  const inspectorVisible =
    Boolean(ui.showInspector) &&
    ui.destination !== 'contacts' &&
    ui.destination !== 'zcloud' &&
    Boolean(store.activeConvId);
  shell.classList.toggle('inspector-open', inspectorVisible);
  shell.dataset.mobilePane = ui.mobilePane;

  const inspectorEl = document.getElementById('inspector-zone');
  if (inspectorEl) inspectorEl.hidden = !inspectorVisible;
}

function getConversationFlags(conv) {
  const bits = store.convActions?.flags?.[conv.id] || 0;
  const hiddenIds = store.convActions?.hidden?.ids || [];
  const strangerIds = store.convExtras?.strangerIds || [];
  const otherIds = store.convExtras?.otherIds || [];
  return {
    isHidden: Boolean((bits & 4) || hiddenIds.includes(conv.id)),
    isOther: Boolean((bits & 2) || otherIds.includes(conv.id)),
    isStranger: Boolean(strangerIds.includes(conv.id)),
    isPinned: Boolean(conv.pinned || (bits & 1)),
    isSelfCloud: Boolean(
      conv.isSelfCloud ||
      conv.id === store.convExtras?.sendToMeId ||
      conv.id === store.cloud?.selfId
    ),
  };
}

function getFilteredConversations() {
  const query = foldVietnamese(ui.convSearch).trim();
  const list = Array.isArray(store.conversations) ? store.conversations : [];

  return list
    .filter((conv) => {
      const f = getConversationFlags(conv);
      if (ui.destination === 'hidden') {
        return ui.unlockedHidden && f.isHidden;
      }
      if (f.isHidden) return false;

      if (ui.destination === 'strangers') return f.isStranger;
      if (ui.destination === 'other') return f.isOther;
      if (ui.destination === 'unread') return Number(conv.unread) > 0;
      if (ui.destination === 'myCloud') return f.isSelfCloud;

      // Default 'inbox' excludes strangers & archived 'other' unless searching
      if (!query && (f.isStranger || f.isOther)) return false;

      if (ui.labelFilter && conv.labelId !== ui.labelFilter) return false;
      return true;
    })
    .filter((conv) => {
      if (!query) return true;
      const hay = foldVietnamese(`${conv.name || ''} ${conv.preview?.text || ''}`);
      return hay.includes(query);
    })
    .sort((a, b) => {
      const fa = getConversationFlags(a);
      const fb = getConversationFlags(b);
      if (fa.isPinned !== fb.isPinned) return fa.isPinned ? -1 : 1;
      return Number(b.preview?.ts || 0) - Number(a.preview?.ts || 0);
    });
}

function renderRail() {
  const railEl = document.getElementById('rail-zone');
  if (!railEl) return;

  const isEn = isEnglishLang();
  const totalUnread = (store.conversations || []).reduce((acc, c) => {
    const f = getConversationFlags(c);
    return f.isHidden ? acc : acc + (Number(c.unread) || 0);
  }, 0);
  const strangerCount = (store.convExtras?.strangerIds || []).length;
  const reqCount = Number(store.contacts?.pendingRequestCount) || 0;
  const userName = store.user?.name || (isEn ? 'Zalo Account' : 'Tài khoản Zalo');
  const collapseLabel = isEn ? 'Collapse sidebar' : 'Thu gọn thanh bên';
  const expandLabel = isEn ? 'Expand sidebar' : 'Mở rộng thanh bên';

  railEl.setAttribute('aria-label', isEn ? 'Primary navigation' : 'Điều hướng chính');

  const navBtn = (dest, iconName, label, badge = 0) => {
    const badgeText = badge > 99 ? '99+' : String(badge);
    const ariaText = badge > 0 ? `${label} (${badgeText})` : label;
    return `
      <button
        type="button"
        class="nav-item"
        data-action="nav-dest"
        data-dest="${dest}"
        aria-current="${ui.destination === dest ? 'page' : 'false'}"
        title="${escapeHtml(ariaText)}"
        aria-label="${escapeHtml(ariaText)}"
      >
        ${icon(iconName)}
        <span class="nav-label">${escapeHtml(label)}</span>
        ${badge > 0 ? `<span class="unread-badge" aria-hidden="true">${badgeText}</span>` : ''}
      </button>
    `;
  };

  const inboxLabel = isEn ? 'Messages' : store.convExtras?.inboxTitle || 'Tin nhắn';
  const qsTitle = isEn ? 'Quick switcher (Ctrl+K)' : 'Chuyển nhanh (Ctrl+K)';
  const qsLabel = isEn ? 'Quick switcher' : 'Chuyển nhanh';
  const syncTitle = isEn ? 'Sync messages from phone' : 'Đồng bộ tin nhắn từ điện thoại';
  const syncLabel = isEn ? 'Sync from phone' : 'Đồng bộ điện thoại';
  const settingsTitle = isEn ? 'Olaz Settings (Ctrl+,)' : 'Thiết lập Olaz (Ctrl+,)';
  const settingsLabel = isEn ? 'Settings' : 'Thiết lập';

  railEl.innerHTML = `
    <div class="rail-account">
      <div class="rail-user" title="${escapeHtml(userName)}">
        ${renderAvatar({
          name: userName,
          avatar: store.user?.avatar,
          online: true,
          sizeClass: 'avatar-sm',
          hasNativeBridge: hasNativeBridge(),
        })}
        <div class="rail-user-info">
          <span class="rail-user-name">${escapeHtml(userName)}</span>
          <span class="rail-user-status">${isEn ? 'Online' : 'Đang trực tuyến'}</span>
        </div>
      </div>
      <div class="rail-top-actions">
        <button
          type="button"
          class="icon-btn"
          data-action="toggle-rail"
          aria-expanded="true"
          title="${escapeHtml(collapseLabel)}"
          aria-label="${escapeHtml(collapseLabel)}"
        >
          ${icon('sidebarCollapse')}
        </button>
      </div>
    </div>

    <div class="rail-nav-scroll scroller">
      ${navBtn('inbox', 'inbox', inboxLabel, totalUnread)}
      ${navBtn('unread', 'unread', isEn ? 'Unread' : 'Chưa đọc')}
      ${navBtn('other', 'other', isEn ? 'Other' : 'Khác')}
      ${navBtn('strangers', 'strangers', isEn ? 'Strangers' : 'Người lạ', strangerCount)}
      ${navBtn('hidden', ui.unlockedHidden ? 'unlock' : 'lock', isEn ? 'Hidden chats' : 'Trò chuyện ẩn')}
      ${navBtn('myCloud', 'cloud', isEn ? 'My Cloud' : 'Cloud của tôi')}

      <p class="nav-section-heading">${isEn ? 'Workspace' : 'Không gian làm việc'}</p>
      ${navBtn('contacts', 'contacts', isEn ? 'Contacts' : 'Danh bạ', reqCount)}
      ${navBtn('zcloud', 'zcloud', isEn ? 'zCloud Storage' : 'Dung lượng zCloud')}
    </div>

    <div class="rail-footer">
      <button
        type="button"
        class="nav-item rail-expand-item"
        data-action="toggle-rail"
        aria-expanded="false"
        title="${escapeHtml(expandLabel)}"
        aria-label="${escapeHtml(expandLabel)}"
      >
        ${icon('sidebarExpand')}
        <span class="nav-label">${escapeHtml(expandLabel)}</span>
      </button>
      <button
        type="button"
        class="nav-item"
        data-action="open-quick-switcher"
        title="${escapeHtml(qsTitle)}"
        aria-label="${escapeHtml(qsTitle)}"
      >
        ${icon('command')}
        <span class="nav-label">${escapeHtml(qsLabel)}</span>
        <kbd class="kbd-hint nav-label" style="flex: 0">Ctrl+K</kbd>
      </button>
      <button
        type="button"
        class="nav-item"
        data-action="start-manual-sync"
        title="${escapeHtml(syncTitle)}"
        aria-label="${escapeHtml(syncTitle)}"
      >
        ${icon('sync')}
        <span class="nav-label">${escapeHtml(syncLabel)}</span>
      </button>
      <button
        type="button"
        class="nav-item"
        data-action="open-settings"
        title="${escapeHtml(settingsTitle)}"
        aria-label="${escapeHtml(settingsTitle)}"
      >
        ${icon('settings')}
        <span class="nav-label">${escapeHtml(settingsLabel)}</span>
      </button>
    </div>
  `;
}

function mountListHeader() {
  const headerEl = document.getElementById('list-header-zone');
  if (!headerEl) return;
  const isEn = isEnglishLang();
  const newGrpLabel = isEn ? 'Create new group' : 'Tạo nhóm mới';
  headerEl.innerHTML = `
    <div class="column-title-row">
      <h2 id="list-column-title" class="column-title">${isEn ? 'Messages' : 'Tin nhắn'}</h2>
      <div class="field-row">
        <button
          type="button"
          id="list-create-group-btn"
          class="icon-btn"
          data-action="open-create-group"
          title="${escapeHtml(newGrpLabel)}"
          aria-label="${escapeHtml(newGrpLabel)}"
        >
          ${icon('plus')}
        </button>
      </div>
    </div>
    <div id="conv-search-box" class="search-box">
      <span class="field-leading-icon">${icon('search')}</span>
      <input
        id="conv-search-input"
        type="search"
        class="text-input"
        placeholder="${isEn ? 'Search names or messages…' : 'Tìm tên không dấu hoặc nội dung…'}"
        autocomplete="off"
        spellcheck="false"
      >
      <kbd class="kbd-hint">Ctrl+F</kbd>
    </div>
    <div id="label-filter-bar" class="label-filter-bar"></div>
  `;

  const searchInput = document.getElementById('conv-search-input');
  if (searchInput) {
    searchInput.value = ui.convSearch;
    searchInput.addEventListener('input', (e) => {
      ui.convSearch = e.target.value;
      renderListBody();
    });
  }
}

function updateListHeaderVisibility() {
  const titleEl = document.getElementById('list-column-title');
  const searchBox = document.getElementById('conv-search-box');
  const searchInput = document.getElementById('conv-search-input');
  const createBtn = document.getElementById('list-create-group-btn');
  const labelBar = document.getElementById('label-filter-bar');
  if (!titleEl) return;

  const isEn = isEnglishLang();
  const titles = isEn
    ? {
        inbox: 'Messages',
        unread: 'Unread messages',
        other: 'Other conversations',
        strangers: 'Messages from strangers',
        hidden: 'Hidden chats',
        myCloud: 'My Cloud',
        contacts: 'Contacts',
        zcloud: 'zCloud Storage',
      }
    : {
        inbox: store.convExtras?.inboxTitle || 'Tin nhắn',
        unread: 'Tin nhắn chưa đọc',
        other: 'Hội thoại Khác',
        strangers: 'Tin nhắn từ người lạ',
        hidden: 'Trò chuyện ẩn',
        myCloud: 'Cloud của tôi',
        contacts: 'Danh bạ',
        zcloud: 'Quản lý zCloud',
      };
  titleEl.textContent = titles[ui.destination] || (isEn ? 'Messages' : 'Tin nhắn');

  if (searchInput) {
    searchInput.placeholder = isEn
      ? 'Search names or messages…'
      : 'Tìm tên không dấu hoặc nội dung…';
  }
  if (createBtn) {
    const newGrpLabel = isEn ? 'Create new group' : 'Tạo nhóm mới';
    createBtn.title = newGrpLabel;
    createBtn.setAttribute('aria-label', newGrpLabel);
  }

  const isConvList = ui.destination !== 'contacts' && ui.destination !== 'zcloud';
  if (searchBox) searchBox.hidden = !isConvList;

  if (labelBar) {
    const labels = store.convExtras?.labels || [];
    const showLabels = ui.destination === 'inbox' && labels.length > 0;
    labelBar.hidden = !showLabels;
    if (showLabels) {
      labelBar.innerHTML = `
        <button type="button" class="filter-chip" data-action="filter-label" data-label-id="" aria-pressed="${ui.labelFilter === ''}">
          <span>${isEn ? 'All' : 'Tất cả'}</span>
        </button>
        ${labels
          .map(
            (l) => `
              <button
                type="button"
                class="filter-chip"
                data-action="filter-label"
                data-label-id="${escapeHtml(l.id)}"
                aria-pressed="${ui.labelFilter === l.id}"
              >
                <span class="label-dot" style="background-color: ${escapeHtml(l.color || '#005be0')}"></span>
                <span>${escapeHtml(l.name)}</span>
              </button>
            `
          )
          .join('')}
      `;
    }
  }
}

function renderTransferBanner() {
  const zone = document.getElementById('list-transfer-zone');
  if (!zone) return;
  const tr = store.transfer;
  if (!tr) {
    zone.innerHTML = '';
    return;
  }

  const isEn = isEnglishLang();

  if (tr.phase === 'offer') {
    zone.innerHTML = `
      <div class="transfer-card">
        <div class="transfer-head">
          ${icon('sync')}
          <span>${escapeHtml(tr.officialTitle || (isEn ? 'Sync messages from phone' : 'Đồng bộ tin nhắn từ điện thoại'))}</span>
        </div>
        <p class="transfer-desc">${escapeHtml(tr.officialDescription || tr.message || (isEn ? 'Transfer recent message history to your Linux desktop.' : 'Chuyển toàn bộ tin nhắn gần đây sang máy tính Linux.'))}</p>
        <div class="transfer-actions">
          <button type="button" class="btn btn-ghost" data-action="transfer-reject">${isEn ? 'Later' : 'Để sau'}</button>
          <button type="button" class="btn btn-primary" data-action="transfer-accept">${isEn ? 'Sync now' : 'Đồng bộ ngay'}</button>
        </div>
      </div>
    `;
    return;
  }

  const pct = tr.progress != null ? Math.min(100, Math.max(0, Math.round(Number(tr.progress) * 100))) : null;
  const syncHeading = isEn ? 'Syncing messages' : 'Đồng bộ tin nhắn';
  const subText =
    tr.phase === 'waitingForPhone'
      ? isEn
        ? 'Open Zalo on your phone and tap Confirm sync…'
        : 'Mở Zalo trên điện thoại và bấm Xác nhận đồng bộ…'
      : tr.phase === 'failed'
        ? isEn
          ? `Sync interrupted (${tr.errorName || 'Connection error'}).`
          : `Đồng bộ gián đoạn (${tr.errorName || 'Lỗi kết nối'}).`
        : isEn
          ? `Transferring data${tr.receivedMessages ? ` (${tr.receivedMessages} messages)` : ''}…`
          : `Đang chuyển dữ liệu${tr.receivedMessages ? ` (${tr.receivedMessages} tin nhắn)` : ''}…`;

  zone.innerHTML = `
    <div class="transfer-card">
      <div class="transfer-head">
        ${icon('sync')}
        <span>${escapeHtml(syncHeading)}</span>
        ${pct !== null ? `<span class="numeric-meta dialog-subtitle" style="margin-left: auto">${pct}%</span>` : ''}
      </div>
      ${
        pct !== null
          ? `<progress max="100" value="${pct}" aria-label="${escapeHtml(syncHeading)}"></progress>`
          : `<progress aria-label="${escapeHtml(syncHeading)}"></progress>`
      }
      <p class="transfer-desc numeric-meta">${escapeHtml(subText)}</p>
      <div class="transfer-actions">
        ${
          tr.phase === 'failed'
            ? `<button type="button" class="btn btn-primary" data-action="transfer-accept">${isEn ? 'Retry' : 'Thử lại'}</button>`
            : ''
        }
        <button type="button" class="btn btn-ghost" data-action="transfer-cancel">${isEn ? 'Cancel' : 'Hủy'}</button>
      </div>
    </div>
  `;
}

function renderListBody() {
  const bodyEl = document.getElementById('list-body-zone');
  if (!bodyEl) return;

  const isEn = isEnglishLang();

  if (ui.destination === 'contacts') {
    const friendsCount = (store.contacts?.friends || []).length;
    const groupsCount = (store.contacts?.groups || []).length;
    const reqCount = Number(store.contacts?.pendingRequestCount) || 0;

    const item = (tab, iconName, title, { badge = 0, count = 0 } = {}) => {
      const badgeHtml =
        badge > 0
          ? `<span class="unread-badge">${badge > 99 ? '99+' : badge}</span>`
          : count > 0
            ? `<span class="subnav-count">${count}</span>`
            : '';
      return `
        <button
          type="button"
          class="subnav-item"
          data-action="contacts-tab"
          data-tab="${tab}"
          aria-current="${ui.contactsTab === tab ? 'page' : 'false'}"
        >
          <span class="subnav-icon" aria-hidden="true">${icon(iconName)}</span>
          <span class="subnav-label">${escapeHtml(title)}</span>
          ${badgeHtml}
        </button>
      `;
    };

    bodyEl.innerHTML = `
      <div class="subnav-list" role="navigation" aria-label="${isEn ? 'Contacts categories' : 'Danh mục danh bạ'}">
        ${item('friends', 'contacts', isEn ? 'Friends list' : 'Danh sách bạn bè', { count: friendsCount })}
        ${item('groups', 'usersGroup', isEn ? 'Groups & Communities' : 'Nhóm & Cộng đồng', { count: groupsCount })}
        ${item('requests', 'userPlus', isEn ? 'Friend requests' : 'Lời mời kết bạn', { badge: reqCount })}
        ${item('search', 'search', isEn ? 'Find by phone number' : 'Tìm qua số điện thoại')}
      </div>
    `;
    return;
  }

  if (ui.destination === 'zcloud') {
    const plan = store.cloud?.plan || {};
    const hasQuota = Number(plan.quota) > 0;
    const used = formatBytes(plan.usage || 0);
    const quota = hasQuota ? formatBytes(plan.quota) : '—';
    const planTitle = plan.name || (isEn ? 'zCloud storage plan' : 'Gói lưu trữ zCloud');
    const summaryLabel = hasQuota
      ? isEn
        ? `${used} / ${quota} used`
        : `${used} / ${quota} đã dùng`
      : isEn
        ? 'Loading storage usage…'
        : 'Đang tải dung lượng…';

    bodyEl.innerHTML = `
      <div class="zcloud-sidebar-stack">
        <div class="quota-panel quota-panel-compact">
          <span class="field-label">${escapeHtml(planTitle)}</span>
          <span class="quota-nums dialog-subtitle">${escapeHtml(summaryLabel)}</span>
          ${
            hasQuota
              ? `<progress max="${Math.max(1, Number(plan.quota) || 1)}" value="${Number(plan.usage) || 0}" aria-label="${escapeHtml(planTitle)}"></progress>`
              : `<progress aria-label="${escapeHtml(planTitle)}"></progress>`
          }
        </div>
        <button type="button" class="btn btn-secondary" data-action="refresh-zcloud">
          ${icon('refresh')}
          <span>${isEn ? 'Refresh storage' : 'Làm mới dung lượng'}</span>
        </button>
      </div>
    `;
    return;
  }

  if (ui.destination === 'hidden' && !ui.unlockedHidden) {
    bodyEl.innerHTML = `
      <div class="empty-stage">
        ${icon('lock')}
        <h3 class="empty-stage-title">Danh sách đang khóa bằng mã PIN</h3>
        <p class="empty-stage-desc">Nhập mã PIN 4 chữ số để mở khóa các cuộc trò chuyện riêng tư của bạn.</p>
        <button type="button" class="btn btn-primary" data-action="prompt-unlock-pin">
          ${icon('unlock')}
          <span>Nhập mã PIN</span>
        </button>
      </div>
    `;
    return;
  }

  const rows = getFilteredConversations();
  if (rows.length === 0) {
    bodyEl.innerHTML = `
      <div class="empty-stage">
        ${icon('inbox')}
        <h3 class="empty-stage-title">Không có cuộc trò chuyện phù hợp</h3>
        <p class="empty-stage-desc">Thử từ khóa khác hoặc nhấn Ctrl+K để tìm trên toàn bộ danh bạ Zalo.</p>
      </div>
    `;
    return;
  }

  const labelsById = new Map((store.convExtras?.labels || []).map((l) => [l.id, l]));

  bodyEl.innerHTML = `
    <ul class="conv-list" role="listbox" aria-label="Danh sách hội thoại">
      ${rows
        .map((conv) => {
          const f = getConversationFlags(conv);
          const selected = conv.id === store.activeConvId;
          const unread = Number(conv.unread) || 0;
          const label = conv.labelId ? labelsById.get(conv.labelId) : null;
          const previewText = conv.preview?.text || 'Bắt đầu trò chuyện mới';
          return `
            <li
              class="conv-row ${unread > 0 ? 'has-unread' : ''}"
              role="option"
              aria-selected="${selected}"
              data-action="select-conv"
              data-conv-id="${escapeHtml(conv.id)}"
              tabindex="0"
            >
              ${renderAvatar({
                name: conv.name,
                avatar: conv.avatar,
                online: Boolean(conv.online),
                isSelfCloud: f.isSelfCloud,
                hasNativeBridge: hasNativeBridge(),
              })}
              <div class="conv-main">
                <div class="conv-top-line">
                  <div class="conv-name-wrap">
                    ${label ? `<span class="label-dot" style="background-color: ${escapeHtml(label.color)}" title="${escapeHtml(label.name)}"></span>` : ''}
                    <span class="conv-name">${escapeHtml(conv.name || 'Hội thoại')}</span>
                    <span class="conv-meta-icons">
                      ${conv.isE2EE ? `<span class="e2ee-icon" title="Mã hóa đầu cuối">${icon('shield')}</span>` : ''}
                      ${conv.isMuted ? `<span title="Đã tắt thông báo">${icon('bellOff')}</span>` : ''}
                      ${f.isPinned ? `<span title="Đã ghim">${icon('pin')}</span>` : ''}
                    </span>
                  </div>
                  <span class="conv-time">${escapeHtml(formatShortTime(conv.preview?.ts))}</span>
                </div>
                <div class="conv-bottom-line">
                  <span class="conv-preview">${escapeHtml(previewText)}</span>
                  ${unread > 0 ? `<span class="unread-badge">${unread > 99 ? '99+' : unread}</span>` : ''}
                </div>
              </div>
              <button
                type="button"
                class="icon-btn conv-more-btn"
                data-action="conv-menu"
                data-conv-id="${escapeHtml(conv.id)}"
                aria-label="Tùy chọn hội thoại"
              >
                ${icon('more')}
              </button>
            </li>
          `;
        })
        .join('')}
    </ul>
  `;
}

/* ==========================================================================
   5. Primary Stage: Chat Timeline, Scroll Preservation & Composer
   ========================================================================== */

function renderChatStage({ convChanged = false } = {}) {
  const headerZone = document.getElementById('stage-header-zone');
  const timelineZone = document.getElementById('stage-timeline-zone');
  const typingZone = document.getElementById('stage-typing-zone');
  const composerZone = document.getElementById('stage-composer-zone');
  if (!headerZone || !timelineZone || !composerZone) return;

  timelineZone.hidden = false;
  if (typingZone) typingZone.hidden = false;
  composerZone.hidden = false;

  const conv = (store.conversations || []).find((c) => c.id === store.activeConvId);
  if (!conv) {
    headerZone.innerHTML = '';
    timelineZone.innerHTML = `
      <div class="empty-stage">
        <div class="brand-badge">${icon('olazLogo')}</div>
        <h2 class="empty-stage-title">Chọn một cuộc trò chuyện để bắt đầu</h2>
        <p class="empty-stage-desc">Nhấn Ctrl+K để mở bảng chuyển nhanh hỗ trợ tìm kiếm tiếng Việt không dấu.</p>
      </div>
    `;
    if (typingZone) typingZone.innerHTML = '';
    composerZone.innerHTML = '';
    ui.composerConvId = null;
    return;
  }

  renderStageHeader();
  renderStageTimeline({ scrollToBottom: convChanged, preserveScroll: !convChanged });
  renderTypingStrip();

  if (ui.composerConvId !== conv.id || !document.getElementById('chat-composer-input')) {
    ui.composerConvId = conv.id;
    ui.replyTo = null;
    ui.mentions = [];
    ui.mentionPopup = { open: false, query: '', startPos: 0, candidates: [], activeIndex: 0 };
    mountComposer(conv);
  } else {
    renderComposerReplyBar();
    renderComposerPopups();
  }
}

function renderMobileBackBtn() {
  const label = isEnglishLang() ? 'Back to conversations' : 'Quay lại danh sách';
  return `
    <button
      type="button"
      class="icon-btn mobile-back-btn"
      data-action="show-list-pane"
      title="${escapeHtml(label)}"
      aria-label="${escapeHtml(label)}"
    >
      <svg class="ui-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M19 12H5"/>
        <path d="m12 19-7-7 7-7"/>
      </svg>
    </button>
  `;
}

function renderStageHeader() {
  const headerZone = document.getElementById('stage-header-zone');
  if (!headerZone) return;
  const conv = (store.conversations || []).find((c) => c.id === store.activeConvId);
  if (!conv) {
    headerZone.innerHTML = '';
    return;
  }

  const f = getConversationFlags(conv);
  const subtitleParts = [];
  if (f.isSelfCloud) {
    subtitleParts.push('Lưu trữ cá nhân đồng bộ giữa điện thoại & Linux');
  } else if (conv.isGroup) {
    subtitleParts.push(`${Number(conv.members) || 2} thành viên`);
  } else {
    subtitleParts.push(conv.online ? 'Đang hoạt động' : 'Truy cập gần đây');
  }
  if (conv.isE2EE) subtitleParts.push('Mã hóa đầu cuối Signal');

  const pins = store.pinsByConv?.[conv.id] || [];
  const topPin = pins[0] || null;

  headerZone.innerHTML = `
    <header class="chat-header">
      <div class="chat-header-identity">
        ${renderMobileBackBtn()}
        ${renderAvatar({
          name: conv.name,
          avatar: conv.avatar,
          online: Boolean(conv.online),
          isSelfCloud: f.isSelfCloud,
          hasNativeBridge: hasNativeBridge(),
        })}
        <div class="chat-header-titles">
          <div class="chat-title-row">
            <h2 class="chat-title">${escapeHtml(conv.name)}</h2>
            ${conv.isE2EE ? `<span class="e2ee-icon" title="Mã hóa đầu cuối">${icon('shield')}</span>` : ''}
          </div>
          <div class="chat-subtitle numeric-meta">${escapeHtml(subtitleParts.join(' · '))}</div>
        </div>
      </div>
      <div class="chat-header-actions">
        ${
          conv.isGroup
            ? `
              <button type="button" class="icon-btn" data-action="open-create-poll" title="Tạo bình chọn nhóm" aria-label="Tạo bình chọn nhóm">
                ${icon('poll')}
              </button>
            `
            : ''
        }
        <button
          type="button"
          class="icon-btn"
          data-action="quick-conv-mute"
          data-conv-id="${escapeHtml(conv.id)}"
          title="${conv.isMuted ? 'Bật lại thông báo' : 'Tắt thông báo'}"
          aria-label="${conv.isMuted ? 'Bật lại thông báo' : 'Tắt thông báo'}"
        >
          ${icon(conv.isMuted ? 'bellOff' : 'speaker')}
        </button>
        <button
          type="button"
          class="icon-btn"
          data-action="toggle-inspector"
          aria-pressed="${Boolean(ui.showInspector)}"
          title="Thông tin hội thoại (Alt+Ctrl+I)"
          aria-label="Thông tin hội thoại"
        >
          ${icon('inspector')}
        </button>
      </div>
    </header>
    ${
      topPin
        ? `
          <div class="pinned-bar">
            <div class="pinned-bar-lead" data-action="jump-to-msg" data-msg-id="${escapeHtml(topPin.msgId || '')}">
              ${icon('pin')}
              <div class="pinned-bar-text">
                <span class="pinned-bar-title">${escapeHtml(topPin.title)}</span>
                <span class="pinned-bar-sub numeric-meta">
                  ${escapeHtml(topPin.sender || 'Đã ghim')}${pins.length > 1 ? ` (+${pins.length - 1} ghim khác)` : ''}
                </span>
              </div>
            </div>
            <button
              type="button"
              class="btn btn-inline"
              data-action="unpin-msg"
              data-conv-id="${escapeHtml(conv.id)}"
              data-msg-id="${escapeHtml(topPin.msgId || '')}"
              data-ref="${escapeHtml(topPin.ref || '')}"
            >
              Bỏ ghim
            </button>
          </div>
        `
        : ''
    }
  `;
}

function renderStageTimeline({ scrollToBottom = false, preserveScroll = false } = {}) {
  const scroller = document.getElementById('stage-timeline-zone');
  if (!scroller) return;

  const convId = store.activeConvId;
  const conv = (store.conversations || []).find((c) => c.id === convId);
  const bucket = store.messagesByConv?.[convId] || { items: [], hasMore: false };
  const items = Array.isArray(bucket.items) ? bucket.items : [];

  const wasNearBottom = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 72;
  const prevScrollTop = scroller.scrollTop;

  let html = '';
  if (bucket.hasMore) {
    html += `
      <div class="load-older-wrap">
        <button type="button" class="btn btn-secondary" data-action="load-older" ${ui.loadingOlderConvId === convId ? 'disabled' : ''}>
          ${icon('clock')}
          <span>${ui.loadingOlderConvId === convId ? 'Đang tải tin nhắn cũ…' : 'Tải tin nhắn cũ hơn'}</span>
        </button>
      </div>
    `;
  }

  let lastDateLabel = '';
  const lastOwnId = [...items].reverse().find((m) => m.isMine)?.id || null;

  for (const msg of items) {
    const dLabel = formatDateLabel(msg.date);
    if (dLabel && dLabel !== lastDateLabel) {
      lastDateLabel = dLabel;
      html += `<div class="date-divider numeric-meta">${escapeHtml(dLabel)}</div>`;
    }

    const kind = normalizeMessageKind(msg);
    if (kind.type === 'system' || kind.type === 'notice') {
      html += `<div class="system-event" data-msg-id="${escapeHtml(msg.id)}">${escapeHtml(msg.text || kind.text || '')}</div>`;
      continue;
    }

    const isSticker = kind.type === 'sticker';
    const isRecalled = Boolean(msg.isRecalled || kind.type === 'recalled');
    const reactions = Array.isArray(msg.reactions) ? msg.reactions : [];

    html += `
      <article class="msg-row ${msg.isMine ? 'is-mine' : 'is-theirs'}" data-msg-id="${escapeHtml(msg.id)}">
        ${
          !msg.isMine
            ? renderAvatar({
                name: msg.senderName || conv?.name,
                avatar: msg.senderAvatar,
                sizeClass: 'avatar-sm',
                hasNativeBridge: hasNativeBridge(),
              })
            : ''
        }
        <div class="msg-stack">
          ${!msg.isMine && conv?.isGroup ? `<span class="msg-sender-name">${escapeHtml(msg.senderName)}</span>` : ''}
          <div class="msg-bubble-wrap">
            <div class="msg-bubble ${isSticker ? 'is-sticker' : ''} ${isRecalled ? 'is-recalled' : ''}">
              ${renderMessageBubbleContent(msg, store, hasNativeBridge(), getVoiceStateForMessage(msg.id))}
              <div class="msg-meta-row">
                ${msg.isE2EE ? `<span class="e2ee-icon" title="Tin nhắn mã hóa đầu cuối">${icon('shield')}</span>` : ''}
                <time class="msg-time">${escapeHtml(formatShortTime(msg.date))}</time>
              </div>
            </div>
            ${
              !isRecalled
                ? `
                  <div class="msg-hover-toolbar" role="toolbar" aria-label="Tác vụ tin nhắn">
                    ${ZALO_REACTIONS.map(
                      (emo) => `
                        <button
                          type="button"
                          class="react-quick-btn"
                          data-action="react-msg"
                          data-msg-id="${escapeHtml(msg.id)}"
                          data-reaction="${emo}"
                          aria-label="Thả biểu cảm ${emo}"
                        >${emo}</button>
                      `
                    ).join('')}
                    <button type="button" class="icon-btn" style="width:28px;height:28px" data-action="reply-msg" data-msg-id="${escapeHtml(msg.id)}" title="Trả lời" aria-label="Trả lời">
                      ${icon('reply')}
                    </button>
                    <button type="button" class="icon-btn" style="width:28px;height:28px" data-action="forward-msg" data-msg-id="${escapeHtml(msg.id)}" title="Chuyển tiếp" aria-label="Chuyển tiếp">
                      ${icon('forward')}
                    </button>
                    <button type="button" class="icon-btn" style="width:28px;height:28px" data-action="msg-menu" data-msg-id="${escapeHtml(msg.id)}" title="Thêm tác vụ" aria-label="Thêm tác vụ">
                      ${icon('more')}
                    </button>
                  </div>
                `
                : ''
            }
          </div>
          ${
            reactions.length > 0
              ? `
                <div class="msg-reactions">
                  ${reactions
                    .map((r) => {
                      const emoji = r.emoji || r.icon?.emoji || '👍';
                      const tooltip = Array.isArray(r.names) && r.names.length > 0 ? r.names.join(', ') : '';
                      return `
                        <button
                          type="button"
                          class="reaction-chip numeric-meta ${r.includesMe ? 'is-mine' : ''}"
                          data-action="react-msg"
                          data-msg-id="${escapeHtml(msg.id)}"
                          data-reaction="${escapeHtml(emoji)}"
                          title="${escapeHtml(tooltip)}"
                        >
                          <span>${escapeHtml(emoji)}</span>
                          <span>${Number(r.count) || 1}</span>
                        </button>
                      `;
                    })
                    .join('')}
                </div>
              `
              : ''
          }
          ${msg.isMine && msg.id === lastOwnId ? renderDeliveryStatus(msg.status) : ''}
        </div>
      </article>
    `;
  }

  scroller.innerHTML = html;

  if (scrollToBottom || (preserveScroll && wasNearBottom)) {
    scroller.scrollTop = scroller.scrollHeight;
  } else if (preserveScroll) {
    scroller.scrollTop = prevScrollTop;
  }
}

/**
 * Loads older messages while preserving exact viewport anchor position so prepending
 * older items never jumps the user's reading spot.
 */
async function triggerLoadOlder(convId) {
  const scroller = document.getElementById('stage-timeline-zone');
  if (!scroller || !convId || ui.loadingOlderConvId === convId) return;

  const firstMsgEl = scroller.querySelector('[data-msg-id]');
  const anchorId = firstMsgEl?.getAttribute('data-msg-id') || null;
  const prevAnchorOffset = firstMsgEl ? firstMsgEl.offsetTop - scroller.scrollTop : 0;
  const prevScrollHeight = scroller.scrollHeight;

  ui.loadingOlderConvId = convId;
  renderStageTimeline({ preserveScroll: true });

  try {
    await window.olazInvoke('load_older', { convId });
  } catch (err) {
    showToast(err.message || 'Không thể tải thêm tin nhắn cũ.');
  } finally {
    ui.loadingOlderConvId = null;
    renderStageTimeline({ preserveScroll: false });

    const newScroller = document.getElementById('stage-timeline-zone');
    if (newScroller) {
      const anchorAfter = anchorId
        ? newScroller.querySelector(`[data-msg-id="${CSS.escape(anchorId)}"]`)
        : null;
      if (anchorAfter) {
        newScroller.scrollTop = anchorAfter.offsetTop - prevAnchorOffset;
      } else {
        newScroller.scrollTop += newScroller.scrollHeight - prevScrollHeight;
      }
    }
  }
}

function renderTypingStrip() {
  const zone = document.getElementById('stage-typing-zone');
  if (!zone) return;
  const who = store.typingByConv?.[store.activeConvId];
  zone.innerHTML = who ? `<span>${escapeHtml(who)} đang soạn tin nhắn…</span>` : '';
}

/* ==========================================================================
   6. Composer + All 5 Mandatory Vietnamese Telex/VNI IME Guards
   ========================================================================== */

function mountComposer(conv) {
  const zone = document.getElementById('stage-composer-zone');
  if (!zone) return;

  zone.innerHTML = `
    <div class="composer-shell">
      <div id="composer-mention-popup" class="composer-popup" hidden role="listbox" aria-label="Gợi ý nhắc tên"></div>
      <div id="composer-sticker-strip" class="sticker-suggest-strip" hidden aria-label="Gợi ý Sticker nhanh"></div>
      <div id="composer-reply-slot"></div>
      <div class="composer-toolbar">
        <div class="composer-tools-left">
          <button type="button" class="icon-btn" data-action="composer-pick-media" title="Gửi hình ảnh hoặc video" aria-label="Gửi hình ảnh hoặc video">
            ${icon('image')}
          </button>
          <button type="button" class="icon-btn" data-action="composer-pick-files" title="Đính kèm tệp" aria-label="Đính kèm tệp">
            ${icon('attachment')}
          </button>
          <button type="button" class="icon-btn" data-action="composer-screenshot" title="Chụp màn hình Wayland" aria-label="Chụp màn hình">
            ${icon('screenshot')}
          </button>
          <button type="button" class="icon-btn" data-action="composer-sticker-picker" title="Chọn Sticker" aria-label="Chọn Sticker">
            ${icon('sticker')}
          </button>
          ${
            conv.isGroup
              ? `
                <button type="button" class="icon-btn" data-action="open-create-poll" title="Tạo bình chọn nhóm" aria-label="Tạo bình chọn nhóm">
                  ${icon('poll')}
                </button>
              `
              : ''
          }
        </div>
        <span class="dialog-subtitle" style="font-size: 0.74rem;">Ctrl+V dán ảnh · Shift+Enter xuống dòng</span>
      </div>
      <div class="composer-input-row">
        <textarea
          id="chat-composer-input"
          class="composer-textarea"
          rows="1"
          placeholder="Nhập tin nhắn tới ${escapeHtml(conv.name)}…"
          aria-label="Nội dung tin nhắn"
        ></textarea>
        <button type="button" id="composer-send-btn" class="btn btn-primary" style="height: 42px; padding: 0 16px;" data-action="composer-send" aria-label="Gửi tin nhắn">
          ${icon('send')}
          <span>Gửi</span>
        </button>
      </div>
    </div>
  `;

  const textarea = document.getElementById('chat-composer-input');
  if (!textarea) return;

  // IME Tracking: fcitx5-bamboo / ibus-bamboo Vietnamese composition lifecycle
  textarea.addEventListener('compositionstart', () => {
    imeComposing = true;
  });

  textarea.addEventListener('compositionend', () => {
    imeComposing = false;
    // Guard 2: Apply any failed send draft that arrived mid-composition without clobbering committed syllable
    if (deferredDraft) {
      const pending = deferredDraft;
      deferredDraft = null;
      applyFailedDraftToComposer(pending);
    }
    // Guards 3 & 4: Evaluate @mention candidates and sticker keyword suggestions now that syllable is committed
    evaluateMentionCandidates(textarea);
    scheduleStickerSuggestionQuery(textarea.value);
  });

  textarea.addEventListener('keydown', (e) => {
    // Guard 1: NEVER intercept Enter or Backspace while Vietnamese IME is composing!
    if (imeComposing || e.isComposing || e.keyCode === 229) {
      return;
    }

    // Navigate @mention popup when open
    if (ui.mentionPopup.open && ui.mentionPopup.candidates.length > 0) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        ui.mentionPopup.activeIndex = (ui.mentionPopup.activeIndex + 1) % ui.mentionPopup.candidates.length;
        renderComposerPopups();
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        ui.mentionPopup.activeIndex =
          (ui.mentionPopup.activeIndex - 1 + ui.mentionPopup.candidates.length) % ui.mentionPopup.candidates.length;
        renderComposerPopups();
        return;
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault();
        const chosen = ui.mentionPopup.candidates[ui.mentionPopup.activeIndex];
        if (chosen) commitMentionCandidate(textarea, chosen);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        ui.mentionPopup.open = false;
        renderComposerPopups();
        return;
      }
    }

    // Guard 3: Atomic @mention token backspace deletion ONLY when NOT composing IME
    if (e.key === 'Backspace' && textarea.selectionStart === textarea.selectionEnd) {
      const caret = textarea.selectionStart;
      const hitIdx = ui.mentions.findIndex((m) => caret > m.pos && caret <= m.pos + m.len);
      if (hitIdx >= 0) {
        e.preventDefault();
        const token = ui.mentions[hitIdx];
        const before = textarea.value.slice(0, token.pos);
        const after = textarea.value.slice(token.pos + token.len);
        textarea.value = before + after;
        textarea.setSelectionRange(token.pos, token.pos);
        ui.mentions.splice(hitIdx, 1);
        realignMentionsAfterEdit(textarea.value);
        autoResizeTextarea(textarea);
        return;
      }
    }

    // Send on Enter without Shift (respecting user setting)
    if (e.key === 'Enter' && !e.shiftKey && store.settings?.enterToSend !== false) {
      e.preventDefault();
      submitComposerMessage();
    }
  });

  textarea.addEventListener('input', () => {
    autoResizeTextarea(textarea);
    notifyTypingThrottled();

    // Guard 3 & Guard 4: Pause @mention popup updates & sticker suggestion queries during IME composition
    if (imeComposing) return;

    realignMentionsAfterEdit(textarea.value);
    evaluateMentionCandidates(textarea);
    scheduleStickerSuggestionQuery(textarea.value);
  });

  // Ctrl+V Clipboard Image Paste -> base64 PNG -> preview confirmation dialog
  textarea.addEventListener('paste', (e) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (const item of items) {
      if (item.type && item.type.startsWith('image/')) {
        const blob = item.getAsFile();
        if (!blob) continue;
        e.preventDefault();
        readBlobAsBase64Png(blob).then((base64Png) => {
          openClipboardPasteDialog(base64Png);
        });
        return;
      }
    }
  });
}

function autoResizeTextarea(textarea) {
  textarea.style.height = 'auto';
  textarea.style.height = `${Math.min(150, Math.max(42, textarea.scrollHeight))}px`;
}

function applyFailedDraftToComposer(draft) {
  const textarea = document.getElementById('chat-composer-input');
  if (!textarea || !draft?.text) return;
  textarea.value = textarea.value ? `${textarea.value}\n${draft.text}` : draft.text;
  if (draft.replyTo && !ui.replyTo) {
    ui.replyTo = draft.replyTo;
    renderComposerReplyBar();
  }
  autoResizeTextarea(textarea);
  showToast('Tin nhắn chưa gửi được đã được khôi phục vào ô soạn thảo.');
}

function realignMentionsAfterEdit(currentText) {
  ui.mentions = ui.mentions.filter((m) => currentText.slice(m.pos, m.pos + m.len) === m.text);
}

function evaluateMentionCandidates(textarea) {
  if (imeComposing) return;
  const conv = (store.conversations || []).find((c) => c.id === store.activeConvId);
  if (!conv?.isGroup) {
    ui.mentionPopup.open = false;
    renderComposerPopups();
    return;
  }

  const caret = textarea.selectionStart ?? textarea.value.length;
  const beforeCaret = textarea.value.slice(0, caret);
  const match = beforeCaret.match(/(?:^|\s)@([^\s@]{0,30})$/);
  if (!match) {
    ui.mentionPopup.open = false;
    renderComposerPopups();
    return;
  }

  const query = foldVietnamese(match[1] || '');
  const atIndex = beforeCaret.lastIndexOf('@');
  const pool =
    Array.isArray(conv.memberList) && conv.memberList.length > 0
      ? conv.memberList
      : [
          { id: 'all', name: 'All', isAll: true },
          ...(store.contacts?.friends || []).map((f) => ({ id: f.id, name: f.name, isAll: false })),
        ];

  const candidates = pool.filter((m) => foldVietnamese(m.name).includes(query)).slice(0, 6);
  ui.mentionPopup = {
    open: candidates.length > 0,
    query,
    startPos: atIndex,
    candidates,
    activeIndex: 0,
  };
  renderComposerPopups();
}

function commitMentionCandidate(textarea, candidate) {
  if (imeComposing) return;
  const caret = textarea.selectionStart ?? textarea.value.length;
  const tokenText = `@${candidate.name}`;
  const insertText = `${tokenText} `;
  const before = textarea.value.slice(0, ui.mentionPopup.startPos);
  const after = textarea.value.slice(caret);
  textarea.value = before + insertText + after;
  const nextCaret = before.length + insertText.length;
  textarea.setSelectionRange(nextCaret, nextCaret);

  ui.mentions.push({
    uid: candidate.id,
    pos: before.length,
    len: tokenText.length,
    isAll: Boolean(candidate.isAll),
    text: tokenText,
  });
  ui.mentionPopup.open = false;
  renderComposerPopups();
  textarea.focus();
}

function scheduleStickerSuggestionQuery(rawText) {
  // Guard 4: Delay sticker keyword suggestion queries until IME composition completes
  if (imeComposing) return;
  clearTimeout(stickerSuggestTimer);
  const trimmed = String(rawText || '').trim();
  if (!trimmed || trimmed.includes('\n') || trimmed.split(/\s+/).length > 4 || !store.settings?.stickerSuggest) {
    if (store.stickers.suggestions?.length) {
      store.stickers.suggestions = [];
      renderComposerPopups();
    }
    return;
  }
  stickerSuggestTimer = setTimeout(() => {
    if (imeComposing || !store.activeConvId) return;
    window.olazInvoke('query_sticker_suggestions', {
      convId: store.activeConvId,
      keyword: trimmed,
    }).catch(() => {});
  }, 220);
}

function notifyTypingThrottled() {
  const now = Date.now();
  if (!store.activeConvId || now - ui.lastTypingSentAt < 3500) return;
  ui.lastTypingSentAt = now;
  window.olazInvoke('send_typing', { convId: store.activeConvId }).catch(() => {});
}

async function submitComposerMessage() {
  if (imeComposing) return;
  const textarea = document.getElementById('chat-composer-input');
  const convId = store.activeConvId;
  if (!textarea || !convId) return;

  const text = textarea.value.trim();
  if (!text) return;

  const mentionsPayload = ui.mentions.map(({ uid, pos, len, isAll }) => ({ uid, pos, len, isAll }));
  const replyPayload = ui.replyTo ? { ...ui.replyTo } : null;

  textarea.value = '';
  autoResizeTextarea(textarea);
  ui.mentions = [];
  ui.replyTo = null;
  ui.mentionPopup.open = false;
  store.stickers.suggestions = [];
  renderComposerReplyBar();
  renderComposerPopups();

  try {
    await window.olazInvoke('send_text', {
      convId,
      text,
      mentions: mentionsPayload,
      replyTo: replyPayload,
    });
  } catch (err) {
    const failed = { text, replyTo: replyPayload };
    if (imeComposing) {
      deferredDraft = failed;
    } else {
      applyFailedDraftToComposer(failed);
    }
  }
}

function renderComposerReplyBar() {
  const slot = document.getElementById('composer-reply-slot');
  if (!slot) return;
  if (!ui.replyTo) {
    slot.innerHTML = '';
    return;
  }
  slot.innerHTML = `
    <div class="composer-reply-bar">
      <div style="min-width: 0;">
        <span class="quote-sender">${icon('reply')}<span>Đang trả lời ${escapeHtml(ui.replyTo.senderName)}</span></span>
        <p class="quote-body">${escapeHtml(ui.replyTo.text)}</p>
      </div>
      <button type="button" class="icon-btn" data-action="clear-reply" aria-label="Hủy trả lời">
        ${icon('close')}
      </button>
    </div>
  `;
}

function renderComposerPopups() {
  const mentionEl = document.getElementById('composer-mention-popup');
  if (mentionEl) {
    if (!ui.mentionPopup.open || ui.mentionPopup.candidates.length === 0) {
      mentionEl.hidden = true;
      mentionEl.innerHTML = '';
    } else {
      mentionEl.hidden = false;
      mentionEl.innerHTML = ui.mentionPopup.candidates
        .map(
          (c, idx) => `
            <button
              type="button"
              class="mention-option"
              role="option"
              aria-selected="${idx === ui.mentionPopup.activeIndex}"
              data-action="pick-mention"
              data-mention-idx="${idx}"
            >
              ${renderAvatar({ name: c.name, avatar: c.avatar, sizeClass: 'avatar-sm', hasNativeBridge: hasNativeBridge() })}
              <span>@${escapeHtml(c.name)}</span>
              ${c.isAll ? `<span class="dialog-subtitle" style="margin-left:auto">Báo cả nhóm</span>` : ''}
            </button>
          `
        )
        .join('');
    }
  }

  const stripEl = document.getElementById('composer-sticker-strip');
  if (stripEl) {
    const sugg = store.stickers?.suggestions || [];
    if (sugg.length === 0) {
      stripEl.hidden = true;
      stripEl.innerHTML = '';
    } else {
      stripEl.hidden = false;
      stripEl.innerHTML = sugg
        .map(
          (s) => `
            <button
              type="button"
              class="sticker-thumb-btn"
              data-action="send-suggested-sticker"
              data-sticker-id="${escapeHtml(s.id)}"
              data-cate-id="${escapeHtml(s.cateId || '')}"
              title="${escapeHtml(s.label || 'Gửi Sticker')}"
            >
              <img src="${escapeHtml(resolveMediaUrl(s.url, hasNativeBridge()))}" alt="${escapeHtml(s.label || 'Sticker')}">
            </button>
          `
        )
        .join('');
    }
  }
}

/* ==========================================================================
   7. Drag-and-Drop Overlay & Clipboard Paste Helpers
   ========================================================================== */

function bindStageDragAndDrop() {
  const stage = document.getElementById('stage-zone');
  const overlay = document.getElementById('drop-overlay');
  if (!stage || !overlay) return;

  let dragDepth = 0;

  stage.addEventListener('dragenter', (e) => {
    if (!store.activeConvId || ui.destination === 'contacts' || ui.destination === 'zcloud') return;
    e.preventDefault();
    dragDepth += 1;
    overlay.hidden = false;
  });

  stage.addEventListener('dragover', (e) => {
    if (!store.activeConvId) return;
    e.preventDefault();
  });

  stage.addEventListener('dragleave', () => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) overlay.hidden = true;
  });

  stage.addEventListener('drop', async (e) => {
    e.preventDefault();
    dragDepth = 0;
    overlay.hidden = true;
    const convId = store.activeConvId;
    if (!convId) return;

    const files = Array.from(e.dataTransfer?.files || []);
    const paths = [];
    const uriList = e.dataTransfer?.getData('text/uri-list') || '';
    for (const line of uriList.split(/\r?\n/)) {
      const trimmed = line.trim();
      if (trimmed.startsWith('file://')) {
        paths.push(decodeURIComponent(trimmed.slice('file://'.length)));
      }
    }
    for (const f of files) {
      if (f.path && !paths.includes(f.path)) paths.push(f.path);
    }

    if (paths.length > 0) {
      await window.olazInvoke('send_dropped_paths', { convId, paths });
      return;
    }

    const firstImg = files.find((f) => f.type && f.type.startsWith('image/'));
    if (firstImg) {
      const base64Png = await readBlobAsBase64Png(firstImg);
      openClipboardPasteDialog(base64Png);
      return;
    }

    if (files.length > 0) {
      await window.olazInvoke('send_dropped_paths', {
        convId,
        paths: files.map((f) => f.name),
      });
    }
  });
}

function readBlobAsBase64Png(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const res = String(reader.result || '');
      const comma = res.indexOf(',');
      resolve(comma >= 0 ? res.slice(comma + 1) : res);
    };
    reader.onerror = () => reject(new Error('Không đọc được dữ liệu ảnh từ bộ nhớ tạm'));
    reader.readAsDataURL(blob);
  });
}

/* ==========================================================================
   8. Contacts, zCloud & Conversation Inspector Renderers
   ========================================================================== */

function renderWorkspaceStage() {
  const headerZone = document.getElementById('stage-header-zone');
  const timelineZone = document.getElementById('stage-timeline-zone');
  const typingZone = document.getElementById('stage-typing-zone');
  const composerZone = document.getElementById('stage-composer-zone');
  if (!headerZone || !timelineZone || !composerZone) return;

  if (typingZone) typingZone.hidden = true;
  composerZone.hidden = true;
  ui.composerConvId = null;

  const isEn = isEnglishLang();

  if (ui.destination === 'contacts') {
    headerZone.innerHTML = '';
    const c = store.contacts || {};

    if (ui.contactsTab === 'friends') {
      const friends = c.friends || [];
      timelineZone.innerHTML = `
        <div class="workspace-pane">
          <div class="pane-header">
            <div class="field-row">
              ${renderMobileBackBtn()}
              <h2 class="pane-title">${isEn ? 'Friends' : 'Bạn bè'} (${friends.length})</h2>
            </div>
            <button type="button" class="btn btn-primary" data-action="open-create-group">
              ${icon('plus')}
              <span>${isEn ? 'Create group' : 'Tạo nhóm'}</span>
            </button>
          </div>
          <div class="data-list">
            ${
              friends.length === 0
                ? `<div class="data-row"><span class="dialog-subtitle">${isEn ? 'No friends yet.' : 'Chưa có danh sách bạn bè.'}</span></div>`
                : friends
                    .map(
                      (f) => `
                        <div class="data-row">
                          <div class="data-row-lead">
                            ${renderAvatar({ name: f.name, avatar: f.avatar, online: Boolean(f.online), hasNativeBridge: hasNativeBridge() })}
                            <div>
                              <div class="conv-name">${escapeHtml(f.name)}</div>
                              <div class="dialog-subtitle">${f.online ? (isEn ? 'Online' : 'Đang trực tuyến') : isEn ? 'Offline' : 'Ngoại tuyến'}</div>
                            </div>
                          </div>
                          <button type="button" class="btn btn-secondary" data-action="select-conv" data-conv-id="${escapeHtml(f.convId || f.id)}">
                            ${isEn ? 'Message' : 'Nhắn tin'}
                          </button>
                        </div>
                      `
                    )
                    .join('')
            }
          </div>
        </div>
      `;
      return;
    }

    if (ui.contactsTab === 'groups') {
      const groups = c.groups || [];
      timelineZone.innerHTML = `
        <div class="workspace-pane">
          <div class="pane-header">
            <div class="field-row">
              ${renderMobileBackBtn()}
              <h2 class="pane-title">${isEn ? 'Groups &amp; Communities' : 'Nhóm &amp; Cộng đồng'} (${groups.length})</h2>
            </div>
            <button type="button" class="btn btn-primary" data-action="open-create-group">
              ${icon('plus')}
              <span>${isEn ? 'New group' : 'Tạo nhóm mới'}</span>
            </button>
          </div>
          <div class="data-list">
            ${
              groups.length === 0
                ? `<div class="data-row"><span class="dialog-subtitle">${isEn ? 'No groups joined yet.' : 'Chưa tham gia nhóm hoặc cộng đồng nào.'}</span></div>`
                : groups
                    .map(
                      (g) => `
                        <div class="data-row">
                          <div class="data-row-lead">
                            ${renderAvatar({ name: g.name, avatar: g.avatar, hasNativeBridge: hasNativeBridge() })}
                            <div>
                              <div class="conv-name">${escapeHtml(g.name)}</div>
                              <div class="dialog-subtitle numeric-meta">${Number(g.members) || 0} ${isEn ? 'members' : 'thành viên'}</div>
                            </div>
                          </div>
                          <button type="button" class="btn btn-secondary" data-action="select-conv" data-conv-id="${escapeHtml(g.id)}">
                            ${isEn ? 'Open group' : 'Mở nhóm'}
                          </button>
                        </div>
                      `
                    )
                    .join('')
            }
          </div>
        </div>
      `;
      return;
    }

    if (ui.contactsTab === 'requests') {
      const rec = c.received || [];
      const sent = c.sent || [];
      timelineZone.innerHTML = `
        <div class="workspace-pane">
          <div class="field-row">
            ${renderMobileBackBtn()}
            <h2 class="pane-title">${isEn ? 'Friend requests' : 'Lời mời kết bạn'}</h2>
          </div>
          <h3 class="section-heading">${isEn ? 'Received' : 'Đã nhận'} (${rec.length})</h3>
          <div class="data-list">
            ${
              rec.length === 0
                ? `<div class="data-row"><span class="dialog-subtitle">${isEn ? 'No incoming friend requests.' : 'Không có lời mời kết bạn mới.'}</span></div>`
                : rec
                    .map(
                      (r) => `
                        <div class="data-row">
                          <div class="data-row-lead">
                            ${renderAvatar({ name: r.name, avatar: r.avatar, hasNativeBridge: hasNativeBridge() })}
                            <div>
                              <div class="conv-name">${escapeHtml(r.name)}</div>
                              <div class="dialog-subtitle">${escapeHtml(r.message || (isEn ? 'Sent you a friend request' : 'Muốn kết bạn với bạn'))}</div>
                            </div>
                          </div>
                          <div class="field-row">
                            <button type="button" class="btn btn-ghost" data-action="friend-op" data-op="decline" data-uid="${escapeHtml(r.id)}">${isEn ? 'Decline' : 'Từ chối'}</button>
                            <button type="button" class="btn btn-primary" data-action="friend-op" data-op="accept" data-uid="${escapeHtml(r.id)}">${isEn ? 'Accept' : 'Đồng ý'}</button>
                          </div>
                        </div>
                      `
                    )
                    .join('')
            }
          </div>
          <h3 class="section-heading">${isEn ? 'Sent' : 'Đã gửi'} (${sent.length})</h3>
          <div class="data-list">
            ${
              sent.length === 0
                ? `<div class="data-row"><span class="dialog-subtitle">${isEn ? 'No sent requests.' : 'Chưa gửi lời mời nào.'}</span></div>`
                : sent
                    .map(
                      (r) => `
                        <div class="data-row">
                          <div class="data-row-lead">
                            ${renderAvatar({ name: r.name, avatar: r.avatar, hasNativeBridge: hasNativeBridge() })}
                            <div>
                              <div class="conv-name">${escapeHtml(r.name)}</div>
                              <div class="dialog-subtitle">${escapeHtml(r.message || '')}</div>
                            </div>
                          </div>
                          <button type="button" class="btn btn-secondary" data-action="friend-op" data-op="cancel" data-uid="${escapeHtml(r.id)}">${isEn ? 'Cancel' : 'Thu hồi'}</button>
                        </div>
                      `
                    )
                    .join('')
            }
          </div>
        </div>
      `;
      return;
    }

    const foundUser = c.search?.user;
    timelineZone.innerHTML = `
      <div class="workspace-pane">
        <div class="field-row">
          ${renderMobileBackBtn()}
          <h2 class="pane-title">${isEn ? 'Find friends by phone number' : 'Tìm bạn qua số điện thoại'}</h2>
        </div>
        <form id="phone-search-form" class="field-row" style="max-width: 460px;">
          <input id="phone-search-input" type="tel" class="text-input" placeholder="${isEn ? 'Enter phone number (+84 or 09…)' : 'Nhập số điện thoại (+84 hoặc 09…)'}" required>
          <button type="submit" class="btn btn-primary">${isEn ? 'Search' : 'Tìm kiếm'}</button>
        </form>
        ${
          foundUser
            ? `
              <div class="data-list" style="max-width: 520px;">
                <div class="data-row">
                  <div class="data-row-lead">
                    ${renderAvatar({ name: foundUser.name, avatar: foundUser.avatar, hasNativeBridge: hasNativeBridge() })}
                    <div>
                      <div class="conv-name">${escapeHtml(foundUser.name)}</div>
                      <div class="dialog-subtitle">${escapeHtml(foundUser.bio || foundUser.zaloName || '')}</div>
                    </div>
                  </div>
                  <button type="button" class="btn btn-primary" data-action="friend-op" data-op="send" data-uid="${escapeHtml(foundUser.id)}">
                    ${isEn ? 'Add friend' : 'Kết bạn'}
                  </button>
                </div>
              </div>
            `
            : ''
        }
      </div>
    `;
    const phoneForm = document.getElementById('phone-search-form');
    phoneForm?.addEventListener('submit', (e) => {
      e.preventDefault();
      const val = document.getElementById('phone-search-input')?.value || '';
      window.olazInvoke('search_phone', { phone: val });
    });
    return;
  }

  // zCloud Workspace
  headerZone.innerHTML = '';
  const plan = store.cloud?.plan || {};
  const mediaTypes = store.cloud?.types?.media || {};
  const quota = Math.max(1, Number(plan.quota) || 1);
  const pct = (val) => Math.min(100, ((Number(val) || 0) / quota) * 100).toFixed(2);
  const cloudConvs = store.cloud?.conversations || [];

  timelineZone.innerHTML = `
    <div class="workspace-pane">
      <div class="pane-header">
        <div class="field-row">
          ${renderMobileBackBtn()}
          <h2 class="pane-title">${isEn ? 'zCloud Storage Management' : 'Quản lý dữ liệu zCloud'}</h2>
        </div>
        <button type="button" class="btn btn-secondary" data-action="refresh-zcloud">
          ${icon('refresh')}
          <span>${isEn ? 'Refresh' : 'Cập nhật'}</span>
        </button>
      </div>
      <div class="quota-panel">
        <div class="field-row-between">
          <strong>${escapeHtml(plan.name || (isEn ? 'Cloud storage quota' : 'Dung lượng lưu trữ đám mây'))}</strong>
          <span class="quota-nums">${formatBytes(plan.usage || 0)} / ${formatBytes(plan.quota || 0)}</span>
        </div>
        <div class="quota-bar-track" role="img" aria-label="${isEn ? 'zCloud storage breakdown' : 'Phân bổ dung lượng zCloud'}">
          <div class="quota-seg-photo" style="width: ${pct(mediaTypes.photo)}%"></div>
          <div class="quota-seg-video" style="width: ${pct(mediaTypes.video)}%"></div>
          <div class="quota-seg-file" style="width: ${pct(mediaTypes.file)}%"></div>
          <div class="quota-seg-voice" style="width: ${pct(mediaTypes.voice)}%"></div>
        </div>
        <div class="quota-legend numeric-meta">
          <span class="legend-item"><span class="label-dot quota-seg-photo"></span>${isEn ? 'Photos' : 'Ảnh'}: ${formatBytes(mediaTypes.photo || 0)}</span>
          <span class="legend-item"><span class="label-dot quota-seg-video"></span>Video: ${formatBytes(mediaTypes.video || 0)}</span>
          <span class="legend-item"><span class="label-dot quota-seg-file"></span>${isEn ? 'Files' : 'Tệp'}: ${formatBytes(mediaTypes.file || 0)}</span>
          <span class="legend-item"><span class="label-dot quota-seg-voice"></span>${isEn ? 'Voice' : 'Thoại'}: ${formatBytes(mediaTypes.voice || 0)}</span>
        </div>
      </div>

      <h3 class="section-heading">${isEn ? 'Usage by conversation' : 'Dung lượng theo trò chuyện'}</h3>
      <div class="data-list">
        ${
          cloudConvs.length === 0
            ? `<div class="data-row"><span class="dialog-subtitle">${isEn ? 'No per-conversation storage stats yet.' : 'Chưa có thống kê theo hội thoại.'}</span></div>`
            : cloudConvs
                .map(
                  (row) => `
                    <div class="data-row">
                      <div class="data-row-lead">
                        ${renderAvatar({ name: row.name, avatar: row.avatar, hasNativeBridge: hasNativeBridge() })}
                        <div>
                          <div class="conv-name">${escapeHtml(row.name)}</div>
                          <div class="dialog-subtitle">${row.isGroup ? (isEn ? 'Group chat' : 'Nhóm trò chuyện') : isEn ? 'Direct chat' : 'Trò chuyện cá nhân'}</div>
                        </div>
                      </div>
                      <span class="quota-nums field-label">${formatBytes(row.total || 0)}</span>
                    </div>
                  `
                )
                .join('')
        }
      </div>
    </div>
  `;
}

function renderInspector() {
  const el = document.getElementById('inspector-zone');
  if (!el || el.hidden) return;
  const conv = (store.conversations || []).find((c) => c.id === store.activeConvId);
  if (!conv) return;

  const f = getConversationFlags(conv);
  const shared = store.sharedMediaByConv?.[conv.id] || { images: [], files: [], links: [] };
  const tabItems = shared[ui.inspectorTab] || [];

  el.innerHTML = `
    <div class="inspector-profile">
      ${renderAvatar({
        name: conv.name,
        avatar: conv.avatar,
        online: Boolean(conv.online),
        sizeClass: 'avatar-lg',
        isSelfCloud: f.isSelfCloud,
        hasNativeBridge: hasNativeBridge(),
      })}
      <h3 class="inspector-name">${escapeHtml(conv.name)}</h3>
      <span class="dialog-subtitle">${conv.isE2EE ? 'Đã bật mã hóa đầu cuối E2EE' : conv.isGroup ? `${conv.members || 2} thành viên` : 'Tài khoản Zalo'}</span>
      <div class="inspector-quick-grid">
        <button type="button" class="inspector-quick-btn" data-action="quick-conv-mute" data-conv-id="${escapeHtml(conv.id)}">
          ${icon(conv.isMuted ? 'bellOff' : 'speaker')}
          <span>${conv.isMuted ? 'Bật báo' : 'Tắt báo'}</span>
        </button>
        <button type="button" class="inspector-quick-btn" data-action="quick-conv-pin" data-conv-id="${escapeHtml(conv.id)}">
          ${icon(f.isPinned ? 'unpin' : 'pin')}
          <span>${f.isPinned ? 'Bỏ ghim' : 'Ghim'}</span>
        </button>
        <button type="button" class="inspector-quick-btn" data-action="quick-conv-hide" data-conv-id="${escapeHtml(conv.id)}">
          ${icon('lock')}
          <span>${f.isHidden ? 'Bỏ ẩn' : 'Ẩn chat'}</span>
        </button>
      </div>
    </div>

    <div class="inspector-section">
      <div class="field-row-between">
        <h4 class="section-heading">Kho lưu trữ chung</h4>
      </div>
      <div class="segmented-tabs" role="tablist">
        <button type="button" class="seg-tab" role="tab" aria-selected="${ui.inspectorTab === 'images'}" data-action="inspector-tab" data-tab="images">Ảnh/Video</button>
        <button type="button" class="seg-tab" role="tab" aria-selected="${ui.inspectorTab === 'files'}" data-action="inspector-tab" data-tab="files">Tệp</button>
        <button type="button" class="seg-tab" role="tab" aria-selected="${ui.inspectorTab === 'links'}" data-action="inspector-tab" data-tab="links">Liên kết</button>
      </div>
      ${
        tabItems.length === 0
          ? `<p class="dialog-subtitle" style="padding: 12px 0;">Chưa có mục nào trong danh mục này.</p>`
          : ui.inspectorTab === 'images'
            ? `
              <div class="media-thumb-grid">
                ${tabItems
                  .map(
                    (it) => `
                      <button
                        type="button"
                        class="media-thumb-item"
                        data-action="open-direct-lightbox"
                        data-url="${escapeHtml(it.url)}"
                        data-media-type="${escapeHtml(it.type || it.kind || '')}"
                        data-poster="${escapeHtml(it.thumbnail || '')}"
                        data-title="${escapeHtml(it.title || 'Ảnh chia sẻ')}"
                      >
                        <img src="${escapeHtml(resolveMediaUrl(it.thumbnail || it.url, hasNativeBridge()))}" alt="${escapeHtml(it.title || 'Ảnh')}">
                      </button>
                    `
                  )
                  .join('')}
              </div>
            `
            : `
              <div class="form-stack-tight">
                ${tabItems
                  .map((it) =>
                    ui.inspectorTab === 'files'
                      ? `
                        <div class="picker-row">
                          <div style="min-width: 0;">
                            <div class="file-name">${escapeHtml(it.name)}</div>
                            <div class="file-size numeric-meta">${formatBytes(it.size || 0)}</div>
                          </div>
                          ${
                            it.path
                              ? `<button type="button" class="icon-btn" data-action="open-file" data-path="${escapeHtml(it.path)}" data-reveal="false" aria-label="Mở tệp">${icon('externalLink')}</button>`
                              : ''
                          }
                        </div>
                      `
                      : `
                        <a class="link-preview" href="${escapeHtml(it.url || '#')}" target="_blank" rel="noopener noreferrer">
                          <span class="link-domain">${escapeHtml(it.source || it.url)}</span>
                          <span class="link-title">${escapeHtml(it.title || it.url)}</span>
                        </a>
                      `
                  )
                  .join('')}
              </div>
            `
      }
    </div>
  `;
}

/* ==========================================================================
   9. Native <dialog> Modals & Mandatory Fallbacks
   ========================================================================== */

function openModalDialog(id) {
  const dlg = document.getElementById(id);
  if (!dlg || typeof dlg.showModal !== 'function') return null;
  if (!dlg.open) {
    dlg.showModal();
  }
  return dlg;
}

function closeAllDialogs() {
  for (const dlg of document.querySelectorAll('dialog[open]')) {
    dlg.close();
  }
}

function initDialogsAndFallbacks() {
  // Hydrate static [data-icon] placeholders inside index.html dialogs
  for (const node of document.querySelectorAll('[data-icon]')) {
    const name = node.getAttribute('data-icon');
    if (name) node.innerHTML = icon(name);
  }

  // Mandatory: reset dialog.returnValue = '' on close to prevent stale returnValue leaks
  for (const dialog of document.querySelectorAll('dialog')) {
    dialog.addEventListener('close', () => {
      dialog.returnValue = '';
      if (dialog.id === 'media-lightbox-dialog') {
        const stage = document.getElementById('lightbox-stage');
        if (stage) stage.innerHTML = '';
      }
    });
  }

  // Mandatory WebKitGTK fallback for <dialog closedby="any"> when HTMLDialogElement.prototype.closedBy is absent
  if (!('closedBy' in HTMLDialogElement.prototype)) {
    for (const dialog of document.querySelectorAll('dialog[closedby="any"]')) {
      dialog.addEventListener('click', (event) => {
        if (event.target !== dialog) return;
        const rect = dialog.getBoundingClientRect();
        const isDialogContent =
          rect.top <= event.clientY &&
          event.clientY <= rect.top + rect.height &&
          rect.left <= event.clientX &&
          event.clientX <= rect.left + rect.width;
        if (isDialogContent) return;
        dialog.close();
      });
    }
  }

  // Progressive enhancement fallback for range inputs when accent-color is unsupported
  if (typeof CSS !== 'undefined' && typeof CSS.supports === 'function' && !CSS.supports('accent-color', '#0068ff')) {
    document.addEventListener('input', (e) => {
      const target = e.target;
      if (target instanceof HTMLInputElement && target.type === 'range') {
        const min = Number(target.min) || 0;
        const max = Number(target.max) || 100;
        const val = Number(target.value) || 0;
        const pct = max > min ? ((val - min) / (max - min)) * 100 : 0;
        target.style.setProperty('--progress', `${pct}%`);
      }
    });
  }

  bindDialogForms();
}

function openQuickSwitcher() {
  const dlg = openModalDialog('quick-switcher-dialog');
  const input = document.getElementById('qs-search-input');
  if (!dlg || !input) return;
  input.value = '';
  ui.qsActiveIndex = 0;
  renderQuickSwitcherResults('');
  input.focus();
}

function renderQuickSwitcherResults(rawQuery) {
  const listEl = document.getElementById('qs-results-list');
  if (!listEl) return;
  const q = foldVietnamese(rawQuery).trim();
  const hiddenIds = new Set(store.convActions?.hidden?.ids || []);

  ui.qsItems = (store.conversations || [])
    .filter((c) => ui.unlockedHidden || !hiddenIds.has(c.id))
    .filter((c) => !q || foldVietnamese(c.name).includes(q))
    .slice(0, 12);

  ui.qsActiveIndex = Math.min(ui.qsActiveIndex, Math.max(0, ui.qsItems.length - 1));

  if (ui.qsItems.length === 0) {
    listEl.innerHTML = `<li class="dialog-subtitle" style="padding: 14px 10px;">Không tìm thấy cuộc trò chuyện nào khớp từ khóa.</li>`;
    return;
  }

  listEl.innerHTML = ui.qsItems
    .map(
      (c, idx) => `
        <li
          class="switcher-item"
          role="option"
          aria-selected="${idx === ui.qsActiveIndex}"
          data-action="qs-pick"
          data-conv-id="${escapeHtml(c.id)}"
        >
          ${renderAvatar({ name: c.name, avatar: c.avatar, sizeClass: 'avatar-sm', hasNativeBridge: hasNativeBridge() })}
          <div style="min-width: 0; flex: 1;">
            <div class="conv-name">${escapeHtml(c.name)}</div>
            <div class="dialog-subtitle" style="white-space:nowrap;overflow:hidden;text-overflow:ellipsis;">${escapeHtml(c.preview?.text || '')}</div>
          </div>
          ${c.unread ? `<span class="unread-badge">${c.unread}</span>` : ''}
        </li>
      `
    )
    .join('');
}

function openLightboxForMessage(msgId) {
  const bucket = store.messagesByConv?.[store.activeConvId];
  const msg = bucket?.items?.find((m) => m.id === msgId);
  if (!msg) return;
  const kind = normalizeMessageKind(msg);
  const rawUrl = kind.path || kind.url || kind.thumbnail || '';
  const src = resolveMediaUrl(rawUrl, hasNativeBridge());
  const poster = resolveMediaUrl(kind.thumbnail, hasNativeBridge());
  openDirectLightbox({
    src,
    rawUrl,
    title: msg.senderName || (isEnglishLang() ? 'Media' : 'Hình ảnh'),
    subtitle: formatShortTime(msg.date),
    localPath: kind.path || '',
    msgId: msg.id,
    mediaType: kind.type || 'photo',
    poster,
    duration: kind.duration || 0,
  });
}

function openDirectLightbox({
  src,
  rawUrl = '',
  title,
  subtitle = '',
  localPath = '',
  msgId = '',
  mediaType = '',
  poster = '',
  duration = 0,
}) {
  const stage = document.getElementById('lightbox-stage');
  const titleEl = document.getElementById('lightbox-dialog-title');
  const subEl = document.getElementById('lightbox-subtitle');
  const dlBtn = document.getElementById('lightbox-download-btn');
  if (!stage || !titleEl) return;

  const isEn = isEnglishLang();
  const resolvedSrc = resolveMediaUrl(src, hasNativeBridge());
  const resolvedPoster = resolveMediaUrl(poster, hasNativeBridge());
  const candidateUrl = String(rawUrl || src || '');
  const isVideo =
    mediaType === 'video' || /\.(mp4|webm|mov|mkv|m4v)(\?|#|$)/i.test(candidateUrl);

  titleEl.textContent = title || (isEn ? 'Media viewer' : 'Xem phương tiện');
  if (subEl) subEl.textContent = subtitle;

  if (!resolvedSrc && !candidateUrl) {
    stage.innerHTML = '';
  } else if (isVideo) {
    if (!canUseInlineGstAudio()) {
      stage.innerHTML = renderSafeVideoCard({
        rawUrl: candidateUrl,
        poster: resolvedPoster,
        duration,
        isEn,
      });
    } else {
      stage.innerHTML = `<video controls preload="none" ${resolvedPoster ? `poster="${escapeHtml(resolvedPoster)}"` : ''} src="${escapeHtml(resolvedSrc)}"></video>`;
    }
  } else {
    stage.innerHTML = `<img src="${escapeHtml(resolvedSrc)}" alt="${escapeHtml(title || '')}">`;
  }

  if (dlBtn) {
    dlBtn.onclick = () => {
      if (localPath) {
        window
          .olazInvoke('open_downloaded_file', { path: localPath, revealOnly: false })
          .catch(() => {});
      } else if (isVideo && candidateUrl) {
        window
          .olazInvoke('open_media_external', { url: unwrapExternalMediaUrl(candidateUrl) })
          .catch(() => {});
      } else if (msgId && store.activeConvId) {
        window
          .olazInvoke('resolve_media', {
            convId: store.activeConvId,
            msgId,
            kind: isVideo ? 'video' : 'photo',
          })
          .catch(() => {});
      }
    };
  }
  openModalDialog('media-lightbox-dialog');
}

function openClipboardPasteDialog(base64Png) {
  ui.pendingPasteBase64 = base64Png;
  const frame = document.getElementById('paste-preview-frame');
  const note = document.getElementById('paste-target-label');
  const conv = (store.conversations || []).find((c) => c.id === store.activeConvId);
  if (frame) {
    frame.innerHTML = `<img src="data:image/png;base64,${escapeHtml(base64Png)}" alt="Ảnh chụp từ bộ nhớ tạm">`;
  }
  if (note) note.textContent = `Sẽ gửi trực tiếp vào "${conv?.name || 'cuộc trò chuyện hiện tại'}".`;
  openModalDialog('clipboard-paste-dialog');
}

function openCreateGroupDialog() {
  if (!(store.contacts?.friends?.length)) {
    requestContactsLoad();
  }
  const nameInput = document.getElementById('group-name-input');
  const filterInput = document.getElementById('group-member-filter');
  if (nameInput) nameInput.value = '';
  if (filterInput) filterInput.value = '';
  renderGroupMemberChecklist('');
  openModalDialog('create-group-dialog');
}

function renderGroupMemberChecklist(rawQuery) {
  const listEl = document.getElementById('group-member-list');
  const countEl = document.getElementById('group-selected-count');
  if (!listEl) return;

  const checkedIds = new Set(
    Array.from(listEl.querySelectorAll('input[type="checkbox"]:checked')).map((el) => el.value)
  );
  const q = foldVietnamese(rawQuery).trim();
  const friends = (store.contacts?.friends || []).filter((f) => !q || foldVietnamese(f.name).includes(q));

  listEl.innerHTML = friends
    .map(
      (f) => `
        <div class="picker-row">
          <label class="check-field" for="grp_mem_${escapeHtml(f.id)}" style="flex: 1;">
            <input
              type="checkbox"
              id="grp_mem_${escapeHtml(f.id)}"
              class="visually-hidden-fallback"
              value="${escapeHtml(f.id)}"
              ${checkedIds.has(f.id) ? 'checked' : ''}
            >
            <span>${escapeHtml(f.name)}</span>
          </label>
        </div>
      `
    )
    .join('');

  const updateCount = () => {
    const n = listEl.querySelectorAll('input[type="checkbox"]:checked').length;
    if (countEl) countEl.textContent = `Đã chọn ${n}`;
  };
  listEl.onchange = updateCount;
  updateCount();
}

function openForwardDialog(msgIds) {
  ui.pendingForwardMsgIds = msgIds;
  const previewEl = document.getElementById('forward-preview-box');
  const filterInput = document.getElementById('forward-filter-input');
  if (filterInput) filterInput.value = '';

  const bucket = store.messagesByConv?.[store.activeConvId];
  const firstMsg = bucket?.items?.find((m) => m.id === msgIds[0]);
  if (previewEl) {
    previewEl.innerHTML = `
      <span class="quote-sender">${icon('forward')}<span>Chuyển tiếp ${msgIds.length} tin nhắn</span></span>
      <p class="quote-body">${escapeHtml(firstMsg?.text || '[Phương tiện]')}</p>
    `;
  }
  renderForwardTargetChecklist('');
  openModalDialog('forward-dialog');
}

function renderForwardTargetChecklist(rawQuery) {
  const listEl = document.getElementById('forward-target-list');
  const countEl = document.getElementById('forward-selected-count');
  if (!listEl) return;

  const checkedIds = new Set(
    Array.from(listEl.querySelectorAll('input[type="checkbox"]:checked')).map((el) => el.value)
  );
  const q = foldVietnamese(rawQuery).trim();
  const targets = (store.conversations || []).filter((c) => !q || foldVietnamese(c.name).includes(q));

  listEl.innerHTML = targets
    .map(
      (c) => `
        <div class="picker-row">
          <label class="check-field" for="fwd_conv_${escapeHtml(c.id)}" style="flex: 1;">
            <input
              type="checkbox"
              id="fwd_conv_${escapeHtml(c.id)}"
              class="visually-hidden-fallback"
              value="${escapeHtml(c.id)}"
              ${checkedIds.has(c.id) ? 'checked' : ''}
            >
            <span>${escapeHtml(c.name)}</span>
          </label>
        </div>
      `
    )
    .join('');

  const updateCount = () => {
    const n = listEl.querySelectorAll('input[type="checkbox"]:checked').length;
    if (countEl) countEl.textContent = `Đã chọn ${n}`;
  };
  listEl.onchange = updateCount;
  updateCount();
}

function openCreatePollDialog() {
  const qInput = document.getElementById('poll-question-input');
  const wrap = document.getElementById('poll-options-inputs');
  if (qInput) qInput.value = '';
  if (wrap) {
    wrap.innerHTML = `
      <input type="text" class="text-input poll-opt-field" placeholder="Phương án 1" required maxlength="120">
      <input type="text" class="text-input poll-opt-field" placeholder="Phương án 2" required maxlength="120">
    `;
  }
  openModalDialog('poll-create-dialog');
}

function openPollDetailDialog(pollId, fallbackQuestion = '') {
  ui.activePollId = pollId;
  const poll = store.polls?.[pollId] || {
    id: pollId,
    question: fallbackQuestion || 'Bình chọn nhóm',
    multi: true,
    allowAdd: true,
    voters: 0,
    options: [],
  };

  const titleEl = document.getElementById('poll-detail-title');
  const metaEl = document.getElementById('poll-detail-meta');
  const optsEl = document.getElementById('poll-detail-options');
  const addWrap = document.getElementById('poll-new-option-wrap');
  const addInput = document.getElementById('poll-new-option-input');

  if (titleEl) titleEl.textContent = poll.question;
  if (metaEl) {
    metaEl.textContent = `${poll.multi ? 'Chọn nhiều phương án' : 'Chọn 1 phương án'} · ${Number(poll.voters) || 0} người đã tham gia`;
  }
  if (addWrap) addWrap.hidden = !poll.allowAdd;
  if (addInput) addInput.value = '';

  const inputType = poll.multi ? 'checkbox' : 'radio';
  if (optsEl) {
    optsEl.innerHTML = (poll.options || [])
      .map(
        (opt) => `
          <div class="picker-row">
            <label class="check-field" for="vote_opt_${escapeHtml(opt.id)}" style="flex: 1;">
              <input
                type="${inputType}"
                name="poll_vote_choice"
                id="vote_opt_${escapeHtml(opt.id)}"
                class="visually-hidden-fallback"
                value="${escapeHtml(opt.id)}"
                ${opt.voted ? 'checked' : ''}
              >
              <span>${escapeHtml(opt.content)}</span>
            </label>
            <span class="poll-count dialog-subtitle">${Number(opt.votes) || 0} phiếu</span>
          </div>
        `
      )
      .join('');
  }

  openModalDialog('poll-detail-dialog');
}

function openHiddenPinDialog(mode = 'unlock', convId = null) {
  ui.hiddenPinContext = { mode, convId };
  const titleEl = document.getElementById('hidden-pin-title');
  const descEl = document.getElementById('hidden-pin-desc');
  const pinInput = document.getElementById('hidden-pin-input');
  const newWrap = document.getElementById('hidden-new-pin-wrap');
  const newInput = document.getElementById('hidden-new-pin-input');
  const errEl = document.getElementById('hidden-pin-error');

  if (pinInput) pinInput.value = '';
  if (newInput) newInput.value = '';
  if (errEl) {
    errEl.hidden = true;
    errEl.textContent = '';
  }

  if (mode === 'change') {
    if (titleEl) titleEl.textContent = 'Đổi mã PIN trò chuyện ẩn';
    if (descEl) descEl.textContent = 'Nhập mã PIN 4 chữ số hiện tại và mã PIN mới.';
    if (newWrap) newWrap.hidden = false;
  } else if (mode === 'set') {
    if (titleEl) titleEl.textContent = 'Thiết lập mã PIN mới';
    if (descEl) descEl.textContent = 'Tạo mã PIN 4 chữ số để bảo vệ các cuộc trò chuyện ẩn.';
    if (newWrap) newWrap.hidden = true;
  } else {
    if (titleEl) titleEl.textContent = 'Mở khóa trò chuyện ẩn';
    if (descEl) descEl.textContent = 'Nhập mã PIN 4 chữ số để tiếp tục.';
    if (newWrap) newWrap.hidden = true;
  }

  openModalDialog('hidden-pin-dialog');
}

function openSettingsDialog() {
  const body = document.getElementById('settings-dialog-body');
  if (!body) return;
  const s = store.settings || {};
  const isEn = isEnglishLang();
  const langValue = isEn ? 'en' : 'vi';
  const isReady = store.phase === 'ready';
  const isOmarchy = document.documentElement.dataset.omarchy === 'true';
  const radiusNow = getComputedStyle(document.documentElement).getPropertyValue('--radius').trim() || '12px';

  const dialogTitleEl = document.getElementById('settings-dialog-title');
  if (dialogTitleEl) {
    dialogTitleEl.textContent = isEn ? 'Olaz Settings' : 'Thiết lập Olaz';
  }

  body.innerHTML = `
    <h3 class="settings-group-heading">${isEn ? 'Appearance &amp; Linux Integration' : 'Giao diện &amp; Tích hợp Linux'}</h3>
    <div class="settings-card">
      <div class="setting-row">
        <div class="setting-copy">
          <span class="setting-title">${isEn ? 'Interface language' : 'Ngôn ngữ hiển thị'}</span>
          <span class="setting-desc">${isEn ? 'Switch between Vietnamese and English immediately' : 'Chuyển đổi tức thì giữa Tiếng Việt và Tiếng Anh'}</span>
        </div>
        <select id="set-language-select" class="select-input" style="width: 170px;" aria-label="${isEn ? 'Interface language' : 'Ngôn ngữ hiển thị'}">
          <option value="vi" ${langValue === 'vi' ? 'selected' : ''}>Tiếng Việt</option>
          <option value="en" ${langValue === 'en' ? 'selected' : ''}>English</option>
        </select>
      </div>
      <div class="setting-row">
        <div class="setting-copy">
          <span class="setting-title">${isEn ? 'Color theme' : 'Chế độ màu sắc'}</span>
          <span class="setting-desc">${isEn ? 'Follow Wayland/GTK system preference or override manually' : 'Tự động theo hệ thống Wayland/GTK hoặc chọn thủ công'}</span>
        </div>
        <select id="set-theme-select" class="select-input" style="width: 170px;" aria-label="${isEn ? 'Color theme' : 'Chế độ màu sắc'}">
          <option value="system" ${s.theme === 'system' ? 'selected' : ''}>${isEn ? 'System' : 'Theo hệ thống'}</option>
          <option value="light" ${s.theme === 'light' ? 'selected' : ''}>${isEn ? 'Light' : 'Giao diện Sáng'}</option>
          <option value="dark" ${s.theme === 'dark' ? 'selected' : ''}>${isEn ? 'Dark' : 'Giao diện Tối'}</option>
        </select>
      </div>
      <div class="setting-row">
        <div class="setting-copy">
          <span class="setting-title">${isEn ? 'Sync Omarchy / Hyprland palette' : 'Đồng bộ bảng màu Omarchy / Hyprland'}</span>
          <span class="setting-desc numeric-meta">${
            isEn
              ? `Status: ${isOmarchy ? 'Live connected' : 'GNOME default'} · Radius: ${escapeHtml(radiusNow)}`
              : `Trạng thái: ${isOmarchy ? 'Đang kết nối trực tiếp' : 'Mặc định GNOME'} · Bo góc: ${escapeHtml(radiusNow)}`
          }</span>
        </div>
        <label class="check-field" for="set-omarchy-check">
          <input type="checkbox" id="set-omarchy-check" class="visually-hidden-fallback" ${s.omarchySync !== false ? 'checked' : ''}>
          <span>${isEn ? 'On' : 'Bật'}</span>
        </label>
      </div>
    </div>

    <h3 class="settings-group-heading">${isEn ? 'Composer &amp; Notifications' : 'Soạn thảo &amp; Thông báo'}</h3>
    <div class="settings-card">
      <div class="setting-row">
        <div class="setting-copy">
          <span class="setting-title">${isEn ? 'Press Enter to send message' : 'Nhấn Enter để gửi tin nhắn'}</span>
          <span class="setting-desc">${isEn ? 'Yields Enter while composing Vietnamese Telex/VNI (fcitx5 / ibus)' : 'Tự động nhường phím Enter khi đang gõ dấu tiếng Việt (fcitx5 / ibus)'}</span>
        </div>
        <label class="check-field" for="set-enter-send-check">
          <input type="checkbox" id="set-enter-send-check" class="visually-hidden-fallback" ${s.enterToSend !== false ? 'checked' : ''}>
          <span>${isEn ? 'On' : 'Bật'}</span>
        </label>
      </div>
      <div class="setting-row">
        <div class="setting-copy">
          <span class="setting-title">${isEn ? 'Keyword Sticker suggestions' : 'Gợi ý Sticker theo từ khóa'}</span>
          <span class="setting-desc">${isEn ? 'Show quick sticker bar when typing short keywords' : 'Hiển thị thanh gợi ý nhanh khi nhập từ khóa ngắn'}</span>
        </div>
        <label class="check-field" for="set-sticker-check">
          <input type="checkbox" id="set-sticker-check" class="visually-hidden-fallback" ${s.stickerSuggest !== false ? 'checked' : ''}>
          <span>${isEn ? 'On' : 'Bật'}</span>
        </label>
      </div>
      <div class="setting-row">
        <div class="setting-copy">
          <span class="setting-title">${isEn ? 'Notification volume' : 'Âm lượng thông báo'}</span>
          <span id="set-vol-label" class="setting-desc numeric-meta">${Number(s.soundVolume ?? 80)}%</span>
        </div>
        <input id="set-vol-range" type="range" min="0" max="100" value="${Number(s.soundVolume ?? 80)}" style="width: 160px;" aria-label="${isEn ? 'Notification volume' : 'Âm lượng thông báo'}">
      </div>
    </div>

    <h3 class="settings-group-heading">${isEn ? 'Diagnostics &amp; Security' : 'Bảo mật &amp; Tài khoản'}</h3>
    <div class="settings-card">
      ${
        isReady
          ? `
            <div class="setting-row">
              <div class="setting-copy">
                <span class="setting-title">${isEn ? 'Hidden chat PIN' : 'Mã PIN trò chuyện ẩn'}</span>
                <span class="setting-desc">${isEn ? 'Change or reset the 4-digit PIN protecting hidden chats' : 'Đổi hoặc đặt lại mã PIN 4 chữ số bảo vệ hội thoại riêng tư'}</span>
              </div>
              <div class="field-row">
                <button type="button" class="btn btn-secondary" data-action="settings-change-pin">${isEn ? 'Change PIN' : 'Đổi PIN'}</button>
                <button type="button" class="btn btn-ghost" data-action="settings-reset-pin">${isEn ? 'Reset' : 'Đặt lại'}</button>
              </div>
            </div>
          `
          : ''
      }
      <div class="setting-row">
        <div class="setting-copy">
          <span class="setting-title">${isEn ? 'Diagnostics &amp; Session' : 'Chẩn đoán &amp; Phiên đăng nhập'}</span>
          <span class="setting-desc">${
            isReady
              ? isEn
                ? 'Inspect background WebView or sign out of your Zalo account'
                : 'Mở cửa sổ kiểm tra WebView nền hoặc đăng xuất tài khoản'
              : isEn
                ? 'Open the background Zalo WebView window for diagnostics'
                : 'Mở cửa sổ kiểm tra WebView nền khi cần chẩn đoán kết nối'
          }</span>
        </div>
        <div class="field-row">
          <button type="button" class="btn btn-secondary" data-action="toggle-debug-webview">${isEn ? 'Background WebView' : 'WebView nền'}</button>
          ${
            isReady
              ? `
                <button type="button" class="btn btn-danger" data-action="logout">
                  ${icon('logout')}
                  <span>${isEn ? 'Sign out' : 'Đăng xuất'}</span>
                </button>
              `
              : ''
          }
        </div>
      </div>
    </div>
  `;

  body.onchange = (e) => {
    const patch = {
      language: document.getElementById('set-language-select')?.value || s.language || 'vi',
      theme: document.getElementById('set-theme-select')?.value || 'system',
      omarchySync: Boolean(document.getElementById('set-omarchy-check')?.checked),
      enterToSend: Boolean(document.getElementById('set-enter-send-check')?.checked),
      stickerSuggest: Boolean(document.getElementById('set-sticker-check')?.checked),
      soundVolume: Number(document.getElementById('set-vol-range')?.value ?? 80),
    };
    store.settings = { ...store.settings, ...patch };
    applyUserThemePref(store.settings.theme);
    applyUserLanguagePref(store.settings.language);

    if (e.target?.id === 'set-vol-range') {
      const lbl = document.getElementById('set-vol-label');
      if (lbl) lbl.textContent = `${patch.soundVolume}%`;
    }

    renderRoot({});
    if (e.target?.id === 'set-language-select') {
      openSettingsDialog();
    }

    window.olazInvoke('update_settings', patch).catch(() => {});
  };

  openModalDialog('settings-dialog');
}

function bindDialogForms() {
  // Quick Switcher search & keyboard navigation
  const qsInput = document.getElementById('qs-search-input');
  qsInput?.addEventListener('input', () => {
    ui.qsActiveIndex = 0;
    renderQuickSwitcherResults(qsInput.value);
  });
  qsInput?.addEventListener('keydown', (e) => {
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === 'ArrowDown' && ui.qsItems.length > 0) {
      e.preventDefault();
      ui.qsActiveIndex = (ui.qsActiveIndex + 1) % ui.qsItems.length;
      renderQuickSwitcherResults(qsInput.value);
    } else if (e.key === 'ArrowUp' && ui.qsItems.length > 0) {
      e.preventDefault();
      ui.qsActiveIndex = (ui.qsActiveIndex - 1 + ui.qsItems.length) % ui.qsItems.length;
      renderQuickSwitcherResults(qsInput.value);
    } else if (e.key === 'Enter' && ui.qsItems[ui.qsActiveIndex]) {
      e.preventDefault();
      const target = ui.qsItems[ui.qsActiveIndex];
      document.getElementById('quick-switcher-dialog')?.close();
      ui.destination = 'inbox';
      ui.mobilePane = 'stage';
      window.olazInvoke('select_conversation', { convId: target.id }).catch(() => {});
    }
  });

  // Clipboard paste confirm
  document.getElementById('paste-confirm-send-btn')?.addEventListener('click', async () => {
    const base64Png = ui.pendingPasteBase64;
    const convId = store.activeConvId;
    document.getElementById('clipboard-paste-dialog')?.close();
    if (base64Png && convId) {
      await window.olazInvoke('paste_clipboard_image', { convId, base64Png });
    }
  });

  // Create Group filter & submit
  const grpFilter = document.getElementById('group-member-filter');
  grpFilter?.addEventListener('input', () => renderGroupMemberChecklist(grpFilter.value));

  document.getElementById('create-group-form')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    const name = document.getElementById('group-name-input')?.value.trim();
    const memberIds = Array.from(
      document.querySelectorAll('#group-member-list input[type="checkbox"]:checked')
    ).map((el) => el.value);
    if (!name) return;
    document.getElementById('create-group-dialog')?.close();
    await window.olazInvoke('create_group', { name, memberIds });
  });

  // Forward filter & submit
  const fwdFilter = document.getElementById('forward-filter-input');
  fwdFilter?.addEventListener('input', () => renderForwardTargetChecklist(fwdFilter.value));

  document.getElementById('forward-form')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    const targetConvIds = Array.from(
      document.querySelectorAll('#forward-target-list input[type="checkbox"]:checked')
    ).map((el) => el.value);
    if (targetConvIds.length === 0 || !store.activeConvId) return;
    document.getElementById('forward-dialog')?.close();
    await window.olazInvoke('forward_messages', {
      fromConvId: store.activeConvId,
      msgIds: ui.pendingForwardMsgIds,
      targetConvIds,
    });
  });

  // Poll Create add option & submit
  document.getElementById('poll-add-option-btn')?.addEventListener('click', () => {
    const wrap = document.getElementById('poll-options-inputs');
    if (!wrap) return;
    const idx = wrap.querySelectorAll('.poll-opt-field').length + 1;
    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'text-input poll-opt-field';
    input.placeholder = `Phương án ${idx}`;
    input.maxLength = 120;
    wrap.appendChild(input);
    input.focus();
  });

  document.getElementById('poll-create-form')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    const question = document.getElementById('poll-question-input')?.value.trim();
    const options = Array.from(document.querySelectorAll('#poll-options-inputs .poll-opt-field'))
      .map((el) => el.value.trim())
      .filter(Boolean);
    if (!question || options.length < 2 || !store.activeConvId) return;
    const multi = Boolean(document.getElementById('poll-multi-check')?.checked);
    const allowAdd = Boolean(document.getElementById('poll-allow-add-check')?.checked);
    const anonymous = Boolean(document.getElementById('poll-anon-check')?.checked);
    document.getElementById('poll-create-dialog')?.close();
    await window.olazInvoke('create_poll', {
      convId: store.activeConvId,
      question,
      options,
      multi,
      anonymous,
      allowAdd,
    });
  });

  // Poll Vote submit
  document.getElementById('poll-vote-form')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (!ui.activePollId || !store.activeConvId) return;
    const votedOptionIds = Array.from(
      document.querySelectorAll('#poll-detail-options input:checked')
    ).map((el) => el.value);
    const extraText = document.getElementById('poll-new-option-input')?.value.trim() || '';
    const newOptions = extraText ? [{ content: extraText, voted: true }] : [];
    document.getElementById('poll-detail-dialog')?.close();
    await window.olazInvoke('vote_poll', {
      convId: store.activeConvId,
      pollId: ui.activePollId,
      votedOptionIds,
      newOptions,
    });
  });

  // Hidden PIN form submit
  document.getElementById('hidden-pin-form')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    const pin = document.getElementById('hidden-pin-input')?.value.trim() || '';
    const newPin = document.getElementById('hidden-new-pin-input')?.value.trim() || '';
    const errEl = document.getElementById('hidden-pin-error');

    try {
      const { mode, convId } = ui.hiddenPinContext;
      if (mode === 'unlock') {
        await window.olazInvoke('unlock_hidden_pin', { pin });
      } else if (mode === 'hide_conv' && convId) {
        await window.olazInvoke('unlock_hidden_pin', { pin });
        await window.olazInvoke('conv_action', { convId, action: 'hide' });
      } else if (mode === 'change') {
        await window.olazInvoke('manage_pin', { op: 'change', pin, newPin });
      } else if (mode === 'set') {
        await window.olazInvoke('manage_pin', { op: 'set', pin, newPin: pin });
      }
      document.getElementById('hidden-pin-dialog')?.close();
    } catch (err) {
      if (errEl) {
        errEl.hidden = false;
        errEl.textContent = err.message || 'Mã PIN không hợp lệ.';
      }
    }
  });
}

/* ==========================================================================
   10. Context Menus, Sticker Picker, Incoming Call & Toasts
   ========================================================================== */

function showFloatingMenu(anchorRect, itemsHtml) {
  const menu = document.getElementById('context-menu');
  if (!menu) return;
  menu.innerHTML = itemsHtml;
  menu.hidden = false;
  const x = Math.min(window.innerWidth - 230, Math.max(8, anchorRect.left));
  const y = Math.min(window.innerHeight - 280, Math.max(8, anchorRect.bottom + 4));
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;
}

function hideFloatingOverlays() {
  const menu = document.getElementById('context-menu');
  if (menu) menu.hidden = true;
  const stk = document.getElementById('sticker-popover');
  if (stk) stk.hidden = true;
}

function openConversationContextMenu(convId, rect) {
  const conv = (store.conversations || []).find((c) => c.id === convId);
  if (!conv) return;
  const f = getConversationFlags(conv);
  const labels = store.convExtras?.labels || [];

  showFloatingMenu(
    rect,
    `
      <button type="button" class="menu-item" data-action="ctx-conv-op" data-conv-id="${escapeHtml(convId)}" data-op="${f.isPinned ? 'unpin' : 'pin'}">
        ${icon(f.isPinned ? 'unpin' : 'pin')}
        <span>${f.isPinned ? 'Bỏ ghim hội thoại' : 'Ghim hội thoại'}</span>
      </button>
      <button type="button" class="menu-item" data-action="ctx-conv-op" data-conv-id="${escapeHtml(convId)}" data-op="${conv.unread ? 'mark_read' : 'mark_unread'}">
        ${icon('check')}
        <span>${conv.unread ? 'Đánh dấu đã đọc' : 'Đánh dấu chưa đọc'}</span>
      </button>
      <button type="button" class="menu-item" data-action="ctx-conv-op" data-conv-id="${escapeHtml(convId)}" data-op="${conv.isMuted ? 'unmute' : 'mute'}" data-duration="3600">
        ${icon(conv.isMuted ? 'speaker' : 'bellOff')}
        <span>${conv.isMuted ? 'Bật lại thông báo' : 'Tắt thông báo (1 giờ)'}</span>
      </button>
      ${
        labels.length > 0
          ? `
            <div class="menu-sep"></div>
            ${labels
              .map(
                (l) => `
                  <button type="button" class="menu-item" data-action="ctx-conv-label" data-conv-id="${escapeHtml(convId)}" data-label-id="${conv.labelId === l.id ? '' : escapeHtml(l.id)}">
                    <span class="label-dot" style="background-color:${escapeHtml(l.color)}"></span>
                    <span>${conv.labelId === l.id ? `Bỏ nhãn ${escapeHtml(l.name)}` : `Gắn nhãn ${escapeHtml(l.name)}`}</span>
                  </button>
                `
              )
              .join('')}
          `
          : ''
      }
      <div class="menu-sep"></div>
      <button type="button" class="menu-item" data-action="quick-conv-hide" data-conv-id="${escapeHtml(convId)}">
        ${icon('lock')}
        <span>${f.isHidden ? 'Bỏ ẩn trò chuyện' : 'Ẩn trò chuyện (PIN)'}</span>
      </button>
    `
  );
}

function openMessageContextMenu(msgId, rect) {
  const convId = store.activeConvId;
  const msg = store.messagesByConv?.[convId]?.items?.find((m) => m.id === msgId);
  if (!msg) return;

  showFloatingMenu(
    rect,
    `
      <button type="button" class="menu-item" data-action="ctx-msg-copy" data-msg-id="${escapeHtml(msg.id)}">
        ${icon('copy')}
        <span>Sao chép văn bản</span>
      </button>
      <button type="button" class="menu-item" data-action="ctx-msg-pin" data-msg-id="${escapeHtml(msg.id)}">
        ${icon('pin')}
        <span>Ghim tin nhắn</span>
      </button>
      <button type="button" class="menu-item" data-action="ctx-msg-star" data-msg-id="${escapeHtml(msg.id)}">
        ${icon('star')}
        <span>Đánh dấu sao</span>
      </button>
      <div class="menu-sep"></div>
      ${
        msg.isMine
          ? `
            <button type="button" class="menu-item is-danger" data-action="ctx-msg-recall" data-msg-id="${escapeHtml(msg.id)}">
              ${icon('refresh')}
              <span>Thu hồi với mọi người</span>
            </button>
          `
          : ''
      }
      <button type="button" class="menu-item is-danger" data-action="ctx-msg-delete" data-msg-id="${escapeHtml(msg.id)}">
        ${icon('trash')}
        <span>Xóa chỉ ở phía tôi</span>
      </button>
    `
  );
}

function toggleStickerPopover(anchorBtn) {
  const pop = document.getElementById('sticker-popover');
  if (!pop) return;
  if (!pop.hidden) {
    pop.hidden = true;
    return;
  }
  const rect = anchorBtn.getBoundingClientRect();
  const items = store.stickers?.items?.length ? store.stickers.items : DEMO_STICKERS;
  pop.innerHTML = `
    <div class="field-row-between">
      <strong style="font-size: 0.85rem;">Bộ sưu tập Sticker</strong>
      <button type="button" class="icon-btn" style="width:26px;height:26px" data-action="close-overlays" aria-label="Đóng bảng Sticker">
        ${icon('close')}
      </button>
    </div>
    <div class="sticker-grid scroller">
      ${items
        .map(
          (s) => `
            <button
              type="button"
              class="sticker-thumb-btn"
              data-action="send-suggested-sticker"
              data-sticker-id="${escapeHtml(s.id)}"
              data-cate-id="${escapeHtml(s.cateId || '')}"
            >
              <img src="${escapeHtml(resolveMediaUrl(s.url, hasNativeBridge()))}" alt="${escapeHtml(s.label || 'Sticker')}">
            </button>
          `
        )
        .join('')}
    </div>
  `;
  pop.hidden = false;
  pop.style.left = `${Math.max(12, Math.min(window.innerWidth - 336, rect.left))}px`;
  pop.style.bottom = `${Math.max(12, window.innerHeight - rect.top + 8)}px`;
}

function renderIncomingCallBanner() {
  const banner = document.getElementById('incoming-call-banner');
  if (!banner) return;
  const call = store.incomingCall;
  if (!call) {
    banner.hidden = true;
    banner.innerHTML = '';
    return;
  }
  banner.hidden = false;
  banner.innerHTML = `
    <div class="data-row-lead">
      ${renderAvatar({ name: call.peerName || 'Cuộc gọi Zalo', avatar: call.avatar, hasNativeBridge: hasNativeBridge() })}
      <div>
        <div class="conv-name">${escapeHtml(call.peerName || 'Cuộc gọi đến')}</div>
        <div class="dialog-subtitle">${call.video ? 'Cuộc gọi video Zalo' : 'Cuộc gọi thoại Zalo'}</div>
      </div>
    </div>
    <button type="button" class="btn btn-danger" data-action="dismiss-call-banner">Đóng</button>
  `;
}

function showToast(text, undoPayload = null) {
  const region = document.getElementById('toast-region');
  if (!region || !text) return;
  const item = document.createElement('div');
  item.className = 'toast-item';
  item.innerHTML = `
    <span>${escapeHtml(text)}</span>
    ${
      undoPayload
        ? `<button type="button" class="btn btn-inline" data-undo="true">Hoàn tác</button>`
        : ''
    }
  `;
  if (undoPayload) {
    item.querySelector('[data-undo]')?.addEventListener('click', () => {
      item.remove();
      window.olazInvoke('undo_delete', undoPayload);
    });
  }
  region.appendChild(item);
  setTimeout(() => item.remove(), 4500);
}

/* ==========================================================================
   11. Global Event Delegation & Keyboard Shortcuts
   ========================================================================== */

function initGlobalListeners() {
  if (typeof window.matchMedia === 'function') {
    const railMql = window.matchMedia('(max-width: 1060px)');
    const onRailMediaChange = () => syncShellClasses();
    if (typeof railMql.addEventListener === 'function') {
      railMql.addEventListener('change', onRailMediaChange);
    } else if (typeof railMql.addListener === 'function') {
      railMql.addListener(onRailMediaChange);
    }
  }

  // Graceful fallback when a remote avatar image fails to load
  document.addEventListener(
    'error',
    (e) => {
      const target = e.target;
      if (target instanceof HTMLImageElement && target.dataset.avatarFallback) {
        const span = document.createElement('span');
        span.textContent = target.dataset.avatarFallback;
        target.replaceWith(span);
      }
    },
    true
  );

  // Close dialog helper buttons & global [data-action] dispatcher
  document.addEventListener('click', async (e) => {
    const targetEl = e.target instanceof Element ? e.target : e.target?.parentElement;
    if (!targetEl) return;

    const closeBtn = targetEl.closest('[data-close-dialog]');
    if (closeBtn) {
      const id = closeBtn.getAttribute('data-close-dialog');
      document.getElementById(id)?.close();
      return;
    }

    const actionEl = targetEl.closest('[data-action]');
    if (!actionEl) {
      hideFloatingOverlays();
      return;
    }

    const action = actionEl.getAttribute('data-action');
    if (
      !action.startsWith('ctx-') &&
      action !== 'conv-menu' &&
      action !== 'msg-menu' &&
      action !== 'composer-sticker-picker'
    ) {
      hideFloatingOverlays();
    }

    try {
      switch (action) {
        case 'close-overlays':
          hideFloatingOverlays();
          break;

        case 'retry-runtime':
          await window.olazInvoke('retry_runtime_install');
          break;

        case 'refresh-qr':
          await window.olazInvoke('refresh_qr');
          break;

        case 'toggle-debug-webview':
          await window.olazInvoke('toggle_debug_webview');
          break;

        case 'logout':
          await window.olazInvoke('logout');
          break;

        case 'transfer-accept':
          await window.olazInvoke('transfer_accept');
          break;

        case 'transfer-reject':
          await window.olazInvoke('transfer_reject');
          break;

        case 'transfer-cancel':
          await window.olazInvoke('transfer_cancel');
          break;

        case 'start-manual-sync':
          await window.olazInvoke('transfer_start_manual');
          break;

        case 'toggle-rail': {
          const hadFocus = document.activeElement === actionEl;
          ui.railMode = isRailCollapsed() ? 'expanded' : 'collapsed';
          syncShellClasses();
          renderRail();
          if (hadFocus) {
            const nextBtnSelector = isRailCollapsed()
              ? '#rail-zone .rail-expand-item[data-action="toggle-rail"]'
              : '#rail-zone .rail-top-actions [data-action="toggle-rail"]';
            document.querySelector(nextBtnSelector)?.focus();
          }
          break;
        }

        case 'show-list-pane':
          ui.mobilePane = 'list';
          syncShellClasses();
          renderRoot({});
          break;

        case 'nav-dest': {
          const prevDest = ui.destination;
          const dest = actionEl.getAttribute('data-dest') || 'inbox';
          ui.destination = dest;
          ui.mobilePane = 'list';
          if (dest === 'myCloud') {
            const selfId = store.convExtras?.sendToMeId || store.cloud?.selfId;
            if (selfId) {
              ui.mobilePane = 'stage';
              await window.olazInvoke('select_conversation', { convId: selfId });
            }
          } else if (dest === 'hidden' && !ui.unlockedHidden) {
            openHiddenPinDialog('unlock');
          } else if (dest === 'contacts') {
            requestContactsLoad({ force: prevDest !== 'contacts' });
          } else if (dest === 'zcloud') {
            requestZcloudLoad({ force: prevDest !== 'zcloud' });
          }
          renderRoot({});
          break;
        }

        case 'prompt-unlock-pin':
          openHiddenPinDialog('unlock');
          break;

        case 'filter-label':
          ui.labelFilter = actionEl.getAttribute('data-label-id') || '';
          updateListHeaderVisibility();
          renderListBody();
          break;

        case 'contacts-tab':
          ui.contactsTab = actionEl.getAttribute('data-tab') || 'friends';
          ui.mobilePane = 'stage';
          requestContactsLoad();
          renderRoot({});
          break;

        case 'friend-op':
          await window.olazInvoke('friend_action', {
            action: actionEl.getAttribute('data-op'),
            userId: actionEl.getAttribute('data-uid'),
          });
          break;

        case 'refresh-zcloud':
          lastZcloudLoadAt = Date.now();
          await window.olazInvoke('load_zcloud');
          showToast(
            isEnglishLang()
              ? 'Refreshed zCloud storage usage.'
              : 'Đã làm mới dữ liệu dung lượng zCloud.'
          );
          break;

        case 'select-conv': {
          if (targetEl.closest('.conv-more-btn')) return;
          const convId = actionEl.getAttribute('data-conv-id');
          if (ui.destination === 'contacts' || ui.destination === 'zcloud') {
            ui.destination = 'inbox';
          }
          if (convId) {
            ui.mobilePane = 'stage';
            syncShellClasses();
            await window.olazInvoke('select_conversation', { convId });
          }
          break;
        }

        case 'qs-pick': {
          const convId = actionEl.getAttribute('data-conv-id');
          document.getElementById('quick-switcher-dialog')?.close();
          ui.destination = 'inbox';
          if (convId) {
            ui.mobilePane = 'stage';
            syncShellClasses();
            await window.olazInvoke('select_conversation', { convId });
          }
          break;
        }

        case 'conv-menu': {
          e.stopPropagation();
          const convId = actionEl.getAttribute('data-conv-id');
          openConversationContextMenu(convId, actionEl.getBoundingClientRect());
          break;
        }

        case 'ctx-conv-op': {
          const convId = actionEl.getAttribute('data-conv-id');
          const op = actionEl.getAttribute('data-op');
          const duration = Number(actionEl.getAttribute('data-duration')) || undefined;
          hideFloatingOverlays();
          await window.olazInvoke('conv_action', { convId, action: op, duration });
          break;
        }

        case 'ctx-conv-label': {
          const convId = actionEl.getAttribute('data-conv-id');
          const labelId = actionEl.getAttribute('data-label-id') || '';
          hideFloatingOverlays();
          await window.olazInvoke('set_conv_label', { convId, labelId });
          break;
        }

        case 'quick-conv-mute': {
          const convId = actionEl.getAttribute('data-conv-id');
          const conv = (store.conversations || []).find((c) => c.id === convId);
          if (conv) {
            await window.olazInvoke('conv_action', {
              convId,
              action: conv.isMuted ? 'unmute' : 'mute',
              duration: 3600,
            });
          }
          break;
        }

        case 'quick-conv-pin': {
          const convId = actionEl.getAttribute('data-conv-id');
          const conv = (store.conversations || []).find((c) => c.id === convId);
          if (conv) {
            const f = getConversationFlags(conv);
            await window.olazInvoke('conv_action', { convId, action: f.isPinned ? 'unpin' : 'pin' });
          }
          break;
        }

        case 'quick-conv-hide': {
          const convId = actionEl.getAttribute('data-conv-id');
          const conv = (store.conversations || []).find((c) => c.id === convId);
          hideFloatingOverlays();
          if (!conv) break;
          const f = getConversationFlags(conv);
          if (f.isHidden) {
            await window.olazInvoke('conv_action', { convId, action: 'unhide' });
          } else if (!ui.unlockedHidden && store.convActions?.hidden?.hasPin) {
            openHiddenPinDialog('hide_conv', convId);
          } else {
            await window.olazInvoke('conv_action', { convId, action: 'hide' });
          }
          break;
        }

        case 'toggle-inspector':
          ui.showInspector = !ui.showInspector;
          if (ui.showInspector && store.activeConvId) {
            window.olazInvoke('load_shared_media', {
              convId: store.activeConvId,
              tab: ui.inspectorTab,
              cursor: null,
            }).catch(() => {});
          }
          syncShellClasses();
          renderStageHeader();
          renderInspector();
          break;

        case 'inspector-tab':
          ui.inspectorTab = actionEl.getAttribute('data-tab') || 'images';
          if (store.activeConvId) {
            window.olazInvoke('load_shared_media', {
              convId: store.activeConvId,
              tab: ui.inspectorTab,
              cursor: null,
            }).catch(() => {});
          }
          renderInspector();
          break;

        case 'load-older':
          await triggerLoadOlder(store.activeConvId);
          break;

        case 'jump-to-msg': {
          const msgId = actionEl.getAttribute('data-msg-id');
          if (msgId) {
            const el = document.querySelector(`[data-msg-id="${CSS.escape(msgId)}"]`);
            el?.scrollIntoView({ behavior: 'smooth', block: 'center' });
          }
          break;
        }

        case 'unpin-msg':
          await window.olazInvoke('unpin_message', {
            convId: actionEl.getAttribute('data-conv-id'),
            msgId: actionEl.getAttribute('data-msg-id'),
            ref: actionEl.getAttribute('data-ref'),
          });
          break;

        case 'open-lightbox':
          openLightboxForMessage(actionEl.getAttribute('data-msg-id'));
          break;

        case 'open-direct-lightbox':
          openDirectLightbox({
            src: actionEl.getAttribute('data-url'),
            rawUrl: actionEl.getAttribute('data-url') || '',
            mediaType: actionEl.getAttribute('data-media-type') || '',
            poster: actionEl.getAttribute('data-poster') || '',
            title: actionEl.getAttribute('data-title') || (isEnglishLang() ? 'Shared media' : 'Ảnh chia sẻ'),
          });
          break;

        case 'open-media-external': {
          const rawUrl = actionEl.getAttribute('data-url') || '';
          const url = unwrapExternalMediaUrl(rawUrl);
          if (url) {
            await window.olazInvoke('open_media_external', { url });
          }
          break;
        }

        case 'resolve-media':
          if (store.activeConvId) {
            await window.olazInvoke('resolve_media', {
              convId: store.activeConvId,
              msgId: actionEl.getAttribute('data-msg-id'),
              kind: actionEl.getAttribute('data-media-kind') || 'file',
            });
          }
          break;

        case 'open-file':
          await window.olazInvoke('open_downloaded_file', {
            path: actionEl.getAttribute('data-path'),
            revealOnly: actionEl.getAttribute('data-reveal') === 'true',
          });
          break;

        case 'toggle-voice':
          await toggleVoicePlayback(actionEl);
          break;

        case 'seek-voice':
          seekVoicePlayback(actionEl, e.clientX);
          break;

        case 'react-msg': {
          const msgId = actionEl.getAttribute('data-msg-id');
          const reaction = actionEl.getAttribute('data-reaction');
          if (store.activeConvId && msgId && reaction) {
            await window.olazInvoke('react_message', {
              convId: store.activeConvId,
              msgId,
              cliMsgId: msgId,
              reaction,
            });
          }
          break;
        }

        case 'reply-msg': {
          const msgId = actionEl.getAttribute('data-msg-id');
          const msg = store.messagesByConv?.[store.activeConvId]?.items?.find((m) => m.id === msgId);
          if (msg) {
            ui.replyTo = { id: msg.id, senderName: msg.senderName, text: msg.text || '[Phương tiện]' };
            renderComposerReplyBar();
            document.getElementById('chat-composer-input')?.focus();
          }
          break;
        }

        case 'clear-reply':
          ui.replyTo = null;
          renderComposerReplyBar();
          break;

        case 'forward-msg': {
          const msgId = actionEl.getAttribute('data-msg-id');
          if (msgId) openForwardDialog([msgId]);
          break;
        }

        case 'msg-menu': {
          e.stopPropagation();
          const msgId = actionEl.getAttribute('data-msg-id');
          openMessageContextMenu(msgId, actionEl.getBoundingClientRect());
          break;
        }

        case 'ctx-msg-copy': {
          const msgId = actionEl.getAttribute('data-msg-id');
          const msg = store.messagesByConv?.[store.activeConvId]?.items?.find((m) => m.id === msgId);
          hideFloatingOverlays();
          if (msg?.text && navigator.clipboard?.writeText) {
            navigator.clipboard.writeText(msg.text).catch(() => {});
            showToast(isEnglishLang() ? 'Copied message text.' : 'Đã sao chép nội dung tin nhắn.');
          }
          break;
        }

        case 'ctx-msg-pin': {
          const msgId = actionEl.getAttribute('data-msg-id');
          hideFloatingOverlays();
          if (store.activeConvId && msgId) {
            await window.olazInvoke('pin_message', { convId: store.activeConvId, msgId });
          }
          break;
        }

        case 'ctx-msg-star': {
          const msgId = actionEl.getAttribute('data-msg-id');
          hideFloatingOverlays();
          if (store.activeConvId && msgId) {
            await window.olazInvoke('star_message', { convId: store.activeConvId, msgId, starred: true });
          }
          break;
        }

        case 'ctx-msg-recall': {
          const msgId = actionEl.getAttribute('data-msg-id');
          hideFloatingOverlays();
          if (store.activeConvId && msgId) {
            await window.olazInvoke('recall_message', { convId: store.activeConvId, msgId, cliMsgId: msgId });
          }
          break;
        }

        case 'ctx-msg-delete': {
          const msgId = actionEl.getAttribute('data-msg-id');
          hideFloatingOverlays();
          if (store.activeConvId && msgId) {
            await window.olazInvoke('delete_message_for_me', { convId: store.activeConvId, msgId, cliMsgId: msgId });
          }
          break;
        }

        case 'pick-mention': {
          const idx = Number(actionEl.getAttribute('data-mention-idx'));
          const candidate = ui.mentionPopup.candidates[idx];
          const textarea = document.getElementById('chat-composer-input');
          if (textarea && candidate) commitMentionCandidate(textarea, candidate);
          break;
        }

        case 'composer-pick-media':
          if (store.activeConvId) await window.olazInvoke('pick_and_send_media', { convId: store.activeConvId });
          break;

        case 'composer-pick-files':
          if (store.activeConvId) await window.olazInvoke('pick_and_send_files', { convId: store.activeConvId });
          break;

        case 'composer-screenshot':
          if (store.activeConvId) {
            await window.olazInvoke('capture_screenshot', {
              convId: store.activeConvId,
              hideWindow: false,
              toMyCloud: false,
            });
          }
          break;

        case 'composer-sticker-picker':
          e.stopPropagation();
          toggleStickerPopover(actionEl);
          break;

        case 'send-suggested-sticker': {
          hideFloatingOverlays();
          const stickerId = actionEl.getAttribute('data-sticker-id');
          const cateId = actionEl.getAttribute('data-cate-id') || '';
          const textarea = document.getElementById('chat-composer-input');
          if (textarea && store.stickers?.suggestions?.length) {
            textarea.value = '';
            autoResizeTextarea(textarea);
          }
          if (store.activeConvId && stickerId) {
            await window.olazInvoke('send_sticker', { convId: store.activeConvId, stickerId, cateId });
          }
          break;
        }

        case 'composer-send':
          await submitComposerMessage();
          break;

        case 'open-quick-switcher':
          openQuickSwitcher();
          break;

        case 'open-create-group':
          openCreateGroupDialog();
          break;

        case 'open-create-poll':
          openCreatePollDialog();
          break;

        case 'open-poll-detail':
          openPollDetailDialog(
            actionEl.getAttribute('data-poll-id'),
            actionEl.getAttribute('data-question')
          );
          break;

        case 'open-settings':
          openSettingsDialog();
          break;

        case 'settings-change-pin':
          document.getElementById('settings-dialog')?.close();
          openHiddenPinDialog('change');
          break;

        case 'settings-reset-pin':
          await window.olazInvoke('manage_pin', { op: 'reset', pin: '', newPin: '' });
          break;

        case 'dismiss-call-banner':
          store.incomingCall = null;
          renderIncomingCallBanner();
          break;

        default:
          break;
      }
    } catch (err) {
      if (!err?._toasted) {
        showToast(
          err?.message || (isEnglishLang() ? 'Operation failed.' : 'Thao tác không thành công.')
        );
      }
    }
  });

  // Right-click context menu on conversation rows and message bubbles
  document.addEventListener('contextmenu', (e) => {
    const targetEl = e.target instanceof Element ? e.target : e.target?.parentElement;
    if (!targetEl) return;
    const convRow = targetEl.closest('.conv-row[data-conv-id]');
    if (convRow) {
      e.preventDefault();
      openConversationContextMenu(convRow.getAttribute('data-conv-id'), {
        left: e.clientX,
        bottom: e.clientY,
      });
      return;
    }
    const msgRow = targetEl.closest('.msg-row[data-msg-id]');
    if (msgRow) {
      e.preventDefault();
      openMessageContextMenu(msgRow.getAttribute('data-msg-id'), {
        left: e.clientX,
        bottom: e.clientY,
      });
    }
  });

  // Automatic load_older trigger when scrolling to the very top of a conversation
  document.addEventListener(
    'scroll',
    (e) => {
      const target = e.target;
      if (target instanceof HTMLElement && target.id === 'stage-timeline-zone') {
        const bucket = store.messagesByConv?.[store.activeConvId];
        if (target.scrollTop <= 8 && bucket?.hasMore && !ui.loadingOlderConvId) {
          triggerLoadOlder(store.activeConvId);
        }
      }
    },
    true
  );

  // Desktop keyboard shortcuts (Ctrl+K, Ctrl+F, Ctrl+R, Ctrl+,, Alt+Ctrl+I, and Enter/Space on .conv-row)
  document.addEventListener('keydown', (e) => {
    if (imeComposing || e.isComposing || e.keyCode === 229) return;

    const targetEl = e.target instanceof Element ? e.target : null;
    if (
      !e.ctrlKey &&
      !e.metaKey &&
      !e.altKey &&
      (e.key === 'Enter' || e.key === ' ') &&
      targetEl?.matches('.conv-row[data-conv-id]')
    ) {
      e.preventDefault();
      const convId = targetEl.getAttribute('data-conv-id');
      if (convId) {
        ui.mobilePane = 'stage';
        syncShellClasses();
        window.olazInvoke('select_conversation', { convId }).catch(() => {});
      }
      return;
    }

    const mod = e.ctrlKey || e.metaKey;
    if (!mod) return;

    const key = e.key.toLowerCase();
    if (key === ',') {
      // Allow opening Settings in ALL phases (preparing, starting, login, signingIn, ready)
      e.preventDefault();
      openSettingsDialog();
    } else if (key === 'k' && store.phase === 'ready') {
      e.preventDefault();
      openQuickSwitcher();
    } else if (key === 'f' && !e.shiftKey && store.phase === 'ready') {
      const searchEl = document.getElementById('conv-search-input');
      if (searchEl && !searchEl.closest('[hidden]')) {
        e.preventDefault();
        searchEl.focus();
        searchEl.select();
      }
    } else if (key === 'i' && e.altKey && store.phase === 'ready') {
      e.preventDefault();
      ui.showInspector = !ui.showInspector;
      syncShellClasses();
      renderStageHeader();
      renderInspector();
    } else if (key === 'r' && !e.shiftKey && store.phase === 'ready' && store.activeConvId) {
      e.preventDefault();
      const items = store.messagesByConv?.[store.activeConvId]?.items || [];
      const latestIncoming = [...items].reverse().find((m) => !m.isMine && !m.isRecalled);
      if (latestIncoming) {
        ui.replyTo = {
          id: latestIncoming.id,
          senderName: latestIncoming.senderName,
          text: latestIncoming.text || '[Phương tiện]',
        };
        renderComposerReplyBar();
        document.getElementById('chat-composer-input')?.focus();
      }
    }
  });
}

/* ==========================================================================
   12. Application Boot
   ========================================================================== */

function boot() {
  applyUserThemePref(store.settings?.theme || 'system');
  applyUserLanguagePref(store.settings?.language || 'vi');
  initDialogsAndFallbacks();
  initGlobalListeners();
  renderRoot({ phaseChanged: true, convChanged: true });

  window
    .olazInvoke('get_initial_state', {})
    .then((initial) => {
      if (initial && typeof initial === 'object') {
        window.__olazApplyState(initial);
      }
    })
    .catch(() => {});
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot, { once: true });
} else {
  boot();
}
