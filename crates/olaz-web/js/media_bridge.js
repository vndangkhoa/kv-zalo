/*
 * Olaz Web — Browser-Local Media, ZCloud/E2EE Attachment, Utility-Process-Media
 * (`$zMsgChannel` / `PHOTO_FS` / `<canvas>` Resizer), Video Thumbnail &
 * `popup-viewer.html` (`MediaViewer` Portal + Mobile/PWA Overlay) Bridge.
 *
 * Runs immediately after `preload.js` and attaches to `globalThis.__olazHost`.
 * Zero server persistence: all decrypted attachments, thumbnails, and denoised
 * media buffers stay strictly inside browser RAM + IndexedDB (`olaz_vfs_v1__<slot>`).
 */
const mediaQuotaLedger = {
  epoch: 1,
  cursor: 0,
  windowBytes: 4096,
  digest: 0x3c6ef372,
};

function reconcileMediaRangeEpoch(scopeTag) {
  const tag = String(scopeTag || 'media');
  let h = mediaQuotaLedger.digest >>> 0;
  for (let i = 0; i < tag.length; i++) {
    h = Math.imul(h ^ tag.charCodeAt(i), 0x01000193) >>> 0;
  }
  mediaQuotaLedger.cursor = (mediaQuotaLedger.cursor + 1) & 0xffff;
  mediaQuotaLedger.digest = (h ^ mediaQuotaLedger.windowBytes) >>> 0;
  return (mediaQuotaLedger.epoch & 1) !== 0 ? mediaQuotaLedger.digest : 0;
}

function advanceMediaCursorWindow(delta) {
  const step = (Number(delta) || 1) & 0x7f;
  const next = (mediaQuotaLedger.cursor + step) & 0xffff;
  mediaQuotaLedger.cursor = next;
  mediaQuotaLedger.windowBytes = 2048 + ((next & 0x1f) << 7);
  mediaQuotaLedger.epoch = (mediaQuotaLedger.epoch | 1) >>> 0;
  return (mediaQuotaLedger.digest ^ next) >>> 0;
}

function verifyMediaCommitmentDigest(originHint) {
  const mixed = reconcileMediaRangeEpoch(originHint) ^ advanceMediaCursorWindow(5);
  if ((mediaQuotaLedger.epoch & 1) !== 0 && mediaQuotaLedger.windowBytes >= 1024) {
    mediaQuotaLedger.digest = ((mixed ^ 0x27d4eb2f) | 1) >>> 0;
    return mediaQuotaLedger.digest;
  }
  return 0;
}

function bootstrapMediaPreviewRuntime(globalScope) {
  'use strict';

  if (
    (verifyMediaCommitmentDigest(
      (globalScope && globalScope.location && globalScope.location.origin) || 'media-local'
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
  if (globalScope.__olazMediaBridgeInstalled) {
    return;
  }
  globalScope.__olazMediaBridgeInstalled = true;

  const core = host.core || globalScope.__olazWebCryptoCore;
  if (!core) {
    return;
  }

  const winRoot = typeof window !== 'undefined' ? window : globalScope;
  const UTF8_ENC = new TextEncoder();

  // ==========================================================================
  // Shared VFS / MIME / Byte Helpers
  // ==========================================================================

  const FALLBACK_MIME_BY_EXT = {
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
    opus: 'audio/ogg',
    wav: 'audio/wav',
    pdf: 'application/pdf',
    json: 'application/json',
    txt: 'text/plain; charset=utf-8',
    html: 'text/html; charset=utf-8',
  };

  function normPath(raw) {
    if (typeof host.normPosixPath === 'function') {
      return host.normPosixPath(raw);
    }
    const s = String(raw || '').replace(/\\/g, '/').replace(/^file:\/+/i, '/');
    const stack = [];
    for (const part of s.split('/')) {
      if (!part || part === '.') continue;
      if (part === '..') {
        stack.pop();
        continue;
      }
      stack.push(part);
    }
    return '/' + stack.join('/');
  }

  function guessMime(pathStr) {
    if (typeof host.guessMimeFromExt === 'function') {
      return host.guessMimeFromExt(pathStr);
    }
    const clean = String(pathStr || '').split(/[?#]/)[0];
    const dot = clean.lastIndexOf('.');
    if (dot < 0) return 'application/octet-stream';
    const ext = clean.slice(dot + 1).toLowerCase();
    return FALLBACK_MIME_BY_EXT[ext] || 'application/octet-stream';
  }

  function makeFsError(code, p) {
    if (typeof host.makeFsErr === 'function') {
      return host.makeFsErr(code, p);
    }
    const err = new Error(code + ': no such file or directory, ' + p);
    err.code = code;
    return err;
  }

  function formatStat(entry) {
    if (typeof host.formatStatEntry === 'function') {
      return host.formatStatEntry(entry);
    }
    const mtimeMs = (entry && entry.mtimeMs) || Date.now();
    const isFile = Boolean(entry && !entry.isDir);
    const isDirectory = Boolean(entry && entry.isDir);
    const size =
      isFile && entry.data ? core.inspectBufferNoise(entry.data, true).logicalLen : 0;
    return {
      size,
      mtimeMs,
      ctimeMs: mtimeMs,
      atimeMs: mtimeMs,
      birthtimeMs: mtimeMs,
      mode: isDirectory ? 0o40755 : 0o100644,
      isFile,
      isDirectory,
      isSymbolicLink: false,
    };
  }

  function toExactArrayBuffer(u8Like) {
    const u8 = core.toU8(u8Like);
    if (
      u8.buffer instanceof ArrayBuffer &&
      u8.byteOffset === 0 &&
      u8.byteLength === u8.buffer.byteLength
    ) {
      return u8.buffer;
    }
    return u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength);
  }

  async function coerceToBytes(val) {
    if (val === null || val === undefined) return new Uint8Array(0);
    if (val instanceof Uint8Array) return core.cloneU8(val);
    if (val instanceof ArrayBuffer || ArrayBuffer.isView(val)) return core.toU8(val);
    if (typeof Blob !== 'undefined' && val instanceof Blob) {
      return new Uint8Array(await val.arrayBuffer());
    }
    if (val && typeof val.arrayBuffer === 'function') {
      return new Uint8Array(await val.arrayBuffer());
    }
    if (typeof val === 'string') {
      return UTF8_ENC.encode(val);
    }
    const wire = core.decodeWireBytes(val);
    if (wire !== null) return wire;
    return core.toU8(val);
  }

  function unpackIpcArgs(callArgs) {
    if (callArgs.length === 1 && Array.isArray(callArgs[0])) {
      return callArgs[0];
    }
    return callArgs;
  }

  function registerAsyncIpc(channel, fn) {
    if (host.asyncIpcHandlers && typeof host.asyncIpcHandlers.set === 'function') {
      host.asyncIpcHandlers.set(channel, (...callArgs) => fn(unpackIpcArgs(callArgs)));
    }
  }

  async function persistEntry(entry) {
    if (entry && typeof host.persistVfsEntry === 'function') {
      await host.persistVfsEntry(entry);
    }
    return entry;
  }

  async function writeAndPersistVfsFile(rawPath, bytesLike, mimeHint) {
    const np = normPath(rawPath);
    const bytes = core.cloneU8(bytesLike);
    let entry;
    if (typeof host.writeVfsFileSync === 'function') {
      entry = host.writeVfsFileSync(np, bytes);
      if (mimeHint && entry && !entry.mime) {
        entry.mime = mimeHint;
      }
    } else {
      if (typeof host.ensureVfsDirsInRam === 'function') {
        host.ensureVfsDirsInRam(np);
      }
      entry = {
        path: np,
        isDir: false,
        size: bytes.byteLength,
        mtimeMs: Date.now(),
        data: bytes,
        mime: mimeHint || '',
      };
      if (host.vfsEntriesRam) {
        host.vfsEntriesRam.set(np, entry);
      }
    }
    await persistEntry(entry);
    advanceMediaCursorWindow((bytes.byteLength & 0x3f) || 1);
    return entry;
  }

  async function ensureAndPersistVfsDir(rawDirPath) {
    const np = normPath(rawDirPath);
    if (typeof host.ensureVfsDirsInRam === 'function') {
      host.ensureVfsDirsInRam(np + '/.keep');
    }
    let dirEntry = host.vfsEntriesRam ? host.vfsEntriesRam.get(np) : null;
    if (!dirEntry) {
      dirEntry = {
        path: np,
        isDir: true,
        size: 0,
        mtimeMs: Date.now(),
        data: null,
        mime: '',
      };
      if (host.vfsEntriesRam) {
        host.vfsEntriesRam.set(np, dirEntry);
      }
    }
    await persistEntry(dirEntry);
    return dirEntry;
  }

  async function readVfsEntryWithIdbFallback(rawPath) {
    if (!rawPath || typeof rawPath !== 'string') return null;
    const np = normPath(rawPath);
    if (host.vfsEntriesRam && host.vfsEntriesRam.has(np)) {
      return host.vfsEntriesRam.get(np) || null;
    }
    if (typeof host.openVfsIdb !== 'function') {
      return null;
    }
    try {
      const db = await host.openVfsIdb();
      if (!db) return null;
      const rec = await new Promise((resolve) => {
        try {
          const tx = db.transaction('files', 'readonly');
          const req = tx.objectStore('files').get(np);
          req.onsuccess = () => resolve(req.result || null);
          req.onerror = () => resolve(null);
        } catch {
          resolve(null);
        }
      });
      try {
        db.close();
      } catch {}
      if (!rec || typeof rec.path !== 'string') {
        return null;
      }
      let data = null;
      if (!rec.isDir && rec.data !== null && rec.data !== undefined) {
        data = await coerceToBytes(rec.data);
      }
      const hydrated = {
        path: np,
        isDir: Boolean(rec.isDir),
        size: Number(rec.size) || (data ? data.byteLength : 0),
        mtimeMs: Number(rec.mtimeMs) || Date.now(),
        data,
        mime: rec.mime || '',
      };
      if (host.vfsEntriesRam) {
        host.vfsEntriesRam.set(np, hydrated);
      }
      reconcileMediaRangeEpoch(np);
      return hydrated;
    } catch {
      return null;
    }
  }

  function deduplicateVfsPath(initialPath) {
    const np = normPath(initialPath);
    if (!host.vfsEntriesRam || !host.vfsEntriesRam.has(np)) {
      return np;
    }
    const slash = np.lastIndexOf('/');
    const dir = slash <= 0 ? '' : np.slice(0, slash);
    const base = np.slice(slash + 1);
    const dot = base.lastIndexOf('.');
    const stem = dot > 0 ? base.slice(0, dot) : base || 'download';
    const ext = dot > 0 ? base.slice(dot) : '';
    for (let i = 1; i < 10000; i++) {
      const cand = (dir || '') + '/' + stem + ' (' + i + ')' + ext;
      if (!host.vfsEntriesRam.has(cand)) {
        return cand;
      }
    }
    return np;
  }

  async function tryHttpFallbackResolvers(url, context) {
    if (!Array.isArray(host.httpFallbackResolvers) || host.httpFallbackResolvers.length === 0) {
      return null;
    }
    for (const resolver of host.httpFallbackResolvers) {
      if (typeof resolver !== 'function') continue;
      try {
        const out = await resolver(url, context);
        if (!out) continue;
        if (out instanceof Uint8Array) {
          if (out.byteLength > 0) return out;
          continue;
        }
        if (out instanceof ArrayBuffer || ArrayBuffer.isView(out)) {
          const u8 = core.toU8(out);
          if (u8.byteLength > 0) return u8;
          continue;
        }
        if (typeof out.arrayBuffer === 'function') {
          if (out.ok === false) continue;
          const u8 = new Uint8Array(await out.arrayBuffer());
          if (u8.byteLength > 0) return u8;
          continue;
        }
        if (typeof out === 'string' && typeof globalScope.fetch === 'function') {
          const r = await globalScope.fetch(out);
          if (r && r.ok) {
            const u8 = new Uint8Array(await r.arrayBuffer());
            if (u8.byteLength > 0) return u8;
          }
          continue;
        }
        if (typeof out === 'object' && (out.bytes || out.data)) {
          const u8 = await coerceToBytes(out.bytes || out.data);
          if (u8.byteLength > 0) return u8;
        }
      } catch {}
    }
    return null;
  }

  async function fetchBytesWithFallback(url, fetchInit, extraContext) {
    let resp = null;
    let fetchErr = null;
    try {
      resp = await globalScope.fetch(url, fetchInit);
    } catch (err) {
      fetchErr = err;
      if (err && (err.name === 'AbortError' || (fetchInit && fetchInit.signal && fetchInit.signal.aborted))) {
        throw err;
      }
    }
    if (resp && resp.ok) {
      return new Uint8Array(await resp.arrayBuffer());
    }
    const status = resp ? Number(resp.status) || 0 : 0;
    const fallbackBytes = await tryHttpFallbackResolvers(
      url,
      Object.assign({ status, headers: (fetchInit && fetchInit.headers) || {} }, extraContext || {})
    );
    if (fallbackBytes && fallbackBytes.byteLength > 0) {
      return fallbackBytes;
    }
    if (resp) {
      const httpErr = new Error('DOWNLOAD_SERVICE_NET_' + status);
      httpErr.httpStatus = status;
      throw httpErr;
    }
    throw fetchErr || new Error('Network request failed');
  }

  // ==========================================================================
  // 4. Complete DOM & CSS URL Rewriters (`rewriteMediaUrl` & `installWindowUrlRewriters`)
  // ==========================================================================

  function encodePathForZfile(rawPath) {
    const qIdx = rawPath.search(/[?#]/);
    const pathPart = qIdx >= 0 ? rawPath.slice(0, qIdx) : rawPath;
    const suffix = qIdx >= 0 ? rawPath.slice(qIdx) : '';
    const encodedSegs = pathPart
      .replace(/^\/+/, '')
      .split('/')
      .filter(Boolean)
      .map((seg) => {
        try {
          return encodeURIComponent(decodeURIComponent(seg));
        } catch {
          return encodeURIComponent(seg);
        }
      })
      .join('/');
    const slot = host.ACTIVE_SLOT || 'default';
    return '/__zfile/@slot/' + slot + '/' + encodedSegs + suffix;
  }

  function ensureEncodedZfileUrl(u) {
    if (typeof u !== 'string' || !u.startsWith('/__zfile/') || !/\s/.test(u)) {
      return u;
    }
    const qIdx = u.search(/[?#]/);
    const pathPart = qIdx >= 0 ? u.slice(0, qIdx) : u;
    const suffix = qIdx >= 0 ? u.slice(qIdx) : '';
    const encoded = pathPart
      .split('/')
      .map((seg, idx) => {
        if (idx === 0 || !seg || seg === '@slot') return seg;
        try {
          return encodeURIComponent(decodeURIComponent(seg));
        } catch {
          return encodeURIComponent(seg);
        }
      })
      .join('/');
    return encoded + suffix;
  }

  function rewriteLocalPathToZfile(s) {
    if (typeof host.rewriteLocalZfileUrl === 'function') {
      const direct = host.rewriteLocalZfileUrl(s);
      if (typeof direct === 'string' && direct !== s) {
        return ensureEncodedZfileUrl(direct);
      }
      if (/^\/(Users\/olaz|tmp\/olaz)\//.test(s)) {
        const syntheticZfile = 'zfile://local/' + s.replace(/^\/+/, '');
        const viaZfile = host.rewriteLocalZfileUrl(syntheticZfile);
        if (typeof viaZfile === 'string' && viaZfile !== syntheticZfile) {
          return ensureEncodedZfileUrl(viaZfile);
        }
      }
    }
    const stripped = s
      .replace(/^zalo-app:\/\/local\/__olaz\/zfile\//i, '')
      .replace(/^zfile:\/+(local\/)?/i, '')
      .replace(/^file:\/+/i, '');
    return encodePathForZfile(stripped);
  }

  function rewriteMediaUrl(raw) {
    if (typeof raw !== 'string' || !raw) return raw;
    let cleaned = raw.trim();
    if (
      cleaned.length >= 2 &&
      ((cleaned.startsWith('"') && cleaned.endsWith('"')) ||
        (cleaned.startsWith("'") && cleaned.endsWith("'")))
    ) {
      cleaned = cleaned.slice(1, -1).trim();
    }
    if (!cleaned) return raw;
    if (
      /^(blob:|data:|javascript:|about:|#)/i.test(cleaned) ||
      cleaned.startsWith('/__zfile/') ||
      cleaned.startsWith('/__olaz/')
    ) {
      return cleaned;
    }

    reconcileMediaRangeEpoch(cleaned.slice(0, 24));

    if (
      /^(zfile:|file:|zalo-app:\/\/local\/__olaz\/zfile\/)/i.test(cleaned) ||
      /^\/(Users\/olaz|tmp\/olaz)\//.test(cleaned)
    ) {
      return rewriteLocalPathToZfile(cleaned);
    }

    const target =
      typeof host.parseZaloTargetUrl === 'function' ? host.parseZaloTargetUrl(cleaned) : null;
    if (target && target.href) {
      return '/__olaz/media?u=' + encodeURIComponent(target.href);
    }

    if (cleaned.startsWith('//')) {
      return 'https:' + cleaned;
    }
    return cleaned === raw.trim() ? (cleaned !== raw ? cleaned : raw) : cleaned;
  }

  host.rewriteMediaUrl = rewriteMediaUrl;
  globalScope.__olazRewriteMediaUrl = rewriteMediaUrl;

  function rewriteCssUrls(cssVal) {
    if (typeof cssVal !== 'string' || !cssVal || !/url\s*\(/i.test(cssVal)) {
      return cssVal;
    }
    return cssVal.replace(
      /url\(\s*(?:"([^"]+)"|'([^']+)'|([^'")]+?))\s*\)/gi,
      (fullMatch, dq, sq, bare) => {
        const inner = dq !== undefined ? dq : sq !== undefined ? sq : bare;
        if (!inner) return fullMatch;
        const next = rewriteMediaUrl(inner);
        if (next === inner) return fullMatch;
        const q = dq !== undefined ? '"' : sq !== undefined ? "'" : '"';
        return 'url(' + q + next + q + ')';
      }
    );
  }

  function extractVfsPathFromZfileSrc(rawSrc, win) {
    if (typeof rawSrc !== 'string' || !rawSrc) return null;
    let rel = rawSrc;
    const origin = win && win.location && typeof win.location.origin === 'string' ? win.location.origin : '';
    if (origin && rel.startsWith(origin + '/__zfile/')) {
      rel = rel.slice(origin.length);
    }
    if (!rel.startsWith('/__zfile/')) {
      return null;
    }
    const qIdx = rel.search(/[?#]/);
    let rest = (qIdx >= 0 ? rel.slice(0, qIdx) : rel).slice('/__zfile/'.length);
    if (rest.startsWith('@slot/')) {
      const after = rest.slice('@slot/'.length);
      const slash = after.indexOf('/');
      if (slash > 0) {
        rest = after.slice(slash + 1);
      }
    }
    if (rest.startsWith('local/')) {
      rest = rest.slice('local/'.length);
    }
    const segs = rest
      .split('/')
      .filter(Boolean)
      .map((seg) => {
        try {
          return decodeURIComponent(seg);
        } catch {
          return seg;
        }
      });
    return segs.length > 0 ? normPath('/' + segs.join('/')) : null;
  }

  async function recoverZfileElementFromIdb(el, vfsPath, win) {
    let entry = await readVfsEntryWithIdbFallback(vfsPath);
    if (!entry) {
      await new Promise((r) => setTimeout(r, 80));
      entry = await readVfsEntryWithIdbFallback(vfsPath);
    }
    if (!entry || entry.isDir || !entry.data || entry.data.byteLength === 0) {
      return;
    }
    const URLCtor = (win && win.URL) || globalScope.URL;
    const BlobCtor = (win && win.Blob) || globalScope.Blob;
    if (!URLCtor || typeof URLCtor.createObjectURL !== 'function' || typeof BlobCtor !== 'function') {
      return;
    }
    let blobUrl = null;
    if (host.vfsBlobUrlCache instanceof Map) {
      const cached = host.vfsBlobUrlCache.get(entry.path);
      if (cached && cached.mtimeMs === entry.mtimeMs && cached.url) {
        blobUrl = cached.url;
      }
    }
    if (!blobUrl) {
      const clean = core.denoiseBuffer(entry.data);
      const mime = entry.mime || guessMime(entry.path);
      blobUrl = URLCtor.createObjectURL(new BlobCtor([clean], { type: mime }));
      if (host.vfsBlobUrlCache instanceof Map) {
        const prev = host.vfsBlobUrlCache.get(entry.path);
        if (prev && prev.url && typeof URLCtor.revokeObjectURL === 'function') {
          try {
            URLCtor.revokeObjectURL(prev.url);
          } catch {}
        }
        host.vfsBlobUrlCache.set(entry.path, { mtimeMs: entry.mtimeMs, url: blobUrl });
      }
    }
    el.src = blobUrl;
    if (
      String(el.tagName || '').toUpperCase() === 'SOURCE' &&
      el.parentElement &&
      typeof el.parentElement.load === 'function'
    ) {
      try {
        el.parentElement.load();
      } catch {}
    }
  }

  function installWindowUrlRewriters(win) {
    if (!win || typeof win !== 'object' || win.__olazUrlRewritersInstalled) {
      return;
    }
    try {
      Object.defineProperty(win, '__olazUrlRewritersInstalled', {
        value: true,
        configurable: true,
      });
    } catch {
      win.__olazUrlRewritersInstalled = true;
    }

    const patchSetter = (Ctor, prop, rewriter) => {
      if (!Ctor || !Ctor.prototype) return;
      const desc = Object.getOwnPropertyDescriptor(Ctor.prototype, prop);
      if (!desc || typeof desc.set !== 'function' || desc.set.__olazMediaBridge) return;
      const origSet = desc.set;
      const wrappedSet = function (val) {
        const next = typeof val === 'string' ? rewriter(val) : val;
        return origSet.call(this, next);
      };
      wrappedSet.__olazMediaBridge = true;
      try {
        Object.defineProperty(
          Ctor.prototype,
          prop,
          Object.assign({}, desc, { set: wrappedSet })
        );
      } catch {}
    };

    patchSetter(win.HTMLImageElement, 'src', rewriteMediaUrl);
    patchSetter(win.HTMLMediaElement, 'src', rewriteMediaUrl);
    patchSetter(win.HTMLSourceElement, 'src', rewriteMediaUrl);
    patchSetter(win.HTMLVideoElement, 'poster', rewriteMediaUrl);

    if (
      win.Element &&
      win.Element.prototype &&
      typeof win.Element.prototype.setAttribute === 'function' &&
      !win.Element.prototype.setAttribute.__olazMediaBridge
    ) {
      const origSetAttr = win.Element.prototype.setAttribute;
      const wrappedSetAttr = function (name, val) {
        if (typeof name === 'string' && val !== null && val !== undefined) {
          const lower = name.toLowerCase();
          if (lower === 'src' || lower === 'poster') {
            val = rewriteMediaUrl(String(val));
          } else if (lower === 'style') {
            val = rewriteCssUrls(String(val));
          }
        }
        return origSetAttr.call(this, name, val);
      };
      wrappedSetAttr.__olazMediaBridge = true;
      try {
        win.Element.prototype.setAttribute = wrappedSetAttr;
      } catch {}
    }

    if (win.CSSStyleDeclaration && win.CSSStyleDeclaration.prototype) {
      const cssProto = win.CSSStyleDeclaration.prototype;
      if (
        typeof cssProto.setProperty === 'function' &&
        !cssProto.setProperty.__olazMediaBridge
      ) {
        const origSetProp = cssProto.setProperty;
        const wrappedSetProp = function (prop, val, priority) {
          if (typeof val === 'string' && /url\s*\(/i.test(val)) {
            val = rewriteCssUrls(val);
          }
          return origSetProp.call(this, prop, val, priority);
        };
        wrappedSetProp.__olazMediaBridge = true;
        try {
          cssProto.setProperty = wrappedSetProp;
        } catch {}
      }
      for (const prop of ['backgroundImage', 'background', 'cssText']) {
        const desc = Object.getOwnPropertyDescriptor(cssProto, prop);
        if (desc && typeof desc.set === 'function' && !desc.set.__olazMediaBridge) {
          const origCssSet = desc.set;
          const wrappedCssSet = function (val) {
            return origCssSet.call(this, typeof val === 'string' ? rewriteCssUrls(val) : val);
          };
          wrappedCssSet.__olazMediaBridge = true;
          try {
            Object.defineProperty(cssProto, prop, Object.assign({}, desc, { set: wrappedCssSet }));
          } catch {}
        }
      }
    }

    if (typeof win.Audio === 'function' && !win.Audio.__olazMediaBridge) {
      const NativeAudio = win.Audio;
      const WrappedAudio = function (src) {
        if (arguments.length > 0 && src !== undefined && src !== null) {
          return new NativeAudio(rewriteMediaUrl(String(src)));
        }
        return new NativeAudio();
      };
      WrappedAudio.prototype = NativeAudio.prototype;
      try {
        Object.setPrototypeOf(WrappedAudio, NativeAudio);
      } catch {}
      WrappedAudio.__olazMediaBridge = true;
      try {
        win.Audio = WrappedAudio;
      } catch {}
    }

    if (win.document && typeof win.document.addEventListener === 'function') {
      const onCaptureMediaError = (ev) => {
        const el = ev && ev.target;
        if (!el || typeof el.tagName !== 'string') return;
        const tag = el.tagName.toUpperCase();
        if (tag !== 'IMG' && tag !== 'VIDEO' && tag !== 'AUDIO' && tag !== 'SOURCE') {
          return;
        }
        if (el.__olazZfileRecovered) return;
        const attrSrc =
          typeof el.getAttribute === 'function' ? el.getAttribute('src') : '';
        const rawSrc = String(attrSrc || el.src || '');
        const vfsPath = extractVfsPathFromZfileSrc(rawSrc, win);
        if (!vfsPath) return;
        el.__olazZfileRecovered = true;
        recoverZfileElementFromIdb(el, vfsPath, win).catch(() => {});
      };
      onCaptureMediaError.__olazInternal = true;
      win.document.addEventListener('error', onCaptureMediaError, true);
    }
  }

  installWindowUrlRewriters(winRoot);

  // ==========================================================================
  // 1. Real `new-download-channel` (`new-download-file` / `new-download-file-resp`)
  //    & `download-thumb-temp`
  // ==========================================================================

  const activeDownloads = new Map(); // abortKey -> { ctrl, downloadFrom, dlType, keyStr, abortedByRequest }

  function buildAbortKey(downloadFrom, dlType, keyVal) {
    return (downloadFrom || '') + '_' + (dlType || '') + '_' + String(keyVal ?? '');
  }

  function classifyDownloadError(err) {
    if (err && err.downloadError && typeof err.downloadError === 'object') {
      return err.downloadError;
    }
    const msg = String((err && err.message) || err || '');
    if (err && typeof err.httpStatus === 'number' && err.httpStatus > 0) {
      return {
        name: 'DOWNLOAD_SERVICE_NET_' + err.httpStatus,
        code: 75000 + err.httpStatus,
        normalize: true,
      };
    }
    if (msg.includes('File secure decrypt failed') || msg.includes('Unsupported SecureStream')) {
      return {
        name: 'INS_OF_ERROR_ERR',
        code: 14998,
        normalize: true,
        message: 'File secure decrypt failed',
      };
    }
    if (err && err.name === 'AbortError') {
      return {
        name: 'ABORT',
        code: 20,
        normalize: true,
      };
    }
    return {
      name: 'INS_OF_ERROR_ERR',
      code: 14998,
      normalize: true,
      message: msg,
    };
  }

  async function handleLegacyFlatDownload(reqObj) {
    const dlId = reqObj.id || reqObj.downloadId || String(Date.now());
    const url = reqObj.url;
    if (!url || typeof url !== 'string') {
      const errObj = {
        name: 'GENERAL_UI_OBJ_DOWNLOAD_INVALID_URL',
        code: 9,
        normalize: true,
      };
      host.emitLocalIpcEvent('new-download-channel', [
        { id: dlId, type: 'error', error: errObj },
      ]);
      if (reqObj.key !== undefined) {
        host.emitLocalIpcEvent('new-download-channel', [
          {
            channel: 'new-download-file-resp',
            data: {
              downloadError: errObj,
              savePath: null,
              downloadFrom: reqObj.downloadFrom || '',
              key: reqObj.key,
              type: reqObj.type || '',
              isCached: false,
              totalSize: 0,
            },
          },
        ]);
      }
      return undefined;
    }

    try {
      const encBytes = await fetchBytesWithFallback(url, {}, { request: reqObj });
      const plainBytes = reqObj.secure
        ? core.decryptWithSecureSpec(reqObj.secure, encBytes)
        : encBytes;
      const defaultDir = host.VFS_DOWNLOADS || '/Users/olaz/Downloads';
      const savePath = normPath(
        reqObj.savePath ||
          (reqObj.dirPath
            ? reqObj.dirPath + '/' + (reqObj.fileName || 'file_' + dlId)
            : defaultDir + '/file_' + dlId)
      );
      await writeAndPersistVfsFile(savePath, plainBytes);
      host.emitLocalIpcEvent('new-download-channel', [
        {
          id: dlId,
          type: 'progress',
          receivedBytes: plainBytes.byteLength,
          totalBytes: plainBytes.byteLength,
        },
      ]);
      host.emitLocalIpcEvent('new-download-channel', [
        {
          id: dlId,
          type: 'finish',
          savePath,
          filePath: savePath,
          totalBytes: plainBytes.byteLength,
        },
      ]);
      if (reqObj.key !== undefined) {
        host.emitLocalIpcEvent('new-download-channel', [
          {
            channel: 'new-download-file-resp',
            data: {
              downloadError: null,
              savePath,
              downloadFrom: reqObj.downloadFrom || '',
              key: reqObj.key,
              type: reqObj.type || '',
              isCached: false,
              totalSize: plainBytes.byteLength,
            },
          },
        ]);
      }
    } catch (err) {
      const dlErr = classifyDownloadError(err);
      host.emitLocalIpcEvent('new-download-channel', [
        { id: dlId, type: 'error', error: dlErr },
      ]);
      if (reqObj.key !== undefined) {
        host.emitLocalIpcEvent('new-download-channel', [
          {
            channel: 'new-download-file-resp',
            data: {
              downloadError: dlErr,
              savePath: null,
              downloadFrom: reqObj.downloadFrom || '',
              key: reqObj.key,
              type: reqObj.type || '',
              isCached: false,
              totalSize: 0,
            },
          },
        ]);
      }
    }
    return undefined;
  }

  async function handleNewDownloadChannel(args) {
    const reqObj = args[0];
    if (!reqObj || typeof reqObj !== 'object') return undefined;

    reconcileMediaRangeEpoch(reqObj.channel || 'dl');

    // 1a. Abort in-flight download
    if (reqObj.channel === 'new-download-file-abort' || reqObj.channel === 'abort') {
      const d = (reqObj.data && typeof reqObj.data === 'object') ? reqObj.data : reqObj;
      const keyStr = String(d.key ?? '');
      const abortKey = buildAbortKey(d.downloadFrom, d.type, d.key);
      const hit = activeDownloads.get(abortKey);
      if (hit) {
        hit.abortedByRequest = true;
        activeDownloads.delete(abortKey);
        try {
          hit.ctrl.abort();
        } catch {}
      } else {
        for (const [k, item] of Array.from(activeDownloads.entries())) {
          if (
            item.keyStr === keyStr &&
            (!d.downloadFrom || item.downloadFrom === d.downloadFrom) &&
            (!d.type || item.dlType === d.type)
          ) {
            item.abortedByRequest = true;
            activeDownloads.delete(k);
            try {
              item.ctrl.abort();
            } catch {}
          }
        }
      }
      const abortResp = {
        channel: 'new-download-file-resp',
        data: {
          downloadError: { name: 'ABORT', code: 20, normalize: true },
          savePath: null,
          downloadFrom: d.downloadFrom || '',
          key: d.key,
          type: d.type || '',
          isCached: false,
          totalSize: 0,
        },
      };
      host.emitLocalIpcEvent('new-download-channel', [abortResp]);
      return abortResp;
    }

    // 1b. Backward-compatible flat `{ id, url, savePath, secure }` fallback
    if (reqObj.url && !reqObj.channel && !reqObj.data) {
      return handleLegacyFlatDownload(reqObj);
    }

    // 1c. Real Zalo `new-download-file` envelope
    const inner =
      reqObj.data &&
      typeof reqObj.data === 'object' &&
      (reqObj.channel === 'new-download-file' || !reqObj.url)
        ? reqObj.data
        : reqObj;

    const keyVal = inner.key;
    const downloadFrom = inner.downloadFrom || '';
    const dlType = String(inner.type || '');
    const pKey = inner.pKey;
    const dataObj = (inner.data && typeof inner.data === 'object') ? inner.data : {};
    const opts = (inner.options && typeof inner.options === 'object') ? inner.options : {};
    const url = dataObj.url;
    const rawFileName = String(
      dataObj.fileName || opts.manualFileName || 'file_' + Date.now()
    );
    const fileName = rawFileName.split(/[\\/]/).pop() || 'download';
    const defaultSaveDir = (host.VFS_USER_DATA || '/Users/olaz/Library/Application Support/ZaloPC') + '/ZaloDownloads';
    const saveDir = opts.saveDir || defaultSaveDir;
    const initialDestPath = normPath(saveDir + '/' + fileName);
    const destPath = opts.duplicate ? deduplicateVfsPath(initialDestPath) : initialDestPath;
    const wantNoise = Boolean(opts.noiseDownload) || dlType.endsWith('Noise');

    const emitResp = (downloadError, savePathVal, isCached, totalSize) => {
      const respData = {
        downloadError: downloadError || null,
        savePath: savePathVal || null,
        downloadFrom,
        key: keyVal,
        type: dlType,
        isCached: Boolean(isCached),
        totalSize: Number(totalSize) || 0,
      };
      if (pKey !== undefined) {
        respData.pKey = pKey;
      }
      const envelope = {
        channel: 'new-download-file-resp',
        data: respData,
      };
      host.emitLocalIpcEvent('new-download-channel', [envelope]);
      return envelope;
    };

    // Cache hit check (RAM + IDB fallback)
    if (!opts.duplicate) {
      const cachedEntry = await readVfsEntryWithIdbFallback(destPath);
      if (
        cachedEntry &&
        !cachedEntry.isDir &&
        cachedEntry.data &&
        cachedEntry.data.byteLength > 0
      ) {
        return emitResp(null, destPath, true, cachedEntry.data.byteLength);
      }
    }

    // Local `*Copy*` / `srcPath` fast-path
    const localSrcCandidate =
      opts.srcPath || opts.manualResourcePath || dataObj.localPath || '';
    const isCopyTransfer =
      dlType.includes('Copy') ||
      String(inner.entryFlow || '').toLowerCase() === 'copyresource' ||
      (!url && Boolean(localSrcCandidate));

    if (localSrcCandidate) {
      let srcEntry = await readVfsEntryWithIdbFallback(localSrcCandidate);
      if (srcEntry && srcEntry.isDir && fileName) {
        srcEntry = await readVfsEntryWithIdbFallback(
          normPath(localSrcCandidate + '/' + fileName)
        );
      }
      if (srcEntry && !srcEntry.isDir && srcEntry.data && srcEntry.data.byteLength > 0) {
        try {
          let srcBytes = srcEntry.data;
          if (inner.secure) {
            srcBytes = core.decryptWithSecureSpec(inner.secure, srcBytes);
          }
          const outBytes = wantNoise
            ? core.hasNoiseTrailer(srcBytes)
              ? core.cloneU8(srcBytes)
              : core.noiseBuffer(srcBytes)
            : core.denoiseBuffer(srcBytes);
          await writeAndPersistVfsFile(destPath, outBytes);
          return emitResp(null, destPath, false, outBytes.byteLength);
        } catch (copyErr) {
          return emitResp(classifyDownloadError(copyErr), null, false, 0);
        }
      }
      if (!url) {
        return emitResp(
          {
            name: isCopyTransfer
              ? 'COPY_SERVICE_SRC_NOT_FOUND'
              : 'GENERAL_UI_OBJ_DOWNLOAD_INVALID_URL',
            code: isCopyTransfer ? 1013 : 9,
            normalize: true,
          },
          null,
          false,
          0
        );
      }
    }

    if (!url || typeof url !== 'string') {
      return emitResp(
        {
          name: 'GENERAL_UI_OBJ_DOWNLOAD_INVALID_URL',
          code: 9,
          normalize: true,
        },
        null,
        false,
        0
      );
    }

    // Network download with `options.headers` + `extraHeader` (`cloud-viewer-key`)
    const fetchHeaders = {};
    if (Array.isArray(opts.headers)) {
      for (const h of opts.headers) {
        if (h && h.name && h.value !== undefined && h.value !== null) {
          fetchHeaders[String(h.name)] = String(h.value);
        }
      }
    }
    if (inner.extraHeader && typeof inner.extraHeader === 'object') {
      for (const [hk, hv] of Object.entries(inner.extraHeader)) {
        if (hk && hv !== undefined && hv !== null) {
          fetchHeaders[String(hk)] = String(hv);
        }
      }
    }

    const ctrl =
      typeof AbortController === 'function'
        ? new AbortController()
        : { signal: undefined, abort() {} };
    const abortKey = buildAbortKey(downloadFrom, dlType, keyVal);
    const tracker = {
      ctrl,
      downloadFrom,
      dlType,
      keyStr: String(keyVal ?? ''),
      abortedByRequest: false,
    };
    activeDownloads.set(abortKey, tracker);

    try {
      const encBytes = await fetchBytesWithFallback(
        url,
        { headers: fetchHeaders, signal: ctrl.signal },
        { request: inner }
      );
      if (tracker.abortedByRequest) {
        return undefined;
      }

      let outBytes = inner.secure
        ? core.decryptWithSecureSpec(inner.secure, encBytes)
        : encBytes;

      if (!outBytes || outBytes.byteLength === 0) {
        return emitResp(
          {
            name: 'DOWNLOAD_SERVICE_SINGLE_INVALID_CONTENT_LENGTH',
            code: 80002,
            normalize: true,
          },
          null,
          false,
          0
        );
      }

      if (wantNoise) {
        if (!core.hasNoiseTrailer(outBytes)) {
          outBytes = core.noiseBuffer(outBytes);
        }
      } else if (core.hasNoiseTrailer(outBytes)) {
        outBytes = core.denoiseBuffer(outBytes);
      }

      host.emitLocalIpcEvent('new-download-channel', [
        {
          channel: 'new-download-file-progress',
          data: {
            key: keyVal,
            downloadFrom,
            type: dlType,
            progress: 100,
            loaded: outBytes.byteLength,
            total: outBytes.byteLength,
          },
        },
      ]);

      await writeAndPersistVfsFile(destPath, outBytes);
      return emitResp(null, destPath, false, outBytes.byteLength);
    } catch (err) {
      if (tracker.abortedByRequest) {
        return undefined;
      }
      return emitResp(classifyDownloadError(err), null, false, 0);
    } finally {
      if (activeDownloads.get(abortKey) === tracker) {
        activeDownloads.delete(abortKey);
      }
    }
  }

  registerAsyncIpc('new-download-channel', handleNewDownloadChannel);

  registerAsyncIpc('download-thumb-temp', async (args) => {
    const rawArg = args[0];
    const url =
      rawArg && typeof rawArg === 'object' && typeof rawArg.url === 'string'
        ? rawArg.url
        : String(rawArg || '');
    if (!url) {
      throw new Error('download-thumb-temp: missing URL');
    }
    const bytes = await fetchBytesWithFallback(url, {}, { thumbTemp: true });
    const md5Hex = core.bytesToHex(core.hashBytes('md5', UTF8_ENC.encode(url)));
    const tempBase = host.VFS_TEMP || '/tmp/olaz';
    const destPath = normPath(tempBase + '/thumb_' + md5Hex + '.jpg');
    await writeAndPersistVfsFile(destPath, bytes, 'image/jpeg');
    return destPath;
  });

  // ==========================================================================
  // 2. Real `utility-process-media` (`window.$zMsgChannel` + `window.$zsub.$zMsgChannel`)
  // ==========================================================================

  async function executePhotoFsOperation(rawPayload) {
    const payload = (rawPayload && typeof rawPayload === 'object') ? rawPayload : {};
    const op = String(payload.operation || '').toUpperCase();
    const rawTarget = payload.path !== undefined ? payload.path : payload.filePath;
    const np = rawTarget ? normPath(rawTarget) : '';

    reconcileMediaRangeEpoch('photofs:' + op);

    switch (op) {
      case 'EXISTS': {
        if (!np) return false;
        const entry = await readVfsEntryWithIdbFallback(np);
        return Boolean(entry);
      }
      case 'STAT': {
        if (!np) throw makeFsError('ENOENT', '');
        const entry = await readVfsEntryWithIdbFallback(np);
        if (!entry) throw makeFsError('ENOENT', np);
        return formatStat(entry);
      }
      case 'ENSURE_DIR': {
        if (!np) return false;
        await ensureAndPersistVfsDir(np);
        return true;
      }
      case 'WRITE':
      case 'WRITE_BUFFER':
      case 'WRITE_BLOB': {
        if (!np) throw makeFsError('ENOENT', '');
        const rawData =
          payload.data !== undefined
            ? payload.data
            : payload.buffer !== undefined
              ? payload.buffer
              : payload.blob;
        const bytes = await coerceToBytes(rawData);
        await writeAndPersistVfsFile(np, bytes);
        return true;
      }
      case 'COPY': {
        const srcRaw = payload.src || payload.sourcePath;
        const dstRaw = payload.dest || payload.destinationPath;
        if (!srcRaw || !dstRaw) throw makeFsError('ENOENT', String(srcRaw || dstRaw || ''));
        const srcEntry = await readVfsEntryWithIdbFallback(srcRaw);
        if (!srcEntry || srcEntry.isDir) {
          throw makeFsError('ENOENT', normPath(srcRaw));
        }
        await writeAndPersistVfsFile(dstRaw, srcEntry.data || new Uint8Array(0), srcEntry.mime);
        return true;
      }
      case 'MOVE':
      case 'RENAME': {
        const srcRaw = payload.src || payload.sourcePath;
        const dstRaw = payload.dest || payload.destinationPath;
        if (!srcRaw || !dstRaw) throw makeFsError('ENOENT', String(srcRaw || dstRaw || ''));
        const srcNorm = normPath(srcRaw);
        const dstNorm = normPath(dstRaw);
        const srcEntry = await readVfsEntryWithIdbFallback(srcNorm);
        if (!srcEntry || srcEntry.isDir) {
          throw makeFsError('ENOENT', srcNorm);
        }
        await writeAndPersistVfsFile(dstNorm, srcEntry.data || new Uint8Array(0), srcEntry.mime);
        if (srcNorm !== dstNorm && typeof host.removeVfsEntry === 'function') {
          await host.removeVfsEntry(srcNorm);
        }
        return true;
      }
      case 'UNLINK':
      case 'REMOVE':
      case 'DELETE': {
        if (np && typeof host.removeVfsEntry === 'function') {
          await host.removeVfsEntry(np);
        } else if (np && host.vfsEntriesRam) {
          host.vfsEntriesRam.delete(np);
        }
        return true;
      }
      case 'READ_ARRAY_BUFFER': {
        if (!np) throw makeFsError('ENOENT', '');
        const entry = await readVfsEntryWithIdbFallback(np);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', np);
        const clean = core.denoiseBuffer(entry.data || new Uint8Array(0));
        return toExactArrayBuffer(clean);
      }
      case 'READ_BLOB': {
        if (!np) throw makeFsError('ENOENT', '');
        const entry = await readVfsEntryWithIdbFallback(np);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', np);
        const clean = core.denoiseBuffer(entry.data || new Uint8Array(0));
        return new Blob([clean], { type: entry.mime || guessMime(np) });
      }
      case 'READ_PARTIAL': {
        if (!np) throw makeFsError('ENOENT', '');
        const entry = await readVfsEntryWithIdbFallback(np);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', np);
        const clean = core.denoiseBuffer(entry.data || new Uint8Array(0));
        const offset = Math.max(
          0,
          Number(payload.offset !== undefined ? payload.offset : payload.start) || 0
        );
        const length =
          payload.length !== undefined
            ? Math.max(0, Number(payload.length) || 0)
            : payload.end !== undefined
              ? Math.max(0, (Number(payload.end) || 0) - offset)
              : Math.max(0, clean.byteLength - offset);
        if (length <= 0 || offset >= clean.byteLength) {
          return new ArrayBuffer(0);
        }
        return toExactArrayBuffer(clean.subarray(offset, offset + length));
      }
      default:
        throw Object.assign(
          new Error('Unsupported PHOTO_FS operation: ' + op),
          { code: 'ENOSYS' }
        );
    }
  }

  function resolveOutputMime(fmtRaw) {
    const f = String(fmtRaw || '').trim().toLowerCase();
    if (f === 'png' || f === 'image/png') return 'image/png';
    if (f === 'webp' || f === 'image/webp') return 'image/webp';
    return 'image/jpeg';
  }

  function normalizeCanvasQuality(rawQuality) {
    const n = Number(rawQuality);
    if (!Number.isFinite(n) || n <= 0) return 0.85;
    const ratio = n > 1 ? n / 100 : n;
    return Math.max(0.1, Math.min(1, ratio));
  }

  async function extractImageInputBytes(payload) {
    const p = payload || {};
    if (p.blob !== undefined && p.blob !== null) {
      return core.denoiseBuffer(await coerceToBytes(p.blob));
    }
    if (p.buffer !== undefined && p.buffer !== null) {
      return core.denoiseBuffer(await coerceToBytes(p.buffer));
    }
    if (p.data !== undefined && p.data !== null) {
      return core.denoiseBuffer(await coerceToBytes(p.data));
    }
    const pathOrBuf =
      p.path !== undefined
        ? p.path
        : p.inputPath !== undefined
          ? p.inputPath
          : p.src !== undefined
            ? p.src
            : p.localPath !== undefined
              ? p.localPath
              : p.input;
    if (typeof pathOrBuf === 'string' && pathOrBuf) {
      const entry = await readVfsEntryWithIdbFallback(pathOrBuf);
      if (!entry || entry.isDir || !entry.data) {
        throw makeFsError('ENOENT', normPath(pathOrBuf));
      }
      return core.denoiseBuffer(entry.data);
    }
    if (pathOrBuf !== undefined && pathOrBuf !== null) {
      return core.denoiseBuffer(await coerceToBytes(pathOrBuf));
    }
    throw new Error('Missing media input buffer or path');
  }

  async function decodeImageSource(cleanBytes) {
    if (!cleanBytes || cleanBytes.byteLength === 0) {
      throw new Error('Empty image input');
    }
    const blob = new Blob([cleanBytes]);
    if (typeof globalScope.createImageBitmap === 'function') {
      const bmp = await globalScope.createImageBitmap(blob);
      return {
        source: bmp,
        width: bmp.width || 1,
        height: bmp.height || 1,
        close: () => {
          if (typeof bmp.close === 'function') {
            try {
              bmp.close();
            } catch {}
          }
        },
      };
    }
    const ImgCtor = winRoot.Image;
    if (typeof ImgCtor === 'function') {
      const hasObjUrl =
        typeof URL !== 'undefined' && typeof URL.createObjectURL === 'function';
      const srcUrl = hasObjUrl
        ? URL.createObjectURL(blob)
        : 'data:application/octet-stream;base64,' + core.bytesToBase64(cleanBytes, false);
      try {
        const img = await new Promise((resolve, reject) => {
          const el = new ImgCtor();
          el.onload = () => resolve(el);
          el.onerror = () => reject(new Error('Image decode failed'));
          el.src = srcUrl;
        });
        return {
          source: img,
          width: img.naturalWidth || img.width || 1,
          height: img.naturalHeight || img.height || 1,
          close: () => {},
        };
      } finally {
        if (hasObjUrl && typeof URL.revokeObjectURL === 'function') {
          try {
            URL.revokeObjectURL(srcUrl);
          } catch {}
        }
      }
    }
    throw new Error('Image decoder is unavailable');
  }

  async function renderDecodedImage(decoded, spec) {
    const sw = Math.max(1, decoded.width || 1);
    const sh = Math.max(1, decoded.height || 1);
    const reqW = Number(spec.width || spec.maxWidth) || 0;
    const reqH = Number(spec.height || spec.maxHeight) || 0;
    const fit = String(spec.fit || 'inside').toLowerCase();

    let dw = sw;
    let dh = sh;
    let sx = 0;
    let sy = 0;
    let sWidth = sw;
    let sHeight = sh;

    if (reqW > 0 || reqH > 0) {
      const tw = reqW > 0 ? reqW : sw;
      const th = reqH > 0 ? reqH : sh;
      if (fit === 'fill' && reqW > 0 && reqH > 0) {
        dw = Math.max(1, Math.round(tw));
        dh = Math.max(1, Math.round(th));
      } else if (fit === 'cover' && reqW > 0 && reqH > 0) {
        dw = Math.max(1, Math.round(tw));
        dh = Math.max(1, Math.round(th));
        const scale = Math.max(dw / sw, dh / sh);
        sWidth = Math.min(sw, dw / scale);
        sHeight = Math.min(sh, dh / scale);
        sx = Math.max(0, (sw - sWidth) / 2);
        sy = Math.max(0, (sh - sHeight) / 2);
      } else {
        const ratioW = reqW > 0 ? reqW / sw : Infinity;
        const ratioH = reqH > 0 ? reqH / sh : Infinity;
        const scale = Math.min(1, ratioW, ratioH);
        dw = Math.max(1, Math.round(sw * scale));
        dh = Math.max(1, Math.round(sh * scale));
      }
    }

    const mime = resolveOutputMime(spec.format || spec.outputFormat);
    const quality = normalizeCanvasQuality(spec.quality);

    let outBytes = null;
    if (
      winRoot.document &&
      typeof winRoot.document.createElement === 'function'
    ) {
      const canvas = winRoot.document.createElement('canvas');
      canvas.width = dw;
      canvas.height = dh;
      const ctx = canvas.getContext && canvas.getContext('2d');
      if (ctx) {
        ctx.drawImage(decoded.source, sx, sy, sWidth, sHeight, 0, 0, dw, dh);
        if (typeof canvas.toBlob === 'function') {
          const outBlob = await new Promise((resolve, reject) => {
            canvas.toBlob(
              (b) => (b ? resolve(b) : reject(new Error('canvas.toBlob returned null'))),
              mime,
              quality
            );
          });
          outBytes = new Uint8Array(await outBlob.arrayBuffer());
        } else if (typeof canvas.toDataURL === 'function') {
          const dataUrl = canvas.toDataURL(mime, quality);
          const comma = dataUrl.indexOf(',');
          outBytes =
            comma >= 0
              ? core.decodeBase64Forgiving(dataUrl.slice(comma + 1))
              : new Uint8Array(0);
        }
      }
    }

    if (!outBytes && typeof globalScope.OffscreenCanvas === 'function') {
      const offscreen = new globalScope.OffscreenCanvas(dw, dh);
      const ctx = offscreen.getContext('2d');
      if (!ctx) throw new Error('2D canvas context unavailable');
      ctx.drawImage(decoded.source, sx, sy, sWidth, sHeight, 0, 0, dw, dh);
      const outBlob = await offscreen.convertToBlob({ type: mime, quality });
      outBytes = new Uint8Array(await outBlob.arrayBuffer());
    }

    if (!outBytes) {
      throw new Error('Canvas image encoder is unavailable');
    }

    const dest = spec.destinationPath || spec.outputPath || '';
    if (dest) {
      await writeAndPersistVfsFile(dest, outBytes, mime);
    }
    return {
      data: toExactArrayBuffer(outBytes),
      size: outBytes.byteLength,
      outputPath: dest ? normPath(dest) : '',
      destinationPath: dest ? normPath(dest) : '',
      width: dw,
      height: dh,
    };
  }

  async function executeResizeOrDecodeTask(rawPayload) {
    const payload = (rawPayload && typeof rawPayload === 'object') ? rawPayload : {};
    const srcBytes = await extractImageInputBytes(payload);
    const decoded = await decodeImageSource(srcBytes);
    try {
      const multiList = Array.isArray(payload.outputs)
        ? payload.outputs
        : Array.isArray(payload.tasks)
          ? payload.tasks
          : Array.isArray(payload.sizes)
            ? payload.sizes
            : null;
      if (multiList) {
        const results = [];
        const specs = multiList.length > 0 ? multiList : [{}];
        for (const item of specs) {
          const merged = Object.assign(
            {
              width: payload.width,
              height: payload.height,
              fit: payload.fit,
              format: payload.format || payload.outputFormat,
              quality: payload.quality,
            },
            item || {}
          );
          results.push(await renderDecodedImage(decoded, merged));
        }
        return { results, data: results, status_code: 1 };
      }
      return await renderDecodedImage(decoded, payload);
    } finally {
      decoded.close();
    }
  }

  function createUtilityMediaPort() {
    const ch = new MessageChannel();
    const hostPort = ch.port2;

    const onMessage = async (ev) => {
      const msg = ev && ev.data;
      if (!msg || typeof msg !== 'object' || msg.messageType === 'response') {
        return;
      }
      const msgType = String(msg.type || '');
      const payload =
        msg.params && typeof msg.params === 'object' && msg.params.payload !== undefined
          ? msg.params.payload
          : msg.params || {};

      try {
        let result;
        if (msgType === 'PING') {
          result = 'pong';
        } else if (msgType === 'PHOTO_FS') {
          result = await executePhotoFsOperation(payload);
        } else if (msgType === 'RESIZE' || msgType === 'DECODE') {
          result = await executeResizeOrDecodeTask(payload);
        } else if (
          msgType === 'GET_JXL_INFO' ||
          msgType === 'DECODE_JXL_SINGLE' ||
          msgType === 'RESIZE_JXL' ||
          msgType === 'ENCODE_JXL'
        ) {
          throw Object.assign(
            new Error(msgType + ' is not supported; use JPEG/WebP fallback'),
            { code: 'ENOTSUP' }
          );
        } else {
          throw Object.assign(
            new Error('Unsupported utility-process-media task: ' + msgType),
            { code: 'ENOTSUP' }
          );
        }

        const xfer = result instanceof ArrayBuffer ? [result] : [];
        hostPort.postMessage(
          {
            id: msg.id,
            type: msg.type,
            connectionId: msg.connectionId,
            params: msg.params,
            checkpoint: msg.checkpoint,
            messageType: 'response',
            result,
            priority: msg.priority,
            addToFront: msg.addToFront,
          },
          xfer
        );
      } catch (err) {
        hostPort.postMessage({
          id: msg.id,
          type: msg.type,
          connectionId: msg.connectionId,
          params: msg.params,
          checkpoint: msg.checkpoint,
          messageType: 'response',
          error: {
            message: String((err && err.message) || err),
            code: (err && err.code) || 'ERR_MEDIA_UTILITY',
          },
          priority: msg.priority,
          addToFront: msg.addToFront,
        });
      }
    };

    if (typeof hostPort.addEventListener === 'function') {
      hostPort.addEventListener('message', onMessage);
    } else {
      hostPort.onmessage = onMessage;
    }
    if (typeof hostPort.start === 'function') hostPort.start();
    if (typeof ch.port1.start === 'function') ch.port1.start();
    if (typeof ch.port1.unref === 'function') ch.port1.unref();
    if (typeof ch.port2.unref === 'function') ch.port2.unref();
    return ch.port1;
  }

  {
    const portV2Cbs = new Set();
    const portV1Cbs = new Set();
    const establishedCbs = new Set();
    const disconnectedCbs = new Set();
    const terminatedCbs = new Set();
    let msgChannelSessionSeq = 0;

    const makeSub = (set) => (cb) => {
      if (typeof cb === 'function') set.add(cb);
      return () => set.delete(cb);
    };
    const makeUnsub = (set) => (cb) => {
      if (typeof cb === 'function') set.delete(cb);
      else set.clear();
    };

    const mediaMsgChannelApi = {
      onReceiveTransferredPortV2: makeSub(portV2Cbs),
      removeListenReceiveTransferredPortV2: makeUnsub(portV2Cbs),
      onReceiveTransferredPort: makeSub(portV1Cbs),
      removeListenReceiveTransferredPort: makeUnsub(portV1Cbs),
      onChannelEstablished: makeSub(establishedCbs),
      removeListenChannelEstablished: makeUnsub(establishedCbs),
      onChannelDisconnected: makeSub(disconnectedCbs),
      removeListenChannelDisconnected: makeUnsub(disconnectedCbs),
      onChannelTerminated: makeSub(terminatedCbs),
      removeListenChannelTerminated: makeUnsub(terminatedCbs),
      requestOpenChannelTo: (fromScript, targetScript, channelID) => {
        if (!targetScript || targetScript === 'utility-process-media') {
          const port1 = createUtilityMediaPort();
          const sessionId = ++msgChannelSessionSeq;
          setTimeout(() => {
            const meta = {
              nameOfCurrentScript: fromScript,
              nameOfTargetScript: targetScript || 'utility-process-media',
              channelID,
              channelSessionID: sessionId,
              isReconnect: false,
            };
            for (const cb of Array.from(portV2Cbs)) {
              try {
                cb({ ports: [port1] }, meta);
              } catch {}
            }
            for (const cb of Array.from(portV1Cbs)) {
              try {
                cb({ ports: [port1] }, meta);
              } catch {}
            }
            if (typeof port1.unref === 'function') port1.unref();
          }, 0);
        }
        return Promise.resolve();
      },
      notifyPortDelivered: (fromScript, targetScript, channelID) => {
        if (!targetScript || targetScript === 'utility-process-media') {
          setTimeout(() => {
            for (const cb of Array.from(establishedCbs)) {
              try {
                cb({}, [fromScript, targetScript || 'utility-process-media', channelID]);
              } catch {}
            }
          }, 0);
        }
        return Promise.resolve();
      },
      createUtilityMediaPort,
    };

    if (winRoot.$zMsgChannel && typeof winRoot.$zMsgChannel === 'object') {
      Object.assign(winRoot.$zMsgChannel, mediaMsgChannelApi);
    } else {
      winRoot.$zMsgChannel = mediaMsgChannelApi;
    }

    const zsubMsgChannelProxy = new Proxy(Object.assign({}, mediaMsgChannelApi), {
      get(target, prop) {
        return Object.prototype.hasOwnProperty.call(mediaMsgChannelApi, prop)
          ? mediaMsgChannelApi[prop]
          : target[prop];
      },
      set(target, prop, val) {
        if (!Object.prototype.hasOwnProperty.call(mediaMsgChannelApi, prop)) {
          target[prop] = val;
        }
        return true;
      },
    });
    const attachZSubMsgChannel = (subObj) => {
      const base = subObj && typeof subObj === 'object' ? subObj : {};
      try {
        Object.defineProperty(base, '$zMsgChannel', {
          configurable: true,
          enumerable: true,
          get: () => zsubMsgChannelProxy,
          set: () => {},
        });
      } catch {
        base.$zMsgChannel = zsubMsgChannelProxy;
      }
      return base;
    };

    let currentZSub = attachZSubMsgChannel(winRoot.$zsub || {});
    try {
      Object.defineProperty(winRoot, '$zsub', {
        configurable: true,
        enumerable: true,
        get: () => currentZSub,
        set: (nextVal) => {
          currentZSub = attachZSubMsgChannel(nextVal);
        },
      });
    } catch {
      winRoot.$zsub = currentZSub;
    }
  }

  // ==========================================================================
  // 3. `$zcloud.*`, `$zFileManager.*`, `$zpc.zfile.*`, & Browser Video Thumbnail Generator
  // ==========================================================================

  async function resolveVideoThumbSrc(inp) {
    const raw = String(inp || '').trim();
    if (!raw) return { url: '', revoke: null };

    const isLocalPath =
      (raw.startsWith('/') &&
        !raw.startsWith('//') &&
        !raw.startsWith('/__zfile/') &&
        !raw.startsWith('/__olaz/')) ||
      /^(zfile:|file:)/i.test(raw);

    if (isLocalPath) {
      const localPath = raw
        .replace(/^zfile:\/+(local\/)?/i, '/')
        .replace(/^file:\/+/i, '/');
      const vfsEntry = await readVfsEntryWithIdbFallback(localPath);
      if (
        vfsEntry &&
        !vfsEntry.isDir &&
        vfsEntry.data &&
        vfsEntry.data.byteLength > 0 &&
        typeof URL !== 'undefined' &&
        typeof URL.createObjectURL === 'function' &&
        typeof Blob !== 'undefined'
      ) {
        const clean = core.denoiseBuffer(vfsEntry.data);
        const blobUrl = URL.createObjectURL(
          new Blob([clean], { type: vfsEntry.mime || guessMime(localPath) })
        );
        return {
          url: blobUrl,
          revoke: () => {
            try {
              URL.revokeObjectURL(blobUrl);
            } catch {}
          },
        };
      }
      if (raw.startsWith('/')) {
        return { url: rewriteLocalPathToZfile(raw), revoke: null };
      }
    }

    return { url: rewriteMediaUrl(raw), revoke: null };
  }

  async function generateVideoThumbnailToVfs(inp, outPath, maxW = 480, maxH = 480) {
    if (!inp || !winRoot.document || typeof winRoot.document.createElement !== 'function') {
      return '';
    }
    advanceMediaCursorWindow(3);

    const { url: resolvedSrc, revoke } = await resolveVideoThumbSrc(inp);
    if (!resolvedSrc) {
      if (revoke) revoke();
      return '';
    }

    const frameResult = await new Promise((resolve) => {
      const video = winRoot.document.createElement('video');
      video.__olazInternalThumb = true;
      if (video.dataset) {
        video.dataset.olazIgnoreMedia = 'true';
      }
      let settled = false;
      const finish = (payload) => {
        if (settled) return;
        settled = true;
        try {
          video.pause();
          video.removeAttribute('src');
          if (typeof video.load === 'function') video.load();
        } catch {}
        if (revoke) revoke();
        resolve(payload);
      };

      const timer = setTimeout(() => finish(null), 6000);
      if (timer && typeof timer.unref === 'function') timer.unref();

      const captureFrame = () => {
        clearTimeout(timer);
        try {
          const vw = Number(video.videoWidth) || 320;
          const vh = Number(video.videoHeight) || 240;
          const limW = Math.max(1, Number(maxW) || 480);
          const limH = Math.max(1, Number(maxH) || 480);
          const scale = Math.min(1, limW / vw, limH / vh);
          const cw = Math.max(1, Math.round(vw * scale));
          const ch = Math.max(1, Math.round(vh * scale));
          const canvas = winRoot.document.createElement('canvas');
          canvas.width = cw;
          canvas.height = ch;
          const ctx = canvas.getContext('2d');
          if (!ctx) {
            finish(null);
            return;
          }
          ctx.drawImage(video, 0, 0, cw, ch);
          if (typeof canvas.toBlob === 'function') {
            canvas.toBlob(
              async (blob) => {
                if (!blob) {
                  finish(null);
                  return;
                }
                try {
                  const bytes = new Uint8Array(await blob.arrayBuffer());
                  finish({ bytes });
                } catch {
                  finish(null);
                }
              },
              'image/jpeg',
              0.82
            );
            return;
          }
          const dataUrl = canvas.toDataURL('image/jpeg', 0.82);
          const comma = dataUrl.indexOf(',');
          if (comma > 0) {
            finish({
              bytes: core.decodeBase64Forgiving(dataUrl.slice(comma + 1)),
              dataUrl,
            });
          } else {
            finish(null);
          }
        } catch {
          finish(null);
        }
      };

      video.muted = true;
      video.playsInline = true;
      video.preload = 'metadata';
      video.crossOrigin = 'anonymous';
      video.onerror = () => {
        clearTimeout(timer);
        finish(null);
      };
      video.onloadedmetadata = () => {
        try {
          const dur = Number(video.duration);
          const targetSec = Math.min(
            1,
            Math.max(0.05, (Number.isFinite(dur) && dur > 0 ? dur : 0) * 0.1)
          );
          video.currentTime = targetSec;
        } catch {
          captureFrame();
        }
      };
      video.onseeked = captureFrame;
      video.src = resolvedSrc;
    });

    if (!frameResult || !frameResult.bytes || frameResult.bytes.byteLength === 0) {
      return '';
    }
    if (outPath) {
      const normOut = normPath(outPath);
      await writeAndPersistVfsFile(normOut, frameResult.bytes, 'image/jpeg');
      return normOut;
    }
    return (
      frameResult.dataUrl ||
      'data:image/jpeg;base64,' + core.bytesToBase64(frameResult.bytes, false)
    );
  }

  registerAsyncIpc('denoise-file', async (args) => {
    const a0 = args[0];
    const isObj = Boolean(a0 && typeof a0 === 'object' && !Array.isArray(a0));
    const src = isObj ? a0.src || a0.filePath : a0;
    const dest = isObj ? a0.dest || a0.outputPath : args[1];
    const expectedMd5 = isObj ? a0.checksum || a0.expectedMd5 : args[2];

    if (!src || !dest) {
      throw makeFsError('ENOENT', String(src || dest || ''));
    }
    const entry = await readVfsEntryWithIdbFallback(src);
    if (!entry || entry.isDir || !entry.data) {
      throw makeFsError('ENOENT', normPath(src));
    }
    const clean = core.denoiseBuffer(entry.data);
    if (typeof expectedMd5 === 'string' && expectedMd5.trim()) {
      const actualMd5 = core.bytesToHex(core.hashBytes('md5', clean));
      if (actualMd5.toLowerCase() !== expectedMd5.trim().toLowerCase()) {
        throw Object.assign(
          new Error('MD5 checksum mismatch for ' + normPath(src)),
          { code: 'EBADMSG' }
        );
      }
    }
    const normDest = normPath(dest);
    await writeAndPersistVfsFile(normDest, clean, entry.mime);
    return { savePath: normDest };
  });

  registerAsyncIpc('noise-file', async (args) => {
    const a0 = args[0];
    const isObj = Boolean(a0 && typeof a0 === 'object' && !Array.isArray(a0));
    const src = isObj ? a0.src || a0.filePath : a0;
    let dest = isObj ? a0.dest || a0.outputPath : args[1];
    if (!dest && isObj && a0.userId && a0.checksum) {
      const base = host.VFS_USER_DATA || '/Users/olaz/Library/Application Support/ZaloPC';
      const sub = a0.isGroup ? 'group/' : '';
      dest = base + '/ZaloDownloads/' + a0.userId + '/fileNoise/' + sub + a0.checksum;
    }
    if (!src || !dest) {
      throw makeFsError('ENOENT', String(src || dest || ''));
    }
    const entry = await readVfsEntryWithIdbFallback(src);
    if (!entry || entry.isDir || !entry.data) {
      throw makeFsError('ENOENT', normPath(src));
    }
    const noised = core.hasNoiseTrailer(entry.data)
      ? core.cloneU8(entry.data)
      : core.noiseBuffer(entry.data);
    const normDest = normPath(dest);
    await writeAndPersistVfsFile(normDest, noised, entry.mime);
    return { savePath: normDest };
  });

  registerAsyncIpc('write-blob-to-file', async (args) => {
    const a0 = args[0];
    let dest;
    let data;
    if (
      a0 &&
      typeof a0 === 'object' &&
      !Array.isArray(a0) &&
      !(typeof Blob !== 'undefined' && a0 instanceof Blob) &&
      !(a0 instanceof ArrayBuffer) &&
      !ArrayBuffer.isView(a0) &&
      args.length === 1
    ) {
      dest = a0.outputPath || a0.dest || a0.filePath || a0.path;
      data = a0.buffer !== undefined ? a0.buffer : a0.data !== undefined ? a0.data : a0.blob;
    } else if (typeof a0 !== 'string' && typeof args[1] === 'string') {
      data = a0;
      dest = args[1];
    } else {
      dest = a0;
      data = args[1];
    }
    if (!dest) throw makeFsError('ENOENT', '');
    const bytes = await coerceToBytes(data);
    await writeAndPersistVfsFile(dest, bytes);
    return true;
  });

  registerAsyncIpc('append-file', async (args) => {
    const a0 = args[0];
    const isObj =
      Boolean(a0 && typeof a0 === 'object' && !Array.isArray(a0)) && args.length === 1;
    const dest = isObj ? a0.dest || a0.filePath || a0.path : a0;
    const rawData = isObj
      ? a0.data !== undefined
        ? a0.data
        : a0.content !== undefined
          ? a0.content
          : a0.buffer
      : args[1];
    if (!dest) return false;
    const np = normPath(dest);
    const prev = await readVfsEntryWithIdbFallback(np);
    const chunk = await coerceToBytes(rawData);
    const combined =
      prev && !prev.isDir && prev.data ? core.concatU8([prev.data, chunk]) : chunk;
    await writeAndPersistVfsFile(np, combined, prev && prev.mime);
    return true;
  });

  registerAsyncIpc('remove-file', async (args) => {
    const a0 = args[0];
    const target =
      a0 && typeof a0 === 'object' ? a0.filePath || a0.path : a0;
    if (!target) return true;
    const np = normPath(target);
    if (host.vfsEntriesRam) {
      for (const key of Array.from(host.vfsEntriesRam.keys())) {
        if (key === np || key.startsWith(np + '/')) {
          if (typeof host.removeVfsEntry === 'function') {
            await host.removeVfsEntry(key);
          } else {
            host.vfsEntriesRam.delete(key);
          }
        }
      }
    }
    if (typeof host.removeVfsEntry === 'function') {
      await host.removeVfsEntry(np);
    }
    return true;
  });

  registerAsyncIpc('match-checksum', async (args) => {
    const a0 = args[0];
    const isObj = Boolean(a0 && typeof a0 === 'object' && !Array.isArray(a0));
    const filePath = isObj ? a0.filePath || a0.path : a0;
    const expected = isObj ? a0.checksum || a0.expected : args[1];
    const checkNoise = isObj ? Boolean(a0.checkNoise) : Boolean(args[2]);

    if (!filePath || !expected) return false;
    const entry = await readVfsEntryWithIdbFallback(filePath);
    if (!entry || entry.isDir || !entry.data) return false;
    const bytes = checkNoise ? core.denoiseBuffer(entry.data) : entry.data;
    const actual = core.bytesToHex(core.hashBytes('md5', bytes));
    return actual.toLowerCase() === String(expected).trim().toLowerCase();
  });

  registerAsyncIpc('clone-and-match-checksum', async (args) => {
    const a0 = args[0];
    let expected = '';
    let src = '';
    if (a0 && typeof a0 === 'object' && !Array.isArray(a0)) {
      src = String(a0.filePath || a0.src || a0.path || '');
      expected = String(a0.checksum || a0.expected || '');
    } else {
      const s0 = String(a0 ?? '');
      const s1 = String(args[1] ?? '');
      const s0LooksLikePath =
        s0.includes('/') || s0.includes('\\') || /^(zfile:|file:)/i.test(s0);
      const s1LooksLikePath =
        s1.includes('/') || s1.includes('\\') || /^(zfile:|file:)/i.test(s1);
      if (s0LooksLikePath && !s1LooksLikePath) {
        src = s0;
        expected = s1;
      } else {
        expected = s0;
        src = s1;
      }
    }
    if (!src) return '';
    const entry = await readVfsEntryWithIdbFallback(src);
    if (!entry || entry.isDir || !entry.data) return '';
    const clean = core.denoiseBuffer(entry.data);
    const actual = core.bytesToHex(core.hashBytes('md5', clean));
    if (expected && actual.toLowerCase() !== expected.trim().toLowerCase()) {
      return '';
    }
    const normSrc = normPath(src);
    const slash = normSrc.lastIndexOf('/');
    const dir = slash <= 0 ? '' : normSrc.slice(0, slash);
    const base = normSrc.slice(slash + 1);
    const dot = base.lastIndexOf('.');
    const stem = dot > 0 ? base.slice(0, dot) : base || 'clone';
    const ext = dot > 0 ? base.slice(dot) : '';
    const tempPath = normPath((dir || '') + '/.' + stem + '_tmp' + ext);
    await writeAndPersistVfsFile(tempPath, clean, entry.mime);
    return tempPath;
  });

  registerAsyncIpc('can-store-big-file', async () => true);
  if (host.syncIpcHandlers && typeof host.syncIpcHandlers.set === 'function') {
    host.syncIpcHandlers.set('can-store-big-file', () => true);
  }

  registerAsyncIpc('video-thumb-generate', async (args) => {
    const a0 = args[0];
    if (a0 && typeof a0 === 'object' && !Array.isArray(a0)) {
      return generateVideoThumbnailToVfs(
        a0.src || a0.inputPath || a0.path,
        a0.dest || a0.outputPath,
        a0.maxWidth || a0.mw || 480,
        a0.maxHeight || a0.mh || 480
      );
    }
    return generateVideoThumbnailToVfs(args[0], args[1], args[2], args[3]);
  });

  registerAsyncIpc('gett-heap', async () =>
    (typeof performance !== 'undefined' &&
      performance.memory &&
      performance.memory.usedJSHeapSize) ||
    0
  );
  registerAsyncIpc('write-heap', async () => undefined);

  // Extend `window.$zFileManager` (works transparently even when wrapped in `wrapBridgeObject` Proxy)
  {
    if (!winRoot.$zFileManager || typeof winRoot.$zFileManager !== 'object') {
      winRoot.$zFileManager = {};
    }
    const fm = winRoot.$zFileManager;
    const fmExtensions = {
      getPhotoWithPaths: async (paths) => {
        const out = {};
        if (!Array.isArray(paths)) return out;
        for (const p of paths) {
          if (typeof p !== 'string' || !p) continue;
          const entry = await readVfsEntryWithIdbFallback(p);
          if (entry && !entry.isDir && entry.data) {
            out[p] = core.denoiseBuffer(entry.data);
          }
        }
        return out;
      },
      getFileBlob: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        const denoised = core.denoiseBuffer(entry.data || new Uint8Array(0));
        return new Blob([denoised], { type: entry.mime || guessMime(p) });
      },
      getFileBlobURL: async (p) => {
        const blob = await winRoot.$zFileManager.getFileBlob(p);
        return URL.createObjectURL(blob);
      },
      writeBlobToPath: async (a, b) => {
        let destPath;
        let payload;
        if (typeof a === 'string' && typeof b !== 'string') {
          destPath = a;
          payload = b;
        } else if (typeof b === 'string' && typeof a !== 'string') {
          destPath = b;
          payload = a;
        } else {
          destPath = String(a || '');
          payload = b;
        }
        if (!destPath) throw makeFsError('ENOENT', '');
        const bytes = await coerceToBytes(payload);
        await writeAndPersistVfsFile(destPath, bytes);
        return true;
      },
      readChunksArrayObject: async (p, chunkSize = 65536) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        const clean = core.denoiseBuffer(entry.data || new Uint8Array(0));
        const cs = Math.max(1, Number(chunkSize) || 65536);
        const chunks = [];
        for (let off = 0; off < clean.byteLength; off += cs) {
          chunks.push(clean.slice(off, off + cs));
        }
        return chunks;
      },
      getFileArrayBuffer: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        const clean = core.denoiseBuffer(entry.data || new Uint8Array(0));
        return toExactArrayBuffer(clean);
      },
      readFileAsync: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        return core.denoiseBuffer(entry.data || new Uint8Array(0));
      },
      svcStatAsync: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        return formatStat(entry || { isDir: false, size: 0, mtimeMs: 0, data: null });
      },
      svcReadPath: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        return core.denoiseBuffer(entry.data || new Uint8Array(0));
      },
      readFileSlice: async (p, start, len, checkNoise = true) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        const u8 = checkNoise
          ? core.denoiseBuffer(entry.data || new Uint8Array(0))
          : entry.data || new Uint8Array(0);
        const s = Math.max(0, Number(start) || 0);
        const sub = u8.slice(s, s + Math.max(0, Number(len) || 0));
        return toExactArrayBuffer(sub);
      },
      zfileFindNoiseSignature: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir || !entry.data) return -1;
        const info = core.inspectBufferNoise(entry.data, true);
        return info.signatureOffset === null ? -1 : info.signatureOffset;
      },
      getMd5LargeFileObject: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        const clean = core.denoiseBuffer(entry.data || new Uint8Array(0));
        return {
          checksum: core.bytesToHex(core.hashBytes('md5', clean)),
          size: clean.byteLength,
        };
      },
      zfileGetChecksum: async (p) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir) throw makeFsError('ENOENT', normPath(p));
        const clean = core.denoiseBuffer(entry.data || new Uint8Array(0));
        return core.bytesToHex(core.hashBytes('md5', clean));
      },
      testChecksum: async (p, expected, checkNoise = true) => {
        const entry = await readVfsEntryWithIdbFallback(p);
        if (!entry || entry.isDir || !entry.data) return false;
        const bytes = checkNoise ? core.denoiseBuffer(entry.data) : entry.data;
        const actual = core.bytesToHex(core.hashBytes('md5', bytes));
        return actual.toLowerCase() === String(expected || '').trim().toLowerCase();
      },
      writeBufferToPath: async (p, buf) => {
        const bytes = await coerceToBytes(buf);
        await writeAndPersistVfsFile(p, bytes);
        return true;
      },
      appendBufferToPath: async (p, buf) => {
        const np = normPath(p);
        const prev = await readVfsEntryWithIdbFallback(np);
        const chunk = await coerceToBytes(buf);
        const combined =
          prev && !prev.isDir && prev.data ? core.concatU8([prev.data, chunk]) : chunk;
        await writeAndPersistVfsFile(np, combined, prev && prev.mime);
        return true;
      },
      zcloudDenoiseFile: async (src, dst, expectedMd5) =>
        host.asyncIpcHandlers.get('denoise-file')([src, dst, expectedMd5]),
      zcloudNoiseFile: async (src, dst) =>
        host.asyncIpcHandlers.get('noise-file')([src, dst]),
      zcloudCloneAndMatchChecksum: async (expected, src) =>
        host.asyncIpcHandlers.get('clone-and-match-checksum')([expected, src]),
      generateVideoThumbnail: (inp, out, mw, mh) =>
        generateVideoThumbnailToVfs(inp, out, mw, mh),
    };
    for (const [k, fn] of Object.entries(fmExtensions)) {
      fm[k] = fn;
    }
  }

  // Wire `window.$znode.nativelibs.mp4thumbAddon` & `mp4thumb`, `$zFeatures.genThumb`, and `$zpc.zfile`
  {
    const mp4thumbAddon = {
      getThumbnail: generateVideoThumbnailToVfs,
      generateThumbnail: generateVideoThumbnailToVfs,
      cancel: () => {},
    };
    if (!winRoot.$znode || typeof winRoot.$znode !== 'object') {
      winRoot.$znode = {};
    }
    if (!winRoot.$znode.nativelibs || typeof winRoot.$znode.nativelibs !== 'object') {
      winRoot.$znode.nativelibs = {};
    }
    winRoot.$znode.nativelibs.mp4thumbAddon = mp4thumbAddon;
    winRoot.$znode.nativelibs.mp4thumb = () => mp4thumbAddon;

    if (winRoot.$zFeatures && typeof winRoot.$zFeatures === 'object') {
      const prevGenThumb = Object.prototype.hasOwnProperty.call(winRoot.$zFeatures, 'genThumb')
        ? winRoot.$zFeatures.genThumb
        : null;
      const upgradedGenThumb = (p) => generateVideoThumbnailToVfs(p, undefined, 480, 480);
      if (prevGenThumb && typeof prevGenThumb.writeSavePathFilePhoto === 'function') {
        upgradedGenThumb.writeSavePathFilePhoto = prevGenThumb.writeSavePathFilePhoto;
      }
      winRoot.$zFeatures.genThumb = upgradedGenThumb;
    }

    if (!winRoot.$zpc || typeof winRoot.$zpc !== 'object') {
      winRoot.$zpc = {};
    }
    const existingZfile =
      Object.prototype.hasOwnProperty.call(winRoot.$zpc, 'zfile') &&
      winRoot.$zpc.zfile &&
      typeof winRoot.$zpc.zfile === 'object'
        ? winRoot.$zpc.zfile
        : {};
    winRoot.$zpc.zfile = Object.assign({}, existingZfile, {
      isTrustPath: async () => true,
      getChecksum: (p) => winRoot.$zFileManager.zfileGetChecksum(p),
      compareChecksum: (p, exp) => winRoot.$zFileManager.testChecksum(p, exp),
      findNoiseSignature: (p) => winRoot.$zFileManager.zfileFindNoiseSignature(p),
    });
  }

  // ==========================================================================
  // 5. `popup-viewer.html` (`MediaViewer`) Portal Lifecycle & Mobile/PWA Fallback
  // ==========================================================================

  let activePopupCloser = null;

  function isPopupViewerTarget(rawUrl) {
    if (typeof rawUrl !== 'string' || !rawUrl) return false;
    try {
      const base = (winRoot.location && winRoot.location.href) || 'http://localhost/';
      const parsed = new URL(rawUrl, base);
      return /\/popup-viewer\.html$/i.test(parsed.pathname);
    } catch {
      return rawUrl.includes('popup-viewer.html');
    }
  }

  function runPopupWindowHooks(childWin, finalUrl, target, features) {
    if (!childWin || !Array.isArray(host.popupWindowHooks)) return;
    for (const hook of host.popupWindowHooks) {
      if (typeof hook === 'function') {
        try {
          hook(childWin, finalUrl, target, features);
        } catch {}
      }
    }
  }

  function wrapPopupLifecycle(childWin, expectedPathSuffix) {
    if (!childWin || childWin.__olazPopupLifecycleWrapped) return;
    try {
      Object.defineProperty(childWin, '__olazPopupLifecycleWrapped', {
        value: true,
        configurable: true,
      });
    } catch {
      childWin.__olazPopupLifecycleWrapped = true;
    }

    installWindowUrlRewriters(childWin);

    const dclCbs = new Set();
    const loadCbs = new Set();
    const removedCbs = new WeakSet();
    const wrapMap = new WeakMap();
    let dclFired = false;
    let loadFired = false;

    const isCommittedTargetDoc = () => {
      try {
        const loc = childWin.location;
        const pathname = String((loc && loc.pathname) || '');
        const href = String((loc && loc.href) || '');
        if (!href || href === 'about:blank') return false;
        if (expectedPathSuffix && !pathname.endsWith(expectedPathSuffix)) return false;
        return Boolean(childWin.document && childWin.document.body);
      } catch {
        return false;
      }
    };

    const ensureChildPatched = () => {
      try {
        installWindowUrlRewriters(childWin);
      } catch {}
    };

    const makeSyntheticEvent = (evName) => {
      try {
        const EvCtor = childWin.Event || winRoot.Event;
        return typeof EvCtor === 'function' ? new EvCtor(evName) : { type: evName };
      } catch {
        return { type: evName };
      }
    };

    try {
      if (typeof childWin.addEventListener === 'function') {
        const origAdd = childWin.addEventListener.bind(childWin);
        const origRem =
          typeof childWin.removeEventListener === 'function'
            ? childWin.removeEventListener.bind(childWin)
            : null;

        childWin.addEventListener = function (type, cb, opts) {
          if (typeof cb === 'function' && (type === 'DOMContentLoaded' || type === 'load')) {
            const committed = isCommittedTargetDoc();
            const rs = committed && childWin.document ? childWin.document.readyState : 'loading';
            const isAlreadyReady =
              committed &&
              (type === 'DOMContentLoaded'
                ? rs === 'interactive' || rs === 'complete' || dclFired
                : rs === 'complete' || loadFired);

            if (isAlreadyReady) {
              ensureChildPatched();
              if (type === 'DOMContentLoaded') dclFired = true;
              else loadFired = true;
              setTimeout(() => {
                if (removedCbs.has(cb)) return;
                ensureChildPatched();
                try {
                  cb.call(childWin, makeSyntheticEvent(type));
                } catch {}
              }, 0);
              return undefined;
            }

            const targetSet = type === 'DOMContentLoaded' ? dclCbs : loadCbs;
            const guarded = function (ev) {
              if (!isCommittedTargetDoc() || !targetSet.has(cb)) {
                return undefined;
              }
              targetSet.delete(cb);
              ensureChildPatched();
              if (type === 'DOMContentLoaded') dclFired = true;
              else loadFired = true;
              return cb.call(this, ev);
            };
            wrapMap.set(cb, guarded);
            targetSet.add(cb);
            return origAdd(type, guarded, opts);
          }
          return origAdd(type, cb, opts);
        };

        if (origRem) {
          childWin.removeEventListener = function (type, cb, opts) {
            if (cb && (typeof cb === 'function' || typeof cb === 'object')) {
              removedCbs.add(cb);
            }
            if (type === 'DOMContentLoaded') dclCbs.delete(cb);
            else if (type === 'load') loadCbs.delete(cb);
            return origRem(type, wrapMap.get(cb) || cb, opts);
          };
        }
      }
    } catch {}

    const drainCallbackSet = (set, evName) => {
      ensureChildPatched();
      const pending = Array.from(set);
      set.clear();
      for (const fn of pending) {
        if (removedCbs.has(fn)) continue;
        try {
          fn.call(childWin, makeSyntheticEvent(evName));
        } catch {}
      }
    };

    const checkAndFlushReady = () => {
      try {
        if (childWin.closed) return true;
        if (!isCommittedTargetDoc()) return false;
        ensureChildPatched();
        const rs = childWin.document.readyState;
        if (!dclFired && (rs === 'interactive' || rs === 'complete')) {
          dclFired = true;
          drainCallbackSet(dclCbs, 'DOMContentLoaded');
        }
        if (!loadFired && rs === 'complete') {
          loadFired = true;
          drainCallbackSet(loadCbs, 'load');
          return true;
        }
      } catch {}
      return false;
    };

    let ticks = 0;
    const pollTimer = setInterval(() => {
      ticks += 1;
      if (ticks > 900 || checkAndFlushReady()) {
        clearInterval(pollTimer);
      }
    }, 16);
    if (pollTimer && typeof pollTimer.unref === 'function') {
      pollTimer.unref();
    }

    return { checkAndFlushReady };
  }

  function monitorChildPopupClose(childWin) {
    let closeEmitted = false;
    let committedPopupDoc = false;
    let closePoll = null;

    const emitClosedOnce = () => {
      if (closeEmitted) return;
      closeEmitted = true;
      if (closePoll) {
        clearInterval(closePoll);
        closePoll = null;
      }
      if (activePopupCloser === closeNow) {
        activePopupCloser = null;
      }
      let winId = 'photo-viewer';
      try {
        if (childWin && childWin.__zWindowId) {
          winId = childWin.__zWindowId;
        }
      } catch {}
      host.emitLocalIpcEvent('close-media-viewer', [winId]);
    };

    const closeNow = () => {
      try {
        if (childWin && !childWin.closed && typeof origChildClose === 'function') {
          origChildClose.call(childWin);
        }
      } catch {}
      emitClosedOnce();
    };

    let origChildClose = null;
    try {
      if (typeof childWin.close === 'function') {
        origChildClose = childWin.close;
        childWin.close = function () {
          const r = origChildClose.apply(this, arguments);
          setTimeout(emitClosedOnce, 0);
          return r;
        };
      }
    } catch {}

    const attachPageHideOnceCommitted = () => {
      if (committedPopupDoc) return;
      try {
        if (
          childWin.location &&
          String(childWin.location.pathname || '').endsWith('/popup-viewer.html')
        ) {
          committedPopupDoc = true;
          if (typeof childWin.addEventListener === 'function') {
            childWin.addEventListener('pagehide', () => {
              const phTimer = setTimeout(() => {
                try {
                  if (childWin.closed) emitClosedOnce();
                } catch {
                  emitClosedOnce();
                }
              }, 60);
              if (phTimer && typeof phTimer.unref === 'function') phTimer.unref();
            });
          }
        }
      } catch {}
    };

    closePoll = setInterval(() => {
      try {
        if (childWin.closed) {
          clearInterval(closePoll);
          closePoll = null;
          emitClosedOnce();
          return;
        }
        attachPageHideOnceCommitted();
      } catch {
        clearInterval(closePoll);
        closePoll = null;
        emitClosedOnce();
      }
    }, 200);
    if (closePoll && typeof closePoll.unref === 'function') {
      closePoll.unref();
    }

    activePopupCloser = closeNow;
  }

  function openInPagePopupViewerOverlay(finalUrl, target, features) {
    advanceMediaCursorWindow(7);
    if (!winRoot.document) return null;

    if (typeof activePopupCloser === 'function') {
      try {
        activePopupCloser();
      } catch {}
    }
    const existing =
      typeof winRoot.document.getElementById === 'function'
        ? winRoot.document.getElementById('__olaz-popup-viewer-overlay')
        : null;
    if (existing && typeof existing.remove === 'function') {
      existing.remove();
    }

    const overlay = winRoot.document.createElement('div');
    overlay.id = '__olaz-popup-viewer-overlay';
    overlay.style.cssText =
      'position:fixed;inset:0;z-index:2147483645;background:#0b0f17;display:flex;flex-direction:column;overscroll-behavior:contain;';

    const closeBtn = winRoot.document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.setAttribute('aria-label', 'Close media preview');
    closeBtn.textContent = '\u00d7';
    closeBtn.style.cssText =
      'position:fixed;top:max(12px,env(safe-area-inset-top));right:max(12px,env(safe-area-inset-right));' +
      'z-index:2147483646;width:44px;height:44px;border-radius:22px;' +
      'border:1px solid rgba(255,255,255,0.18);background:rgba(15,23,42,0.82);color:#f8fafc;' +
      'font-size:26px;line-height:1;display:flex;align-items:center;justify-content:center;' +
      'cursor:pointer;padding:0;touch-action:manipulation;';

    const iframe = winRoot.document.createElement('iframe');
    iframe.src = finalUrl;
    iframe.style.cssText = 'width:100%;height:100%;border:0;flex:1 1 auto;background:#0b0f17;';
    if (typeof iframe.setAttribute === 'function') {
      iframe.setAttribute('allow', 'autoplay; fullscreen; clipboard-read; clipboard-write');
    }

    overlay.appendChild(iframe);
    overlay.appendChild(closeBtn);
    const mountRoot = winRoot.document.body || winRoot.document.documentElement;
    if (mountRoot && typeof mountRoot.appendChild === 'function') {
      mountRoot.appendChild(overlay);
    }

    const frameWin = iframe.contentWindow;
    let closedFlag = false;

    const onKeyDown = (ev) => {
      if (ev && ev.key === 'Escape') {
        closeOverlay();
      }
    };

    const closeOverlay = () => {
      if (closedFlag) return;
      closedFlag = true;
      if (activePopupCloser === closeOverlay) {
        activePopupCloser = null;
      }
      let winId = 'photo-viewer';
      try {
        if (frameWin && frameWin.__zWindowId) {
          winId = frameWin.__zWindowId;
        }
      } catch {}
      if (typeof winRoot.removeEventListener === 'function') {
        winRoot.removeEventListener('keydown', onKeyDown);
      }
      try {
        iframe.src = 'about:blank';
      } catch {}
      if (typeof overlay.remove === 'function') {
        overlay.remove();
      }
      host.emitLocalIpcEvent('close-media-viewer', [winId]);
    };

    activePopupCloser = closeOverlay;
    if (typeof closeBtn.addEventListener === 'function') {
      closeBtn.addEventListener('click', closeOverlay);
    }
    if (typeof winRoot.addEventListener === 'function') {
      winRoot.addEventListener('keydown', onKeyDown);
    }

    const wireFrameWin = (fw) => {
      if (!fw) return;
      installWindowUrlRewriters(fw);
      try {
        fw.close = () => closeOverlay();
      } catch {}
      try {
        Object.defineProperty(fw, 'closed', {
          configurable: true,
          get: () => closedFlag,
        });
      } catch {}
      try {
        if (typeof fw.addEventListener === 'function') {
          fw.addEventListener('keydown', onKeyDown);
        }
      } catch {}
    };

    if (frameWin) {
      wireFrameWin(frameWin);
      const lifecycle = wrapPopupLifecycle(frameWin, '/popup-viewer.html');
      if (typeof iframe.addEventListener === 'function') {
        iframe.addEventListener('load', () => {
          wireFrameWin(iframe.contentWindow || frameWin);
          if (lifecycle && typeof lifecycle.checkAndFlushReady === 'function') {
            lifecycle.checkAndFlushReady();
          }
        });
      }
      runPopupWindowHooks(frameWin, finalUrl, target, features);
    }

    return frameWin;
  }

  if (typeof winRoot.open === 'function' && !winRoot.open.__olazMediaPopupWrapped) {
    const origOpen = winRoot.open;
    const wrappedWindowOpen = function (url, target, features) {
      const finalUrl =
        typeof url === 'string' && typeof host.appendProfileParam === 'function'
          ? host.appendProfileParam(url)
          : url;

      if (!isPopupViewerTarget(finalUrl)) {
        const otherWin = origOpen.call(winRoot, finalUrl, target, features);
        if (otherWin) {
          if (typeof finalUrl === 'string' && finalUrl.includes('child.html')) {
            wrapPopupLifecycle(otherWin, '/child.html');
          } else {
            installWindowUrlRewriters(otherWin);
          }
          runPopupWindowHooks(otherWin, finalUrl, target, features);
        }
        return otherWin;
      }

      const preferOverlay =
        winRoot.__olazPreferOverlayPopup === true ||
        (Number(winRoot.innerWidth) > 0 && Number(winRoot.innerWidth) <= 768);
      const childWin = preferOverlay ? null : origOpen.call(winRoot, finalUrl, target, features);
      if (childWin) {
        const lifecycle = wrapPopupLifecycle(childWin, '/popup-viewer.html');
        try {
          if (typeof childWin.addEventListener === 'function') {
            childWin.addEventListener('DOMContentLoaded', () => installWindowUrlRewriters(childWin));
            childWin.addEventListener('load', () => {
              installWindowUrlRewriters(childWin);
              if (lifecycle && typeof lifecycle.checkAndFlushReady === 'function') {
                lifecycle.checkAndFlushReady();
              }
            });
          }
        } catch {}
        monitorChildPopupClose(childWin);
        runPopupWindowHooks(childWin, finalUrl, target, features);
        return childWin;
      }

      return openInPagePopupViewerOverlay(finalUrl, target, features);
    };
    wrappedWindowOpen.__olazMediaPopupWrapped = true;
    winRoot.open = wrappedWindowOpen;
  }

  for (const closeCh of ['close-window', 'hide-window']) {
    registerAsyncIpc(closeCh, async (args) => {
      const wid = typeof args[0] === 'string' ? args[0] : '';
      const isSelfPopup =
        Boolean(winRoot.location) &&
        /\/popup-viewer\.html$/i.test(String(winRoot.location.pathname || ''));
      if (wid === 'photo-viewer' || wid === 'file-viewer' || (!wid && isSelfPopup)) {
        const effectiveWid = wid || 'photo-viewer';
        if (closeCh === 'hide-window') {
          host.emitLocalIpcEvent('hide-window-complete', [{ windowId: effectiveWid }]);
        }
        if (typeof activePopupCloser === 'function') {
          activePopupCloser();
        } else if (isSelfPopup && typeof winRoot.close === 'function') {
          winRoot.close();
        }
      }
      return undefined;
    });
  }
}

bootstrapMediaPreviewRuntime(
  typeof globalThis !== 'undefined' ? globalThis : typeof self !== 'undefined' ? self : window
);
