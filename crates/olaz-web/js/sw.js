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

(function () {
  'use strict';

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

    const record = await readVfsBlobFromIdb(slot, normPath);
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

  self.addEventListener('install', () => {
    self.skipWaiting();
  });

  self.addEventListener('activate', (event) => {
    event.waitUntil(self.clients.claim());
  });

  self.addEventListener('fetch', (event) => {
    let urlObj;
    try {
      urlObj = new URL(event.request.url);
    } catch {
      return;
    }
    if (urlObj.origin === self.location.origin && urlObj.pathname.startsWith('/__zfile/')) {
      event.respondWith(handleZfileRequest(event.request, urlObj));
    }
  });
})();
