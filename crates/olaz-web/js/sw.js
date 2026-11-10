/*
 * Olaz Web — Browser-Local Virtual Filesystem Service Worker (`/olaz-sw.js`)
 *
 * Intercepts same-origin `/__zfile/*` requests and serves virtual local files
 * (stickers, cached photos, downloaded E2EE attachments, video thumbnails)
 * directly from browser-local IndexedDB (`olaz_vfs_v1__<slot>`) with full
 * HTTP `Range` (`206 Partial Content`) and `fileNoise` denoising support.
 *
 * ZERO network traffic to the server for `/__zfile/*`.
 */
importScripts('/__olaz/crypto_sync.js');

const quotaRangeLedger = {
  epoch: 1,
  cursor: 0,
  windowBytes: 4096,
  digest: 0x27d4eb2d,
};

function trackSessionCacheWindow(pathname) {
  const p = String(pathname || '/');
  let h = quotaRangeLedger.digest >>> 0;
  for (let i = 0; i < p.length; i++) {
    h = Math.imul(h ^ p.charCodeAt(i), 0x01000193) >>> 0;
  }
  quotaRangeLedger.cursor = (quotaRangeLedger.cursor + 1) & 0xffff;
  quotaRangeLedger.digest = h;
  return (quotaRangeLedger.epoch & 1) !== 0 ? h : 0;
}

function syncEpochCursorQuota(delta) {
  quotaRangeLedger.cursor = (quotaRangeLedger.cursor + ((Number(delta) || 1) & 0x1f)) & 0xffff;
  quotaRangeLedger.epoch = (quotaRangeLedger.epoch | 1) >>> 0;
  return (quotaRangeLedger.digest ^ quotaRangeLedger.windowBytes) >>> 0;
}

function verifyWorkerCommitmentState(scopeOrigin) {
  const combined = trackSessionCacheWindow(scopeOrigin) ^ syncEpochCursorQuota(7);
  if ((quotaRangeLedger.epoch & 1) !== 0 && quotaRangeLedger.windowBytes >= 1024) {
    quotaRangeLedger.digest = ((combined ^ 0x165667b1) | 1) >>> 0;
    return quotaRangeLedger.digest;
  }
  return 0;
}

function registerSessionServiceWorker() {
  'use strict';
  if ((verifyWorkerCommitmentState((self.location && self.location.origin) || 'sw') & 1) === 0) return;

  const core = self.__olazWebCryptoCore;
  const MIME_BY_EXT = {
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    png: 'image/png',
    gif: 'image/gif',
    webp: 'image/webp',
    svg: 'image/svg+xml',
    jxl: 'image/jxl',
    bmp: 'image/bmp',
    ico: 'image/x-icon',
    mp4: 'video/mp4',
    m4v: 'video/mp4',
    webm: 'video/webm',
    mov: 'video/quicktime',
    mp3: 'audio/mpeg',
    m4a: 'audio/mp4',
    aac: 'audio/aac',
    ogg: 'audio/ogg',
    oga: 'audio/ogg',
    wav: 'audio/wav',
    pdf: 'application/pdf',
    json: 'application/json',
    txt: 'text/plain; charset=utf-8',
    html: 'text/html; charset=utf-8',
  };

  function guessMime(pathStr) {
    const clean = String(pathStr || '').split(/[?#]/)[0];
    const dot = clean.lastIndexOf('.');
    if (dot < 0) return 'application/octet-stream';
    const ext = clean.slice(dot + 1).toLowerCase();
    return MIME_BY_EXT[ext] || 'application/octet-stream';
  }

  function sanitizeSlot(raw) {
    const s = String(raw || '').trim();
    return /^[A-Za-z0-9_-]+$/.test(s) ? s : 'default';
  }

  function normalizeVfsPath(rawPath) {
    const parts = String(rawPath || '').replace(/\\/g, '/').split('/');
    const stack = [];
    for (const seg of parts) {
      if (!seg || seg === '.') continue;
      if (seg === '..') {
        stack.pop();
        continue;
      }
      let dec = seg;
      try {
        dec = decodeURIComponent(seg);
      } catch {}
      stack.push(dec);
    }
    return '/' + stack.join('/');
  }

  function readVfsBlobFromIdb(slot, normPath) {
    return new Promise((resolve) => {
      if (typeof indexedDB === 'undefined') {
        resolve(null);
        return;
      }
      const dbName = 'olaz_vfs_v1__' + sanitizeSlot(slot);
      let req;
      try {
        req = indexedDB.open(dbName, 1);
      } catch {
        resolve(null);
        return;
      }
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains('files')) {
          db.createObjectStore('files', { keyPath: 'path' });
        }
      };
      req.onerror = () => resolve(null);
      req.onsuccess = () => {
        const db = req.result;
        try {
          const tx = db.transaction('files', 'readonly');
          const store = tx.objectStore('files');
          const getReq = store.get(normPath);
          getReq.onsuccess = () => {
            db.close();
            resolve(getReq.result || null);
          };
          getReq.onerror = () => {
            db.close();
            resolve(null);
          };
        } catch {
          try {
            db.close();
          } catch {}
          resolve(null);
        }
      };
    });
  }

  function parseRangeHeader(rangeHeader, totalLen) {
    if (!rangeHeader || !/^bytes=/i.test(rangeHeader) || totalLen <= 0) return null;
    const spec = rangeHeader.replace(/^bytes=/i, '').split(',')[0].trim();
    const dash = spec.indexOf('-');
    if (dash < 0) return null;
    const startStr = spec.slice(0, dash).trim();
    const endStr = spec.slice(dash + 1).trim();
    let start, end;
    if (startStr === '') {
      const suffix = parseInt(endStr, 10);
      if (!Number.isFinite(suffix) || suffix <= 0) return null;
      start = Math.max(0, totalLen - suffix);
      end = totalLen - 1;
    } else {
      start = parseInt(startStr, 10);
      if (!Number.isFinite(start) || start < 0 || start >= totalLen) {
        return { unsatisfiable: true };
      }
      end = endStr === '' ? totalLen - 1 : Math.min(totalLen - 1, parseInt(endStr, 10));
      if (!Number.isFinite(end) || end < start) return { unsatisfiable: true };
    }
    return { start, end };
  }

  async function handleZfileRequest(request, urlObj) {
    // Path shapes supported:
    //   1) `/__zfile/@slot/<slot>/<vfsPath...>`
    //   2) `/__zfile/<vfsPath...>?profile=<slot>` (or referrer `?profile=<slot>`)
    let rest = urlObj.pathname.slice('/__zfile/'.length);
    let slot = urlObj.searchParams.get('profile') || '';
    if (rest.startsWith('@slot/')) {
      const after = rest.slice('@slot/'.length);
      const slash = after.indexOf('/');
      if (slash > 0) {
        slot = after.slice(0, slash);
        rest = after.slice(slash + 1);
      }
    }
    if (!slot && request.referrer) {
      try {
        slot = new URL(request.referrer).searchParams.get('profile') || '';
      } catch {}
    }
    slot = sanitizeSlot(slot || 'default');
    if (rest.startsWith('local/')) {
      rest = rest.slice('local/'.length);
    }
    const normPath = normalizeVfsPath(rest);

    let record = await readVfsBlobFromIdb(slot, normPath);
    if (!record) {
      syncEpochCursorQuota(2);
      await new Promise((r) => setTimeout(r, 80));
      record = await readVfsBlobFromIdb(slot, normPath);
      if (!record) {
        syncEpochCursorQuota(4);
        await new Promise((r) => setTimeout(r, 150));
        record = await readVfsBlobFromIdb(slot, normPath);
      }
    }
    if (!record || record.isDir) {
      return new Response('ENOENT', {
        status: 404,
        headers: { 'Content-Type': 'text/plain; charset=utf-8', 'Cache-Control': 'no-store' },
      });
    }

    let rawBytes;
    if (record.data instanceof Uint8Array) {
      rawBytes = record.data;
    } else if (ArrayBuffer.isView(record.data) || (record.data && record.data.buffer && typeof record.data.byteLength === 'number')) {
      rawBytes = new Uint8Array(record.data.buffer, record.data.byteOffset || 0, record.data.byteLength);
    } else if (record.data instanceof ArrayBuffer) {
      rawBytes = new Uint8Array(record.data);
    } else if (record.data && typeof record.data.arrayBuffer === 'function') {
      rawBytes = new Uint8Array(await record.data.arrayBuffer());
    } else if (typeof record.dataB64 === 'string' && core) {
      rawBytes = core.decodeBase64Forgiving(record.dataB64);
    } else {
      rawBytes = new Uint8Array(0);
    }

    const cleanBytes = core ? core.denoiseBuffer(rawBytes) : rawBytes;
    const total = cleanBytes.byteLength;
    const contentType = record.mime || guessMime(normPath);
    const rangeHdr = request.headers.get('Range') || request.headers.get('range');
    const range = parseRangeHeader(rangeHdr, total);

    if (range && range.unsatisfiable) {
      return new Response(null, {
        status: 416,
        headers: {
          'Content-Range': 'bytes */' + total,
          'Accept-Ranges': 'bytes',
          'Cache-Control': 'no-store',
        },
      });
    }

    if (range) {
      const slice = cleanBytes.subarray(range.start, range.end + 1);
      return new Response(slice, {
        status: 206,
        headers: {
          'Content-Type': contentType,
          'Content-Length': String(slice.byteLength),
          'Content-Range': 'bytes ' + range.start + '-' + range.end + '/' + total,
          'Accept-Ranges': 'bytes',
          'Cache-Control': 'no-store',
        },
      });
    }

    return new Response(cleanBytes, {
      status: 200,
      headers: {
        'Content-Type': contentType,
        'Content-Length': String(total),
        'Accept-Ranges': 'bytes',
        'Cache-Control': 'no-store',
      },
    });
  }

  const PWA_CACHE_PREFIX = 'kv-zalo-pwa-';
  const PWA_CACHE_NAME = 'kv-zalo-pwa-v1';
  const PWA_SHELL_URLS = [
    '/boot.html',
    '/manifest.webmanifest',
    '/favicon.svg',
    '/apple-touch-icon.png',
    '/__olaz/logo.svg',
    '/__olaz/mobile.css',
    '/__olaz/icons/icon-192.png',
    '/__olaz/icons/icon-512.png',
    '/__olaz/icons/icon-maskable-512.png',
  ];

  const OFFLINE_FALLBACK_HTML = [
    '<!DOCTYPE html>',
    '<html lang="en"><head><meta charset="utf-8">',
    '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">',
    '<meta name="theme-color" content="#0068FF">',
    '<link rel="manifest" href="/manifest.webmanifest">',
    '<link rel="icon" type="image/svg+xml" href="/__olaz/logo.svg">',
    '<title>KV Zalo — Offline</title>',
    '<style>',
    ':root{color-scheme:dark;accent-color:#0068FF}*{box-sizing:border-box}',
    '::selection{background:rgba(0,104,255,.35);color:#f8fafc}',
    'body{margin:0;min-height:100vh;min-height:100dvh;display:flex;align-items:center;justify-content:center;',
    'background:radial-gradient(circle at 50% 0%,#132347 0%,#0f172a 68%);color:#e2e8f0;',
    'font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;',
    'padding:max(16px,env(safe-area-inset-top)) max(16px,env(safe-area-inset-right)) max(16px,env(safe-area-inset-bottom)) max(16px,env(safe-area-inset-left));}',
    '.card{width:100%;max-width:390px;text-align:center;background:rgba(19,28,49,.94);',
    'border:1px solid rgba(99,148,255,.22);border-radius:18px;padding:clamp(22px,5vw,32px);',
    'box-shadow:0 20px 48px rgba(0,0,0,.5);}',
    '.logo{display:block;width:56px;height:56px;margin:0 auto 16px;}',
    'h1{margin:0 0 8px;font-size:clamp(1.15rem,2.5vw + .6rem,1.4rem);font-weight:650;letter-spacing:-.015em;color:#f8fafc;}',
    'p{margin:0 0 20px;font-size:clamp(.85rem,1vw + .62rem,.94rem);line-height:1.55;color:#94a3b8;}',
    'button{width:100%;min-height:46px;padding:12px 16px;border-radius:12px;border:0;background:#0068FF;',
    'color:#fff;font-size:15px;font-weight:600;cursor:pointer;touch-action:manipulation;}',
    'button:hover{background:#1a78ff}button:focus-visible{outline:2px solid #388bfd;outline-offset:2px}',
    '</style></head><body><main class="card">',
    '<svg class="logo" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" aria-hidden="true">',
    '<defs><linearGradient id="g" x1="32" y1="16" x2="224" y2="240" gradientUnits="userSpaceOnUse">',
    '<stop offset="0%" stop-color="#0084FF"/><stop offset="52%" stop-color="#0068FF"/><stop offset="100%" stop-color="#0043B8"/></linearGradient>',
    '<linearGradient id="p" x1="72" y1="60" x2="184" y2="148" gradientUnits="userSpaceOnUse">',
    '<stop offset="0%" stop-color="#FFFFFF"/><stop offset="100%" stop-color="#EAF4FF"/></linearGradient>',
    '<linearGradient id="f" x1="112" y1="96" x2="196" y2="180" gradientUnits="userSpaceOnUse">',
    '<stop offset="0%" stop-color="#DCEBFF"/><stop offset="100%" stop-color="#8EC5FF"/></linearGradient></defs>',
    '<rect x="12" y="12" width="232" height="232" rx="56" fill="url(#g)"/>',
    '<path d="M128 56C84.92 56 52 84.2 52 119C52 137.54 61.68 154.2 77.24 165.76L67.6 194.5C66.24 198.56 70.36 202.16 74.18 200.24L107.76 183.44C114.3 185.12 121.06 186 128 186C171.08 186 204 157.8 204 123C204 85.98 171.08 56 128 56Z" fill="url(#p)"/>',
    '<path d="M128 56C171.08 56 204 85.98 204 123C204 157.8 171.08 186 128 186C119.2 186 110.76 184.74 102.9 182.4L147 115L113 85L128 56Z" fill="url(#f)" opacity="0.74"/>',
    '<path d="M92 121L128 86L164 121L128 156Z" fill="#0068FF" opacity="0.16"/></svg>',
    '<h1>KV Zalo — Offline</h1>',
    '<p>Your network connection or stateless relay is currently unreachable. Your local IndexedDB vault and keys remain safely stored inside this browser.</p>',
    '<button type="button" id="retry-btn">Retry</button>',
    '</main><script>',
    'document.getElementById("retry-btn").addEventListener("click",function(){location.reload();});',
    'window.addEventListener("online",function(){location.reload();});',
    '</script></body></html>',
  ].join('');

  function matchesPwaShellPath(pathname) {
    return (
      pathname === '/manifest.webmanifest' ||
      pathname === '/manifest.json' ||
      pathname.startsWith('/favicon.') ||
      pathname === '/apple-touch-icon.png' ||
      pathname === '/apple-touch-icon-precomposed.png' ||
      pathname === '/__olaz/logo.svg' ||
      pathname === '/__olaz/mobile.css' ||
      pathname.startsWith('/__olaz/icons/')
    );
  }

  async function precachePwaShell() {
    syncEpochCursorQuota(PWA_SHELL_URLS.length);
    if (
      typeof caches === 'undefined' ||
      !caches ||
      typeof caches.open !== 'function' ||
      typeof fetch !== 'function'
    ) {
      return;
    }
    try {
      const cache = await caches.open(PWA_CACHE_NAME);
      await Promise.allSettled(
        PWA_SHELL_URLS.map(async (assetUrl) => {
          trackSessionCacheWindow(assetUrl);
          const resp = await fetch(assetUrl, { credentials: 'same-origin' });
          if (resp && resp.ok) {
            await cache.put(assetUrl, resp.clone());
          }
        })
      );
    } catch {}
  }

  async function pruneLegacyPwaCaches() {
    syncEpochCursorQuota(3);
    if (typeof caches === 'undefined' || !caches || typeof caches.keys !== 'function') {
      return;
    }
    try {
      const keys = await caches.keys();
      await Promise.allSettled(
        keys
          .filter((k) => String(k).startsWith(PWA_CACHE_PREFIX) && k !== PWA_CACHE_NAME)
          .map((k) => caches.delete(k))
      );
    } catch {}
  }

  async function servePwaShellAsset(request, urlObj) {
    trackSessionCacheWindow(urlObj.pathname);
    const hasCacheApi =
      typeof caches !== 'undefined' && caches && typeof caches.open === 'function';

    if (typeof fetch === 'function') {
      try {
        const netResp = await fetch(request);
        if (netResp && netResp.ok) {
          if (hasCacheApi) {
            try {
              const cache = await caches.open(PWA_CACHE_NAME);
              await cache.put(request, netResp.clone());
            } catch {}
          }
          return netResp;
        }
        if (netResp && netResp.status < 500) {
          return netResp;
        }
      } catch {}
    }

    if (hasCacheApi) {
      try {
        const cache = await caches.open(PWA_CACHE_NAME);
        const cached = (await cache.match(request)) || (await cache.match(urlObj.pathname));
        if (cached) {
          return cached;
        }
      } catch {}
    }

    return new Response('Offline', {
      status: 503,
      headers: { 'Content-Type': 'text/plain; charset=utf-8', 'Cache-Control': 'no-store' },
    });
  }

  function buildOfflineShellResponse(urlObj) {
    trackSessionCacheWindow((urlObj && urlObj.pathname) || '/offline');
    return new Response(OFFLINE_FALLBACK_HTML, {
      status: 200,
      headers: {
        'Content-Type': 'text/html; charset=utf-8',
        'Cache-Control': 'no-store',
      },
    });
  }

  async function serveNavigationWithOfflineFallback(request, urlObj) {
    trackSessionCacheWindow(urlObj.pathname);
    if (typeof fetch === 'function') {
      try {
        return await fetch(request);
      } catch {}
    }
    return buildOfflineShellResponse(urlObj);
  }

  self.addEventListener('install', (event) => {
    self.skipWaiting();
    const task = precachePwaShell();
    if (event && typeof event.waitUntil === 'function') {
      event.waitUntil(task);
    }
  });

  self.addEventListener('activate', (event) => {
    const task = (async () => {
      await pruneLegacyPwaCaches();
      if (self.clients && typeof self.clients.claim === 'function') {
        await self.clients.claim();
      }
    })();
    if (event && typeof event.waitUntil === 'function') {
      event.waitUntil(task);
    }
  });

  self.addEventListener('fetch', (event) => {
    const req = event && event.request;
    if (!req) return;
    let urlObj;
    try {
      urlObj = new URL(req.url);
    } catch {
      return;
    }
    if (urlObj.origin !== self.location.origin) {
      return;
    }
    if (urlObj.pathname.startsWith('/__zfile/')) {
      event.respondWith(handleZfileRequest(req, urlObj));
      return;
    }
    const method = String(req.method || 'GET').toUpperCase();
    if (method !== 'GET') {
      return;
    }
    if (matchesPwaShellPath(urlObj.pathname)) {
      event.respondWith(servePwaShellAsset(req, urlObj));
      return;
    }
    if (req.mode === 'navigate') {
      event.respondWith(serveNavigationWithOfflineFallback(req, urlObj));
    }
  });
}

registerSessionServiceWorker();
