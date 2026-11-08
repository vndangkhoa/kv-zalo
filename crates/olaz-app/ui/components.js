// Pure UI renderers, Vietnamese text folding, and media URL resolution for Olaz Linux.

import { icon } from './icons.js';

export const ZALO_REACTIONS = ['❤️', '👍', '😆', '😮', '😢', '😡'];

/**
 * Guard 5: Diacritic-insensitive Vietnamese folding for instant search & @mentions.
 * @param {string} str
 * @returns {string}
 */
export function foldVietnamese(str) {
  return String(str || '')
    .replace(/[đĐ]/g, 'd')
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase();
}

export function escapeHtml(val) {
  return String(val ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

export function initialsOf(name) {
  const clean = String(name || '').trim();
  if (!clean) return 'OZ';
  const parts = clean.split(/\s+/).filter(Boolean);
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
}

/**
 * Routes remote Zalo CDN media through `olaz-media://cdn?url=` when hosted inside
 * WebKit2GTK, and local/decrypted E2EE files through `olaz-media://file?path=`.
 */
export function resolveMediaUrl(raw, hasNativeBridge) {
  if (!raw) return '';
  const s = String(raw).trim();
  if (!s) return '';
  if (s.startsWith('data:') || s.startsWith('blob:') || s.startsWith('olaz-media://')) {
    return s;
  }
  if (s.startsWith('file://')) {
    const filePath = decodeURIComponent(s.slice('file://'.length));
    return `olaz-media://file?path=${encodeURIComponent(filePath)}`;
  }
  if (s.startsWith('/')) {
    return `olaz-media://file?path=${encodeURIComponent(s)}`;
  }
  if ((s.startsWith('http://') || s.startsWith('https://')) && hasNativeBridge) {
    return `olaz-media://cdn?url=${encodeURIComponent(s)}`;
  }
  return s;
}

/**
 * Extracts the underlying external URL or local filesystem path from an `olaz-media://` URI
 * so external desktop media players (`open_media_external`) receive standard URLs/paths.
 */
export function unwrapExternalMediaUrl(raw) {
  if (!raw) return '';
  const s = String(raw).trim();
  if (!s) return '';
  if (s.startsWith('olaz-media://')) {
    try {
      const parsed = new URL(s);
      const pathParam = parsed.searchParams.get('path');
      if (pathParam) return pathParam;
      const urlParam = parsed.searchParams.get('url');
      if (urlParam) return urlParam;
    } catch {
      // Fall back to raw string if URL parsing fails
    }
  }
  return s;
}

export function formatBytes(bytes) {
  const n = Number(bytes) || 0;
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

export function formatShortTime(ts) {
  if (!ts) return '';
  const d = new Date(Number(ts));
  if (Number.isNaN(d.getTime())) return '';
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  if (sameDay) return `${hh}:${mm}`;
  const dd = String(d.getDate()).padStart(2, '0');
  const mo = String(d.getMonth() + 1).padStart(2, '0');
  return `${dd}/${mo}`;
}

export function formatDateLabel(ts) {
  if (!ts) return '';
  const d = new Date(Number(ts));
  if (Number.isNaN(d.getTime())) return '';
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const targetStart = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const diffDays = Math.round((todayStart - targetStart) / 86400000);
  if (diffDays === 0) return 'Hôm nay';
  if (diffDays === 1) return 'Hôm qua';
  const dd = String(d.getDate()).padStart(2, '0');
  const mo = String(d.getMonth() + 1).padStart(2, '0');
  return `${dd}/${mo}/${d.getFullYear()}`;
}

export function formatDuration(seconds) {
  const s = Math.max(0, Math.round(Number(seconds) || 0));
  const m = Math.floor(s / 60);
  const rem = String(s % 60).padStart(2, '0');
  return `${m}:${rem}`;
}

export function renderAvatar({ name, avatar, online = false, sizeClass = '', isSelfCloud = false, hasNativeBridge = false }) {
  const resolved = resolveMediaUrl(avatar, hasNativeBridge);
  const inner = isSelfCloud
    ? icon('cloud')
    : resolved
      ? `<img src="${escapeHtml(resolved)}" alt="" loading="lazy" data-avatar-fallback="${escapeHtml(initialsOf(name))}">`
      : `<span>${escapeHtml(initialsOf(name))}</span>`;
  const dot = online ? '<span class="online-dot" aria-label="Đang hoạt động"></span>' : '';
  return `<span class="avatar ${sizeClass}">${inner}${dot}</span>`;
}

/**
 * Formats message body with UTF-16 @mention highlights and clickable HTTP(S) links.
 */
export function renderRichMessageText(text, mentions = []) {
  const raw = String(text || '');
  if (!raw) return '';

  const sortedMentions = Array.isArray(mentions)
    ? [...mentions]
        .filter((m) => Number.isFinite(Number(m.pos)) && Number(m.len) > 0)
        .sort((a, b) => Number(a.pos) - Number(b.pos))
    : [];

  if (sortedMentions.length === 0) {
    return linkifyEscaped(raw);
  }

  let cursor = 0;
  let html = '';
  for (const m of sortedMentions) {
    const start = Math.max(cursor, Number(m.pos));
    const end = Math.min(raw.length, start + Number(m.len));
    if (start > cursor) {
      html += linkifyEscaped(raw.slice(cursor, start));
    }
    if (end > start) {
      html += `<span class="mention-tag">${escapeHtml(raw.slice(start, end))}</span>`;
      cursor = end;
    }
  }
  if (cursor < raw.length) {
    html += linkifyEscaped(raw.slice(cursor));
  }
  return html;
}

function linkifyEscaped(segment) {
  const urlRe = /(https?:\/\/[^\s<>"']+)/g;
  const parts = segment.split(urlRe);
  return parts
    .map((part, idx) => {
      if (idx % 2 === 1) {
        const safe = escapeHtml(part);
        return `<a href="${safe}" target="_blank" rel="noopener noreferrer">${safe}</a>`;
      }
      return escapeHtml(part);
    })
    .join('');
}

function isEnglishLang(store) {
  return String(store?.settings?.language || 'vi').toLowerCase().startsWith('en');
}

function canUseInlineGstAudio(store) {
  if (store?.capabilities && store.capabilities.gstAudioSink === false) {
    return false;
  }
  if (typeof window !== 'undefined' && window.__olazStore?.capabilities?.gstAudioSink === false) {
    return false;
  }
  return true;
}

function renderLifecycleTopbar(isEn) {
  const label = isEn ? 'Settings' : 'Thiết lập';
  const tooltip = isEn ? 'Olaz Settings (Ctrl+,)' : 'Thiết lập Olaz (Ctrl+,)';
  return `
    <header class="lifecycle-topbar">
      <button
        type="button"
        class="btn btn-secondary lifecycle-settings-btn"
        data-action="open-settings"
        title="${escapeHtml(tooltip)}"
        aria-label="${escapeHtml(label)}"
      >
        ${icon('settings')}
        <span>${escapeHtml(label)}</span>
      </button>
    </header>
  `;
}

export function renderLifecycleScreen(store, hasNativeBridge) {
  const phase = store.phase || 'starting';
  const isEn = isEnglishLang(store);
  const topbarHtml = renderLifecycleTopbar(isEn);

  if (phase === 'preparing') {
    const prog = store.runtimeProgress || {};
    const received = Number(prog.received) || 0;
    const total = Number(prog.total) || 0;
    const stepLabels = isEn
      ? {
          looking: 'Checking official Zalo PC runtime package…',
          downloading:
            total > 0
              ? `Downloading Zalo PC core… ${formatBytes(received)} / ${formatBytes(total)}`
              : `Downloading Zalo PC core… ${formatBytes(received)}`,
          verifying: 'Verifying official VNG digital signature…',
          installing: 'Extracting and initializing runtime environment…',
        }
      : {
          looking: 'Đang kiểm tra gói tài nguyên Zalo PC…',
          downloading:
            total > 0
              ? `Đang tải lõi Zalo PC… ${formatBytes(received)} / ${formatBytes(total)}`
              : `Đang tải lõi Zalo PC… ${formatBytes(received)}`,
          verifying: 'Đang xác thực chữ ký số chính thức của VNG…',
          installing: 'Đang giải nén và khởi tạo môi trường chạy…',
        };
    const statusText =
      stepLabels[prog.step] ||
      (isEn ? 'Preparing runtime environment…' : 'Đang chuẩn bị môi trường chạy…');

    return `
      <main class="lifecycle-screen">
        ${topbarHtml}
        <div class="lifecycle-center">
          <section class="lifecycle-panel" aria-labelledby="prep-heading">
            <div class="brand-badge">${icon('olazLogo')}</div>
            <h1 id="prep-heading" class="lifecycle-title">${isEn ? 'Preparing Olaz for Linux' : 'Chuẩn bị Olaz cho Linux'}</h1>
            <p class="lifecycle-desc">
              ${
                isEn
                  ? 'On first launch, Olaz downloads the official Zalo PC runtime from VNG servers and verifies its integrity locally on your device.'
                  : 'Lần khởi chạy đầu tiên, Olaz tải gói lõi Zalo PC chính thức từ máy chủ VNG và xác minh chữ ký toàn vẹn ngay trên thiết bị của bạn.'
              }
            </p>
            ${
              prog.failure
                ? `
                  <div class="login-callout is-error" role="alert">
                    ${icon('alert')}
                    <span>${escapeHtml(prog.failure)}</span>
                  </div>
                  <div class="lifecycle-footer">
                    <button type="button" class="btn btn-primary" data-action="retry-runtime">
                      ${icon('refresh')}
                      <span>${isEn ? 'Retry download' : 'Thử tải lại'}</span>
                    </button>
                    <button type="button" class="btn btn-secondary" data-action="toggle-debug-webview">
                      ${icon('terminal')}
                      <span>${isEn ? 'Background WebView' : 'Trình duyệt nền'}</span>
                    </button>
                  </div>
                `
                : `
                  <div class="runtime-progress-wrap">
                    ${
                      total > 0
                        ? `<progress max="${total}" value="${received}" aria-label="${escapeHtml(statusText)}"></progress>`
                        : `<progress aria-label="${escapeHtml(statusText)}"></progress>`
                    }
                    <span class="numeric-meta dialog-subtitle">${escapeHtml(statusText)}</span>
                  </div>
                `
            }
          </section>
        </div>
      </main>
    `;
  }

  if (phase === 'login') {
    const login = store.login || {};
    const qrSrc = resolveMediaUrl(login.qr, hasNativeBridge);
    const waitingConfirm =
      login.waitingConfirm && typeof login.waitingConfirm === 'object' ? login.waitingConfirm : null;
    const kickoutNotice = String(login.kickoutNotice || '').trim();
    const errorNotice = String(login.error || '').trim();
    const isExpired = Boolean(qrSrc && login.invalid);

    const headingText = waitingConfirm
      ? isEn
        ? 'Confirm on your phone'
        : 'Xác nhận trên điện thoại'
      : isEn
        ? 'Sign in to Zalo with QR'
        : 'Đăng nhập Zalo bằng mã QR';

    let subtitleText;
    if (waitingConfirm) {
      subtitleText = isEn
        ? 'Tap Sign in on your phone to continue.'
        : 'Chọn Đăng nhập trên điện thoại của bạn để tiếp tục.';
    } else if (isExpired) {
      subtitleText = isEn
        ? 'This QR code has expired. Refresh to generate a new code.'
        : 'Mã QR đã hết hạn. Bấm Lấy mã mới để tiếp tục.';
    } else if (isEn) {
      subtitleText = 'Open Zalo on your phone, tap the QR scanner icon, and scan the code below.';
    } else {
      const customText = String(login.text || '').trim();
      const isDuplicateBanner =
        customText && (customText === kickoutNotice || customText === errorNotice);
      subtitleText =
        customText && !isDuplicateBanner
          ? customText
          : 'Mở ứng dụng Zalo trên điện thoại và quét mã QR bên dưới để đồng bộ tài khoản.';
    }

    const calloutsHtml = `
      ${
        kickoutNotice
          ? `
            <div class="login-callout is-warning" role="status">
              ${icon('alert')}
              <span>${escapeHtml(kickoutNotice)}</span>
            </div>
          `
          : ''
      }
      ${
        errorNotice
          ? `
            <div class="login-callout is-error" role="alert">
              ${icon('alert')}
              <span>${escapeHtml(errorNotice)}</span>
            </div>
          `
          : ''
      }
    `;

    let qrCardHtml;
    if (waitingConfirm) {
      const accountName =
        String(waitingConfirm.name || '').trim() ||
        (isEn ? 'Zalo Account' : 'Tài khoản Zalo');
      const accountAvatar = waitingConfirm.avatar ?? login.avatar ?? null;
      qrCardHtml = `
        <div class="qr-confirm-card">
          ${renderAvatar({
            name: accountName,
            avatar: accountAvatar,
            sizeClass: 'avatar-xl',
            hasNativeBridge,
          })}
          <strong class="confirm-account-name">${escapeHtml(accountName)}</strong>
          <div class="confirm-status-pill">
            ${icon('check')}
            <span>${
              isEn
                ? 'QR scanned — confirm sign-in on your phone'
                : 'Đã quét — xác nhận đăng nhập trên điện thoại'
            }</span>
          </div>
          <button type="button" class="btn btn-secondary" data-action="refresh-qr">
            ${icon('refresh')}
            <span>${isEn ? 'Use another QR code' : 'Dùng mã QR khác'}</span>
          </button>
        </div>
      `;
    } else if (!qrSrc) {
      qrCardHtml = `
        <div class="qr-frame is-loading ${kickoutNotice || errorNotice ? 'is-compact' : ''}">
          <div class="qr-loading-stack">
            <span class="qr-spinner" aria-hidden="true"></span>
            <span class="qr-loading-label">${isEn ? 'Generating QR code…' : 'Đang tạo mã QR…'}</span>
            <button type="button" class="btn btn-secondary btn-inline" data-action="refresh-qr">
              ${icon('refresh')}
              <span>${isEn ? 'Retry' : 'Thử lại'}</span>
            </button>
          </div>
        </div>
      `;
    } else {
      qrCardHtml = `
        <div class="qr-frame ${isExpired ? 'is-expired' : ''} ${kickoutNotice || errorNotice ? 'is-compact' : ''}">
          <img
            class="qr-img"
            src="${escapeHtml(qrSrc)}"
            alt="${escapeHtml(isEn ? 'Zalo sign-in QR code' : 'Mã QR đăng nhập Zalo')}"
          >
          ${
            isExpired
              ? `
                <div class="qr-expired-overlay">
                  <span class="qr-expired-title">${isEn ? 'QR code expired' : 'Mã QR đã hết hạn'}</span>
                  <button type="button" class="btn btn-primary" data-action="refresh-qr">
                    ${icon('refresh')}
                    <span>${isEn ? 'Refresh QR' : 'Lấy mã mới'}</span>
                  </button>
                </div>
              `
              : ''
          }
        </div>
      `;
    }

    const steps = isEn
      ? [
          'Open Zalo on your phone',
          'Tap the Scan QR icon',
          'Scan the code and confirm',
        ]
      : [
          'Mở Zalo trên điện thoại',
          'Chọn biểu tượng Quét mã QR',
          'Quét mã và xác nhận',
        ];

    const stepsHtml = !waitingConfirm
      ? `
        <ol class="qr-steps">
          ${steps
            .map(
              (step, idx) => `
                <li class="qr-step-item">
                  <span class="qr-step-badge numeric-meta" aria-hidden="true">${idx + 1}</span>
                  <span>${escapeHtml(step)}</span>
                </li>
              `
            )
            .join('')}
        </ol>
      `
      : '';

    return `
      <main class="lifecycle-screen">
        ${topbarHtml}
        <div class="lifecycle-center">
          <section class="lifecycle-panel login-panel" aria-labelledby="login-heading">
            ${calloutsHtml}
            <div class="brand-badge">${icon('olazLogo')}</div>
            <h1 id="login-heading" class="lifecycle-title">${escapeHtml(headingText)}</h1>
            <p class="lifecycle-desc">${escapeHtml(subtitleText)}</p>
            ${qrCardHtml}
            ${stepsHtml}
            <div class="lifecycle-footer">
              ${
                qrSrc && !isExpired && !waitingConfirm
                  ? `
                    <button type="button" class="btn btn-secondary" data-action="refresh-qr">
                      ${icon('refresh')}
                      <span>${isEn ? 'Refresh QR' : 'Làm mới QR'}</span>
                    </button>
                  `
                  : ''
              }
              <button type="button" class="btn btn-ghost" data-action="toggle-debug-webview">
                ${icon('terminal')}
                <span>${isEn ? 'Background WebView' : 'Hiện WebView nền'}</span>
              </button>
            </div>
          </section>
        </div>
      </main>
    `;
  }

  const isSigningIn = phase === 'signingIn';
  const splashHeading = isSigningIn
    ? isEn
      ? 'Signing in…'
      : 'Đang đăng nhập…'
    : 'Olaz';
  const splashText = isSigningIn
    ? isEn
      ? 'Loading your Zalo account and restoring end-to-end encryption keys…'
      : 'Đang đăng nhập và khôi phục khóa mã hóa đầu cuối…'
    : isEn
      ? 'Starting Olaz service on Linux…'
      : 'Đang khởi động dịch vụ Olaz trên Linux…';

  return `
    <main class="lifecycle-screen">
      ${topbarHtml}
      <div class="lifecycle-center">
        <section class="lifecycle-panel" aria-labelledby="splash-heading">
          <div class="brand-badge">${icon('olazLogo')}</div>
          <h1 id="splash-heading" class="lifecycle-title">${escapeHtml(splashHeading)}</h1>
          <div class="runtime-progress-wrap">
            <progress aria-label="${escapeHtml(splashText)}"></progress>
            <p class="lifecycle-desc">${escapeHtml(splashText)}</p>
          </div>
          <div class="lifecycle-footer">
            ${
              !isSigningIn
                ? `
                  <button type="button" class="btn btn-secondary" data-action="refresh-qr">
                    ${icon('refresh')}
                    <span>${isEn ? 'Retry' : 'Thử lại'}</span>
                  </button>
                `
                : ''
            }
            <button type="button" class="btn btn-ghost" data-action="toggle-debug-webview">
              ${icon('terminal')}
              <span>${isEn ? 'Background WebView' : 'Kiểm tra WebView nền'}</span>
            </button>
          </div>
        </section>
      </div>
    </main>
  `;
}

export function normalizeMessageKind(msg) {
  const k = msg?.kind;
  if (!k) return { type: 'text' };
  if (typeof k === 'string') return { type: k };
  return k;
}

/**
 * Crash-proof lazy voice message bubble: renders ONLY the play/pause button,
 * deterministic waveform bars, and duration label — NEVER mounts `<audio>` in DOM!
 */
export function renderVoiceBubble(msg, kind, hasNativeBridge, voiceState = null, isEn = false) {
  const rawSrc = kind?.path || kind?.url || '';
  const src = resolveMediaUrl(rawSrc, hasNativeBridge);
  const dur = Math.max(0, Number(kind?.duration || kind?.seconds || 0));
  const isPlaying = Boolean(voiceState?.playing);
  const progressPct = Number.isFinite(voiceState?.progressPct)
    ? Math.min(100, Math.max(0, voiceState.progressPct))
    : 0;
  const shownSeconds =
    voiceState && Number.isFinite(voiceState.currentTime) && (isPlaying || voiceState.currentTime > 0)
      ? voiceState.currentTime
      : dur;

  // Deterministic 20-bar waveform seeded from msg.id so rendering is pure and stable
  const seedStr = String(msg?.id || 'voice');
  let hash = 2166136261;
  for (let i = 0; i < seedStr.length; i++) {
    hash ^= seedStr.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  const bars = [];
  const barCount = 20;
  for (let i = 0; i < barCount; i++) {
    hash ^= i + 1;
    hash = Math.imul(hash, 16777619);
    const norm = ((hash >>> 0) % 100) / 100;
    const heightPct = Math.round(26 + norm * 74);
    const barThreshold = ((i + 0.5) / barCount) * 100;
    const playedClass = progressPct >= barThreshold ? 'is-played' : '';
    bars.push(`<span class="voice-bar ${playedClass}" style="height: ${heightPct}%"></span>`);
  }

  const btnLabel = isEn ? 'Play or pause voice message' : 'Phát hoặc tạm dừng tin nhắn thoại';

  return `
    <div
      class="voice-player ${isPlaying ? 'is-playing' : ''}"
      data-voice-msg="${escapeHtml(msg.id)}"
      data-voice-playing="${isPlaying ? 'true' : 'false'}"
    >
      <button
        type="button"
        class="icon-btn voice-play-btn"
        data-action="toggle-voice"
        data-msg-id="${escapeHtml(msg.id)}"
        data-voice-src="${escapeHtml(src)}"
        data-duration="${dur}"
        aria-label="${escapeHtml(btnLabel)}"
      >
        ${icon(isPlaying ? 'pause' : 'play')}
      </button>
      <div
        class="voice-waveform"
        data-action="seek-voice"
        data-msg-id="${escapeHtml(msg.id)}"
        data-voice-waveform="${escapeHtml(msg.id)}"
        style="--voice-progress: ${progressPct.toFixed(1)}%"
        aria-hidden="true"
      >
        ${bars.join('')}
      </div>
      <span class="voice-time numeric-meta dialog-subtitle" data-voice-label="${escapeHtml(msg.id)}">${formatDuration(shownSeconds)}</span>
    </div>
  `;
}

/**
 * Crash-proof video card rendered whenever GStreamer audio sink (`gst-plugins-good`) is absent,
 * avoiding `MediaPlayerPrivateGStreamer::createAudioSink()` SIGABRT in WebKit2GTK.
 */
export function renderSafeVideoCard({ rawUrl, poster = '', duration = 0, isEn = false }) {
  const externalUrl = unwrapExternalMediaUrl(rawUrl);
  const label = isEn ? 'Open video in external player' : 'Mở video bằng trình phát ngoài';
  const pillText =
    Number(duration) > 0
      ? `${formatDuration(duration)} · ${isEn ? 'External player' : 'Trình phát ngoài'}`
      : isEn
        ? 'Open in external player'
        : 'Mở bằng trình phát ngoài';

  return `
    <button
      type="button"
      class="msg-video-fallback-card"
      data-action="open-media-external"
      data-url="${escapeHtml(externalUrl)}"
      aria-label="${escapeHtml(label)}"
    >
      ${
        poster
          ? `<img class="msg-video-poster" src="${escapeHtml(poster)}" alt="" loading="lazy">`
          : `<div class="msg-video-placeholder" aria-hidden="true">${icon('video')}</div>`
      }
      <span class="msg-video-play-badge" aria-hidden="true">${icon('play')}</span>
      <span class="msg-video-meta-pill numeric-meta">
        ${icon('externalLink')}
        <span>${escapeHtml(pillText)}</span>
      </span>
    </button>
  `;
}

export function renderMessageBubbleContent(msg, store, hasNativeBridge, voiceState = null) {
  const kind = normalizeMessageKind(msg);
  const type = kind.type || 'text';
  const isEn = isEnglishLang(store);

  if (type === 'recalled' || msg.isRecalled) {
    return `<p class="msg-text">${isEn ? 'Message was recalled' : 'Tin nhắn đã được thu hồi'}</p>`;
  }

  const quoteHtml = msg.quote
    ? `
      <div class="quote-callout">
        <span class="quote-sender">${icon('reply')}<span>${escapeHtml(msg.quote.senderName || (isEn ? 'Reply' : 'Trả lời'))}</span></span>
        <p class="quote-body">${escapeHtml(msg.quote.text || msg.quote.preview || '')}</p>
      </div>
    `
    : '';

  if (type === 'photo' || type === 'image' || type === 'gif') {
    const src = resolveMediaUrl(kind.path || kind.url || kind.thumbnail, hasNativeBridge);
    const caption = msg.text && !msg.text.startsWith('[')
      ? `<p class="msg-text">${renderRichMessageText(msg.text, msg.mentions)}</p>`
      : '';
    return `
      ${quoteHtml}
      ${
        src
          ? `
            <button
              type="button"
              class="msg-photo-btn"
              data-action="open-lightbox"
              data-msg-id="${escapeHtml(msg.id)}"
              aria-label="${isEn ? 'Zoom image' : 'Phóng to hình ảnh'}"
            >
              <img src="${escapeHtml(src)}" alt="${escapeHtml(msg.text || (isEn ? 'Image in conversation' : 'Hình ảnh trong cuộc trò chuyện'))}" loading="lazy">
            </button>
          `
          : `
            <button type="button" class="btn btn-secondary" data-action="resolve-media" data-msg-id="${escapeHtml(msg.id)}" data-media-kind="photo">
              ${icon('download')}
              <span>${isEn ? 'Decrypt & download photo' : 'Tải ảnh giải mã E2EE'}</span>
            </button>
          `
      }
      ${caption}
    `;
  }

  if (type === 'sticker') {
    const src = resolveMediaUrl(kind.url, hasNativeBridge);
    return src
      ? `<img class="msg-sticker-img" src="${escapeHtml(src)}" alt="Sticker" loading="lazy">`
      : `<div class="quote-callout"><span class="dialog-subtitle">Sticker</span></div>`;
  }

  if (type === 'video') {
    const rawSrc = kind.path || kind.url || '';
    const src = resolveMediaUrl(rawSrc, hasNativeBridge);
    const poster = resolveMediaUrl(kind.thumbnail, hasNativeBridge);
    if (!src) {
      return `
        ${quoteHtml}
        <button type="button" class="btn btn-secondary" data-action="resolve-media" data-msg-id="${escapeHtml(msg.id)}" data-media-kind="video">
          ${icon('video')}
          <span>${isEn ? 'Download video' : 'Tải video'} (${formatDuration(kind.duration)})</span>
        </button>
      `;
    }
    if (!canUseInlineGstAudio(store)) {
      return `
        ${quoteHtml}
        ${renderSafeVideoCard({ rawUrl: rawSrc || src, poster, duration: kind.duration, isEn })}
      `;
    }
    return `
      ${quoteHtml}
      <div class="msg-video-wrap">
        <video controls preload="none" ${poster ? `poster="${escapeHtml(poster)}"` : ''} src="${escapeHtml(src)}"></video>
      </div>
    `;
  }

  if (type === 'voice') {
    return `
      ${quoteHtml}
      ${renderVoiceBubble(msg, kind, hasNativeBridge, voiceState, isEn)}
    `;
  }

  if (type === 'file') {
    const fileName = kind.name || msg.text || 'Tệp đính kèm';
    const fileSize = formatBytes(kind.size || kind.bytes || 0);
    const localPath = kind.path || '';
    return `
      ${quoteHtml}
      <div class="file-attachment">
        <div class="file-icon-box">${icon('file')}</div>
        <div class="file-meta">
          <span class="file-name" title="${escapeHtml(fileName)}">${escapeHtml(fileName)}</span>
          <span class="file-size numeric-meta">${escapeHtml(fileSize)}</span>
        </div>
        ${
          localPath
            ? `
              <button type="button" class="icon-btn" data-action="open-file" data-path="${escapeHtml(localPath)}" data-reveal="false" title="Mở tệp" aria-label="Mở tệp">
                ${icon('externalLink')}
              </button>
              <button type="button" class="icon-btn" data-action="open-file" data-path="${escapeHtml(localPath)}" data-reveal="true" title="Hiện trong thư mục" aria-label="Hiện trong thư mục">
                ${icon('folder')}
              </button>
            `
            : `
              <button type="button" class="icon-btn" data-action="resolve-media" data-msg-id="${escapeHtml(msg.id)}" data-media-kind="file" title="Tải tệp xuống" aria-label="Tải tệp xuống">
                ${icon('download')}
              </button>
            `
        }
      </div>
    `;
  }

  if (type === 'link') {
    const url = kind.url || msg.text || '#';
    return `
      ${quoteHtml}
      <a class="link-preview" href="${escapeHtml(url)}" target="_blank" rel="noopener noreferrer">
        <span class="link-domain">${escapeHtml(kind.source || url)}</span>
        <span class="link-title">${escapeHtml(kind.title || url)}</span>
        ${kind.description ? `<p class="link-desc">${escapeHtml(kind.description)}</p>` : ''}
      </a>
    `;
  }

  if (type === 'poll') {
    const pollId = kind.pollId || '';
    const poll = (store.polls && store.polls[pollId]) || null;
    const question = poll?.question || kind.question || msg.text || 'Bình chọn nhóm';
    const options = Array.isArray(poll?.options) ? poll.options : [];
    const maxVotes = Math.max(1, ...options.map((o) => Number(o.votes) || 0));

    return `
      <div class="poll-block">
        <div class="field-row-between">
          <span class="quote-sender">${icon('poll')}<span>Bình chọn nhóm</span></span>
          <span class="poll-count dialog-subtitle">${Number(poll?.voters || 0)} người tham gia</span>
        </div>
        <p class="poll-question">${escapeHtml(question)}</p>
        ${
          options.length > 0
            ? `
              <div class="poll-options-summary">
                ${options
                  .slice(0, 4)
                  .map((opt) => {
                    const votes = Number(opt.votes) || 0;
                    const pct = Math.round((votes / maxVotes) * 100);
                    return `
                      <div class="poll-opt-bar">
                        <span class="poll-opt-fill" style="width: ${pct}%"></span>
                        <span class="poll-opt-text">${escapeHtml(opt.content)}</span>
                        <span class="poll-count">${votes}</span>
                      </div>
                    `;
                  })
                  .join('')}
              </div>
            `
            : ''
        }
        <button type="button" class="btn btn-secondary" data-action="open-poll-detail" data-poll-id="${escapeHtml(pollId)}" data-question="${escapeHtml(question)}">
          <span>Xem &amp; bình chọn</span>
        </button>
      </div>
    `;
  }

  if (type === 'location') {
    const mapUrl =
      kind.latitude && kind.longitude
        ? `https://maps.google.com/maps?q=${encodeURIComponent(`${kind.latitude},${kind.longitude}`)}&z=14`
        : '#';
    return `
      ${quoteHtml}
      <a class="link-preview" href="${escapeHtml(mapUrl)}" target="_blank" rel="noopener noreferrer">
        <span class="quote-sender">${icon('mapPin')}<span>Vị trí chia sẻ</span></span>
        <span class="link-title">${escapeHtml(kind.title || 'Xem trên bản đồ')}</span>
        ${kind.address ? `<p class="link-desc">${escapeHtml(kind.address)}</p>` : ''}
      </a>
    `;
  }

  if (type === 'call') {
    const callLabel = kind.missed
      ? kind.video ? 'Cuộc gọi video nhỡ' : 'Cuộc gọi thoại nhỡ'
      : kind.video ? 'Cuộc gọi video' : 'Cuộc gọi thoại';
    return `
      <div class="file-attachment">
        <div class="file-icon-box">${icon(kind.video ? 'video' : 'phone')}</div>
        <div class="file-meta">
          <span class="file-name">${escapeHtml(kind.title || callLabel)}</span>
          <span class="file-size numeric-meta">${escapeHtml(kind.detail || formatDuration(kind.duration || 0))}</span>
        </div>
      </div>
    `;
  }

  return `
    ${quoteHtml}
    <p class="msg-text">${renderRichMessageText(msg.text, msg.mentions)}</p>
  `;
}

export function renderDeliveryStatus(status) {
  switch (status) {
    case 'sending':
      return `<span class="msg-delivery-status">${icon('clock')}<span>Đang gửi</span></span>`;
    case 'sent':
      return `<span class="msg-delivery-status">${icon('check')}<span>Đã gửi</span></span>`;
    case 'delivered':
      return `<span class="msg-delivery-status">${icon('checkDouble')}<span>Đã nhận</span></span>`;
    case 'seen':
      return `<span class="msg-delivery-status">${icon('checkDouble')}<span>Đã xem</span></span>`;
    case 'failed':
      return `<span class="msg-delivery-status" style="color: var(--danger)">${icon('alert')}<span>Gửi chưa thành công</span></span>`;
    default:
      return '';
  }
}
