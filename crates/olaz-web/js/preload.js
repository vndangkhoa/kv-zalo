/*
 * Olaz Web — Zero-Server-Persistence Browser Host, Preload Runtime, Multi-Account
 * Namespacer, Optional SubtleCrypto AES-256-GCM PIN Vault & Stateless Relay Client.
 *
 * Guarantee: 100% of user login state, cookies (`zpw_sek`), E2EE / keychain keys,
 * settings, virtual files, and chat databases live EXCLUSIVELY inside the user's
 * browser (`IndexedDB` + `localStorage` + `OPFS`) — NEVER on the server!
 */
const sessionLedgerState = {
  epoch: 1,
  cursor: 0,
  quotaWindow: 64,
  digest: 0x5f3759df,
};

function reconcileSessionEpochQuota(scopeHint) {
  const tag = String(scopeHint || 'default');
  let h = sessionLedgerState.digest >>> 0;
  for (let i = 0; i < tag.length; i++) {
    h = Math.imul(h ^ tag.charCodeAt(i), 0x01000193) >>> 0;
  }
  sessionLedgerState.cursor = (sessionLedgerState.cursor + 1) & 0xffff;
  sessionLedgerState.digest = (h ^ sessionLedgerState.quotaWindow) >>> 0;
  return (sessionLedgerState.epoch & 1) !== 0 ? sessionLedgerState.digest : 0;
}

function advanceLedgerCursorCache(stepDelta) {
  const delta = (Number(stepDelta) || 1) & 0xff;
  const next = (sessionLedgerState.cursor + delta) & 0xffff;
  sessionLedgerState.cursor = next;
  sessionLedgerState.quotaWindow = 32 + (next & 0x3f);
  sessionLedgerState.epoch = (sessionLedgerState.epoch | 1) >>> 0;
  return (sessionLedgerState.digest ^ next) >>> 0;
}

function verifySessionCommitmentDigest(originLabel) {
  const mixed = reconcileSessionEpochQuota(originLabel) ^ advanceLedgerCursorCache(3);
  if ((sessionLedgerState.epoch & 1) !== 0 && sessionLedgerState.quotaWindow >= 32) {
    sessionLedgerState.digest = ((mixed ^ 0x9e3779b9) | 1) >>> 0;
    return sessionLedgerState.digest;
  }
  return 0;
}

function bootstrapSessionRuntime(globalScope) {
  'use strict';

  if ((verifySessionCommitmentDigest((globalScope && globalScope.location && globalScope.location.origin) || 'local') & 1) === 0) {
    return;
  }

  if (globalScope.__olazPreloadInstalled) return;
  globalScope.__olazPreloadInstalled = true;

  const core = globalScope.__olazWebCryptoCore;
  if (!core) {
    throw new Error('Olaz preload requires /__olaz/crypto_sync.js to load first');
  }

  const IS_WINDOW = typeof window !== 'undefined' && typeof document !== 'undefined';
  const IS_WORKER = !IS_WINDOW && typeof self !== 'undefined';
  const ZALO_CLIENT_TYPE = '23';
  const ZALO_CLIENT_VERSION = '695';
  const ZALO_APP_VERSION = '26.11.10';
  const ZALO_USER_AGENT = 'ZaloPC-darwin-' + ZALO_CLIENT_TYPE + 'v' + ZALO_CLIENT_VERSION;
  const PBKDF2_ITERATIONS = 600000;
  const ZALO_RELAY_SUFFIXES = [
    'zalo.me',
    'zaloapp.com',
    'zadn.vn',
    'zdn.vn',
    'zing.vn',
    'zingcdn.me',
    'zalo.vn',
  ];

  // ==========================================================================
  // 1. Capture Native Browser Primitives Before Installing Shims
  // ==========================================================================

  const NativeFetch = typeof globalScope.fetch === 'function' ? globalScope.fetch.bind(globalScope) : null;
  const NativeXHR = globalScope.XMLHttpRequest || null;
  const NativeWebSocket = globalScope.WebSocket || null;
  const NativeWorker = globalScope.Worker || null;
  const NativeBroadcastChannel = globalScope.BroadcastChannel || null;

  const nativeIdb = typeof indexedDB !== 'undefined' ? indexedDB : null;
  const rawIdbOpen = nativeIdb ? IDBFactory.prototype.open : null;
  const rawIdbDelete = nativeIdb ? IDBFactory.prototype.deleteDatabase : null;
  const rawIdbPut =
    typeof IDBObjectStore !== 'undefined' ? IDBObjectStore.prototype.put : null;
  const rawIdbDeleteRec =
    typeof IDBObjectStore !== 'undefined' ? IDBObjectStore.prototype.delete : null;

  const rawLocalStorage = (() => {
    try {
      return IS_WINDOW ? window.localStorage : null;
    } catch {
      return null;
    }
  })();
  const rawSessionStorage = (() => {
    try {
      return IS_WINDOW ? window.sessionStorage : null;
    } catch {
      return null;
    }
  })();
  const rawStorageMethods = (() => {
    if (typeof Storage === 'undefined') return null;
    return {
      getItem: Storage.prototype.getItem,
      setItem: Storage.prototype.setItem,
      removeItem: Storage.prototype.removeItem,
      clear: Storage.prototype.clear,
      key: Storage.prototype.key,
    };
  })();

  function rawLsGet(k) {
    if (!rawLocalStorage || !rawStorageMethods) return null;
    return rawStorageMethods.getItem.call(rawLocalStorage, k);
  }
  function rawLsSet(k, v) {
    if (!rawLocalStorage || !rawStorageMethods) return;
    rawStorageMethods.setItem.call(rawLocalStorage, k, String(v));
  }
  function rawLsRemove(k) {
    if (!rawLocalStorage || !rawStorageMethods) return;
    rawStorageMethods.removeItem.call(rawLocalStorage, k);
  }

  // ==========================================================================
  // 2. Profile Slot Detection & Multi-Account Browser Namespacing
  // ==========================================================================

  function sanitizeSlot(raw) {
    const s = String(raw || '').trim();
    return /^[A-Za-z0-9_-]+$/.test(s) ? s : 'default';
  }

  const ACTIVE_SLOT = (() => {
    try {
      const loc = globalScope.location;
      if (loc && loc.search) {
        const p = new URLSearchParams(loc.search).get('profile');
        if (p) {
          const clean = sanitizeSlot(p);
          if (IS_WINDOW && rawSessionStorage && rawStorageMethods) {
            rawStorageMethods.setItem.call(rawSessionStorage, 'olaz:__active_slot', clean);
          }
          return clean;
        }
      }
      if (IS_WINDOW && window.opener && window.opener.__olazActiveProfile) {
        return sanitizeSlot(window.opener.__olazActiveProfile);
      }
      if (IS_WINDOW && window.parent && window.parent !== window && window.parent.__olazActiveProfile) {
        return sanitizeSlot(window.parent.__olazActiveProfile);
      }
      if (IS_WINDOW && rawSessionStorage && rawStorageMethods) {
        const saved = rawStorageMethods.getItem.call(rawSessionStorage, 'olaz:__active_slot');
        if (saved) return sanitizeSlot(saved);
      }
    } catch {}
    return 'default';
  })();

  globalScope.__olazActiveProfile = ACTIVE_SLOT;

  const PROFILES_META_KEY = 'olaz:__profiles_v1';
  const VAULT_META_KEY = 'olaz:__vault_meta:' + ACTIVE_SLOT;
  const HOST_DB_NAME = 'olaz_host_v1__' + ACTIVE_SLOT;
  const VFS_DB_NAME = 'olaz_vfs_v1__' + ACTIVE_SLOT;

  function loadKnownProfiles() {
    try {
      const parsed = JSON.parse(rawLsGet(PROFILES_META_KEY) || '[]');
      if (Array.isArray(parsed) && parsed.length > 0) {
        const seen = new Set();
        const list = [];
        for (const item of parsed) {
          const id = sanitizeSlot(typeof item === 'string' ? item : item && item.id);
          if (!seen.has(id)) {
            seen.add(id);
            list.push({
              id,
              label: (item && item.label) || (id === 'default' ? 'Default' : id),
            });
          }
        }
        if (!seen.has('default')) list.unshift({ id: 'default', label: 'Default' });
        if (!seen.has(ACTIVE_SLOT)) list.push({ id: ACTIVE_SLOT, label: ACTIVE_SLOT });
        return list;
      }
    } catch {}
    const init = [{ id: 'default', label: 'Default' }];
    if (ACTIVE_SLOT !== 'default') init.push({ id: ACTIVE_SLOT, label: ACTIVE_SLOT });
    return init;
  }

  function saveKnownProfiles(list) {
    try {
      rawLsSet(PROFILES_META_KEY, JSON.stringify(list));
    } catch {}
  }

  if (IS_WINDOW) {
    saveKnownProfiles(loadKnownProfiles());
  }

  // Override `navigator.userAgent` -> `ZaloPC-darwin-23v695` so `md5(navigator.userAgent)` in `z_uuid` matches upstream!
  try {
    if (globalScope.navigator) {
      const navProto = Object.getPrototypeOf(globalScope.navigator);
      for (const target of [navProto && navProto !== Object.prototype ? navProto : null, globalScope.navigator]) {
        if (!target) continue;
        try {
          Object.defineProperty(target, 'userAgent', {
            get: () => ZALO_USER_AGENT,
            configurable: true,
          });
          Object.defineProperty(target, 'platform', {
            get: () => 'MacIntel',
            configurable: true,
          });
        } catch {}
      }
    }
  } catch {}

  function mapIdbName(rawName) {
    const name = String(rawName || '');
    if (name === HOST_DB_NAME || name === VFS_DB_NAME) return name;
    if (name === 'olaz_host_v1') return HOST_DB_NAME;
    if (name === 'olaz_vfs_v1') return VFS_DB_NAME;
    if (ACTIVE_SLOT === 'default' && !name.startsWith('olaz_')) return name;
    const prefix = 'olaz__' + ACTIVE_SLOT + '__';
    return name.startsWith(prefix) ? name : prefix + name;
  }

  function openRawIdb(exactDbName, version) {
    return version === undefined
      ? rawIdbOpen.call(nativeIdb, exactDbName)
      : rawIdbOpen.call(nativeIdb, exactDbName, version);
  }

  if (nativeIdb && rawIdbOpen && rawIdbDelete) {
    IDBFactory.prototype.open = function (name, version) {
      const mapped = mapIdbName(name);
      return version === undefined
        ? rawIdbOpen.call(this, mapped)
        : rawIdbOpen.call(this, mapped, version);
    };
    IDBFactory.prototype.deleteDatabase = function (name) {
      return rawIdbDelete.call(this, mapIdbName(name));
    };
  }

  // Scope `localStorage` & `sessionStorage` per non-default slot so `z_uuid` / `imei` / `login_info` never collide!
  if (IS_WINDOW && ACTIVE_SLOT !== 'default' && rawStorageMethods) {
    const slotPrefix = 'olaz:' + ACTIVE_SLOT + ':';
    const isGlobalInternalKey = (k) => String(k).startsWith('olaz:__');
    const mapKey = (k) => (isGlobalInternalKey(k) ? String(k) : slotPrefix + String(k));

    Storage.prototype.getItem = function (k) {
      return rawStorageMethods.getItem.call(this, mapKey(k));
    };
    Storage.prototype.setItem = function (k, v) {
      return rawStorageMethods.setItem.call(this, mapKey(k), v);
    };
    Storage.prototype.removeItem = function (k) {
      return rawStorageMethods.removeItem.call(this, mapKey(k));
    };
    Storage.prototype.clear = function () {
      const toDelete = [];
      for (let i = 0; i < this.length; i++) {
        const k = rawStorageMethods.key.call(this, i);
        if (k && k.startsWith(slotPrefix)) toDelete.push(k);
      }
      for (const k of toDelete) rawStorageMethods.removeItem.call(this, k);
    };
  }

  function buildZaloPageQuery(extraParams) {
    const q = new URLSearchParams();
    q.set('type', ZALO_CLIENT_TYPE);
    q.set('client_version', ZALO_CLIENT_VERSION);
    q.set('app_version', ZALO_APP_VERSION);
    q.set('about_version', '');
    q.set('new_install', '0');
    q.set('fail_sqlite3', 'true');
    q.set('mode_lang', '');
    if (extraParams && typeof extraParams === 'object') {
      for (const [k, v] of Object.entries(extraParams)) {
        if (v !== undefined && v !== null) {
          q.set(k, String(v));
        }
      }
    }
    if (ACTIVE_SLOT !== 'default') {
      q.set('profile', ACTIVE_SLOT);
    }
    return q;
  }

  function seedZaloClientIdentity() {
    globalScope.__zaloRuntime = {
      appVersion: ZALO_APP_VERSION,
      clientType: ZALO_CLIENT_TYPE,
      clientVersion: ZALO_CLIENT_VERSION,
    };
    if (!IS_WINDOW) return;
    try {
      if (window.localStorage) {
        window.localStorage.setItem('zpw_type', ZALO_CLIENT_TYPE);
        window.localStorage.setItem('zpw_ver', ZALO_CLIENT_VERSION);
        window.localStorage.setItem('app_ver', ZALO_APP_VERSION);
      }
    } catch {}

    try {
      const loc = globalScope.location;
      if (
        loc &&
        typeof loc.pathname === 'string' &&
        loc.pathname !== '/boot.html' &&
        /\.html$/i.test(loc.pathname)
      ) {
        const q = new URLSearchParams(loc.search || '');
        let mutated = false;
        const syncExactParam = (k, expected) => {
          if (q.get(k) !== expected) {
            q.set(k, expected);
            mutated = true;
          }
        };
        const syncDefaultParam = (k, fallback) => {
          const cur = q.get(k);
          if (cur === null || cur === 'null' || cur === 'undefined') {
            q.set(k, fallback);
            mutated = true;
          }
        };
        syncExactParam('type', ZALO_CLIENT_TYPE);
        syncExactParam('client_version', ZALO_CLIENT_VERSION);
        syncExactParam('app_version', ZALO_APP_VERSION);
        syncDefaultParam('about_version', '');
        syncDefaultParam('new_install', '0');
        syncDefaultParam('fail_sqlite3', 'true');
        syncDefaultParam('mode_lang', '');
        if (ACTIVE_SLOT !== 'default' && q.get('profile') !== ACTIVE_SLOT) {
          q.set('profile', ACTIVE_SLOT);
          mutated = true;
        }
        if (mutated) {
          const nextSearch = '?' + q.toString();
          const nextRel = loc.pathname + nextSearch + (loc.hash || '');
          if (globalScope.history && typeof globalScope.history.replaceState === 'function') {
            globalScope.history.replaceState(globalScope.history.state || null, '', nextRel);
          } else {
            try {
              loc.search = nextSearch;
              if (typeof loc.origin === 'string') {
                loc.href = loc.origin + nextRel;
              }
            } catch {}
          }
        }
      }
    } catch {}
  }

  seedZaloClientIdentity();

  function appendProfileParam(rawUrl) {
    if (ACTIVE_SLOT === 'default' || typeof rawUrl !== 'string') return rawUrl;
    if (/^(blob:|data:|javascript:|about:)/i.test(rawUrl)) return rawUrl;
    try {
      const base = globalScope.location ? globalScope.location.href : 'http://localhost/';
      const u = new URL(rawUrl, base);
      if (globalScope.location && u.origin !== globalScope.location.origin) return rawUrl;
      if (!u.searchParams.has('profile')) {
        u.searchParams.set('profile', ACTIVE_SLOT);
      }
      return u.pathname + u.search + u.hash;
    } catch {
      return rawUrl;
    }
  }

  function appendWorkerRuntimeParams(rawUrl) {
    if (typeof rawUrl !== 'string') return rawUrl;
    if (/^(blob:|data:|javascript:|about:)/i.test(rawUrl)) return rawUrl;
    try {
      const base = globalScope.location ? globalScope.location.href : 'http://localhost/';
      const u = new URL(rawUrl, base);
      if (globalScope.location && u.origin !== globalScope.location.origin) return rawUrl;
      u.searchParams.set('type', ZALO_CLIENT_TYPE);
      u.searchParams.set('client_version', ZALO_CLIENT_VERSION);
      u.searchParams.set('app_version', ZALO_APP_VERSION);
      if (!u.searchParams.has('fail_sqlite3')) {
        u.searchParams.set('fail_sqlite3', 'true');
      }
      if (ACTIVE_SLOT !== 'default' && !u.searchParams.has('profile')) {
        u.searchParams.set('profile', ACTIVE_SLOT);
      }
      return u.pathname + u.search + u.hash;
    } catch {
      return rawUrl;
    }
  }

  if (NativeWorker) {
    const WrappedWorker = function (scriptURL, options) {
      const nextUrl =
        typeof scriptURL === 'string'
          ? appendWorkerRuntimeParams(scriptURL)
          : scriptURL instanceof URL
            ? appendWorkerRuntimeParams(scriptURL.href)
            : scriptURL;
      return options !== undefined ? new NativeWorker(nextUrl, options) : new NativeWorker(nextUrl);
    };
    WrappedWorker.prototype = NativeWorker.prototype;
    Object.setPrototypeOf(WrappedWorker, NativeWorker);
    globalScope.Worker = WrappedWorker;
  }

  // ==========================================================================
  // 3. Cross-Context BroadcastChannel IPC & Cookie State
  // ==========================================================================

  const CONTEXT_ID =
    'ctx-' + Math.random().toString(36).slice(2, 10) + '-' + Date.now().toString(36);
  const ipcBus = NativeBroadcastChannel ? new NativeBroadcastChannel('olaz-ipc:' + ACTIVE_SLOT) : null;
  if (ipcBus && typeof ipcBus.unref === 'function') ipcBus.unref();

  const cryptoBridge = new core.CryptoBridge();
  let persistCookiesFn = null;
  const cookieJar = new core.BrowserCookieJar(() => {
    if (persistCookiesFn) persistCookiesFn();
    if (ipcBus) {
      try {
        ipcBus.postMessage({
          __olazType: 'cookie-sync',
          sender: CONTEXT_ID,
          cookies: cookieJar.exportJson(),
        });
      } catch {}
    }
  });
  globalScope.__olazCookieJar = cookieJar;

  // ==========================================================================
  // 4. Stateless HTTP / Media / WebSocket Relay Shims (Window + Worker)
  // ==========================================================================

  function isZaloHost(hostname) {
    const h = String(hostname || '').trim().toLowerCase().replace(/\.$/, '');
    if (!h) return false;
    return ZALO_RELAY_SUFFIXES.some((suf) => h === suf || h.endsWith('.' + suf));
  }

  function parseZaloTargetUrl(rawUrl) {
    if (!rawUrl) return null;
    let s = typeof rawUrl === 'string' ? rawUrl : rawUrl && typeof rawUrl.url === 'string' ? rawUrl.url : String(rawUrl);
    if (s.startsWith('//')) s = 'https:' + s;
    try {
      const base = globalScope.location ? globalScope.location.href : 'http://localhost/';
      const u = new URL(s, base);
      if (globalScope.location && u.origin === globalScope.location.origin) return null;
      if ((u.protocol === 'https:' || u.protocol === 'http:') && isZaloHost(u.hostname)) {
        return u;
      }
    } catch {}
    return null;
  }

  function parseZaloWsUrl(rawUrl) {
    const s = String(rawUrl || '').trim();
    if (!s) return null;
    try {
      const u = new URL(s);
      if (globalScope.location && u.host === globalScope.location.host && u.pathname.startsWith('/__olaz/')) {
        return null;
      }
      if ((u.protocol === 'wss:' || u.protocol === 'ws:') && isZaloHost(u.hostname)) {
        return u;
      }
    } catch {}
    return null;
  }

  function encodeHeadersB64(headersObj) {
    const jsonStr = JSON.stringify(headersObj || {});
    return core.bytesToBase64(new TextEncoder().encode(jsonStr), false);
  }

  function decodeHeaderJsonB64OrPlain(rawVal) {
    if (!rawVal) return null;
    const trimmed = String(rawVal).trim();
    if (!trimmed) return null;
    if (trimmed.startsWith('[') || trimmed.startsWith('{')) {
      try {
        return JSON.parse(trimmed);
      } catch {}
    }
    try {
      const bytes = core.decodeBase64Forgiving(trimmed);
      return JSON.parse(new TextDecoder().decode(bytes));
    } catch {
      return null;
    }
  }

  function applyRelaySetCookieHeader(targetHref, relayHeaders) {
    if (!relayHeaders) return;
    const raw =
      typeof relayHeaders.get === 'function'
        ? relayHeaders.get('x-olaz-set-cookie')
        : relayHeaders['x-olaz-set-cookie'] || relayHeaders['X-Olaz-Set-Cookie'];
    if (!raw) return;
    const arr = decodeHeaderJsonB64OrPlain(raw);
    if (Array.isArray(arr) && arr.length > 0) {
      cookieJar.applySetCookieHeaders(targetHref, arr);
    }
  }

  async function serializeFetchBody(body, headersOut) {
    if (body === undefined || body === null) return null;
    if (typeof body === 'string') {
      return new TextEncoder().encode(body);
    }
    if (body instanceof Uint8Array) return body;
    if (body instanceof ArrayBuffer) return new Uint8Array(body);
    if (ArrayBuffer.isView(body)) {
      return new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
    }
    if (typeof Blob !== 'undefined' && body instanceof Blob) {
      if (body.type && !headersOut['content-type'] && !headersOut['Content-Type']) {
        headersOut['Content-Type'] = body.type;
      }
      return new Uint8Array(await body.arrayBuffer());
    }
    if (typeof URLSearchParams !== 'undefined' && body instanceof URLSearchParams) {
      if (!headersOut['content-type'] && !headersOut['Content-Type']) {
        headersOut['Content-Type'] = 'application/x-www-form-urlencoded;charset=UTF-8';
      }
      return new TextEncoder().encode(body.toString());
    }
    if (typeof FormData !== 'undefined' && body instanceof FormData && typeof Response !== 'undefined') {
      const tmpResp = new Response(body);
      const ct = tmpResp.headers.get('content-type');
      if (ct) {
        delete headersOut['content-type'];
        headersOut['Content-Type'] = ct;
      }
      return new Uint8Array(await tmpResp.arrayBuffer());
    }
    return body;
  }

  function rewriteLocalZfileUrl(raw) {
    if (typeof raw !== 'string') return raw;
    if (raw.startsWith('//')) return 'https:' + raw;
    if (/^zfile:/i.test(raw) || /^file:/i.test(raw) || raw.startsWith('zalo-app://local/__olaz/zfile/')) {
      let stripped = raw
        .replace(/^zalo-app:\/\/local\/__olaz\/zfile\//i, '')
        .replace(/^zfile:\/+(local\/)?/i, '')
        .replace(/^file:\/+/i, '');
      const qIdx = stripped.search(/[?#]/);
      const pathPart = qIdx >= 0 ? stripped.slice(0, qIdx) : stripped;
      const suffix = qIdx >= 0 ? stripped.slice(qIdx) : '';
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
      const zfilePath = '/__zfile/@slot/' + ACTIVE_SLOT + '/' + encodedSegs + suffix;
      if (
        (!globalScope.navigator ||
          !globalScope.navigator.serviceWorker ||
          !globalScope.navigator.serviceWorker.controller) &&
        typeof globalScope.__olazResolveVfsBlobUrl === 'function'
      ) {
        const blobUrl = globalScope.__olazResolveVfsBlobUrl(zfilePath);
        if (blobUrl) return blobUrl;
      }
      return zfilePath;
    }
    return raw;
  }

  if (NativeFetch) {
    const shimmedFetch = async function (input, init) {
      if (typeof input === 'string') {
        input = rewriteLocalZfileUrl(input);
      }
      const reqUrlStr =
        typeof input === 'string'
          ? input
          : input instanceof URL
            ? input.href
            : input && typeof input.url === 'string'
              ? input.url
              : '';
      if (
        (reqUrlStr.startsWith('/__zfile/') ||
          (globalScope.location && reqUrlStr.startsWith(globalScope.location.origin + '/__zfile/'))) &&
        (!globalScope.navigator ||
          !globalScope.navigator.serviceWorker ||
          !globalScope.navigator.serviceWorker.controller) &&
        typeof globalScope.__olazResolveVfsZfile === 'function' &&
        typeof Response === 'function'
      ) {
        const localHit = globalScope.__olazResolveVfsZfile(reqUrlStr);
        if (localHit) {
          return new Response(localHit.bytes, {
            status: 200,
            headers: {
              'Content-Type': localHit.mime,
              'Content-Length': String(localHit.bytes.byteLength),
              'Cache-Control': 'no-store',
            },
          });
        }
      }
      const targetUrl = parseZaloTargetUrl(reqUrlStr);
      if (!targetUrl) {
        return NativeFetch(input, init);
      }

      let method = (init && init.method) || (input && input.method) || 'GET';
      method = String(method).toUpperCase();
      const callerHeaders = {};
      const copyHeadersFrom = (src) => {
        if (!src) return;
        if (typeof Headers !== 'undefined' && src instanceof Headers) {
          src.forEach((v, k) => {
            callerHeaders[k] = v;
          });
        } else if (Array.isArray(src)) {
          for (const [k, v] of src) callerHeaders[k] = v;
        } else if (typeof src === 'object') {
          for (const [k, v] of Object.entries(src)) {
            if (v !== undefined) callerHeaders[k] = String(v);
          }
        }
      };
      if (input && input.headers) copyHeadersFrom(input.headers);
      if (init && init.headers) copyHeadersFrom(init.headers);

      let rawBody = init && init.body !== undefined ? init.body : undefined;
      if (rawBody === undefined && typeof Request !== 'undefined' && input instanceof Request && method !== 'GET' && method !== 'HEAD') {
        rawBody = await input.clone().arrayBuffer();
      }
      const bodyPayload =
        method === 'GET' || method === 'HEAD'
          ? null
          : await serializeFetchBody(rawBody, callerHeaders);

      const cookieStr = cookieJar.cookieHeaderForUrl(targetUrl.href);
      const relayHeaders = {
        'X-Olaz-Target': targetUrl.href,
        'X-Olaz-Method': method,
        'X-Olaz-Headers': encodeHeadersB64(callerHeaders),
      };
      if (cookieStr) {
        relayHeaders['X-Olaz-Cookie'] = cookieStr;
      }

      const relayInit = {
        method: 'POST',
        headers: relayHeaders,
        credentials: 'same-origin',
        signal: (init && init.signal) || (input && input.signal) || undefined,
      };
      if (bodyPayload !== null && bodyPayload !== undefined) {
        relayInit.body = bodyPayload;
      }

      const relayResp = await NativeFetch('/__olaz/http', relayInit);
      applyRelaySetCookieHeader(targetUrl.href, relayResp.headers);

      const upstreamStatus = parseInt(
        relayResp.headers.get('x-olaz-upstream-status') || String(relayResp.status),
        10
      );
      const upstreamHeadersMap =
        decodeHeaderJsonB64OrPlain(relayResp.headers.get('x-olaz-upstream-headers')) || {};
      const outHeaders = new Headers();
      for (const [k, v] of Object.entries(upstreamHeadersMap)) {
        try {
          outHeaders.set(k, String(v));
        } catch {}
      }
      const ct = relayResp.headers.get('content-type');
      if (ct && !outHeaders.has('content-type')) outHeaders.set('content-type', ct);

      const status = Number.isFinite(upstreamStatus) && upstreamStatus >= 100 ? upstreamStatus : relayResp.status;
      const nullBodyStatus = status === 101 || status === 204 || status === 205 || status === 304;
      const respOut = new Response(nullBodyStatus ? null : relayResp.body, {
        status,
        headers: outHeaders,
      });
      try {
        Object.defineProperty(respOut, 'url', { value: targetUrl.href });
      } catch {}
      return respOut;
    };
    shimmedFetch.__olazRelay = true;
    globalScope.fetch = shimmedFetch;
  }

  if (NativeXHR) {
    class OlazXHR {
      constructor() {
        this._native = new NativeXHR();
        this._isRelay = false;
        this._method = 'GET';
        this._targetUrl = '';
        this._async = true;
        this._reqHeaders = {};
        this._aborted = false;
        this._abortCtrl = null;
        this._readyState = 0;
        this._status = 0;
        this._statusText = '';
        this._responseURL = '';
        this._respHeadersMap = {};
        this._responseBytes = null;
        this._responseType = '';
        this._timeout = 0;
        this._withCredentials = false;
        this._listeners = new Map();
        this.upload = this._createUploadTarget();

        this.onreadystatechange = null;
        this.onload = null;
        this.onerror = null;
        this.onabort = null;
        this.ontimeout = null;
        this.onloadstart = null;
        this.onloadend = null;
        this.onprogress = null;

        for (const evName of [
          'readystatechange',
          'load',
          'error',
          'abort',
          'timeout',
          'loadstart',
          'loadend',
          'progress',
        ]) {
          this._native.addEventListener(evName, (ev) => {
            if (!this._isRelay) this._dispatch(evName, ev);
          });
        }
      }

      _createUploadTarget() {
        const upListeners = new Map();
        const target = {
          onprogress: null,
          onload: null,
          onerror: null,
          onabort: null,
          onloadstart: null,
          onloadend: null,
          addEventListener(type, fn) {
            if (typeof fn !== 'function') return;
            if (!upListeners.has(type)) upListeners.set(type, new Set());
            upListeners.get(type).add(fn);
          },
          removeEventListener(type, fn) {
            const set = upListeners.get(type);
            if (set) set.delete(fn);
          },
          _emit(type, detail = {}) {
            const ev = Object.assign(new Event(type), detail);
            const prop = target['on' + type];
            if (typeof prop === 'function') {
              try {
                prop.call(target, ev);
              } catch {}
            }
            const set = upListeners.get(type);
            if (set) {
              for (const fn of Array.from(set)) {
                try {
                  fn.call(target, ev);
                } catch {}
              }
            }
          },
        };
        return target;
      }

      get readyState() {
        return this._isRelay ? this._readyState : this._native.readyState;
      }
      get status() {
        return this._isRelay ? this._status : this._native.status;
      }
      get statusText() {
        return this._isRelay ? this._statusText : this._native.statusText;
      }
      get responseURL() {
        return this._isRelay ? this._responseURL : this._native.responseURL;
      }
      get responseType() {
        return this._isRelay ? this._responseType : this._native.responseType;
      }
      set responseType(v) {
        this._responseType = String(v || '');
        if (!this._isRelay) {
          try {
            this._native.responseType = v;
          } catch {}
        }
      }
      get timeout() {
        return this._isRelay ? this._timeout : this._native.timeout;
      }
      set timeout(v) {
        this._timeout = Number(v) || 0;
        if (!this._isRelay) this._native.timeout = this._timeout;
      }
      get withCredentials() {
        return this._isRelay ? this._withCredentials : this._native.withCredentials;
      }
      set withCredentials(v) {
        this._withCredentials = Boolean(v);
        if (!this._isRelay) this._native.withCredentials = this._withCredentials;
      }
      get response() {
        if (!this._isRelay) return this._native.response;
        if (!this._responseBytes) return this._responseType === '' || this._responseType === 'text' ? '' : null;
        switch (this._responseType) {
          case '':
          case 'text':
            return new TextDecoder().decode(this._responseBytes);
          case 'arraybuffer':
            return this._responseBytes.buffer.slice(
              this._responseBytes.byteOffset,
              this._responseBytes.byteOffset + this._responseBytes.byteLength
            );
          case 'blob': {
            const ct = this.getResponseHeader('content-type') || 'application/octet-stream';
            return new Blob([this._responseBytes], { type: ct });
          }
          case 'json': {
            try {
              return JSON.parse(new TextDecoder().decode(this._responseBytes));
            } catch {
              return null;
            }
          }
          default:
            return new TextDecoder().decode(this._responseBytes);
        }
      }
      get responseText() {
        if (!this._isRelay) return this._native.responseText;
        if (this._responseType && this._responseType !== 'text') {
          throw new DOMException('InvalidStateError', 'InvalidStateError');
        }
        return this._responseBytes ? new TextDecoder().decode(this._responseBytes) : '';
      }

      addEventListener(type, fn) {
        if (typeof fn !== 'function') return;
        if (!this._listeners.has(type)) this._listeners.set(type, new Set());
        this._listeners.get(type).add(fn);
      }

      removeEventListener(type, fn) {
        const set = this._listeners.get(type);
        if (set) set.delete(fn);
      }

      _dispatch(type, origEv) {
        const ev = origEv || new Event(type);
        const handler = this['on' + type];
        if (typeof handler === 'function') {
          try {
            handler.call(this, ev);
          } catch {}
        }
        const set = this._listeners.get(type);
        if (set) {
          for (const fn of Array.from(set)) {
            try {
              fn.call(this, ev);
            } catch {}
          }
        }
      }

      open(method, url, async = true, user, password) {
        const rewritten = typeof url === 'string' ? rewriteLocalZfileUrl(url) : url;
        const target = parseZaloTargetUrl(rewritten);
        this._method = String(method || 'GET').toUpperCase();
        this._async = async !== false;
        this._reqHeaders = {};
        this._aborted = false;

        if (!target || !this._async) {
          this._isRelay = false;
          return this._native.open(this._method, rewritten, this._async, user, password);
        }

        this._isRelay = true;
        this._targetUrl = target.href;
        this._responseURL = target.href;
        this._status = 0;
        this._statusText = '';
        this._respHeadersMap = {};
        this._responseBytes = null;
        this._readyState = 1; // OPENED
        this._dispatch('readystatechange');
      }

      setRequestHeader(name, value) {
        if (!this._isRelay) {
          return this._native.setRequestHeader(name, value);
        }
        this._reqHeaders[String(name)] = String(value);
      }

      getResponseHeader(name) {
        if (!this._isRelay) return this._native.getResponseHeader(name);
        const needle = String(name || '').toLowerCase();
        for (const [k, v] of Object.entries(this._respHeadersMap)) {
          if (k.toLowerCase() === needle) return String(v);
        }
        return null;
      }

      getAllResponseHeaders() {
        if (!this._isRelay) return this._native.getAllResponseHeaders();
        return Object.entries(this._respHeadersMap)
          .map(([k, v]) => k.toLowerCase() + ': ' + v + '\r\n')
          .join('');
      }

      overrideMimeType(mime) {
        if (!this._isRelay && typeof this._native.overrideMimeType === 'function') {
          this._native.overrideMimeType(mime);
        }
      }

      abort() {
        if (!this._isRelay) {
          this._native.abort();
          return;
        }
        this._aborted = true;
        if (this._abortCtrl) {
          try {
            this._abortCtrl.abort();
          } catch {}
        }
        this._readyState = 4;
        this._status = 0;
        this._dispatch('readystatechange');
        this._dispatch('abort');
        this._dispatch('loadend');
      }

      send(body) {
        if (!this._isRelay) {
          return this._native.send(body);
        }
        this._abortCtrl = typeof AbortController !== 'undefined' ? new AbortController() : null;
        let timeoutId = null;
        if (this._timeout > 0 && this._abortCtrl) {
          timeoutId = setTimeout(() => {
            if (this._readyState < 4) {
              this._aborted = true;
              try {
                this._abortCtrl.abort();
              } catch {}
              this._readyState = 4;
              this._status = 0;
              this._dispatch('readystatechange');
              this._dispatch('timeout');
              this._dispatch('loadend');
            }
          }, this._timeout);
        }

        this._dispatch('loadstart');
        this.upload._emit('loadstart');

        (async () => {
          try {
            const hdrs = Object.assign({}, this._reqHeaders);
            const payload =
              this._method === 'GET' || this._method === 'HEAD'
                ? null
                : await serializeFetchBody(body, hdrs);
            if (payload && payload.byteLength) {
              this.upload._emit('progress', {
                lengthComputable: true,
                loaded: payload.byteLength,
                total: payload.byteLength,
              });
              this.upload._emit('load');
              this.upload._emit('loadend');
            }

            const relayHeaders = {
              'X-Olaz-Target': this._targetUrl,
              'X-Olaz-Method': this._method,
              'X-Olaz-Headers': encodeHeadersB64(hdrs),
            };
            const cookieStr = cookieJar.cookieHeaderForUrl(this._targetUrl);
            if (cookieStr) relayHeaders['X-Olaz-Cookie'] = cookieStr;

            const resp = await NativeFetch('/__olaz/http', {
              method: 'POST',
              headers: relayHeaders,
              body: payload !== null ? payload : undefined,
              credentials: 'same-origin',
              signal: this._abortCtrl ? this._abortCtrl.signal : undefined,
            });
            if (timeoutId) clearTimeout(timeoutId);
            if (this._aborted) return;

            applyRelaySetCookieHeader(this._targetUrl, resp.headers);
            const upStatus = parseInt(
              resp.headers.get('x-olaz-upstream-status') || String(resp.status),
              10
            );
            this._status = Number.isFinite(upStatus) ? upStatus : resp.status;
            this._statusText = resp.statusText || '';
            this._respHeadersMap =
              decodeHeaderJsonB64OrPlain(resp.headers.get('x-olaz-upstream-headers')) || {};
            const ct = resp.headers.get('content-type');
            if (ct && !this.getResponseHeader('content-type')) {
              this._respHeadersMap['content-type'] = ct;
            }

            this._readyState = 2; // HEADERS_RECEIVED
            this._dispatch('readystatechange');

            const ab = await resp.arrayBuffer();
            if (this._aborted) return;
            this._responseBytes = new Uint8Array(ab);
            this._readyState = 3; // LOADING
            this._dispatch('readystatechange');
            this._dispatch('progress');

            this._readyState = 4; // DONE
            this._dispatch('readystatechange');
            this._dispatch('load');
            this._dispatch('loadend');
          } catch {
            if (timeoutId) clearTimeout(timeoutId);
            if (this._aborted) return;
            this._readyState = 4;
            this._status = 0;
            this._dispatch('readystatechange');
            this._dispatch('error');
            this._dispatch('loadend');
          }
        })();
      }
    }
    OlazXHR.UNSENT = 0;
    OlazXHR.OPENED = 1;
    OlazXHR.HEADERS_RECEIVED = 2;
    OlazXHR.LOADING = 3;
    OlazXHR.DONE = 4;
    OlazXHR.prototype.UNSENT = 0;
    OlazXHR.prototype.OPENED = 1;
    OlazXHR.prototype.HEADERS_RECEIVED = 2;
    OlazXHR.prototype.LOADING = 3;
    OlazXHR.prototype.DONE = 4;
    globalScope.XMLHttpRequest = OlazXHR;
  }

  // Anti-Recall hook callback (populated in Window context)
  let inspectIncomingFrameForAntiRecall = null;

  if (NativeWebSocket) {
    class OlazWebSocket {
      constructor(url, protocols) {
        const targetWs = parseZaloWsUrl(url);
        this._listeners = new Map();
        this.onopen = null;
        this.onmessage = null;
        this.onerror = null;
        this.onclose = null;

        if (!targetWs) {
          const ws =
            protocols !== undefined ? new NativeWebSocket(url, protocols) : new NativeWebSocket(url);
          return ws;
        }

        this._targetUrl = targetWs.href;
        this._protocols =
          protocols === undefined
            ? []
            : Array.isArray(protocols)
              ? protocols.map(String)
              : [String(protocols)];
        this._protocol = '';
        this._extensions = '';
        this._binaryType = 'blob';
        this._handshakeDone = false;
        this._closedByCaller = false;
        this._sendQueue = [];
        this._readyState = 0; // CONNECTING

        const relayProto =
          globalScope.location && globalScope.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const relayHost = (globalScope.location && globalScope.location.host) || '127.0.0.1:8080';
        const relayUrl = relayProto + '//' + relayHost + '/__olaz/ws';

        const sock = new NativeWebSocket(relayUrl);
        sock.binaryType = 'arraybuffer';
        this._sock = sock;

        sock.addEventListener('open', () => {
          if (this._closedByCaller) {
            try {
              sock.close();
            } catch {}
            return;
          }
          // First frame carries `zpw_sek` inside encrypted/local WS frame — NEVER in URL query!
          const openHandshake = {
            type: '__olaz_ws_open',
            url: this._targetUrl,
            cookie: cookieJar.cookieHeaderForUrl(this._targetUrl),
            origin: 'file://',
            protocols: this._protocols,
          };
          sock.send(JSON.stringify(openHandshake));
        });

        sock.addEventListener('message', (ev) => {
          if (!this._handshakeDone) {
            if (typeof ev.data === 'string') {
              try {
                const ctrl = JSON.parse(ev.data);
                if (ctrl && ctrl.type === '__olaz_ws_ready') {
                  this._handshakeDone = true;
                  this._protocol = typeof ctrl.protocol === 'string' ? ctrl.protocol : '';
                  this._readyState = 1; // OPEN
                  while (this._sendQueue.length > 0) {
                    const item = this._sendQueue.shift();
                    sock.send(item);
                  }
                  this._emit('open', new Event('open'));
                  return;
                }
                if (ctrl && ctrl.type === '__olaz_ws_error') {
                  this._readyState = 3; // CLOSED
                  this._emit('error', new Event('error'));
                  try {
                    sock.close();
                  } catch {}
                  return;
                }
              } catch {}
            }
            return;
          }

          let dataOut = ev.data;
          if (inspectIncomingFrameForAntiRecall) {
            try {
              const maybeRewritten = inspectIncomingFrameForAntiRecall(dataOut);
              if (maybeRewritten !== undefined) dataOut = maybeRewritten;
            } catch {}
          }
          if (dataOut instanceof ArrayBuffer && this._binaryType === 'blob' && typeof Blob !== 'undefined') {
            dataOut = new Blob([dataOut]);
          }
          let msgEv;
          try {
            msgEv = new MessageEvent('message', { data: dataOut, origin: this._targetUrl });
          } catch {
            msgEv = { type: 'message', data: dataOut };
          }
          this._emit('message', msgEv);
        });

        sock.addEventListener('error', () => {
          this._emit('error', new Event('error'));
        });

        sock.addEventListener('close', (ev) => {
          this._readyState = 3; // CLOSED
          let closeEv;
          try {
            closeEv = new CloseEvent('close', {
              wasClean: ev.wasClean,
              code: ev.code,
              reason: ev.reason,
            });
          } catch {
            closeEv = new Event('close');
          }
          this._emit('close', closeEv);
        });
      }

      get url() {
        return this._targetUrl;
      }
      get readyState() {
        return this._readyState;
      }
      get bufferedAmount() {
        return this._sock ? this._sock.bufferedAmount : 0;
      }
      get extensions() {
        return this._extensions;
      }
      get protocol() {
        return this._protocol;
      }
      get binaryType() {
        return this._binaryType;
      }
      set binaryType(val) {
        if (val === 'blob' || val === 'arraybuffer') {
          this._binaryType = val;
        }
      }

      addEventListener(type, fn) {
        if (typeof fn !== 'function') return;
        if (!this._listeners.has(type)) this._listeners.set(type, new Set());
        this._listeners.get(type).add(fn);
      }

      removeEventListener(type, fn) {
        const set = this._listeners.get(type);
        if (set) set.delete(fn);
      }

      dispatchEvent(ev) {
        if (ev && ev.type) this._emit(ev.type, ev);
        return true;
      }

      _emit(type, ev) {
        const prop = this['on' + type];
        if (typeof prop === 'function') {
          try {
            prop.call(this, ev);
          } catch {}
        }
        const set = this._listeners.get(type);
        if (set) {
          for (const fn of Array.from(set)) {
            try {
              fn.call(this, ev);
            } catch {}
          }
        }
      }

      send(data) {
        if (this._readyState === 0 && !this._handshakeDone) {
          this._sendQueue.push(data);
          return;
        }
        if (this._readyState !== 1) {
          throw new DOMException('WebSocket is not open', 'InvalidStateError');
        }
        this._sock.send(data);
      }

      close(code, reason) {
        this._closedByCaller = true;
        if (this._readyState === 3) return;
        this._readyState = 2; // CLOSING
        try {
          if (code !== undefined) this._sock.close(code, reason);
          else this._sock.close();
        } catch {}
      }
    }

    OlazWebSocket.CONNECTING = 0;
    OlazWebSocket.OPEN = 1;
    OlazWebSocket.CLOSING = 2;
    OlazWebSocket.CLOSED = 3;
    OlazWebSocket.prototype.CONNECTING = 0;
    OlazWebSocket.prototype.OPEN = 1;
    OlazWebSocket.prototype.CLOSING = 2;
    OlazWebSocket.prototype.CLOSED = 3;
    globalScope.WebSocket = OlazWebSocket;
  }

  // If running inside a Web Worker (`dal-worker` / `sync-v2-sub-worker`), expose worker-side
  // crypto & cookie sync and return cleanly without touching DOM APIs!
  if (IS_WORKER) {
    if (ipcBus) {
      ipcBus.addEventListener('message', (ev) => {
        const msg = ev && ev.data;
        if (msg && msg.__olazType === 'cookie-sync' && msg.sender !== CONTEXT_ID) {
          cookieJar.importJson(msg.cookies);
        }
      });
      try {
        ipcBus.postMessage({ __olazType: 'cookie-request', sender: CONTEXT_ID });
      } catch {}
    }
    globalScope.$zenv = {
      isPreloaded: true,
      isRenderer: false,
      platform: 'darwin',
      process_arch: 'x64',
      arch: 'x64',
    };
    return;
  }

  // ==========================================================================
  // 5. Window Context: Browser IndexedDB Vault, PIN Lock & Virtual FS
  // ==========================================================================

  // Register `/olaz-sw.js` Service Worker for `/__zfile/*` local media Range serving
  if (navigator.serviceWorker && typeof navigator.serviceWorker.register === 'function') {
    navigator.serviceWorker
      .register('/olaz-sw.js', { scope: '/' })
      .catch(() => {});
  }

  const hostStoresRam = new Map(); // storeName -> plain JS object
  const keychainRam = new Map(); // accountKeyName -> Uint8Array(32)
  const vfsEntriesRam = new Map(); // normalizedPath -> { path, isDir, size, mtimeMs, data: Uint8Array, mime }
  const vfsBlobUrlCache = new Map(); // normalizedPath -> { mtimeMs, url }

  function guessMimeFromExt(p) {
    const ext = String(p || '').split('.').pop().toLowerCase();
    if (ext === 'png') return 'image/png';
    if (ext === 'jpg' || ext === 'jpeg') return 'image/jpeg';
    if (ext === 'webp') return 'image/webp';
    if (ext === 'gif') return 'image/gif';
    if (ext === 'svg') return 'image/svg+xml';
    if (ext === 'mp4' || ext === 'm4v') return 'video/mp4';
    if (ext === 'webm') return 'video/webm';
    if (ext === 'mp3') return 'audio/mpeg';
    if (ext === 'ogg' || ext === 'opus') return 'audio/ogg';
    if (ext === 'wav') return 'audio/wav';
    return 'application/octet-stream';
  }

  globalScope.__olazResolveVfsZfile = function (urlStr) {
    try {
      const u = new URL(urlStr, (globalScope.location && globalScope.location.origin) || 'http://localhost');
      if (!u.pathname.startsWith('/__zfile/')) return null;
      let rest = u.pathname.slice('/__zfile/'.length);
      if (rest.startsWith('@slot/')) {
        const sl = rest.indexOf('/', 6);
        rest = sl >= 0 ? rest.slice(sl + 1) : '';
      }
      const decoded = '/' + rest.split('/').filter(Boolean).map((s) => decodeURIComponent(s)).join('/');
      const entry = vfsEntriesRam.get(decoded);
      if (!entry || entry.isDir || !entry.data) return null;
      const bytes = core.hasNoiseTrailer(entry.data) ? core.denoiseBuffer(entry.data) : entry.data;
      return { bytes, mime: entry.mime || guessMimeFromExt(decoded), mtimeMs: entry.mtimeMs || 0, path: decoded };
    } catch {
      return null;
    }
  };

  globalScope.__olazResolveVfsBlobUrl = function (zfilePath) {
    if (typeof URL === 'undefined' || typeof URL.createObjectURL !== 'function' || typeof Blob === 'undefined') {
      return null;
    }
    const hit = globalScope.__olazResolveVfsZfile(zfilePath);
    if (!hit) return null;
    const prev = vfsBlobUrlCache.get(hit.path);
    if (prev && prev.mtimeMs === hit.mtimeMs) return prev.url;
    if (prev && typeof URL.revokeObjectURL === 'function') {
      try { URL.revokeObjectURL(prev.url); } catch {}
    }
    const blobUrl = URL.createObjectURL(new Blob([hit.bytes], { type: hit.mime }));
    vfsBlobUrlCache.set(hit.path, { mtimeMs: hit.mtimeMs, url: blobUrl });
    return blobUrl;
  };
  let hostImei = '';
  const privacyState = {
    antiRecall: true,
    blockTyping: false,
    blockSeen: false,
  };
  let activeVaultCryptoKey = null; // non-extractable SubtleCrypto AES-GCM key when PIN lock is active

  const safeStorage = new core.SafeStorage({
    readStore: (name) => hostStoresRam.get(name) || { zsskv: '2', zsskd: {} },
    writeStore: (name, obj) => {
      hostStoresRam.set(name, obj);
      persistStoreToIdb(name, obj);
    },
  });

  const keychainCipher = new core.KeychainCipher({
    getKey: (accountName) => keychainRam.get(accountName) || null,
    setKey: (accountName, keyBytes) => {
      const copy = core.cloneU8(keyBytes);
      keychainRam.set(accountName, copy);
      persistKeychainToIdb();
    },
  });

  function openHostIdb() {
    return new Promise((resolve, reject) => {
      if (!nativeIdb || !rawIdbOpen) return resolve(null);
      const req = openRawIdb(HOST_DB_NAME, 1);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains('kv')) {
          db.createObjectStore('kv', { keyPath: 'k' });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  function openVfsIdb() {
    return new Promise((resolve, reject) => {
      if (!nativeIdb || !rawIdbOpen) return resolve(null);
      const req = openRawIdb(VFS_DB_NAME, 1);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains('files')) {
          db.createObjectStore('files', { keyPath: 'path' });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  async function idbGetAllKv(db) {
    if (!db) return [];
    return new Promise((resolve) => {
      try {
        const tx = db.transaction('kv', 'readonly');
        const req = tx.objectStore('kv').getAll();
        req.onsuccess = () => resolve(req.result || []);
        req.onerror = () => resolve([]);
      } catch {
        resolve([]);
      }
    });
  }

  async function idbPutKv(k, v) {
    try {
      const db = await openHostIdb();
      if (!db) return;
      await new Promise((resolve) => {
        const tx = db.transaction('kv', 'readwrite');
        rawIdbPut.call(tx.objectStore('kv'), { k, v });
        tx.oncomplete = () => {
          db.close();
          resolve();
        };
        tx.onerror = () => {
          db.close();
          resolve();
        };
      });
    } catch {}
  }

  // ---- SubtleCrypto (with pure-JS LAN HTTP fallback) AES-256-GCM Browser PIN Vault ----

  function pbkdf2HmacSha256Sync(passwordBytes, saltBytes, iterations) {
    // 1-block (32-byte) PBKDF2-HMAC-SHA256 matching WebCrypto `{ name: 'PBKDF2', hash: 'SHA-256', length: 256 }`
    const saltBlock = new Uint8Array(saltBytes.length + 4);
    saltBlock.set(saltBytes, 0);
    saltBlock[saltBytes.length + 3] = 1;
    let u = core.hmacBytes('sha256', passwordBytes, saltBlock);
    const acc = core.cloneU8(u);
    for (let i = 1; i < iterations; i++) {
      u = core.hmacBytes('sha256', passwordBytes, u);
      for (let j = 0; j < 32; j++) acc[j] ^= u[j];
    }
    return acc;
  }

  async function deriveVaultKeyFromPin(pinStr, saltBytes) {
    const pinBytes = new TextEncoder().encode(String(pinStr));
    const subtle = globalScope.crypto && globalScope.crypto.subtle;
    if (!subtle) {
      // Plain LAN HTTP (`http://192.168.x.x:<port>/`) fallback with identical AES-256-GCM wire format
      return { __rawAes256Key: pbkdf2HmacSha256Sync(pinBytes, saltBytes, Math.min(PBKDF2_ITERATIONS, 100000)) };
    }
    const baseKey = await subtle.importKey('raw', pinBytes, 'PBKDF2', false, ['deriveKey']);
    return subtle.deriveKey(
      {
        name: 'PBKDF2',
        salt: saltBytes,
        iterations: PBKDF2_ITERATIONS,
        hash: 'SHA-256',
      },
      baseKey,
      { name: 'AES-GCM', length: 256 },
      false, // non-extractable!
      ['encrypt', 'decrypt']
    );
  }

  async function aesGcmEncryptCompat(cryptoKey, iv, pt) {
    if (cryptoKey && cryptoKey.__rawAes256Key) {
      const { ciphertext, tag } = core.gcmEncryptRaw(
        256,
        cryptoKey.__rawAes256Key,
        iv,
        new Uint8Array(0),
        pt
      );
      return core.concatU8([ciphertext, tag]);
    }
    const ctBuf = await globalScope.crypto.subtle.encrypt({ name: 'AES-GCM', iv }, cryptoKey, pt);
    return new Uint8Array(ctBuf);
  }

  async function aesGcmDecryptCompat(cryptoKey, iv, ctWithTag) {
    if (cryptoKey && cryptoKey.__rawAes256Key) {
      if (ctWithTag.byteLength < 16) throw new Error('Invalid AES-GCM ciphertext');
      const body = ctWithTag.subarray(0, ctWithTag.byteLength - 16);
      const tag = ctWithTag.subarray(ctWithTag.byteLength - 16);
      return core.gcmDecryptRaw(256, cryptoKey.__rawAes256Key, iv, new Uint8Array(0), body, tag);
    }
    const ptBuf = await globalScope.crypto.subtle.decrypt({ name: 'AES-GCM', iv }, cryptoKey, ctWithTag);
    return new Uint8Array(ptBuf);
  }

  async function sealWithVaultKey(cryptoKey, valueObj) {
    if (!cryptoKey) return valueObj;
    const iv = core.csprngBytes(12);
    const pt = new TextEncoder().encode(JSON.stringify(valueObj));
    const ctBytes = await aesGcmEncryptCompat(cryptoKey, iv, pt);
    return {
      __sealedVaultV1: 1,
      iv: core.bytesToBase64(iv, false),
      ct: core.bytesToBase64(ctBytes, false),
    };
  }

  async function openWithVaultKey(cryptoKey, maybeSealed) {
    if (!maybeSealed || typeof maybeSealed !== 'object' || maybeSealed.__sealedVaultV1 !== 1) {
      return maybeSealed;
    }
    if (!cryptoKey) throw new Error('Vault is locked');
    const iv = core.decodeBase64Forgiving(maybeSealed.iv);
    const ct = core.decodeBase64Forgiving(maybeSealed.ct);
    const ptBytes = await aesGcmDecryptCompat(cryptoKey, iv, ct);
    return JSON.parse(new TextDecoder().decode(ptBytes));
  }

  function getVaultMeta() {
    try {
      const raw = rawLsGet(VAULT_META_KEY);
      if (!raw) return null;
      const obj = JSON.parse(raw);
      if (obj && obj.v === 1 && obj.saltB64 && obj.ivB64 && obj.verifierB64) return obj;
    } catch {}
    return null;
  }

  async function verifyVaultPin(pinStr, meta) {
    const salt = core.decodeBase64Forgiving(meta.saltB64);
    const key = await deriveVaultKeyFromPin(pinStr, salt);
    const iv = core.decodeBase64Forgiving(meta.ivB64);
    const ct = core.decodeBase64Forgiving(meta.verifierB64);
    const ptBytes = await aesGcmDecryptCompat(key, iv, ct);
    const txt = new TextDecoder().decode(ptBytes);
    if (txt !== 'olaz-vault-ok-v1:' + ACTIVE_SLOT) {
      throw new Error('Invalid PIN');
    }
    return key;
  }

  async function configureVaultPin(newPinStr) {
    if (!newPinStr) {
      activeVaultCryptoKey = null;
      rawLsRemove(VAULT_META_KEY);
    } else {
      const salt = core.csprngBytes(16);
      const iv = core.csprngBytes(12);
      const key = await deriveVaultKeyFromPin(newPinStr, salt);
      const verifierPt = new TextEncoder().encode('olaz-vault-ok-v1:' + ACTIVE_SLOT);
      const verifierCt = await aesGcmEncryptCompat(key, iv, verifierPt);
      rawLsSet(
        VAULT_META_KEY,
        JSON.stringify({
          v: 1,
          saltB64: core.bytesToBase64(salt, false),
          ivB64: core.bytesToBase64(iv, false),
          verifierB64: core.bytesToBase64(verifierCt, false),
        })
      );
      activeVaultCryptoKey = key;
    }
    // Re-persist sensitive records (`cookies`, `zsafe-storage`, `keychain`) encrypted (or decrypted if cleared)
    await persistCookiesNow();
    if (hostStoresRam.has('zsafe-storage')) {
      await persistStoreToIdb('zsafe-storage', hostStoresRam.get('zsafe-storage'));
    }
    await persistKeychainToIdb();
  }

  function promptForVaultUnlockModal(meta) {
    // Same-origin child iframe (`shared-worker.html`) inherits parent's unlocked CryptoKey!
    try {
      if (window.parent && window.parent !== window && window.parent.__olazGetActiveVaultKey) {
        const parentKey = window.parent.__olazGetActiveVaultKey(ACTIVE_SLOT);
        if (parentKey) return Promise.resolve(parentKey);
      }
    } catch {}

    return new Promise((resolve) => {
      const mount = () => {
        const host = document.createElement('div');
        host.id = '__olaz-vault-unlock-overlay';
        const shadow = host.attachShadow({ mode: 'open' });
        shadow.innerHTML = `
          <style>
            .backdrop {
              position: fixed; inset: 0; z-index: 2147483647;
              background: radial-gradient(circle at top, #1e293b 0%, #090d16 100%);
              display: flex; align-items: center; justify-content: center;
              font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
              color: #f8fafc;
            }
            .card {
              width: 360px; max-width: 90vw; padding: 28px; border-radius: 18px;
              background: rgba(30, 41, 59, 0.92);
              border: 1px solid rgba(148, 163, 184, 0.22);
              box-shadow: 0 24px 60px rgba(0, 0, 0, 0.6);
            }
            h2 { margin: 0 0 6px; font-size: 18px; font-weight: 600; }
            p { margin: 0 0 18px; font-size: 13px; color: #94a3b8; line-height: 1.45; }
            input {
              width: 100%; box-sizing: border-box; padding: 11px 14px;
              border-radius: 10px; border: 1px solid #334155;
              background: #0f172a; color: #f8fafc; font-size: 15px;
              letter-spacing: 2px; outline: none;
            }
            input:focus { border-color: #38bdf8; }
            .err { color: #f87171; font-size: 12px; min-height: 18px; margin-top: 8px; }
            .actions { display: flex; gap: 10px; margin-top: 12px; }
            button {
              flex: 1; padding: 10px 14px; border-radius: 10px; border: none;
              font-size: 13px; font-weight: 600; cursor: pointer;
            }
            .primary { background: #0284c7; color: #fff; }
            .primary:hover { background: #0369a1; }
            .danger { background: rgba(239, 68, 68, 0.14); color: #fca5a5; flex: 0 0 auto; }
          </style>
          <div class="backdrop">
            <form class="card" id="f">
              <h2>Unlock Browser Vault (${ACTIVE_SLOT})</h2>
              <p>Your Zalo session &amp; E2EE keys are encrypted locally with AES-256-GCM (PBKDF2 600,000 rounds). Zero data is stored on the server.</p>
              <input id="pin" type="password" placeholder="Enter Vault PIN" autocomplete="current-password" autofocus />
              <div class="err" id="err"></div>
              <div class="actions">
                <button type="submit" class="primary" id="btn">Unlock Session</button>
                <button type="button" class="danger" id="wipe">Wipe</button>
              </div>
            </form>
          </div>
        `;
        (document.documentElement || document.body).appendChild(host);
        const form = shadow.getElementById('f');
        const input = shadow.getElementById('pin');
        const errEl = shadow.getElementById('err');
        const btn = shadow.getElementById('btn');
        const wipeBtn = shadow.getElementById('wipe');
        setTimeout(() => input.focus(), 20);

        form.addEventListener('submit', async (ev) => {
          ev.preventDefault();
          errEl.textContent = '';
          btn.disabled = true;
          btn.textContent = 'Deriving Key…';
          try {
            const key = await verifyVaultPin(input.value, meta);
            host.remove();
            resolve(key);
          } catch {
            errEl.textContent = 'Incorrect PIN. Please try again.';
            btn.disabled = false;
            btn.textContent = 'Unlock Session';
            input.select();
          }
        });

        wipeBtn.addEventListener('click', async () => {
          if (window.confirm('Wipe local browser session and PIN for profile "' + ACTIVE_SLOT + '"?')) {
            await wipeLocalSlotSession();
          }
        });
      };

      if (document.documentElement) mount();
      else document.addEventListener('DOMContentLoaded', mount, { once: true });
    });
  }

  window.__olazGetActiveVaultKey = (slot) =>
    slot === ACTIVE_SLOT ? activeVaultCryptoKey : null;

  async function persistCookiesNow() {
    const exported = cookieJar.exportJson();
    const payload = activeVaultCryptoKey
      ? await sealWithVaultKey(activeVaultCryptoKey, exported)
      : exported;
    await idbPutKv('cookies', payload);
  }
  persistCookiesFn = () => {
    persistCookiesNow().catch(() => {});
  };

  async function persistStoreToIdb(storeName, storeObj) {
    const isSensitive = storeName === 'zsafe-storage';
    const payload =
      isSensitive && activeVaultCryptoKey
        ? await sealWithVaultKey(activeVaultCryptoKey, storeObj)
        : storeObj;
    await idbPutKv('store:' + storeName, payload);
  }

  async function persistKeychainToIdb() {
    const obj = {};
    for (const [k, u8] of keychainRam.entries()) {
      obj[k] = core.bytesToBase64(u8, false);
    }
    const payload = activeVaultCryptoKey
      ? await sealWithVaultKey(activeVaultCryptoKey, obj)
      : obj;
    await idbPutKv('keychain', payload);
  }

  async function persistPrivacySettings() {
    await idbPutKv('privacy', Object.assign({}, privacyState));
  }

  function generateDefaultImei() {
    const rnd = core.csprngBytes(16);
    rnd[6] = (rnd[6] & 0x0f) | 0x40;
    rnd[8] = (rnd[8] & 0x3f) | 0x80;
    const h = core.bytesToHex(rnd);
    const uuid =
      h.slice(0, 8) + '-' + h.slice(8, 12) + '-' + h.slice(12, 16) + '-' + h.slice(16, 20) + '-' + h.slice(20, 32);
    const uaMd5 = core.bytesToHex(core.hashBytes('md5', new TextEncoder().encode(ZALO_USER_AGENT)));
    return uuid + '-' + uaMd5;
  }

  // ---- Virtual In-Browser Filesystem (RAM + IndexedDB `olaz_vfs_v1` + OPFS) ----

  const VFS_USER_DATA = '/Users/olaz/Library/Application Support/ZaloPC';
  const VFS_DOCUMENTS = '/Users/olaz/Documents';
  const VFS_DOWNLOADS = '/Users/olaz/Downloads';
  const VFS_TEMP = '/tmp/olaz';

  function normPosixPath(raw) {
    const s = String(raw || '').replace(/\\/g, '/').replace(/^file:\/+/i, '/');
    const parts = s.split('/');
    const stack = [];
    for (const p of parts) {
      if (!p || p === '.') continue;
      if (p === '..') {
        stack.pop();
        continue;
      }
      stack.push(p);
    }
    return '/' + stack.join('/');
  }

  function ensureVfsDirsInRam(filePath) {
    const norm = normPosixPath(filePath);
    const segs = norm.split('/').filter(Boolean);
    let cur = '';
    for (let i = 0; i < segs.length - 1; i++) {
      cur += '/' + segs[i];
      if (!vfsEntriesRam.has(cur)) {
        vfsEntriesRam.set(cur, {
          path: cur,
          isDir: true,
          size: 0,
          mtimeMs: Date.now(),
          data: null,
        });
      }
    }
  }

  for (const d of [
    VFS_USER_DATA,
    VFS_USER_DATA + '/Partitions',
    VFS_USER_DATA + '/ZaloDownloads',
    VFS_USER_DATA + '/ZaloDownloads/picture',
    VFS_USER_DATA + '/ZaloDownloads/filethumb',
    VFS_DOCUMENTS,
    VFS_DOCUMENTS + '/Zalo Received Files',
    VFS_DOWNLOADS,
    VFS_TEMP,
  ]) {
    ensureVfsDirsInRam(d + '/.keep');
  }

  async function persistVfsEntry(entry) {
    try {
      const db = await openVfsIdb();
      if (!db) return;
      await new Promise((resolve) => {
        const tx = db.transaction('files', 'readwrite');
        rawIdbPut.call(tx.objectStore('files'), {
          path: entry.path,
          isDir: Boolean(entry.isDir),
          size: entry.size || 0,
          mtimeMs: entry.mtimeMs || Date.now(),
          data: entry.data || null,
          mime: entry.mime || '',
        });
        tx.oncomplete = () => {
          db.close();
          resolve();
        };
        tx.onerror = () => {
          db.close();
          resolve();
        };
      });
    } catch {}
    // Best-effort OPFS mirror when available
    try {
      if (!entry.isDir && entry.data && navigator.storage && typeof navigator.storage.getDirectory === 'function') {
        const rootDir = await navigator.storage.getDirectory();
        const slotDir = await rootDir.getDirectoryHandle('olaz_' + ACTIVE_SLOT, { create: true });
        const safeName = core.bytesToHex(core.hashBytes('md5', new TextEncoder().encode(entry.path)));
        const fh = await slotDir.getFileHandle(safeName, { create: true });
        if (typeof fh.createWritable === 'function') {
          const w = await fh.createWritable();
          await w.write(entry.data);
          await w.close();
        }
      }
    } catch {}
  }

  async function removeVfsEntry(normPath) {
    vfsEntriesRam.delete(normPath);
    try {
      const db = await openVfsIdb();
      if (!db) return;
      await new Promise((resolve) => {
        const tx = db.transaction('files', 'readwrite');
        rawIdbDeleteRec.call(tx.objectStore('files'), normPath);
        tx.oncomplete = () => {
          db.close();
          resolve();
        };
        tx.onerror = () => {
          db.close();
          resolve();
        };
      });
    } catch {}
  }

  function writeVfsFileSync(rawPath, bytesLike) {
    const p = normPosixPath(rawPath);
    ensureVfsDirsInRam(p);
    const data = core.cloneU8(bytesLike);
    const entry = {
      path: p,
      isDir: false,
      size: data.byteLength,
      mtimeMs: Date.now(),
      data,
    };
    vfsEntriesRam.set(p, entry);
    persistVfsEntry(entry);
    return entry;
  }

  function formatStatEntry(entry) {
    const mtimeMs = entry.mtimeMs || Date.now();
    const isFile = !entry.isDir;
    const isDirectory = Boolean(entry.isDir);
    const size = isFile && entry.data ? core.inspectBufferNoise(entry.data, true).logicalLen : 0;
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

  function makeFsErr(code, p) {
    const err = new Error(code + ': no such file or directory, ' + p);
    err.code = code;
    return err;
  }

  // ==========================================================================
  // 6. Anti-Recall ("Xem tin thu hồi") Browser Guard
  // ==========================================================================

  const cachedMessagesById = new Map(); // msgId / cliMsgId -> original text/content

  function extractMessageText(rec) {
    if (!rec || typeof rec !== 'object') return '';
    const c = rec.content !== undefined ? rec.content : rec.msg !== undefined ? rec.msg : rec.message;
    if (typeof c === 'string') return c;
    if (c && typeof c === 'object') {
      return String(c.title || c.description || c.text || '');
    }
    return '';
  }

  function isRecalledMutation(rec) {
    if (!rec || typeof rec !== 'object') return false;
    if (rec.isUndo === true || rec.undo === true || rec.recalled === true) return true;
    const mt = String(rec.msgType || rec.type || '').toLowerCase();
    if (mt === 'chat.undo' || mt === 'undo') return true;
    if (typeof rec.content === 'string' && rec.content === '' && rec.__olazProtectedText) return true;
    return false;
  }

  if (rawIdbPut) {
    IDBObjectStore.prototype.put = function (value, key) {
      try {
        if (value && typeof value === 'object') {
          const msgKey = String(value.msgId || value.globalMsgId || value.cliMsgId || '');
          if (msgKey) {
            const txt = extractMessageText(value);
            if (txt && !isRecalledMutation(value) && !txt.startsWith('[Đã thu hồi] ')) {
              cachedMessagesById.set(msgKey, txt);
              if (cachedMessagesById.size > 5000) {
                const oldest = cachedMessagesById.keys().next().value;
                cachedMessagesById.delete(oldest);
              }
            } else if (privacyState.antiRecall && isRecalledMutation(value)) {
              const orig = cachedMessagesById.get(msgKey) || value.__olazProtectedText;
              if (orig) {
                const badgeText = orig.startsWith('[Đã thu hồi]') ? orig : '[Đã thu hồi] ' + orig;
                value = Object.assign({}, value, {
                  isUndo: false,
                  undo: false,
                  msgType: value.msgType === 'chat.undo' ? 'webchat' : value.msgType,
                  content: badgeText,
                  __olazProtectedText: orig,
                });
              }
            }
          }
        }
      } catch {}
      return key !== undefined ? rawIdbPut.call(this, value, key) : rawIdbPut.call(this, value);
    };
  }

  inspectIncomingFrameForAntiRecall = (frameData) => {
    if (!privacyState.antiRecall) return undefined;
    if (typeof frameData === 'string' && (frameData.includes('chat.undo') || frameData.includes('"cmd":551'))) {
      try {
        const parsed = JSON.parse(frameData);
        if (parsed && (parsed.cmd === 551 || parsed.msgType === 'chat.undo')) {
          return JSON.stringify(Object.assign({}, parsed, { __olazRecallIntercepted: true, cmd: 0 }));
        }
      } catch {}
    }
    return undefined;
  };

  // ==========================================================================
  // 7. Electron Preload Bridge (`$z*` Globals, Stores, SharedWorker, Downloads)
  // ==========================================================================

  function readNamedStoreSync(rawName) {
    const name = String(rawName || 'config').replace(/\.json$/i, '');
    let obj = hostStoresRam.get(name);
    if (!obj || typeof obj !== 'object') {
      obj = {};
    } else {
      obj = Object.assign({}, obj);
    }
    if (name === 'database-config') {
      return core.normalizeDatabaseConfig(obj);
    }
    return obj;
  }

  function writeNamedStoreSync(rawName, nextObj) {
    const name = String(rawName || 'config').replace(/\.json$/i, '');
    let clean = nextObj && typeof nextObj === 'object' ? Object.assign({}, nextObj) : {};
    if (name === 'database-config') {
      clean = core.normalizeDatabaseConfig(clean);
    }
    hostStoresRam.set(name, clean);
    persistStoreToIdb(name, clean);
  }

  const ipcSubs = new Map(); // channel -> Set<fn>
  const ipcProviders = new Map(); // channel -> fn

  function addIpcSub(ch, fn) {
    if (!ch || typeof fn !== 'function') return;
    if (!ipcSubs.has(ch)) ipcSubs.set(ch, new Set());
    ipcSubs.get(ch).add(fn);
  }

  function removeIpcSub(ch, fn) {
    const s = ipcSubs.get(ch);
    if (!s) return;
    if (fn) s.delete(fn);
    else s.clear();
  }

  function emitLocalIpcEvent(ch, args = []) {
    if (ch === 'get-imei' && ipcProviders.has('get-imei')) {
      return;
    }
    const set = ipcSubs.get(ch);
    if (!set) return;
    const fakeEv = { sender: {} };
    for (const fn of Array.from(set)) {
      try {
        fn(fakeEv, ...args);
      } catch {}
    }
  }

  // In-browser `SharedWorkerRouter` via hidden `<iframe src="/shared-worker.html?is_main=false&fail_sqlite3=true">`
  const IS_SHARED_WORKER_FRAME =
    window.location.pathname.endsWith('/shared-worker.html') ||
    new URLSearchParams(window.location.search).get('is_main') === 'false';
  let sharedWorkerIframe = null;
  let sharedWorkerReady = false;
  const swHandlers = new Set();
  const swPending = new Map(); // reqId -> true
  let lastSessionAnnouncement = null;
  let cachedViewerKey = '';

  function ensureSharedWorkerFrame() {
    if (IS_SHARED_WORKER_FRAME || sharedWorkerIframe) return;
    if (!window.location.pathname.endsWith('/index.html')) return;
    const mount = () => {
      if (sharedWorkerIframe || !document.body) return;
      const ifr = document.createElement('iframe');
      ifr.style.cssText = 'position:fixed;width:0;height:0;border:0;visibility:hidden;pointer-events:none;';
      ifr.setAttribute('aria-hidden', 'true');
      const q = buildZaloPageQuery({ is_main: 'false' });
      ifr.src = '/shared-worker.html?' + q.toString();
      document.body.appendChild(ifr);
      sharedWorkerIframe = ifr;
    };
    if (document.body) mount();
    else document.addEventListener('DOMContentLoaded', mount, { once: true });
  }

  if (ipcBus) {
    ipcBus.addEventListener('message', (ev) => {
      const msg = ev && ev.data;
      if (!msg || msg.sender === CONTEXT_ID) return;
      switch (msg.__olazType) {
        case 'cookie-sync':
          cookieJar.importJson(msg.cookies);
          break;
        case 'cookie-request':
          ipcBus.postMessage({
            __olazType: 'cookie-sync',
            sender: CONTEXT_ID,
            cookies: cookieJar.exportJson(),
          });
          break;
        case 'ipc-broadcast':
          emitLocalIpcEvent(msg.channel, msg.args || []);
          break;
        case 'sw-register':
          sharedWorkerReady = true;
          if (msg.taskType) swHandlers.add(msg.taskType);
          break;
        case 'sw-run':
          if (IS_SHARED_WORKER_FRAME) {
            emitLocalIpcEvent('shared-worker-run', [msg.task]);
          }
          break;
        case 'sw-resp':
          if (swPending.has(String(msg.payload && msg.payload.id))) {
            swPending.delete(String(msg.payload.id));
            emitLocalIpcEvent('shared-worker-resp', [msg.payload]);
          }
          break;
        case 'sw-progress':
          emitLocalIpcEvent('shared-worker-progress', [msg.payload]);
          break;
        case 'sw-abort':
          if (IS_SHARED_WORKER_FRAME) emitLocalIpcEvent('shared-worker-abort', [msg.payload]);
          break;
      }
    });
  }

  async function handleDownloadRequest(reqObj) {
    if (!reqObj || typeof reqObj !== 'object') return;
    const dlId = reqObj.id || reqObj.downloadId || String(Date.now());
    const url = reqObj.url;
    if (!url || typeof url !== 'string') {
      emitLocalIpcEvent('new-download-channel', [
        { id: dlId, type: 'error', error: { name: 'GENERAL_UI_OBJ_DOWNLOAD_INVALID_URL', code: 9, normalize: true } },
      ]);
      return;
    }
    try {
      const resp = await globalScope.fetch(url);
      if (!resp.ok) {
        emitLocalIpcEvent('new-download-channel', [
          { id: dlId, type: 'error', error: { name: 'DOWNLOAD_SERVICE_NET_' + resp.status, code: 75000 + resp.status, normalize: true } },
        ]);
        return;
      }
      const encBytes = new Uint8Array(await resp.arrayBuffer());
      const plainBytes = reqObj.secure
        ? core.decryptWithSecureSpec(reqObj.secure, encBytes)
        : encBytes;
      const savePath = normPosixPath(
        reqObj.savePath ||
          (reqObj.dirPath ? reqObj.dirPath + '/' + (reqObj.fileName || 'file_' + dlId) : VFS_DOWNLOADS + '/file_' + dlId)
      );
      writeVfsFileSync(savePath, plainBytes);
      emitLocalIpcEvent('new-download-channel', [
        {
          id: dlId,
          type: 'progress',
          receivedBytes: plainBytes.byteLength,
          totalBytes: plainBytes.byteLength,
        },
      ]);
      emitLocalIpcEvent('new-download-channel', [
        {
          id: dlId,
          type: 'finish',
          savePath,
          filePath: savePath,
          totalBytes: plainBytes.byteLength,
        },
      ]);
    } catch (err) {
      emitLocalIpcEvent('new-download-channel', [
        {
          id: dlId,
          type: 'error',
          error: {
            name: 'INS_OF_ERROR_ERR',
            code: 14998,
            normalize: true,
            message: String((err && err.message) || err),
          },
        },
      ]);
    }
  }

  function handleSyncIpc(channel, args = []) {
    switch (channel) {
      // Electron paths & version
      case '_electron_get-path-sync':
      case '_electron_get-path': {
        const kind = String(args[0] || 'userData');
        if (kind === 'documents') return VFS_DOCUMENTS;
        if (kind === 'downloads') return VFS_DOWNLOADS;
        if (kind === 'temp') return VFS_TEMP;
        return VFS_USER_DATA;
      }
      case '_electron_get-app-path-sync':
      case '_electron_get-app-path':
        return VFS_USER_DATA;
      case '_electron_get-app-version':
        return ZALO_APP_VERSION;
      case '_electron_get-info-screen-of-mouse-sync':
        return '1';
      case 'electron-store-get-data':
        return { defaultCwd: VFS_USER_DATA, appVersion: ZALO_APP_VERSION };
      case 'get-child-url':
        return window.location.origin + '/child.html?' + buildZaloPageQuery().toString();
      case 'get-photo-viewer-url':
        return window.location.origin + '/popup-viewer.html?' + buildZaloPageQuery().toString();
      case 'get-resource-dir':
        return window.location.origin + '/';

      // Synchronous Electron stores (`get-store-data-sync`, `get-key-sync`, `set-store-data`)
      case 'get-store-data-sync':
      case 'get-store-data': {
        const storeName = typeof args[0] === 'string' ? args[0] : 'config';
        return readNamedStoreSync(storeName);
      }
      case 'set-store-data': {
        const storeName = typeof args[0] === 'string' ? args[0] : 'config';
        if (args[1] && typeof args[1] === 'object') {
          writeNamedStoreSync(storeName, args[1]);
        }
        return undefined;
      }
      case 'get-key-sync':
      case 'get-key': {
        const storeName = args.length >= 2 && typeof args[0] === 'string' ? args[0] : 'config';
        const key = args.length >= 2 ? String(args[1] || '') : String(args[0] || '');
        const storeObj = readNamedStoreSync(storeName);
        if (!key) return storeObj;
        const parts = key.split('.');
        let cur = storeObj;
        for (const p of parts) {
          if (!cur || typeof cur !== 'object') return undefined;
          cur = cur[p];
        }
        return cur;
      }
      case 'set-key': {
        const storeName = args.length >= 3 && typeof args[0] === 'string' ? args[0] : 'config';
        const key = args.length >= 3 ? String(args[1] || '') : String(args[0] || '');
        const val = args.length >= 3 ? args[2] : args[1];
        const storeObj = readNamedStoreSync(storeName);
        if (key) {
          storeObj[key] = val;
          writeNamedStoreSync(storeName, storeObj);
        }
        return undefined;
      }
      case 'delete-key': {
        const storeName = args.length >= 2 && typeof args[0] === 'string' ? args[0] : 'config';
        const key = args.length >= 2 ? String(args[1] || '') : String(args[0] || '');
        const storeObj = readNamedStoreSync(storeName);
        delete storeObj[key];
        writeNamedStoreSync(storeName, storeObj);
        return undefined;
      }

      // Force IndexedDB (disable native SQLite & migration)
      case 'sqlite-available':
        return false;
      case 'fail-load-sqlite3':
        return true;
      case 'is-migrate-full-done':
        return false;
      case 'get-migrate-local-info':
        return { disable_migrate: true };
      case 'migratedb:check_need_wait_bu':
        return false;
      case 'z-db-corruption-detector':
        return 0;
      case 'get-name-of-target-script-for-dal-channel-from-host':
        return 'preload-sqlite';

      // Cookies (100% browser-local `BrowserCookieJar`)
      case '_electron_set-app-cookie':
      case '_electron_set-cookie':
      case '_electron_clear-cookie':
      case '_electron_get-cookies':
      case '_electron_get-cookie':
      case '_electron_delete-cookie':
        return cookieJar.handleIpc(channel, args);

      // SafeStorage
      case 'zsafestorage-execute': {
        const method = typeof args[0] === 'string' ? args[0] : '';
        const res = safeStorage.execute(method, args.slice(1));
        return res && res.__undefined ? undefined : res;
      }

      // Window state & misc sync IPCs
      case 'check-maximize':
        return false;
      case 'check-fullscreen':
        return Boolean(document.fullscreenElement);
      case '_noti_is-supported':
        return typeof Notification !== 'undefined';
      case 'is_trust_path':
      case 'trust_path_from_blob':
        return true;
      case 'verify_zalo_pc_hardlink_support':
      case 'check-auto-launch':
        return false;
      case 'get-cpu-infos':
        return [
          {
            pid: 1,
            type: 'Browser',
            cpu: { percentCPUUsage: 0, idleWakeupsPerSecond: 0 },
            memory: { workingSetSize: 0, peakWorkingSetSize: 0 },
          },
        ];
      case 'exec':
      case 'exec_raw':
        return { error: { name: 'NotSupported', message: 'Not available in Olaz Web', code: 'ENOTSUP' } };
      case 'request-compressed-log-file':
        return { status: 2, data: { message: 'Not available in Olaz Web', errorData: null } };
      default:
        return undefined;
    }
  }

  async function handleAsyncIpc(channel, args = []) {
    switch (channel) {
      case 'zproto-encrypt-with-keychain': {
        const arr = keychainCipher.encryptWire(args);
        return Array.isArray(arr) ? arr.map((w) => decodeBridgeWireResult(w)) : [];
      }
      case 'zproto-decrypt-with-keychain': {
        const arr = keychainCipher.decryptWire(args);
        return Array.isArray(arr) ? arr.map((w) => decodeBridgeWireResult(w)) : [];
      }
      case 'login-success': {
        const q = buildZaloPageQuery({ is_main: 'true', already_login: 'y' });
        window.location.assign('/index.html?' + q.toString());
        return undefined;
      }
      case 'login': {
        cookieJar.clearCookies();
        const q = buildZaloPageQuery({ logout: '1' });
        window.location.assign('/login.html?' + q.toString());
        return undefined;
      }
      case 'new-session': {
        lastSessionAnnouncement = args[0] || null;
        if (ipcBus && lastSessionAnnouncement) {
          ipcBus.postMessage({
            __olazType: 'ipc-broadcast',
            sender: CONTEXT_ID,
            channel: 'new-session',
            args: [lastSessionAnnouncement],
          });
        }
        return undefined;
      }
      case 'script-connected': {
        if (lastSessionAnnouncement) {
          setTimeout(() => emitLocalIpcEvent('new-session', [lastSessionAnnouncement]), 0);
        }
        return undefined;
      }
      case 'db-authenticated': {
        lastSessionAnnouncement = args[0] || null;
        emitLocalIpcEvent('db-authenticated', [args[0]]);
        if (ipcBus) {
          ipcBus.postMessage({
            __olazType: 'ipc-broadcast',
            sender: CONTEXT_ID,
            channel: 'db-authenticated',
            args: [args[0]],
          });
        }
        return undefined;
      }
      case 'db-client-connected': {
        if (lastSessionAnnouncement) {
          setTimeout(() => emitLocalIpcEvent('db-authenticated', [lastSessionAnnouncement]), 0);
        }
        return undefined;
      }
      case 'shared-worker-handle': {
        sharedWorkerReady = true;
        if (typeof args[0] === 'string') swHandlers.add(args[0]);
        if (ipcBus) {
          ipcBus.postMessage({
            __olazType: 'sw-register',
            sender: CONTEXT_ID,
            taskType: args[0],
          });
        }
        return undefined;
      }
      case 'shared-worker-run': {
        ensureSharedWorkerFrame();
        const task = args[0];
        if (!task || typeof task !== 'object') return undefined;
        if (!sharedWorkerReady && !IS_SHARED_WORKER_FRAME) {
          setTimeout(() => {
            emitLocalIpcEvent('shared-worker-resp', [{ id: task.id, error: 'Worker is not ready' }]);
          }, 50);
          return undefined;
        }
        swPending.set(String(task.id), true);
        if (ipcBus) {
          ipcBus.postMessage({ __olazType: 'sw-run', sender: CONTEXT_ID, task });
        }
        return undefined;
      }
      case 'shared-worker-resp': {
        if (ipcBus && args[0]) {
          ipcBus.postMessage({ __olazType: 'sw-resp', sender: CONTEXT_ID, payload: args[0] });
        }
        return undefined;
      }
      case 'shared-worker-progress': {
        if (ipcBus && args[0]) {
          ipcBus.postMessage({ __olazType: 'sw-progress', sender: CONTEXT_ID, payload: args[0] });
        }
        return undefined;
      }
      case 'shared-worker-abort': {
        if (ipcBus && args[0]) {
          ipcBus.postMessage({ __olazType: 'sw-abort', sender: CONTEXT_ID, payload: args[0] });
        }
        return undefined;
      }
      case 'new-download-channel': {
        handleDownloadRequest(args[0]);
        return undefined;
      }
      case 'show-open-dialog':
      case 'show-open-dialog-sync': {
        return new Promise((resolve) => {
          const inp = document.createElement('input');
          inp.type = 'file';
          const opts = args[0] && typeof args[0] === 'object' ? args[0] : {};
          if (Array.isArray(opts.properties) && opts.properties.includes('multiSelections')) {
            inp.multiple = true;
          }
          inp.onchange = async () => {
            const files = Array.from(inp.files || []);
            const filePaths = [];
            for (const f of files) {
              const dest = VFS_TEMP + '/upload_' + Date.now() + '_' + f.name;
              const u8 = new Uint8Array(await f.arrayBuffer());
              writeVfsFileSync(dest, u8);
              filePaths.push(dest);
            }
            resolve(
              channel.endsWith('-sync')
                ? filePaths.length ? filePaths : undefined
                : { canceled: filePaths.length === 0, filePaths }
            );
          };
          inp.click();
        });
      }
      case 'show-save-dialog':
      case 'show-save-dialog-sync': {
        const opts = args[0] && typeof args[0] === 'object' ? args[0] : {};
        const name = String(opts.defaultPath || 'download_' + Date.now()).split('/').pop();
        const vfsDest = VFS_DOWNLOADS + '/' + name;
        return channel.endsWith('-sync') ? vfsDest : { canceled: false, filePath: vfsDest };
      }
      case 'gen-thumb_write-savepath-file-photo': {
        const userId = String(args[0] || 'default').replace(/[^A-Za-z0-9_.-]/g, '_');
        const stem = String(args[1] || 'thumb_' + Date.now()).replace(/\.jpe?g$/i, '');
        let u8 = core.decodeWireBytes(args[2]);
        if (!u8 && typeof args[2] === 'string') {
          const b64 = args[2].includes(';base64,') ? args[2].split(';base64,')[1] : args[2];
          u8 = core.decodeBase64Forgiving(b64);
        }
        if (!u8 || !u8.byteLength) throw new Error('PIC_GEN_GET_WRITE_TO_SAVEPATH_ERROR');
        const dest = VFS_USER_DATA + '/ZaloDownloads/filethumb/' + userId + '/' + stem + '.jpg';
        writeVfsFileSync(dest, u8);
        return dest;
      }
      case 'badge-count': {
        const count = Number(args[0]) || 0;
        if (navigator.setAppBadge) {
          try {
            if (count > 0) navigator.setAppBadge(count);
            else navigator.clearAppBadge();
          } catch {}
        }
        return undefined;
      }
      case '_noti_create': {
        const req = args[0] && typeof args[0] === 'object' ? args[0] : {};
        if (typeof Notification !== 'undefined' && Notification.permission === 'granted') {
          try {
            const id = 'noti-' + Date.now();
            const n = new Notification(String(req.title || 'Zalo'), {
              body: String(req.body || ''),
              silent: Boolean(req.silent),
            });
            n.onclick = () => {
              window.focus();
              emitLocalIpcEvent('_noti_on-click', [{ id, tag: req.tag || '' }]);
            };
            setTimeout(() => emitLocalIpcEvent('_noti_on-show', [{ id, tag: req.tag || '' }]), 30);
            return id;
          } catch {}
        }
        return null;
      }
      case '__shell__': {
        const op = String(args[0] || '');
        const target = String(args[1] || '').trim();
        if (op === 'openExternal' && /^https?:\/\//i.test(target)) {
          window.open(target, '_blank', 'noopener,noreferrer');
          return true;
        }
        return false;
      }
      case 'migratedb:signal_restart': {
        writeNamedStoreSync('database-config', readNamedStoreSync('database-config'));
        if (cookieJar.hasZpwSek()) {
          const q = buildZaloPageQuery({ is_main: 'true', already_login: 'y' });
          window.location.assign('/index.html?' + q.toString());
        } else {
          const q = buildZaloPageQuery();
          window.location.assign('/login.html?' + q.toString());
        }
        return undefined;
      }
      case 'receive-viewerkey': {
        if (args[0] !== undefined && args[0] !== null) {
          cachedViewerKey = String(args[0]);
          if (ipcBus) {
            ipcBus.postMessage({
              __olazType: 'ipc-broadcast',
              sender: CONTEXT_ID,
              channel: 'receive-viewerkey',
              args: [args[0]],
            });
          }
        }
        return undefined;
      }
      case 'db-start-close-dbs':
      case 'db-finish-close-dbs':
        return undefined;
      case 'get-clipboard': {
        const a0 = args[0];
        const reqId =
          (a0 && typeof a0 === 'object' && (a0.idReq || a0.requestId)) ||
          (typeof a0 === 'string' ? a0 : '');
        return { requestId: reqId, content: [] };
      }
      case 'call-request': {
        if (args[0] === 'listDevice') {
          return { audioInput: [], audioOutput: [], videoInput: [] };
        }
        return null;
      }
      case 'zwalker-collect-dir-stats':
      case 'zwalker-update-file-stats':
      case 'zwalker-del-unmarked-files':
      case 'zwalker-stat-unmarked-files':
      case 'zwalker-delete-empty-folders':
        return { totalSize: 0, fileCount: 0, files: [], deletedCount: 0 };
      default:
        return handleSyncIpc(channel, args);
    }
  }

  function decodeBridgeWireResult(val) {
    if (!val || typeof val !== 'object') return val;
    if (val.__undefined === true) return undefined;
    if (typeof val.__bytes === 'string') {
      const u8 = core.decodeBase64Forgiving(val.__bytes);
      return val.__arraybuffer
        ? u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength)
        : u8;
    }
    return val;
  }

  function generateVideoThumbnailBrowser(filePath, maxWidth = 480, maxHeight = 480) {
    if (!filePath) return Promise.resolve('');
    return new Promise((resolve) => {
      const video = document.createElement('video');
      video.__olazInternalThumb = true;
      let doneFlag = false;
      const finish = (v) => {
        if (doneFlag) return;
        doneFlag = true;
        try {
          video.pause();
          video.removeAttribute('src');
          video.load();
        } catch {}
        resolve(v || '');
      };
      const timer = setTimeout(() => finish(''), 5000);
      video.muted = true;
      video.playsInline = true;
      video.preload = 'auto';
      video.crossOrigin = 'anonymous';
      video.onerror = () => {
        clearTimeout(timer);
        finish('');
      };
      video.onloadeddata = () => {
        try {
          video.currentTime = Math.min(0.1, Math.max(0, (video.duration || 0) * 0.05));
        } catch {
          video.onseeked();
        }
      };
      video.onseeked = () => {
        clearTimeout(timer);
        try {
          const vw = video.videoWidth || 320;
          const vh = video.videoHeight || 240;
          const scale = Math.min(1, (maxWidth || 480) / vw, (maxHeight || 480) / vh);
          const cw = Math.max(1, Math.round(vw * scale));
          const ch = Math.max(1, Math.round(vh * scale));
          const canvas = document.createElement('canvas');
          canvas.width = cw;
          canvas.height = ch;
          canvas.getContext('2d').drawImage(video, 0, 0, cw, ch);
          finish(canvas.toDataURL('image/jpeg', 0.82));
        } catch {
          finish('');
        }
      };
      const s = String(filePath);
      video.src = /^https?:/i.test(s)
        ? '/__olaz/media?u=' + encodeURIComponent(s)
        : rewriteLocalZfileUrl('zfile://local/' + s.replace(/^\/+/, ''));
    });
  }

  function installPreloadGlobals() {
    const spec = window.__zaloBridgeSpec || { globals: [], members: {}, values: {}, ipc: {} };
    const setByPath = (dotPath, val) => {
      const parts = dotPath.split('.');
      let cur = window;
      for (let i = 0; i < parts.length - 1; i++) {
        const k = parts[i];
        if (cur[k] === undefined || cur[k] === null) cur[k] = {};
        cur = cur[k];
      }
      cur[parts[parts.length - 1]] = val;
    };

    for (const g of spec.globals || []) {
      if (window[g] === undefined) window[g] = {};
    }
    for (const [p, v] of Object.entries(spec.values || {})) {
      setByPath(p, v);
    }

    // Install all `bridge-table.json` IPC channels
    for (const [memberPath, info] of Object.entries(spec.ipc || {})) {
      const ch = info.channel;
      switch (info.type) {
        case 'REQUEST':
          setByPath(memberPath, (...args) =>
            ch ? handleAsyncIpc(ch, args) : Promise.resolve(undefined)
          );
          break;
        case 'REQUEST_SYNC':
          setByPath(memberPath, (...args) => (ch ? handleSyncIpc(ch, args) : undefined));
          break;
        case 'NOTIFY':
        case 'RAW_SEND':
          setByPath(memberPath, (...args) => {
            if (ch) handleAsyncIpc(ch, args).catch(() => {});
          });
          break;
        case 'SUBSCRIBE':
        case 'SUBSCRIBE_V2':
          setByPath(memberPath, (fn) => addIpcSub(ch, fn));
          break;
        case 'SUBSCRIBE_ONCE':
          setByPath(memberPath, (fn) => {
            if (typeof fn !== 'function') return;
            const onceWrapper = (ev, ...rest) => {
              removeIpcSub(ch, onceWrapper);
              fn(ev, ...rest);
            };
            addIpcSub(ch, onceWrapper);
          });
          break;
        case 'UNSUBSCRIBE':
          setByPath(memberPath, (fn) => removeIpcSub(ch, fn));
          break;
        case 'PROVIDE':
          setByPath(memberPath, (fn) => {
            if (!ch || typeof fn !== 'function') return;
            ipcProviders.set(ch, fn);
            if (ch === 'get-imei') {
              setTimeout(() => {
                try {
                  fn({ sender: {} }, { imei: hostImei });
                } catch {}
              }, 0);
            }
          });
          break;
        case 'STOP_PROVIDE':
          setByPath(memberPath, () => {
            if (ch) ipcProviders.delete(ch);
          });
          break;
      }
    }

    // `$zenv`
    window.$zenv = Object.assign(window.$zenv || {}, {
      isPreloaded: true,
      isRenderer: true,
      platform: 'darwin',
      process_arch: 'x64',
      arch: 'x64',
    });

    // `$znode.path`, `$znode.os`, `$znode.url`, `$znode.events`, `$znode.crypto`, `$znode.fs*`
    const znode = (window.$znode = window.$znode || {});
    const posixPath = {
      sep: '/',
      delimiter: ':',
      normalize: (p) => normPosixPath(p),
      join: (...segs) => normPosixPath(segs.filter(Boolean).join('/')),
      resolve: (...segs) => normPosixPath('/' + segs.filter(Boolean).join('/')),
      dirname: (p) => {
        const n = normPosixPath(p);
        const i = n.lastIndexOf('/');
        return i <= 0 ? '/' : n.slice(0, i);
      },
      basename: (p, ext) => {
        const n = normPosixPath(p);
        let b = n.slice(n.lastIndexOf('/') + 1);
        if (ext && b.endsWith(ext)) b = b.slice(0, -ext.length);
        return b;
      },
      extname: (p) => {
        const b = posixPath.basename(p);
        const i = b.lastIndexOf('.');
        return i <= 0 ? '' : b.slice(i);
      },
      isAbsolute: (p) => String(p || '').startsWith('/'),
      relative: (from, to) => {
        const f = posixPath.resolve(from).split('/').filter(Boolean);
        const t = posixPath.resolve(to).split('/').filter(Boolean);
        let i = 0;
        while (i < f.length && i < t.length && f[i] === t[i]) i++;
        return [...Array(f.length - i).fill('..'), ...t.slice(i)].join('/');
      },
      parse: (p) => {
        const dir = posixPath.dirname(p);
        const base = posixPath.basename(p);
        const ext = posixPath.extname(p);
        return { root: '/', dir, base, ext, name: ext ? base.slice(0, -ext.length) : base };
      },
      format: (o) => {
        const obj = o && typeof o === 'object' ? o : {};
        const dir = obj.dir || obj.root || '';
        const base = obj.base || (obj.name || '') + (obj.ext || '');
        return dir ? (dir === obj.root ? dir + base : dir + '/' + base) : base;
      },
      toNamespacedPath: (p) => p,
    };
    posixPath.posix = posixPath;
    znode.path = posixPath;

    const osInfo = (znode.os && znode.os.__info) || {
      release: '22.6.0',
      version: 'Darwin Kernel Version 22.6.0',
      arch: 'x64',
      hostname: 'OlazWeb',
      homedir: '/Users/olaz',
      tmpdir: VFS_TEMP,
      totalmem: 8 * 1024 * 1024 * 1024,
      freemem: 4 * 1024 * 1024 * 1024,
      cpus: navigator.hardwareConcurrency || 4,
      uptime: 3600,
      username: 'olaz',
    };
    znode.os = {
      platform: () => 'darwin',
      type: () => 'Darwin',
      release: () => osInfo.release || '22.6.0',
      version: () => osInfo.version || 'Darwin Kernel Version 22.6.0',
      arch: () => 'x64',
      hostname: () => osInfo.hostname || 'OlazWeb',
      homedir: () => '/Users/olaz',
      tmpdir: () => VFS_TEMP,
      endianness: () => 'LE',
      EOL: '\n',
      totalmem: () => osInfo.totalmem,
      freemem: () => osInfo.freemem,
      uptime: () => osInfo.uptime || 3600,
      loadavg: () => [0, 0, 0],
      cpus: () =>
        Array.from({ length: osInfo.cpus || 4 }, () => ({
          model: 'BrowserVCPU',
          speed: 2400,
          times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 },
        })),
      userInfo: () => ({ username: 'olaz', uid: 1000, gid: 1000, shell: '/bin/sh', homedir: '/Users/olaz' }),
      networkInterfaces: () => ({}),
    };

    znode.url = {
      URL,
      URLSearchParams,
      pathToFileURL: (p) => new URL('file://' + normPosixPath(p)),
      fileURLToPath: (u) => normPosixPath(typeof u === 'string' ? u : u.pathname),
    };

    znode.crypto = {
      encrypt: (...a) => decodeBridgeWireResult(cryptoBridge.dispatchSync('encrypt', a)),
      decrypt: (...a) => decodeBridgeWireResult(cryptoBridge.dispatchSync('decrypt', a)),
      hash: (...a) => decodeBridgeWireResult(cryptoBridge.dispatchSync('hash', a)),
      randomBytes: (n) => core.csprngBytes(n),
      createHash: (alg) => {
        const h = cryptoBridge.hashCreate([alg]);
        return {
          update(data, enc) {
            cryptoBridge.hashUpdate([h, data, enc]);
            return this;
          },
          digest(enc) {
            return decodeBridgeWireResult(cryptoBridge.hashDigest([h, enc]));
          },
        };
      },
      createHmac: (alg, key) => {
        const chunks = [];
        const keyBytes = core.decodeInput(key, 'utf8');
        return {
          update(data, enc) {
            chunks.push(core.decodeInput(data, enc));
            return this;
          },
          digest(enc) {
            const mac = core.hmacBytes(alg, keyBytes, core.concatU8(chunks));
            return decodeBridgeWireResult(core.encodeOutput(mac, enc));
          },
        };
      },
      createCipheriv: (alg, key, iv) => {
        const h = cryptoBridge.cipherCreate(['cipher', alg, key, iv]);
        return {
          setAutoPadding(ap) {
            cryptoBridge.cipherOption([h, 'autoPadding', ap]);
            return this;
          },
          setAAD(aad) {
            cryptoBridge.cipherSetAAD([h, aad]);
            return this;
          },
          update(data, inEnc, outEnc) {
            return decodeBridgeWireResult(cryptoBridge.cipherUpdate([h, data, inEnc, outEnc]));
          },
          final(outEnc) {
            return decodeBridgeWireResult(cryptoBridge.cipherFinal([h, outEnc]));
          },
          getAuthTag() {
            const w = cryptoBridge.cipherGetAuthTag([h]);
            return w ? core.decodeBase64Forgiving(w.__bytes) : new Uint8Array(0);
          },
        };
      },
      createDecipheriv: (alg, key, iv) => {
        const h = cryptoBridge.cipherCreate(['decipher', alg, key, iv]);
        return {
          setAutoPadding(ap) {
            cryptoBridge.cipherOption([h, 'autoPadding', ap]);
            return this;
          },
          setAAD(aad) {
            cryptoBridge.cipherSetAAD([h, aad]);
            return this;
          },
          setAuthTag(tag) {
            cryptoBridge.cipherSetAuthTag([h, tag]);
            return this;
          },
          update(data, inEnc, outEnc) {
            return decodeBridgeWireResult(cryptoBridge.cipherUpdate([h, data, inEnc, outEnc]));
          },
          final(outEnc) {
            return decodeBridgeWireResult(cryptoBridge.cipherFinal([h, outEnc]));
          },
        };
      },
    };

    const wrapStatObj = (st) => ({
      size: st.size,
      mtimeMs: st.mtimeMs,
      ctimeMs: st.ctimeMs,
      atimeMs: st.atimeMs,
      birthtimeMs: st.birthtimeMs,
      mtime: new Date(st.mtimeMs),
      ctime: new Date(st.ctimeMs),
      atime: new Date(st.atimeMs),
      birthtime: new Date(st.birthtimeMs),
      mode: st.mode,
      isFile: () => st.isFile,
      isDirectory: () => st.isDirectory,
      isSymbolicLink: () => false,
    });

    const fsSync = {
      constants: { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1 },
      existsSync: (p) => vfsEntriesRam.has(normPosixPath(p)),
      accessSync: (p) => {
        const np = normPosixPath(p);
        if (!vfsEntriesRam.has(np)) throw makeFsErr('ENOENT', np);
      },
      statSync: (p) => {
        const np = normPosixPath(p);
        const e = vfsEntriesRam.get(np);
        if (!e) throw makeFsErr('ENOENT', np);
        return wrapStatObj(formatStatEntry(e));
      },
      lstatSync: (p) => fsSync.statSync(p),
      mkdirSync: (p) => {
        const np = normPosixPath(p);
        ensureVfsDirsInRam(np + '/.keep');
        if (!vfsEntriesRam.has(np)) {
          const dirEntry = { path: np, isDir: true, size: 0, mtimeMs: Date.now(), data: null };
          vfsEntriesRam.set(np, dirEntry);
          persistVfsEntry(dirEntry);
        }
        return np;
      },
      readdirSync: (p) => {
        const np = normPosixPath(p);
        const prefix = np === '/' ? '/' : np + '/';
        const names = new Set();
        for (const k of vfsEntriesRam.keys()) {
          if (k.startsWith(prefix)) {
            const rest = k.slice(prefix.length);
            const seg = rest.split('/')[0];
            if (seg) names.add(seg);
          }
        }
        return Array.from(names);
      },
      readFileSync: (p, enc) => {
        const np = normPosixPath(p);
        const e = vfsEntriesRam.get(np);
        if (!e || e.isDir) throw makeFsErr('ENOENT', np);
        const clean = core.denoiseBuffer(e.data || new Uint8Array(0));
        const encStr = typeof enc === 'string' ? enc : enc && enc.encoding;
        return encStr ? core.encodeOutput(clean, encStr) : clean;
      },
      writeFileSync: (p, data, enc) => {
        const bytes =
          typeof data === 'string'
            ? core.decodeString(data, typeof enc === 'string' ? enc : (enc && enc.encoding) || 'utf8')
            : core.toU8(data);
        writeVfsFileSync(p, bytes);
      },
      unlinkSync: (p) => {
        removeVfsEntry(normPosixPath(p));
      },
      rmSync: (p) => {
        const np = normPosixPath(p);
        for (const k of Array.from(vfsEntriesRam.keys())) {
          if (k === np || k.startsWith(np + '/')) removeVfsEntry(k);
        }
      },
      rmdirSync: (p) => fsSync.rmSync(p),
      renameSync: (src, dst) => {
        const s = normPosixPath(src);
        const d = normPosixPath(dst);
        const e = vfsEntriesRam.get(s);
        if (!e) throw makeFsErr('ENOENT', s);
        if (!e.isDir) writeVfsFileSync(d, e.data || new Uint8Array(0));
        removeVfsEntry(s);
      },
      copyFileSync: (src, dst) => {
        const s = normPosixPath(src);
        const e = vfsEntriesRam.get(s);
        if (!e || e.isDir) throw makeFsErr('ENOENT', s);
        writeVfsFileSync(dst, e.data || new Uint8Array(0));
      },
    };

    const fsPromise = {
      access: async (p) => fsSync.accessSync(p),
      stat: async (p) => fsSync.statSync(p),
      lstat: async (p) => fsSync.lstatSync(p),
      mkdir: async (p) => fsSync.mkdirSync(p),
      readdir: async (p) => fsSync.readdirSync(p),
      readFile: async (p, enc) => fsSync.readFileSync(p, enc),
      writeFile: async (p, d, enc) => fsSync.writeFileSync(p, d, enc),
      unlink: async (p) => fsSync.unlinkSync(p),
      rm: async (p) => fsSync.rmSync(p),
      rmdir: async (p) => fsSync.rmdirSync(p),
      rename: async (s, d) => fsSync.renameSync(s, d),
      copyFile: async (s, d) => fsSync.copyFileSync(s, d),
      link: async (s, d) => fsSync.copyFileSync(s, d),
    };

    const withNodeCb = (promiseFn) => (...args) => {
      const cb = typeof args[args.length - 1] === 'function' ? args.pop() : null;
      const p = promiseFn(...args);
      if (cb) {
        p.then((res) => cb(null, res), (err) => cb(err));
        return undefined;
      }
      return p;
    };

    znode.fs = Object.assign({}, fsSync, {
      constanst: fsSync.constants,
      promises: fsPromise,
      access: withNodeCb(fsPromise.access),
      exists: (p, cb) => {
        const ok = fsSync.existsSync(p);
        if (typeof cb === 'function') cb(ok);
        return Promise.resolve(ok);
      },
      stat: withNodeCb(fsPromise.stat),
      lstat: withNodeCb(fsPromise.lstat),
      mkdir: withNodeCb(fsPromise.mkdir),
      readdir: withNodeCb(fsPromise.readdir),
      readFile: withNodeCb(fsPromise.readFile),
      writeFile: withNodeCb(fsPromise.writeFile),
      unlink: withNodeCb(fsPromise.unlink),
      rm: withNodeCb(fsPromise.rm),
      rmdir: withNodeCb(fsPromise.rmdir),
      rename: withNodeCb(fsPromise.rename),
      copyFile: withNodeCb(fsPromise.copyFile),
      link: withNodeCb(fsPromise.link),
    });
    znode.fsPromise = fsPromise;
    znode.fsExtra = Object.assign({}, znode.fs, {
      ensureDir: async (p) => fsSync.mkdirSync(p),
      ensureDirSync: (p) => fsSync.mkdirSync(p),
      pathExists: async (p) => fsSync.existsSync(p),
      remove: async (p) => fsSync.rmSync(p),
      removeSync: (p) => fsSync.rmSync(p),
      copy: async (s, d) => fsSync.copyFileSync(s, d),
      move: async (s, d) => fsSync.renameSync(s, d),
      readJson: async (p) => JSON.parse(fsSync.readFileSync(p, 'utf8')),
      writeJson: async (p, obj) => fsSync.writeFileSync(p, JSON.stringify(obj, null, 2), 'utf8'),
    });
    znode.rimraf = async (p) => fsSync.rmSync(p);
    const unusedSync = (p) => {
      let cand = normPosixPath(p);
      let idx = 1;
      const parsed = posixPath.parse(cand);
      while (fsSync.existsSync(cand)) {
        cand = parsed.dir + '/' + parsed.name + ' (' + idx++ + ')' + parsed.ext;
      }
      return cand;
    };
    const unusedAsync = async (p) => unusedSync(p);
    unusedAsync.sync = unusedSync;
    znode.unusedFilename = unusedAsync;

    znode.shell = {
      openPath: async () => '',
      openExternal: async (u) => handleAsyncIpc('__shell__', ['openExternal', u]),
      showItemInFolder: async () => undefined,
    };

    const buildDiskMapProxy = () => {
      const rootEntry = {
        label: '/',
        mountpoint: '/',
        totalSpace: 100e9,
        freeSpace: 50e9,
        total: 100e9,
        free: 50e9,
      };
      const disks = { '/': rootEntry };
      return new Proxy(disks, {
        get(target, key) {
          if (typeof key === 'symbol') return target[key];
          if (Object.prototype.hasOwnProperty.call(target, key)) return target[key];
          if (typeof key === 'string' && key.startsWith('/')) return rootEntry;
          return target[key];
        },
      });
    };

    znode.nativelibs = {
      zfileAddon: {
        canRead: (p) => fsSync.existsSync(p),
        canWrite: () => true,
        canReadAndWrite: () => true,
        getDiskInfo: async () => ({ freeBytes: 50e9, totalBytes: 100e9 }),
        getInfo: async (p) => (fsSync.existsSync(p) ? fsSync.statSync(p) : null),
        copyFolder: async () => true,
        cancelCopy: () => {},
      },
      zfile: () => ({
        canRead: (p) => fsSync.existsSync(p),
        canWrite: () => true,
        canReadAndWrite: () => true,
        getInfo: async (p) => (fsSync.existsSync(p) ? fsSync.statSync(p) : null),
        getDiskInfo: async () => buildDiskMapProxy(),
        diskInfo: async () => buildDiskMapProxy(),
        copyFolder: async (_src, _dst, cb) => {
          if (typeof cb === 'function') cb(null, true);
          return true;
        },
        cancelCopy: async () => undefined,
      }),
      fileUtilsAddon: {
        getDiskUsage: () => ({ available: 50e9, free: 50e9, total: 100e9 }),
      },
      fileUtils: () => znode.nativelibs.fileUtilsAddon,
      fileUtilitiesAddon: {
        getDirectorySizeSync: () => ({ totalSize: 0, fileCount: 0 }),
        getDirectorySizeAsync: async () => ({ totalSize: 0, fileCount: 0 }),
        getDirectorySizeTreeSync: () => ({ totalSize: 0, children: [] }),
        getDirectorySizeTreeAsync: async () => ({ totalSize: 0, children: [] }),
        getDirectorySizeByGlobSync: () => ({ totalSize: 0, fileCount: 0 }),
        getDirectorySizeByGlobAsync: async () => ({ totalSize: 0, fileCount: 0 }),
        detectHardlinksSync: () => [],
        detectHardlinksAsync: async () => [],
        detectFilesystemSync: () => ({ filesystemType: 'ext4' }),
        detectFilesystemAsync: async () => ({ filesystemType: 'ext4' }),
        cancelJob: () => {},
      },
      fileUtilities: () => znode.nativelibs.fileUtilitiesAddon,
      dbUtilsAddon: {
        decompressAndDecryptDb_V2: () => null,
        decompressAndDecryptDb: () => null,
        parseBinNet: () => ({}),
      },
      dbUtils: () => znode.nativelibs.dbUtilsAddon,
      zcallAddon: {},
      zcall: () => znode.nativelibs.zcallAddon,
      mp4thumb: () => ({
        generateThumbnail: async (inp, out, mw, mh) => {
          const dataUrl = await generateVideoThumbnailBrowser(inp, mw, mh);
          if (!dataUrl || !dataUrl.includes(',')) return '';
          const u8 = core.decodeBase64Forgiving(dataUrl.split(',')[1]);
          writeVfsFileSync(out, u8);
          return out;
        },
        cancel: () => {},
      }),
    };

    // `$zelectron` (`getElectronStore` factory + `Store` class + sync/async helpers)
    const getDotPath = (obj, key) =>
      String(key)
        .split('.')
        .reduce((o, k) => (o == null || typeof o !== 'object' ? undefined : o[k]), obj);
    const setDotPath = (obj, key, val) => {
      const parts = String(key).split('.');
      let cur = obj;
      for (let i = 0; i < parts.length - 1; i++) {
        if (cur[parts[i]] == null || typeof cur[parts[i]] !== 'object') cur[parts[i]] = {};
        cur = cur[parts[i]];
      }
      cur[parts[parts.length - 1]] = val;
    };
    const deleteDotPath = (obj, key) => {
      const parts = String(key).split('.');
      let cur = obj;
      for (let i = 0; i < parts.length - 1; i++) {
        if (cur == null || typeof cur !== 'object') return;
        cur = cur[parts[i]];
      }
      if (cur && typeof cur === 'object') delete cur[parts[parts.length - 1]];
    };
    const cloneStoreVal = (v) => (v === undefined ? undefined : JSON.parse(JSON.stringify(v)));

    class BrowserElectronStore {
      constructor(opts = {}) {
        const cleanOpts = opts && typeof opts === 'object' ? opts : {};
        this._name = String(cleanOpts.name || 'config');
        this._defaults =
          cleanOpts.defaults && typeof cleanOpts.defaults === 'object'
            ? cloneStoreVal(cleanOpts.defaults)
            : {};
        this._listeners = new Set();
      }
      get path() {
        return VFS_USER_DATA + '/' + this._name + '.json';
      }
      _readMerged() {
        return Object.assign({}, this._defaults, readNamedStoreSync(this._name));
      }
      _persist(nextObj, prevObj) {
        writeNamedStoreSync(this._name, nextObj);
        for (const fn of Array.from(this._listeners)) {
          try {
            fn(nextObj, prevObj);
          } catch {}
        }
      }
      get store() {
        return cloneStoreVal(this._readMerged());
      }
      set store(val) {
        const old = cloneStoreVal(this._readMerged());
        const next = Object.assign({}, val && typeof val === 'object' ? val : {});
        this._persist(next, old);
      }
      get size() {
        return Object.keys(this._readMerged()).length;
      }
      get(key, defaultValue) {
        const obj = this._readMerged();
        if (key === undefined) return cloneStoreVal(obj);
        const v = getDotPath(obj, key);
        return v === undefined ? defaultValue : cloneStoreVal(v);
      }
      set(key, val) {
        const obj = this._readMerged();
        const old = cloneStoreVal(obj);
        if (key && typeof key === 'object') {
          for (const [k, v] of Object.entries(key)) setDotPath(obj, k, v);
        } else {
          setDotPath(obj, key, val);
        }
        this._persist(obj, old);
      }
      has(key) {
        return getDotPath(this._readMerged(), key) !== undefined;
      }
      delete(key) {
        const obj = this._readMerged();
        const old = cloneStoreVal(obj);
        deleteDotPath(obj, key);
        this._persist(obj, old);
      }
      clear() {
        const old = cloneStoreVal(this._readMerged());
        this._persist({}, old);
      }
      reset(...keys) {
        const obj = this._readMerged();
        const old = cloneStoreVal(obj);
        for (const k of keys) {
          const defVal = getDotPath(this._defaults, k);
          if (defVal === undefined) deleteDotPath(obj, k);
          else setDotPath(obj, k, cloneStoreVal(defVal));
        }
        this._persist(obj, old);
      }
      onDidChange(key, cb) {
        if (typeof cb !== 'function') return () => {};
        const listener = (n, o) => {
          const a = getDotPath(n, key);
          const b = getDotPath(o, key);
          if (JSON.stringify(a) !== JSON.stringify(b)) cb(a, b);
        };
        this._listeners.add(listener);
        return () => this._listeners.delete(listener);
      }
      onDidAnyChange(cb) {
        if (typeof cb !== 'function') return () => {};
        this._listeners.add(cb);
        return () => this._listeners.delete(cb);
      }
      openInEditor() {}
    }

    const electronStoresByKey = new Map();
    const getElectronStore = (opts) => {
      const cacheKey = (opts && opts.name) || 'default';
      if (!electronStoresByKey.has(cacheKey)) {
        electronStoresByKey.set(cacheKey, new BrowserElectronStore(opts));
      }
      return electronStoresByKey.get(cacheKey);
    };

    window.$zelectron = Object.assign(window.$zelectron || {}, {
      Store: BrowserElectronStore,
      getElectronStore,
      storePath: (n) => VFS_USER_DATA + '/' + (n || 'config') + '.json',
      storeRead: (n) => readNamedStoreSync(n),
      storeWrite: (n, o) => writeNamedStoreSync(n, o),
      getCookies: (f) => Promise.resolve(cookieJar.getCookies(f)),
      removeCookie: (u, n) => Promise.resolve(cookieJar.deleteCookie(u, n)),
    });

    let lastClipText = '';
    const writeClipText = (t) => {
      lastClipText = String(t == null ? '' : t);
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(lastClipText).catch(() => {});
      }
    };
    const emptyUsageBucket = { count: 0, size: 0, liveSize: 0 };
    window.$zelectronNative = Object.assign(window.$zelectronNative || {}, {
      lockWebframeZoom: () => undefined,
      setZoomFactor: () => undefined,
      clipboardText: () => lastClipText,
      getClipboardText: () => lastClipText,
      readClipboard: () => lastClipText,
      clipboardWriteText: writeClipText,
      writeTextToClipboard: writeClipText,
      clipboardFormats: () => (lastClipText ? ['text/plain'] : []),
      availableClipboardFormats: () => (lastClipText ? ['text/plain'] : []),
      clipboardImagePng: () => null,
      clipboardWriteImagePng: () => {},
      getClipboardImage: () => ({
        isEmpty: () => true,
        toJPEG: () => new Uint8Array(0),
        toPNG: () => new Uint8Array(0),
      }),
      getResourceUsage: () => ({
        images: emptyUsageBucket,
        scripts: emptyUsageBucket,
        cssStyleSheets: emptyUsageBucket,
        xslStyleSheets: emptyUsageBucket,
        fonts: emptyUsageBucket,
        other: emptyUsageBucket,
      }),
      clearCache: () => {},
      getAvailableHeapSize: () => 4 * 1024 * 1024 * 1024,
    });

    // `$zFileManager`
    window.$zFileManager = Object.assign(window.$zFileManager || {}, {
      getSentryConfig: () => null,
      saveSentryConfig: async () => {},
      getViewerkey: async () => cachedViewerKey,
      writeRecentId: async () => {},
      writeZLog: async () => {},
      writeZLogBetween: async () => {},
      readZLog: async () => '',
      scandirLog: async () => [],
      getExtraLogFiles: async () => [],
      getQosLog: async () => '',
      getActionLog: async () => '',
      readUptimeLog: async () => '',
      removeQosLog: async () => {},
      removeActionLog: async () => {},
      writeActionLog: async () => {},
      writeUptimeLog: async () => {},
      truncateFileUpto: async () => {},
      cleanupLogFileDescriptors: async () => {},
      initBinaryLogFile: async () => {},
      sealCalfLog: async () => {},
      mergeCalfLogGroup: async () => {},
      writeLogRange: async () => {},
      readLogRange: async () => new ArrayBuffer(0),
      trustBlob: async () => true,
      isTrustPath: async () => true,
      trustPathFromBlob: async () => true,
      getInstallSource: async () => 'dmg',
      getFileArrayBuffer: async (p) => {
        const u8 = fsSync.readFileSync(p);
        return u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength);
      },
      readFileAsync: async (p) => fsSync.readFileSync(p),
      svcStatAsync: async (p) => formatStatEntry(vfsEntriesRam.get(normPosixPath(p)) || { isDir: false, size: 0, mtimeMs: 0 }),
      svcReadPath: async (p) => fsSync.readFileSync(p),
      readFileSlice: async (p, start, len) => {
        const u8 = fsSync.readFileSync(p);
        const s = Math.max(0, Number(start) || 0);
        const sub = u8.slice(s, s + Math.max(0, Number(len) || 0));
        return sub.buffer;
      },
      zfileFindNoiseSignature: async (p) => {
        const e = vfsEntriesRam.get(normPosixPath(p));
        if (!e || !e.data) return -1;
        const info = core.inspectBufferNoise(e.data, true);
        return info.signatureOffset === null ? -1 : info.signatureOffset;
      },
      getMd5LargeFileObject: async (p) => {
        const u8 = fsSync.readFileSync(p);
        return { checksum: core.bytesToHex(core.hashBytes('md5', u8)), size: u8.byteLength };
      },
      zfileGetChecksum: async (p) => core.bytesToHex(core.hashBytes('md5', fsSync.readFileSync(p))),
      testChecksum: async (p, expected) => {
        const actual = core.bytesToHex(core.hashBytes('md5', fsSync.readFileSync(p)));
        return actual.toLowerCase() === String(expected || '').toLowerCase();
      },
      writeBufferToPath: async (p, buf) => {
        writeVfsFileSync(p, core.toU8(buf));
        return true;
      },
      appendBufferToPath: async (p, buf) => {
        const np = normPosixPath(p);
        const prev = vfsEntriesRam.get(np);
        const combined = prev && prev.data ? core.concatU8([prev.data, core.toU8(buf)]) : core.toU8(buf);
        writeVfsFileSync(np, combined);
        return true;
      },
      zcloudCloneAndMatchChecksum: async (src, dst) => {
        fsSync.copyFileSync(src, dst);
        return true;
      },
      zcloudDenoiseFile: async (src, dst) => {
        const e = vfsEntriesRam.get(normPosixPath(src));
        if (!e || !e.data) throw makeFsErr('ENOENT', src);
        writeVfsFileSync(dst, core.denoiseBuffer(e.data));
        return true;
      },
      zcloudNoiseFile: async (src, dst) => {
        const e = vfsEntriesRam.get(normPosixPath(src));
        if (!e || !e.data) throw makeFsErr('ENOENT', src);
        writeVfsFileSync(dst, core.noiseBuffer(e.data));
        return true;
      },
      generateVideoThumbnail: async (inp, out, mw, mh) => {
        const dataUrl = await generateVideoThumbnailBrowser(inp, mw, mh);
        if (!dataUrl || !dataUrl.includes(',')) return '';
        writeVfsFileSync(out, core.decodeBase64Forgiving(dataUrl.split(',')[1]));
        return out;
      },
    });

    const unsupportedFeature = (featureName) => () =>
      Promise.reject(
        Object.assign(new Error(featureName + ' is not available in Olaz Web'), { code: 'ENOTSUP' })
      );

    let keepDelState = null;
    const createKeepDelDb = (initialConfig) => {
      if (!keepDelState) {
        keepDelState = {
          config: initialConfig || {},
          data: null,
          pending: [],
          trans: 0,
          get storeName() {
            return (
              'keepdel-' +
              String(this.config.db_name || 'db')
                .replace('$0$', this.config.userId || '')
                .replace(/[^\w.-]/g, '_')
            );
          },
          load() {
            if (!this.data) {
              const s = readNamedStoreSync(this.storeName) || {};
              this.data = Array.isArray(s.items) ? s.items.slice() : [];
            }
          },
          save(force) {
            if (!this.data) return;
            const limit = this.config.keep_limit || 5000;
            const batch = this.config.trans_batch || 1;
            if (!force && this.trans < batch) return;
            this.trans = 0;
            if (this.data.length > limit) this.data.splice(0, this.data.length - limit);
            writeNamedStoreSync(this.storeName, { items: this.data });
          },
          add(item, front) {
            if (!item) return;
            this.trans++;
            if (!this.data) {
              this.pending.push(item);
              return;
            }
            for (const it of [...this.pending.splice(0), item]) {
              const json = JSON.stringify(it);
              if (front) this.data.unshift(json);
              else this.data.push(json);
            }
            this.save(false);
          },
        };
      }
      keepDelState.config = Object.assign({}, keepDelState.config, initialConfig || {});
      return {
        loadConfig: (c) => {
          keepDelState.config = Object.assign({}, keepDelState.config, c || {});
        },
        setup: () => {
          keepDelState.load();
        },
        destroy: () => {
          keepDelState.save(true);
          keepDelState.data = null;
          keepDelState.trans = 0;
        },
        uninstall: async () => {
          keepDelState.data = [];
          writeNamedStoreSync(keepDelState.storeName, { items: [] });
        },
        connect: async () => {
          keepDelState.load();
        },
        unshift: (item) => keepDelState.add(item, true),
        push: (item) => keepDelState.add(item, false),
        pop: () => {
          if (!keepDelState.data) return undefined;
          keepDelState.trans++;
          const v = keepDelState.data.pop();
          keepDelState.save(false);
          try {
            return v ? JSON.parse(v) : undefined;
          } catch {
            return undefined;
          }
        },
      };
    };

    const createDetachCleaner = (target) => ({
      start: () => fsPromise.rm(String(target)).then(() => 0, () => 0),
      stop: () => Promise.resolve(),
    });

    const genThumbFn = (p) => generateVideoThumbnailBrowser(p, 480, 480);
    genThumbFn.writeSavePathFilePhoto = (...a) =>
      handleAsyncIpc('gen-thumb_write-savepath-file-photo', a);
    window.$zFeatures = Object.assign(window.$zFeatures || {}, {
      genThumb: genThumbFn,
      libvips: {
        resizeImage: unsupportedFeature('libvips'),
      },
      libjxl: {
        decodeToJpeg: unsupportedFeature('libjxl'),
        encodeJxl: unsupportedFeature('libjxl'),
        getJxlInfo: unsupportedFeature('libjxl'),
        jxlDecompressMulti: unsupportedFeature('libjxl'),
        moduleReady: () => Promise.resolve(false),
      },
      fileRes: {
        createKeepDelDb,
        createDetachCleaner,
      },
    });

    window.$zconfig = Object.assign(window.$zconfig || {}, {
      getAutostartState: async () => ({ openAtLogin: false, startInTray: false }),
      setStartInTray: async () => ({ openAtLogin: false, startInTray: false }),
      migrateLegacyAutoLaunch: async () => ({ openAtLogin: false, startInTray: false }),
    });

    window.$zpreload = Object.assign(window.$zpreload || {}, {
      triggerWindowLoadedEvent: () => undefined,
    });

    window.$zcommon = Object.assign(window.$zcommon || {}, {
      send: () => undefined,
      receive: () => undefined,
      remove: () => undefined,
    });

    // `$zsqlite` & `MetriczGlobal` parity with `local-modules.js`
    const noSqlite = (what) =>
      Promise.reject(
        Object.assign(new Error('SQLite is not available in Olaz Web (' + what + ')'), {
          code: 'ENOTSUP',
        })
      );
    const sqliteMethods = [
      'openDb',
      'query',
      'queryV2',
      'queryGlobalMessage',
      'queryGlobalMessageV2',
      'queryGlobalMessageV3',
      'queryGlobalMessageV4',
      'deleteRow',
      'deleteRows',
      'write',
      'writev2',
      'pureQuery',
      'prepareInsertSearch',
      'prepareDeleteSearch',
      'getConversationMatch',
      'flushDb',
      'flushDbBatchv2',
      'flushDbBatch',
      'closeDb',
      'finalizeInsertStatement',
      'finalizeDeleteStatement',
      'doListSqls',
      'createKey',
      'initDebug',
      'countTotal',
    ];
    const sqliteStub = {
      isLoadedLib: () => false,
      isDbTransactionStepError: () => false,
      createConnection: () =>
        new Proxy(
          {},
          {
            get: (_c, m) =>
              typeof m === 'string' && /^(open|close|run|get|each|all|exec|prepare)/.test(m)
                ? () => noSqlite(m)
                : undefined,
          }
        ),
    };
    for (const m of sqliteMethods) sqliteStub[m] = () => noSqlite(m);
    window.$zsqlite = sqliteStub;

    window.MetriczGlobal = {
      getProcessStats: () => ({ heapUsed: 0, heapTotal: 0, rss: 0, cpu: 0 }),
      getProcessMemory: () => 0,
      startCPUMonitor: () => {},
      stopCPUMonitor: () => {},
      getCPUUsage: () => 0,
      onCPUChange: () => {},
      removeCPUChangeListener: () => {},
      getArch: () => 'x64',
    };

    // `$zMsgChannel` in-renderer utility-process-media port
    const portV2Subs = new Set();
    const establishedSubs = new Set();
    let chanSeq = 0;
    const createMediaPort = () => {
      const ch = new MessageChannel();
      ch.port2.onmessage = async (ev) => {
        const msg = ev.data || {};
        try {
          if (msg.type === 'PING') {
            ch.port2.postMessage({ id: msg.id, result: 'PONG' });
          } else {
            ch.port2.postMessage({ id: msg.id, result: true });
          }
        } catch (err) {
          ch.port2.postMessage({ id: msg.id, error: String(err) });
        }
      };
      if (ch.port1.start) ch.port1.start();
      if (ch.port2.start) ch.port2.start();
      return ch.port1;
    };
    window.$zMsgChannel = Object.assign(window.$zMsgChannel || {}, {
      onReceiveTransferredPortV2: (cb) => {
        if (typeof cb === 'function') portV2Subs.add(cb);
      },
      removeListenReceiveTransferredPortV2: (cb) => portV2Subs.delete(cb),
      onChannelEstablished: (cb) => {
        if (typeof cb === 'function') establishedSubs.add(cb);
      },
      removeListenChannelEstablished: (cb) => establishedSubs.delete(cb),
      requestOpenChannelTo: (fromScript, targetScript, channelID) => {
        if (targetScript === 'utility-process-media') {
          const port1 = createMediaPort();
          const sid = ++chanSeq;
          setTimeout(() => {
            const meta = {
              nameOfCurrentScript: fromScript,
              nameOfTargetScript: targetScript,
              channelID,
              channelSessionID: sid,
              isReconnect: false,
            };
            for (const cb of Array.from(portV2Subs)) {
              try {
                cb({ ports: [port1] }, meta);
              } catch {}
            }
          }, 0);
        }
      },
      notifyPortDelivered: (fromScript, targetScript, channelID) => {
        if (targetScript === 'utility-process-media') {
          setTimeout(() => {
            for (const cb of Array.from(establishedSubs)) {
              try {
                cb({}, [fromScript, targetScript, channelID]);
              } catch {}
            }
          }, 0);
        }
      },
    });
    window.__olazDenoiseU8 = (raw) => core.denoiseBuffer(raw);

    // Ensure `navigator.connection` exists (Firefox/Safari lack Chromium's Network Information API)
    try {
      if (window.navigator && !('connection' in window.navigator) && typeof EventTarget !== 'undefined') {
        const conn = new EventTarget();
        Object.assign(conn, {
          effectiveType: '4g',
          type: 'wifi',
          downlink: 10,
          downlinkMax: Infinity,
          rtt: 50,
          saveData: false,
          onchange: null,
        });
        conn.addEventListener('change', (e) => {
          if (typeof conn.onchange === 'function') conn.onchange(e);
        });
        Object.defineProperty(Object.getPrototypeOf(window.navigator), 'connection', {
          configurable: true,
          get: () => conn,
        });
      }
    } catch {}

    // Wrap `$z*` bridge namespaces in a recursive fallback hybrid Proxy (`bridge.js` parity)
    // so any unmapped preload member degrades cleanly to a callable/indexable stub rather than throwing.
    const fallbackHybridCache = new Map();
    const wrappedRealObjects = new WeakMap();
    const NON_STUB_PROPS = new Set([
      'then',
      'toJSON',
      'prototype',
      'constructor',
      'valueOf',
      'toString',
      '$$typeof',
      'nodeType',
      '__esModule',
    ]);
    const createFallbackHybrid = (dotPath) => {
      if (fallbackHybridCache.has(dotPath)) return fallbackHybridCache.get(dotPath);
      const leaf = dotPath.slice(dotPath.lastIndexOf('.') + 1);
      const stubFn = (...args) => {
        if (/^(on|once)[A-Z]/.test(leaf)) {
          const cb = args.find((a) => typeof a === 'function');
          if (!cb) return () => {};
          addIpcSub(dotPath, cb);
          return () => removeIpcSub(dotPath, cb);
        }
        if (/^remove(Listen|Listener)/.test(leaf)) {
          const targetPath = dotPath.replace(
            /\.(removeListen|removeListener|off)([A-Z])/,
            (_m, _p, c) => '.on' + c
          );
          const cb = args.find((a) => typeof a === 'function');
          removeIpcSub(targetPath, cb);
          return undefined;
        }
        if (/Sync$/.test(leaf)) return undefined;
        const cb = args.find((a) => typeof a === 'function');
        if (cb) setTimeout(() => cb(null), 0);
        return Promise.resolve(undefined);
      };
      const proxy = new Proxy(stubFn, {
        get(target, prop) {
          if (typeof prop === 'symbol') return target[prop];
          if (NON_STUB_PROPS.has(prop)) return undefined;
          if (prop === 'bind' || prop === 'call' || prop === 'apply') {
            return Function.prototype[prop].bind(target);
          }
          return createFallbackHybrid(dotPath + '.' + prop);
        },
      });
      fallbackHybridCache.set(dotPath, proxy);
      return proxy;
    };
    const wrapBridgeObject = (obj, dotPath) => {
      if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return obj;
      if (wrappedRealObjects.has(obj)) return wrappedRealObjects.get(obj);
      const proxy = new Proxy(obj, {
        get(target, prop, receiver) {
          if (typeof prop === 'symbol') return Reflect.get(target, prop, receiver);
          if (Reflect.has(target, prop)) {
            const val = Reflect.get(target, prop, receiver);
            if (
              val &&
              typeof val === 'object' &&
              Object.getPrototypeOf(val) === Object.prototype
            ) {
              return wrapBridgeObject(val, dotPath + '.' + prop);
            }
            return val;
          }
          if (NON_STUB_PROPS.has(prop)) return undefined;
          return createFallbackHybrid(dotPath + '.' + prop);
        },
      });
      wrappedRealObjects.set(obj, proxy);
      return proxy;
    };
    for (const g of spec.globals || []) {
      if (g === '$zsqlite' || g === 'MetriczGlobal' || g === '$zenv') continue;
      if (window[g] && typeof window[g] === 'object') {
        window[g] = wrapBridgeObject(window[g], g);
      }
    }

    // Rewrite DOM `<img>` / `<video>` / `<audio>` `zfile://` & cross-origin media URLs
    const rewriteMediaUrl = (u) => {
      if (typeof u !== 'string') return u;
      if (/^(zfile:|file:)/i.test(u)) return rewriteLocalZfileUrl(u);
      const target = parseZaloTargetUrl(u);
      if (target) return '/__olaz/media?u=' + encodeURIComponent(target.href);
      return u;
    };
    for (const [Ctor, prop, fn] of [
      [window.HTMLImageElement, 'src', rewriteLocalZfileUrl],
      [window.HTMLMediaElement, 'src', rewriteMediaUrl],
      [window.HTMLSourceElement, 'src', rewriteMediaUrl],
    ]) {
      if (Ctor && Ctor.prototype) {
        const desc = Object.getOwnPropertyDescriptor(Ctor.prototype, prop);
        if (desc && desc.set) {
          Object.defineProperty(Ctor.prototype, prop, Object.assign({}, desc, {
            set(v) {
              return desc.set.call(this, fn(v));
            },
          }));
        }
      }
    }

    if (typeof window.open === 'function') {
      const origOpen = window.open;
      window.open = function (url, target, features) {
        return origOpen.call(this, appendProfileParam(url), target, features);
      };
    }
  }

  // ==========================================================================
  // 8. KV Zalo Branding, Multi-Account PWA, Media Session & Mobile Controller
  // ==========================================================================

  function renderKvZaloLogoSvg(uid) {
    const s = String(uid || 'd').replace(/[^A-Za-z0-9_-]/g, '');
    return (
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" aria-hidden="true" focusable="false">' +
      '<defs>' +
      '<linearGradient id="kvz-bg-' + s + '" x1="16" y1="8" x2="112" y2="120" gradientUnits="userSpaceOnUse">' +
      '<stop offset="0%" stop-color="#1A7CFF"/>' +
      '<stop offset="52%" stop-color="#0068FF"/>' +
      '<stop offset="100%" stop-color="#0050CC"/>' +
      '</linearGradient>' +
      '<linearGradient id="kvz-pt-' + s + '" x1="36" y1="30" x2="92" y2="74" gradientUnits="userSpaceOnUse">' +
      '<stop offset="0%" stop-color="#FFFFFF"/>' +
      '<stop offset="100%" stop-color="#EBF3FF"/>' +
      '</linearGradient>' +
      '<linearGradient id="kvz-pf-' + s + '" x1="56" y1="48" x2="98" y2="90" gradientUnits="userSpaceOnUse">' +
      '<stop offset="0%" stop-color="#DCEBFF"/>' +
      '<stop offset="100%" stop-color="#B9D7FF"/>' +
      '</linearGradient>' +
      '</defs>' +
      '<rect x="6" y="6" width="116" height="116" rx="28" fill="url(#kvz-bg-' + s + ')"/>' +
      '<rect x="7" y="7" width="114" height="114" rx="27" fill="none" stroke="#FFFFFF" stroke-opacity="0.18" stroke-width="2"/>' +
      '<path d="M64 28C42.46 28 26 42.1 26 59.5C26 68.77 30.84 77.1 38.62 82.88L33.8 97.25C33.12 99.28 35.18 101.08 37.09 100.12L53.88 91.72C57.15 92.56 60.53 93 64 93C85.54 93 102 78.9 102 61.5C102 42.99 85.54 28 64 28Z" fill="url(#kvz-pt-' + s + ')"/>' +
      '<path d="M64 28C85.54 28 102 42.99 102 61.5C102 78.9 85.54 93 64 93C59.6 93 55.38 92.37 51.45 91.2L73.5 57.5L56.5 42.5L64 28Z" fill="url(#kvz-pf-' + s + ')" opacity="0.68"/>' +
      '<path d="M46 60.5L64 43L82 60.5L64 78Z" fill="#0068FF" opacity="0.16"/>' +
      '</svg>'
    );
  }
  const KV_ZALO_LOGO_SVG = renderKvZaloLogoSvg('art');

  const isAuxiliaryFrame =
    IS_SHARED_WORKER_FRAME ||
    /\/(child|popup-viewer|znotification|sqlite)\.html$/i.test(window.location.pathname);

  let deferredPwaPrompt = null;
  let refreshDeckUiFn = null;
  let renderMediaCardUiFn = null;

  function syncSlotWebManifest() {
    if (!document.head || typeof document.head.querySelector !== 'function') return;
    const vp = document.head.querySelector('meta[name="viewport"]');
    if (vp) {
      const vc = String(vp.getAttribute('content') || '');
      if (!/minimum-scale\s*=/i.test(vc)) {
        vp.setAttribute(
          'content',
          'width=device-width, initial-scale=1, minimum-scale=1, viewport-fit=cover'
        );
      }
    }
    const mobileCss = document.head.querySelector('link[rel="stylesheet"][href*="mobile.css"]');
    if (mobileCss && document.head.lastElementChild !== mobileCss) {
      document.head.appendChild(mobileCss);
    }
    const expectedHref =
      ACTIVE_SLOT !== 'default'
        ? '/manifest.webmanifest?profile=' + encodeURIComponent(ACTIVE_SLOT)
        : '/manifest.webmanifest';
    let link = document.head.querySelector('link[rel="manifest"]');
    if (!link) {
      if (ACTIVE_SLOT === 'default') return;
      link = document.createElement('link');
      link.rel = 'manifest';
      link.crossOrigin = 'use-credentials';
      link.href = expectedHref;
      document.head.appendChild(link);
      return;
    }
    if (ACTIVE_SLOT !== 'default' && link.getAttribute('href') !== expectedHref) {
      link.setAttribute('href', expectedHref);
    }
  }

  try {
    syncSlotWebManifest();
    window.addEventListener('beforeinstallprompt', (ev) => {
      try {
        ev.preventDefault();
      } catch {}
      deferredPwaPrompt = ev;
      if (refreshDeckUiFn) refreshDeckUiFn();
    });
    window.addEventListener('appinstalled', () => {
      deferredPwaPrompt = null;
      if (refreshDeckUiFn) refreshDeckUiFn();
    });
  } catch {}

  async function wipeLocalSlotSession() {
    cookieJar.clearAll();
    rawLsRemove(VAULT_META_KEY);
    if (ACTIVE_SLOT === 'default' && rawLocalStorage && rawStorageMethods) {
      const keep = new Map();
      for (let i = 0; i < rawLocalStorage.length; i++) {
        const k = rawStorageMethods.key.call(rawLocalStorage, i);
        if (k && (k.startsWith('olaz:') || k.startsWith('olaz:__'))) {
          keep.set(k, rawLsGet(k));
        }
      }
      rawStorageMethods.clear.call(rawLocalStorage);
      for (const [k, v] of keep.entries()) rawLsSet(k, v);
    } else if (rawLocalStorage) {
      window.localStorage.clear();
    }

    if (nativeIdb && typeof nativeIdb.databases === 'function') {
      try {
        const dbs = await nativeIdb.databases();
        for (const info of dbs || []) {
          const n = info && info.name;
          if (!n) continue;
          const belongs =
            n === HOST_DB_NAME ||
            n === VFS_DB_NAME ||
            (ACTIVE_SLOT === 'default'
              ? !n.startsWith('olaz__') && !n.startsWith('olaz_host_v1__') && !n.startsWith('olaz_vfs_v1__')
              : n.startsWith('olaz__' + ACTIVE_SLOT + '__'));
          if (belongs) {
            try {
              rawIdbDelete.call(nativeIdb, n);
            } catch {}
          }
        }
      } catch {}
    } else if (nativeIdb && rawIdbDelete) {
      try {
        rawIdbDelete.call(nativeIdb, HOST_DB_NAME);
        rawIdbDelete.call(nativeIdb, VFS_DB_NAME);
      } catch {}
    }

    const q = buildZaloPageQuery({ logout: '1' });
    window.location.replace('/login.html?' + q.toString());
  }

  // --------------------------------------------------------------------------
  // 8A. Web Media Session (`navigator.mediaSession`) + Media Controller
  // --------------------------------------------------------------------------

  const PLAYBACK_RATES = [1, 1.25, 1.5, 2];
  const boundMediaSet = typeof WeakSet === 'function' ? new WeakSet() : new Set();
  const mediaRegistry = []; // Array<{ deref: () => HTMLMediaElement | undefined, seq: number }>
  let mediaSeqCounter = 0;
  let activeMediaEl = null;
  let activeMediaMeta = null;
  let mediaCardDismissed = true;

  function formatMediaClock(sec) {
    if (!Number.isFinite(sec) || sec < 0) return '0:00';
    const whole = Math.floor(sec);
    const hrs = Math.floor(whole / 3600);
    const mins = Math.floor((whole % 3600) / 60);
    const secs = whole % 60;
    const ss = secs < 10 ? '0' + secs : String(secs);
    if (hrs > 0) {
      const mm = mins < 10 ? '0' + mins : String(mins);
      return hrs + ':' + mm + ':' + ss;
    }
    return mins + ':' + ss;
  }

  function shouldIgnoreMediaPlayback(el) {
    if (!el || typeof el.play !== 'function') return true;
    if (el.__olazInternalThumb === true || el.dataset?.olazIgnoreMedia === 'true') return true;
    const rawSrc = String(el.currentSrc || el.getAttribute?.('src') || el.src || '');
    const dur = Number(el.duration);
    if (el.muted && el.loop && (!Number.isFinite(dur) || dur < 1.5)) {
      return true;
    }
    if (/\/sound\/(noti|notification|ring|beep|click)/i.test(rawSrc) && (!Number.isFinite(dur) || dur < 4)) {
      return true;
    }
    return false;
  }

  function cleanExtractedMediaTitle(rawUrl) {
    if (!rawUrl || typeof rawUrl !== 'string') return '';
    try {
      let target = rawUrl;
      const parsed = new URL(rawUrl, window.location.href);
      if (parsed.pathname === '/__olaz/media' && parsed.searchParams.get('u')) {
        target = parsed.searchParams.get('u');
      }
      const inner = new URL(target, window.location.href);
      const base = decodeURIComponent(inner.pathname.split('/').pop() || '').trim();
      if (
        base &&
        /\.(mp3|m4a|aac|wav|ogg|opus|flac|mp4|webm|mov|mkv)$/i.test(base) &&
        !/^[0-9a-f-]{20,}\.[a-z0-9]+$/i.test(base)
      ) {
        return base;
      }
    } catch {}
    return '';
  }

  function extractMediaContext(mediaEl) {
    const isVideo = mediaEl && String(mediaEl.tagName || '').toUpperCase() === 'VIDEO';
    const isVi =
      String(document.documentElement.lang || '').toLowerCase().startsWith('vi') ||
      Boolean(document.querySelector('[suggestion*="gợi ý"]'));
    const defaultTitle = isVideo ? 'Chat Video' : isVi ? 'Tin nhắn thoại' : 'Voice Message';
    const album = ACTIVE_SLOT === 'default' ? 'KV Zalo' : 'KV Zalo · ' + ACTIVE_SLOT;

    let title = '';
    let artist = '';
    let artworkUrl = '';

    if (mediaEl) {
      title =
        String(
          mediaEl.dataset?.olazTitle ||
            mediaEl.getAttribute?.('title') ||
            mediaEl.getAttribute?.('aria-label') ||
            ''
        ).trim();

      const container =
        typeof mediaEl.closest === 'function'
          ? mediaEl.closest(
              '.chat-item, .chat-message, [data-id], .card, .zl-modal__dialog, .media-viewer'
            )
          : null;

      if (container) {
        if (!title) {
          const titleNode = container.querySelector(
            '.file-title, .voice-message-normal__title-content-wrapper, .card--sound__title, .preview-title, .caption, [data-component="text-container"]'
          );
          if (titleNode && titleNode.textContent) {
            const candidate = titleNode.textContent.trim().replace(/\s+/g, ' ');
            if (candidate && candidate.length <= 120) title = candidate;
          }
        }
        if (
          !title &&
          container.matches(
            '.card--sound, .audio-message, .voice-message-normal, .voice-message-old'
          )
        ) {
          title = isVi ? 'Tin nhắn thoại' : 'Voice Message';
        }
        const senderNode = container.querySelector(
          '.card-sender-name, .chat-item__sender-name, .image-show__bottom__sender__info'
        );
        if (senderNode && senderNode.textContent) {
          artist = senderNode.textContent.trim().replace(/\s+/g, ' ');
        }
        const avaImg = container.querySelector('.avatar-img, .avatar img, img.avatar');
        if (avaImg && avaImg.src) {
          artworkUrl = avaImg.src;
        }
      }

      if (!title) {
        title = cleanExtractedMediaTitle(mediaEl.currentSrc || mediaEl.src || '');
      }
      if (!artworkUrl && isVideo && mediaEl.poster) {
        artworkUrl = mediaEl.poster;
      }
    }

    const headerTitleNode = document.querySelector(
      '#header .header-title, #header .threadChat__title .title, .header-title-v2, .header-title'
    );
    if (!artist && headerTitleNode && headerTitleNode.textContent) {
      artist = headerTitleNode.textContent.trim().replace(/\s+/g, ' ');
    }
    if (!artworkUrl) {
      const headerAva = document.querySelector('#header .avatar-img, #header .avatar img, #header img');
      if (headerAva && headerAva.src) {
        artworkUrl = headerAva.src;
      }
    }

    if (
      artworkUrl &&
      !/^(https?:|blob:|data:|\/__zfile\/|\/__olaz\/)/i.test(artworkUrl)
    ) {
      artworkUrl = '';
    }

    const finalTitle = title || defaultTitle;
    const finalArtist = artist || 'KV Zalo';
    const artwork = [];
    if (artworkUrl) {
      artwork.push({ src: artworkUrl, sizes: '96x96' });
    }
    artwork.push(
      { src: '/__olaz/icons/icon-192.png', sizes: '192x192', type: 'image/png' },
      { src: '/__olaz/icons/icon-512.png', sizes: '512x512', type: 'image/png' }
    );

    return {
      title: finalTitle,
      artist: finalArtist,
      album,
      artworkUrl: artworkUrl || '/__olaz/logo.svg',
      artwork,
    };
  }

  function pruneAndOrderMediaList() {
    const alive = [];
    const seen = new Set();
    for (let i = mediaRegistry.length - 1; i >= 0; i--) {
      const el = mediaRegistry[i].deref();
      if (!el || shouldIgnoreMediaPlayback(el)) {
        mediaRegistry.splice(i, 1);
        continue;
      }
      if (seen.has(el)) {
        mediaRegistry.splice(i, 1);
        continue;
      }
      seen.add(el);
      alive.push({ el, seq: mediaRegistry[i].seq });
    }
    alive.sort((a, b) => {
      const aConn = Boolean(a.el.isConnected);
      const bConn = Boolean(b.el.isConnected);
      if (aConn && bConn && a.el !== b.el && typeof a.el.compareDocumentPosition === 'function') {
        const pos = a.el.compareDocumentPosition(b.el);
        if (pos & Node.DOCUMENT_POSITION_FOLLOWING) return -1;
        if (pos & Node.DOCUMENT_POSITION_PRECEDING) return 1;
      }
      return a.seq - b.seq;
    });
    return alive.map((item) => item.el);
  }

  function syncOsPositionState(el) {
    if (!el || !('mediaSession' in navigator) || !navigator.mediaSession) return;
    if (typeof navigator.mediaSession.setPositionState !== 'function') return;
    const duration = Number(el.duration);
    if (!Number.isFinite(duration) || duration <= 0) return;
    const rawPos = Number(el.currentTime) || 0;
    const position = Math.min(duration, Math.max(0, rawPos));
    const rate = Number(el.playbackRate);
    const playbackRate = Number.isFinite(rate) && rate > 0 ? rate : 1;
    try {
      navigator.mediaSession.setPositionState({ duration, playbackRate, position });
    } catch {}
  }

  function syncOsMediaSession() {
    if (!('mediaSession' in navigator) || !navigator.mediaSession) return;
    const ms = navigator.mediaSession;
    if (!activeMediaEl || mediaCardDismissed) {
      try {
        ms.playbackState = 'none';
      } catch {}
      return;
    }
    const meta = activeMediaMeta || extractMediaContext(activeMediaEl);
    activeMediaMeta = meta;
    if (typeof MediaMetadata === 'function') {
      try {
        ms.metadata = new MediaMetadata({
          title: meta.title,
          artist: meta.artist,
          album: meta.album,
          artwork: meta.artwork,
        });
      } catch {}
    }
    try {
      ms.playbackState = activeMediaEl.paused ? 'paused' : 'playing';
    } catch {}
    syncOsPositionState(activeMediaEl);
  }

  function activateMediaElement(el, revealCard = true) {
    if (!el || shouldIgnoreMediaPlayback(el)) return;
    registerMediaElement(el);
    activeMediaEl = el;
    activeMediaMeta = extractMediaContext(el);
    if (revealCard) mediaCardDismissed = false;
    syncOsMediaSession();
    if (renderMediaCardUiFn) renderMediaCardUiFn();
  }

  function registerMediaElement(el) {
    if (!el || typeof el !== 'object' || boundMediaSet.has(el)) return;
    if (el.__olazInternalThumb === true) return;
    boundMediaSet.add(el);

    const holder =
      typeof WeakRef === 'function'
        ? (() => {
            const wr = new WeakRef(el);
            return { deref: () => wr.deref(), seq: ++mediaSeqCounter };
          })()
        : { deref: () => el, seq: ++mediaSeqCounter };
    mediaRegistry.push(holder);

    const onPlayLike = () => {
      if (shouldIgnoreMediaPlayback(el)) return;
      activateMediaElement(el, true);
    };
    const onStateUpdate = () => {
      if (el !== activeMediaEl) return;
      syncOsMediaSession();
      if (renderMediaCardUiFn) renderMediaCardUiFn();
    };
    const onTimeUpdate = () => {
      if (el !== activeMediaEl) return;
      syncOsPositionState(el);
      if (renderMediaCardUiFn) renderMediaCardUiFn();
    };
    const onMetadataUpdate = () => {
      if (el !== activeMediaEl) return;
      activeMediaMeta = extractMediaContext(el);
      syncOsMediaSession();
      if (renderMediaCardUiFn) renderMediaCardUiFn();
    };

    el.addEventListener('play', onPlayLike);
    el.addEventListener('playing', onPlayLike);
    el.addEventListener('pause', onStateUpdate);
    el.addEventListener('ended', onStateUpdate);
    el.addEventListener('ratechange', onStateUpdate);
    el.addEventListener('volumechange', onStateUpdate);
    el.addEventListener('loadedmetadata', onMetadataUpdate);
    el.addEventListener('durationchange', onMetadataUpdate);
    el.addEventListener('timeupdate', onTimeUpdate);
    el.addEventListener('emptied', () => {
      if (el === activeMediaEl) {
        if (renderMediaCardUiFn) renderMediaCardUiFn();
      }
    });
  }

  function installMediaSessionController() {
    if (window.__olazMediaController) return window.__olazMediaController;

    // Wrap `new Audio(...)` so detached voice notes/streams register immediately
    if (typeof window.Audio === 'function' && !window.Audio.__olazWrapped) {
      const NativeAudio = window.Audio;
      const WrappedAudio = function (...args) {
        const inst = new NativeAudio(...args);
        registerMediaElement(inst);
        return inst;
      };
      WrappedAudio.prototype = NativeAudio.prototype;
      Object.setPrototypeOf(WrappedAudio, NativeAudio);
      WrappedAudio.__olazWrapped = true;
      window.Audio = WrappedAudio;
    }

    // Wrap `HTMLMediaElement.prototype.play` to catch detached & in-DOM playback
    if (
      window.HTMLMediaElement &&
      window.HTMLMediaElement.prototype &&
      typeof window.HTMLMediaElement.prototype.play === 'function' &&
      !window.HTMLMediaElement.prototype.play.__olazWrapped
    ) {
      const origPlay = window.HTMLMediaElement.prototype.play;
      const wrappedPlay = function (...args) {
        try {
          if (!shouldIgnoreMediaPlayback(this)) {
            activateMediaElement(this, true);
          }
        } catch {}
        return origPlay.apply(this, args);
      };
      wrappedPlay.__olazWrapped = true;
      window.HTMLMediaElement.prototype.play = wrappedPlay;
    }

    // Capturing document listeners for native browser controls & DOM media
    const docEvents = [
      'play',
      'playing',
      'pause',
      'ended',
      'timeupdate',
      'loadedmetadata',
      'durationchange',
      'ratechange',
      'volumechange',
      'emptied',
    ];
    for (const evName of docEvents) {
      document.addEventListener(
        evName,
        (ev) => {
          const target = ev && ev.target;
          if (!target || !(target instanceof window.HTMLMediaElement)) return;
          if (shouldIgnoreMediaPlayback(target)) return;
          registerMediaElement(target);
          if (evName === 'play' || evName === 'playing') {
            activateMediaElement(target, true);
          } else if (target === activeMediaEl) {
            if (evName === 'loadedmetadata' || evName === 'durationchange') {
              activeMediaMeta = extractMediaContext(target);
            }
            syncOsMediaSession();
            if (renderMediaCardUiFn) renderMediaCardUiFn();
          }
        },
        true
      );
    }

    const controller = {
      getState() {
        const list = pruneAndOrderMediaList();
        const el = activeMediaEl;
        const dur = el && Number.isFinite(el.duration) && el.duration > 0 ? Number(el.duration) : 0;
        const pos = el && Number.isFinite(el.currentTime) ? Math.max(0, Number(el.currentTime)) : 0;
        const isVideo = Boolean(el && String(el.tagName || '').toUpperCase() === 'VIDEO');
        const pipAvailable = Boolean(isVideo && document.pictureInPictureEnabled && !el.disablePictureInPicture);
        return {
          active: Boolean(el && !mediaCardDismissed),
          visible: Boolean(el && !mediaCardDismissed),
          paused: el ? Boolean(el.paused) : true,
          muted: el ? Boolean(el.muted) : false,
          currentTime: dur > 0 ? Math.min(dur, pos) : pos,
          duration: dur,
          playbackRate: el && Number.isFinite(el.playbackRate) && el.playbackRate > 0 ? el.playbackRate : 1,
          mediaType: el ? String(el.tagName || '').toLowerCase() : null,
          pipAvailable,
          pipActive: Boolean(el && document.pictureInPictureElement === el),
          trackCount: list.length,
          metadata: activeMediaMeta ? Object.assign({}, activeMediaMeta) : null,
        };
      },
      play() {
        if (!activeMediaEl) {
          const list = pruneAndOrderMediaList();
          if (list.length > 0) activateMediaElement(list[list.length - 1], true);
        }
        if (!activeMediaEl) return Promise.resolve();
        mediaCardDismissed = false;
        try {
          const p = activeMediaEl.play();
          syncOsMediaSession();
          if (renderMediaCardUiFn) renderMediaCardUiFn();
          return p && typeof p.catch === 'function' ? p.catch(() => {}) : Promise.resolve();
        } catch {
          return Promise.resolve();
        }
      },
      pause() {
        if (!activeMediaEl) return;
        try {
          activeMediaEl.pause();
        } catch {}
        syncOsMediaSession();
        if (renderMediaCardUiFn) renderMediaCardUiFn();
      },
      toggle() {
        if (!activeMediaEl) return controller.play();
        if (activeMediaEl.paused || activeMediaEl.ended) {
          if (activeMediaEl.ended) {
            try {
              activeMediaEl.currentTime = 0;
            } catch {}
          }
          return controller.play();
        }
        controller.pause();
        return Promise.resolve();
      },
      stop() {
        if (activeMediaEl) {
          try {
            activeMediaEl.pause();
            activeMediaEl.currentTime = 0;
          } catch {}
          if (document.pictureInPictureElement === activeMediaEl && typeof document.exitPictureInPicture === 'function') {
            document.exitPictureInPicture().catch(() => {});
          }
        }
        mediaCardDismissed = true;
        syncOsMediaSession();
        if (renderMediaCardUiFn) renderMediaCardUiFn();
      },
      seekTo(sec, fastSeek = false) {
        if (!activeMediaEl) return;
        const targetSec = Number(sec);
        if (!Number.isFinite(targetSec)) return;
        const dur = Number(activeMediaEl.duration);
        const clamped =
          Number.isFinite(dur) && dur > 0
            ? Math.min(dur, Math.max(0, targetSec))
            : Math.max(0, targetSec);
        try {
          if (fastSeek && typeof activeMediaEl.fastSeek === 'function') {
            activeMediaEl.fastSeek(clamped);
          } else {
            activeMediaEl.currentTime = clamped;
          }
        } catch {}
        syncOsPositionState(activeMediaEl);
        if (renderMediaCardUiFn) renderMediaCardUiFn();
      },
      seekBy(delta) {
        if (!activeMediaEl) return;
        const cur = Number(activeMediaEl.currentTime) || 0;
        controller.seekTo(cur + (Number(delta) || 0));
      },
      cycleRate() {
        if (!activeMediaEl) return 1;
        const cur = Number(activeMediaEl.playbackRate) || 1;
        const idx = PLAYBACK_RATES.findIndex((r) => Math.abs(r - cur) < 0.05);
        const nextRate = PLAYBACK_RATES[(idx + 1) % PLAYBACK_RATES.length];
        try {
          activeMediaEl.playbackRate = nextRate;
        } catch {}
        syncOsMediaSession();
        if (renderMediaCardUiFn) renderMediaCardUiFn();
        return nextRate;
      },
      toggleMute() {
        if (!activeMediaEl) return false;
        try {
          activeMediaEl.muted = !activeMediaEl.muted;
        } catch {}
        if (renderMediaCardUiFn) renderMediaCardUiFn();
        return Boolean(activeMediaEl.muted);
      },
      previousTrack() {
        const list = pruneAndOrderMediaList();
        if (activeMediaEl && (Number(activeMediaEl.currentTime) || 0) > 3) {
          controller.seekTo(0);
          return;
        }
        if (list.length > 1 && activeMediaEl) {
          const idx = list.indexOf(activeMediaEl);
          const prevIdx = idx > 0 ? idx - 1 : list.length - 1;
          try {
            activeMediaEl.pause();
          } catch {}
          const nextEl = list[prevIdx];
          activateMediaElement(nextEl, true);
          controller.seekTo(0);
          controller.play();
          return;
        }
        controller.seekTo(0);
      },
      nextTrack() {
        const list = pruneAndOrderMediaList();
        if (list.length > 1 && activeMediaEl) {
          const idx = list.indexOf(activeMediaEl);
          const nextIdx = idx >= 0 ? (idx + 1) % list.length : 0;
          try {
            activeMediaEl.pause();
          } catch {}
          const nextEl = list[nextIdx];
          activateMediaElement(nextEl, true);
          controller.seekTo(0);
          controller.play();
          return;
        }
        if (activeMediaEl) {
          controller.seekTo(0);
        }
      },
      async togglePip() {
        if (
          !activeMediaEl ||
          String(activeMediaEl.tagName || '').toUpperCase() !== 'VIDEO' ||
          !document.pictureInPictureEnabled
        ) {
          return false;
        }
        try {
          if (document.pictureInPictureElement === activeMediaEl) {
            await document.exitPictureInPicture();
          } else if (typeof activeMediaEl.requestPictureInPicture === 'function') {
            await activeMediaEl.requestPictureInPicture();
          }
        } catch {}
        if (renderMediaCardUiFn) renderMediaCardUiFn();
        return document.pictureInPictureElement === activeMediaEl;
      },
      attachMedia(el) {
        if (!el) return controller.getState();
        el.__olazInternalThumb = false;
        activateMediaElement(el, true);
        return controller.getState();
      },
    };

    if ('mediaSession' in navigator && navigator.mediaSession && typeof navigator.mediaSession.setActionHandler === 'function') {
      const actionMap = [
        ['play', () => controller.play()],
        ['pause', () => controller.pause()],
        ['stop', () => controller.stop()],
        ['seekbackward', (details) => controller.seekBy(-((details && Number(details.seekOffset)) || 10))],
        ['seekforward', (details) => controller.seekBy((details && Number(details.seekOffset)) || 10)],
        [
          'seekto',
          (details) => {
            if (details && Number.isFinite(details.seekTime)) {
              controller.seekTo(details.seekTime, Boolean(details.fastSeek));
            }
          },
        ],
        ['previoustrack', () => controller.previousTrack()],
        ['nexttrack', () => controller.nextTrack()],
      ];
      for (const [action, fn] of actionMap) {
        try {
          navigator.mediaSession.setActionHandler(action, fn);
        } catch {}
      }
    }

    window.__olazMediaController = controller;
    return controller;
  }

  // --------------------------------------------------------------------------
  // 8B. Mobile Responsive Master-Detail Navigation Controller
  // --------------------------------------------------------------------------

  const mobileViewportMql =
    typeof window.matchMedia === 'function' ? window.matchMedia('(max-width: 767.98px)') : null;

  let fallbackMobilePaneState = 'list';

  function getRootMobilePane() {
    const docEl = document && document.documentElement;
    if (docEl && docEl.dataset && typeof docEl.dataset.kvzaloMobilePane === 'string') {
      return docEl.dataset.kvzaloMobilePane || fallbackMobilePaneState;
    }
    if (docEl && typeof docEl.getAttribute === 'function') {
      const attr = docEl.getAttribute('data-kvzalo-mobile-pane');
      if (attr) return attr;
    }
    return fallbackMobilePaneState;
  }

  function setRootMobilePane(val) {
    fallbackMobilePaneState = val;
    const docEl = document && document.documentElement;
    if (docEl && docEl.dataset) {
      docEl.dataset.kvzaloMobilePane = val;
    } else if (docEl && typeof docEl.setAttribute === 'function') {
      docEl.setAttribute('data-kvzalo-mobile-pane', val);
    }
  }

  function syncDeckHostMobileAttrs() {
    const host =
      typeof document.getElementById === 'function'
        ? document.getElementById('__olaz-privacy-pill-host')
        : null;
    if (!host) return;
    const pane = getRootMobilePane() || 'list';
    const isMobile = Boolean(mobileViewportMql && mobileViewportMql.matches);
    host.setAttribute('data-mobile-pane', pane);
    host.setAttribute('data-is-mobile', isMobile ? 'true' : 'false');
    if (refreshDeckUiFn) refreshDeckUiFn();
  }

  function setMobilePane(nextPane, opts = {}) {
    const normalized = nextPane === 'chat' ? 'chat' : 'list';
    setRootMobilePane(normalized);
    syncDeckHostMobileAttrs();

    if (
      normalized === 'chat' &&
      opts.pushHistory !== false &&
      mobileViewportMql &&
      mobileViewportMql.matches
    ) {
      try {
        const curState = window.history && window.history.state;
        if (!curState || curState.__kvzaloMobilePane !== 'chat') {
          const nextState = Object.assign({}, curState && typeof curState === 'object' ? curState : {}, {
            __kvzaloMobilePane: 'chat',
          });
          const url = window.location.pathname + window.location.search + '#chat';
          if (window.history && typeof window.history.pushState === 'function') {
            window.history.pushState(nextState, '', url);
          }
        }
      } catch {}
    }
  }

  function ensureMobileBackButton() {
    if (typeof document.querySelector !== 'function') return null;
    const headerEl = document.querySelector(
      '#header, #chatViewContainer > header, #messageViewContainer header, article#chatView header'
    );
    if (!headerEl || typeof headerEl.querySelector !== 'function') return null;
    const existing = headerEl.querySelector('.kvzalo-mobile-back-btn');
    if (existing) return existing;

    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'kvzalo-mobile-back-btn';
    btn.setAttribute('aria-label', 'Back to conversations');
    btn.innerHTML =
      '<svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M15 18l-6-6 6-6"/>' +
      '</svg>' +
      '<span>Chats</span>';
    btn.addEventListener('click', (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      setMobilePane('list', { pushHistory: false });
      try {
        if (window.history && window.history.state && window.history.state.__kvzaloMobilePane === 'chat') {
          window.history.back();
        }
      } catch {}
    });
    if (typeof headerEl.insertBefore === 'function') {
      headerEl.insertBefore(btn, headerEl.firstChild);
    } else if (typeof headerEl.appendChild === 'function') {
      headerEl.appendChild(btn);
    }
    return btn;
  }

  function installMobilePaneController() {
    if (isAuxiliaryFrame || window.__olazMobileController) return;
    const isMainChatRoute =
      window.location.pathname.endsWith('/index.html') ||
      window.location.pathname === '/' ||
      Boolean(typeof document.getElementById === 'function' && document.getElementById('container'));

    if (!isMainChatRoute) return;

    if (!getRootMobilePane()) {
      setRootMobilePane('list');
    } else {
      setRootMobilePane(getRootMobilePane());
    }
    syncDeckHostMobileAttrs();

    if (mobileViewportMql) {
      const onViewportChange = () => {
        if (mobileViewportMql.matches && !getRootMobilePane()) {
          setRootMobilePane('list');
        }
        ensureMobileBackButton();
        syncDeckHostMobileAttrs();
      };
      if (typeof mobileViewportMql.addEventListener === 'function') {
        mobileViewportMql.addEventListener('change', onViewportChange);
      } else if (typeof mobileViewportMql.addListener === 'function') {
        mobileViewportMql.addListener(onViewportChange);
      }
    }

    window.addEventListener('popstate', (ev) => {
      const st = ev && ev.state;
      if (st && st.__kvzaloMobilePane === 'chat') {
        setMobilePane('chat', { pushHistory: false });
      } else if (getRootMobilePane() === 'chat') {
        setMobilePane('list', { pushHistory: false });
      }
    });

    const schedFrame =
      typeof window.requestAnimationFrame === 'function'
        ? window.requestAnimationFrame.bind(window)
        : (fn) => setTimeout(fn, 16);

    // Switch to active chat pane when a conversation row is selected
    document.addEventListener(
      'click',
      (ev) => {
        if (!mobileViewportMql || !mobileViewportMql.matches) return;
        const t = ev && ev.target;
        if (!t || typeof t.closest !== 'function') return;
        if (
          t.closest(
            '.kvzalo-mobile-back-btn, .conv-action__menu, .conv-action__popup, .zmenu-body, .popover-v2, .popover-v3, #main-tab, #contact-search'
          )
        ) {
          return;
        }
        const convRow = t.closest(
          '.conv-item, .msg-item, #conversationListId .item, #conversationListId [data-id], #searchResultList .item, [data-id^="div_ReceivedMsg"], [data-id^="div_SentMsg"]'
        );
        if (convRow) {
          setMobilePane('chat', { pushHistory: true });
          schedFrame(() => ensureMobileBackButton());
        }
      },
      false
    );

    if (typeof window.MutationObserver === 'function' && document.body) {
      let rafPending = false;
      const observer = new window.MutationObserver(() => {
        if (rafPending) return;
        rafPending = true;
        schedFrame(() => {
          rafPending = false;
          ensureMobileBackButton();
        });
      });
      observer.observe(document.body, { childList: true, subtree: true });
    }
    ensureMobileBackButton();

    window.__olazMobileController = {
      getPane: () => getRootMobilePane() || 'list',
      setPane: (pane, opts) => setMobilePane(pane, opts),
      ensureBackButton: ensureMobileBackButton,
    };
  }

  // --------------------------------------------------------------------------
  // 8C. Floating KV Zalo Account Pill, Vault Panel & Now-Playing Media Card
  // --------------------------------------------------------------------------

  function mountAccountAndPrivacyPill() {
    try {
      syncSlotWebManifest();
    } catch {}
    const mediaCtrl = installMediaSessionController();
    installMobilePaneController();

    if (isAuxiliaryFrame) return;
    if (typeof document.getElementById === 'function' && document.getElementById('__olaz-privacy-pill-host')) return;

    const host = document.createElement('div');
    host.id = '__olaz-privacy-pill-host';
    host.setAttribute('data-mobile-pane', getRootMobilePane() || 'list');
    host.setAttribute(
      'data-has-bottom-nav',
      window.location.pathname.endsWith('/index.html') ||
        Boolean(typeof document.getElementById === 'function' && document.getElementById('main-tab'))
        ? 'true'
        : 'false'
    );
    host.setAttribute(
      'data-is-mobile',
      mobileViewportMql && mobileViewportMql.matches ? 'true' : 'false'
    );

    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        :host {
          all: initial;
          display: block;
        }
        *, *::before, *::after {
          box-sizing: border-box;
        }
        button, input, select {
          font: inherit;
          color: inherit;
        }
        .vv-probe {
          position: fixed;
          right: 0;
          bottom: 0;
          width: 0;
          height: 0;
          pointer-events: none;
          visibility: hidden;
        }
        .wrap {
          position: fixed;
          right: 16px;
          bottom: calc(16px + var(--kvz-vv-offset, 0px) + env(safe-area-inset-bottom, 0px));
          z-index: 2147483640;
          display: flex;
          flex-direction: column;
          align-items: flex-end;
          gap: 10px;
          font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
          color: #f8fafc;
          user-select: none;
          -webkit-user-select: none;
        }
        .pill {
          display: inline-flex;
          align-items: center;
          gap: 8px;
          min-height: 36px;
          padding: 6px 13px 6px 8px;
          border-radius: 999px;
          background: rgba(15, 23, 42, 0.92);
          border: 1px solid rgba(56, 189, 248, 0.36);
          box-shadow: 0 10px 26px rgba(2, 6, 23, 0.42);
          cursor: pointer;
          font-size: 12px;
          font-weight: 600;
          color: #f8fafc;
          backdrop-filter: blur(12px);
          -webkit-backdrop-filter: blur(12px);
          transition: transform 0.14s ease, border-color 0.14s ease;
        }
        .pill:hover {
          border-color: rgba(96, 165, 250, 0.72);
        }
        .pill:focus-visible,
        .btn:focus-visible,
        .acct:focus-visible,
        .mc-btn:focus-visible,
        .mc-seek:focus-visible {
          outline: 2px solid #38bdf8;
          outline-offset: 2px;
        }
        .brand-logo {
          width: 20px;
          height: 20px;
          display: inline-flex;
          flex: none;
        }
        .brand-logo svg {
          width: 100%;
          height: 100%;
          display: block;
        }
        .dot {
          width: 7px;
          height: 7px;
          border-radius: 50%;
          background: #10b981;
          box-shadow: 0 0 8px #10b981;
          flex: none;
        }
        .pill-label {
          white-space: nowrap;
        }
        .panel {
          display: none;
          width: 312px;
          max-width: calc(100vw - 24px);
          max-width: calc(100dvw - 24px);
          max-height: calc(var(--kvz-vv-height, 100dvh) - 96px);
          overflow-y: auto;
          overscroll-behavior: contain;
          padding: 14px;
          border-radius: 16px;
          background: rgba(15, 23, 42, 0.96);
          border: 1px solid rgba(148, 163, 184, 0.24);
          box-shadow: 0 20px 48px rgba(2, 6, 23, 0.58);
          backdrop-filter: blur(16px);
          -webkit-backdrop-filter: blur(16px);
        }
        .panel.open {
          display: block;
        }
        .brand-hdr {
          display: flex;
          align-items: center;
          gap: 10px;
          margin-bottom: 8px;
        }
        .brand-hdr-icon {
          width: 32px;
          height: 32px;
          flex: none;
        }
        .brand-hdr-icon svg {
          width: 100%;
          height: 100%;
          display: block;
        }
        .hdr {
          font-size: 12.5px;
          letter-spacing: 0.02em;
          color: #f8fafc;
          font-weight: 700;
          line-height: 1.25;
        }
        .sub {
          font-size: 11px;
          color: #94a3b8;
          line-height: 1.35;
        }
        .sec {
          border-top: 1px solid rgba(148, 163, 184, 0.14);
          padding-top: 10px;
          margin-top: 10px;
        }
        .sec-lbl {
          font-size: 11px;
          color: #94a3b8;
          margin-bottom: 6px;
          font-weight: 600;
        }
        .acct-list {
          display: flex;
          flex-wrap: wrap;
          gap: 6px;
          margin-bottom: 8px;
        }
        .acct {
          padding: 5px 11px;
          border-radius: 999px;
          font-size: 12px;
          cursor: pointer;
          background: rgba(51, 65, 85, 0.68);
          color: #e2e8f0;
          border: 1px solid transparent;
        }
        .acct.active {
          background: rgba(0, 104, 255, 0.36);
          border-color: #60a5fa;
          color: #ffffff;
          font-weight: 600;
        }
        .row {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 8px;
          font-size: 12.5px;
          margin: 7px 0;
          cursor: pointer;
        }
        .btn {
          width: 100%;
          min-height: 36px;
          padding: 8px 11px;
          border-radius: 10px;
          border: 1px solid rgba(148, 163, 184, 0.24);
          background: rgba(30, 41, 59, 0.92);
          color: #f1f5f9;
          font-size: 12px;
          font-weight: 600;
          cursor: pointer;
          margin-top: 6px;
          display: inline-flex;
          align-items: center;
          justify-content: center;
          gap: 7px;
        }
        .btn:hover {
          background: rgba(51, 65, 85, 0.96);
        }
        .btn.primary {
          background: linear-gradient(135deg, #1a7cff 0%, #0068ff 100%);
          border-color: rgba(147, 197, 253, 0.45);
          color: #ffffff;
          box-shadow: 0 6px 18px rgba(0, 104, 255, 0.35);
        }
        .btn.primary:hover {
          background: linear-gradient(135deg, #2584ff 0%, #005ce6 100%);
        }
        .btn.danger {
          border-color: rgba(239, 68, 68, 0.42);
          color: #fca5a5;
          background: rgba(127, 29, 29, 0.3);
        }
        .btn svg {
          width: 15px;
          height: 15px;
          flex: none;
        }
        .hidden {
          display: none !important;
        }
        input[type="checkbox"] {
          accent-color: #0068ff;
          width: 16px;
          height: 16px;
          cursor: pointer;
          flex: none;
        }

        /* Now-Playing Media Card & Transport Bar */
        .media-card {
          display: none;
          width: min(340px, calc(100vw - 24px));
          width: min(340px, calc(100dvw - 24px));
          padding: 12px 13px;
          border-radius: 16px;
          background: rgba(15, 23, 42, 0.96);
          border: 1px solid rgba(96, 165, 250, 0.34);
          box-shadow: 0 18px 42px rgba(2, 6, 23, 0.56);
          backdrop-filter: blur(16px);
          -webkit-backdrop-filter: blur(16px);
          color: #f8fafc;
        }
        .media-card.visible {
          display: block;
        }
        .mc-top {
          display: flex;
          align-items: center;
          gap: 10px;
        }
        .mc-art {
          width: 40px;
          height: 40px;
          border-radius: 10px;
          overflow: hidden;
          background: rgba(30, 41, 59, 0.9);
          border: 1px solid rgba(148, 163, 184, 0.22);
          flex: none;
          display: flex;
          align-items: center;
          justify-content: center;
        }
        .mc-art img,
        .mc-art svg {
          width: 100%;
          height: 100%;
          object-fit: cover;
          display: block;
        }
        .mc-meta {
          min-width: 0;
          flex: 1 1 auto;
        }
        .mc-title {
          font-size: 13px;
          font-weight: 600;
          color: #f8fafc;
          white-space: nowrap;
          overflow: hidden;
          text-overflow: ellipsis;
          line-height: 1.3;
        }
        .mc-artist {
          font-size: 11.5px;
          color: #93c5fd;
          white-space: nowrap;
          overflow: hidden;
          text-overflow: ellipsis;
          line-height: 1.3;
          margin-top: 2px;
        }
        .mc-progress {
          display: flex;
          align-items: center;
          gap: 8px;
          margin-top: 10px;
        }
        .mc-seek {
          flex: 1 1 auto;
          min-width: 0;
          height: 5px;
          accent-color: #0068ff;
          cursor: pointer;
          margin: 0;
        }
        .mc-time {
          font-size: 11px;
          color: #cbd5e1;
          font-variant-numeric: tabular-nums;
          white-space: nowrap;
          flex: none;
        }
        .mc-controls {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 4px;
          margin-top: 9px;
        }
        .mc-btn {
          min-width: 32px;
          min-height: 32px;
          padding: 0 6px;
          border-radius: 9px;
          border: 1px solid transparent;
          background: transparent;
          color: #e2e8f0;
          display: inline-flex;
          align-items: center;
          justify-content: center;
          cursor: pointer;
          transition: background-color 0.14s ease, color 0.14s ease;
        }
        .mc-btn:hover {
          background: rgba(51, 65, 85, 0.72);
          color: #ffffff;
        }
        .mc-btn svg {
          width: 17px;
          height: 17px;
          display: block;
          flex: none;
        }
        .mc-btn.mc-play {
          min-width: 38px;
          min-height: 38px;
          border-radius: 999px;
          background: #0068ff;
          color: #ffffff;
          box-shadow: 0 6px 16px rgba(0, 104, 255, 0.42);
        }
        .mc-btn.mc-play:hover {
          background: #1a7cff;
        }
        .mc-btn.mc-rate {
          font-size: 11px;
          font-weight: 700;
          font-variant-numeric: tabular-nums;
          background: rgba(30, 41, 59, 0.9);
          border-color: rgba(148, 163, 184, 0.24);
          padding: 0 7px;
        }
        .mc-close {
          min-width: 28px;
          min-height: 28px;
          border-radius: 8px;
          color: #94a3b8;
        }

        @media (pointer: coarse) {
          .pill,
          .btn,
          .acct {
            min-height: 44px;
          }
          .mc-btn {
            min-width: 40px;
            min-height: 44px;
          }
          .mc-btn.mc-play {
            min-width: 44px;
            min-height: 44px;
          }
          .mc-seek {
            height: 24px;
          }
        }

        @media (max-width: 767.98px) {
          .wrap {
            right: 12px;
            bottom: calc(14px + var(--kvz-vv-offset, 0px) + env(safe-area-inset-bottom, 0px));
          }
          :host([data-has-bottom-nav="true"]) .wrap {
            bottom: calc(64px + var(--kvz-vv-offset, 0px) + env(safe-area-inset-bottom, 0px));
          }
          .panel {
            width: calc(100vw - 24px);
            width: calc(100dvw - 24px);
            max-width: 340px;
            max-height: calc(var(--kvz-vv-height, 100dvh) - 82px);
          }
          /* Lift pill out of the bottom composer area when inside an active mobile chat */
          :host([data-mobile-pane="chat"]) .wrap {
            top: max(10px, env(safe-area-inset-top, 0px));
            right: 10px;
            bottom: auto;
            flex-direction: column-reverse;
          }
          :host([data-mobile-pane="chat"]) .pill {
            min-height: 34px;
            padding: 4px 10px 4px 6px;
            font-size: 11.5px;
          }
          :host([data-mobile-pane="chat"]) .pill-slot {
            display: none;
          }
        }
      </style>
      <div class="vv-probe" id="vvProbe" aria-hidden="true"></div>
      <div class="wrap" id="wrap">
        <div class="panel" id="panel" role="region" aria-label="KV Zalo Vault and Accounts">
          <div class="brand-hdr">
            <span class="brand-hdr-icon">${renderKvZaloLogoSvg('panel')}</span>
            <div>
              <div class="hdr">KV Zalo Vault &amp; Accounts</div>
              <div class="sub">100% Browser IndexedDB · 0 Bytes on Server</div>
            </div>
          </div>

          <div class="sec hidden" id="pwaSec">
            <button type="button" class="btn primary" id="installPwaBtn">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M12 3v12"/><path d="M7 10l5 5 5-5"/><path d="M5 21h14"/>
              </svg>
              <span>Install KV Zalo App</span>
            </button>
          </div>

          <div class="sec hidden" id="mobileNavSec">
            <button type="button" class="btn" id="mobilePaneBtn">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>
              </svg>
              <span id="mobilePaneLabel">Mobile View: Chats</span>
            </button>
          </div>

          <div class="sec">
            <div class="sec-lbl">Accounts (Isolated Browser DBs)</div>
            <div class="acct-list" id="accts"></div>
            <button type="button" class="btn" id="addAcct">+ Add Account</button>
          </div>

          <div class="sec">
            <label class="row" for="antiRecall">
              <span>Xem tin thu hồi (Anti-Recall)</span>
              <input type="checkbox" id="antiRecall" />
            </label>
            <div class="row">
              <span>Vault PIN Lock: <strong id="pinState">Off</strong></span>
            </div>
            <button type="button" class="btn" id="pinBtn">Set / Clear Local PIN</button>
          </div>

          <div class="sec">
            <button type="button" class="btn danger" id="wipeBtn">Wipe Local Session (${ACTIVE_SLOT})</button>
          </div>
        </div>

        <section class="media-card" id="mediaCard" aria-label="Now Playing Media Controls">
          <div class="mc-top">
            <div class="mc-art" id="mcArt">${renderKvZaloLogoSvg('media')}</div>
            <div class="mc-meta">
              <div class="mc-title" id="mcTitle">Voice Message</div>
              <div class="mc-artist" id="mcArtist">KV Zalo</div>
            </div>
            <button type="button" class="mc-btn mc-close" id="mcClose" aria-label="Stop and close media player">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6L6 18"/><path d="M6 6l12 12"/>
              </svg>
            </button>
          </div>

          <div class="mc-progress">
            <input type="range" class="mc-seek" id="mcSeek" min="0" max="1000" value="0" step="1" aria-label="Seek media" />
            <span class="mc-time" id="mcTime">0:00 / 0:00</span>
          </div>

          <div class="mc-controls">
            <button type="button" class="mc-btn" id="mcPrev" aria-label="Previous media">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
                <path d="M6 6h2v12H6V6zm3.5 6 8.5 6V6l-8.5 6z"/>
              </svg>
            </button>
            <button type="button" class="mc-btn" id="mcRew" aria-label="Rewind 10 seconds">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M11 17l-5-5 5-5"/><path d="M18 17l-5-5 5-5"/>
              </svg>
            </button>
            <button type="button" class="mc-btn mc-play" id="mcPlayPause" aria-label="Play">
              <svg id="mcPlayIcon" viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
                <path d="M8 5.14v13.72a1 1 0 0 0 1.52.85l11-6.86a1 1 0 0 0 0-1.7l-11-6.86A1 1 0 0 0 8 5.14z"/>
              </svg>
              <svg id="mcPauseIcon" class="hidden" viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
                <rect x="6" y="5" width="4" height="14" rx="1"/><rect x="14" y="5" width="4" height="14" rx="1"/>
              </svg>
            </button>
            <button type="button" class="mc-btn" id="mcFwd" aria-label="Forward 10 seconds">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M13 17l5-5-5-5"/><path d="M6 17l5-5-5-5"/>
              </svg>
            </button>
            <button type="button" class="mc-btn" id="mcNext" aria-label="Next media">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
                <path d="M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z"/>
              </svg>
            </button>
            <button type="button" class="mc-btn mc-rate" id="mcRate" aria-label="Playback speed">1×</button>
            <button type="button" class="mc-btn" id="mcMute" aria-label="Mute">
              <svg id="mcVolOn" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
                <path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/>
              </svg>
              <svg id="mcVolOff" class="hidden" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
                <line x1="23" y1="9" x2="17" y2="15"/><line x1="17" y1="9" x2="23" y2="15"/>
              </svg>
            </button>
            <button type="button" class="mc-btn hidden" id="mcPip" aria-label="Picture in Picture">
              <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <rect x="2" y="4" width="20" height="15" rx="2"/><rect x="11" y="10" width="9" height="7" rx="1" fill="currentColor"/>
              </svg>
            </button>
          </div>
        </section>

        <button type="button" class="pill" id="pill" aria-expanded="false" aria-controls="panel" aria-label="KV Zalo Multi-Account and Vault">
          <span class="brand-logo">${renderKvZaloLogoSvg('pill')}</span>
          <span class="dot" aria-hidden="true"></span>
          <span class="pill-label">KV Zalo<span class="pill-slot"> · ${ACTIVE_SLOT}</span></span>
        </button>
      </div>
    `;

    document.body.appendChild(host);

    const vvProbe = shadow.getElementById('vvProbe');
    const wrapEl = shadow.getElementById('wrap');
    const pill = shadow.getElementById('pill');
    const panel = shadow.getElementById('panel');
    const pwaSec = shadow.getElementById('pwaSec');
    const installPwaBtn = shadow.getElementById('installPwaBtn');
    const mobileNavSec = shadow.getElementById('mobileNavSec');
    const mobilePaneBtn = shadow.getElementById('mobilePaneBtn');
    const mobilePaneLabel = shadow.getElementById('mobilePaneLabel');
    const acctsEl = shadow.getElementById('accts');
    const addBtn = shadow.getElementById('addAcct');
    const antiRecallCb = shadow.getElementById('antiRecall');
    const pinStateEl = shadow.getElementById('pinState');
    const pinBtn = shadow.getElementById('pinBtn');
    const wipeBtn = shadow.getElementById('wipeBtn');

    const syncViewportOffset = () => {
      if (!vvProbe || !wrapEl || typeof vvProbe.getBoundingClientRect !== 'function') return;
      const vv = window.visualViewport;
      const visibleTop = vv && Number.isFinite(vv.offsetTop) ? Math.max(0, vv.offsetTop) : 0;
      const visibleHeight =
        vv && Number.isFinite(vv.height) && vv.height > 0
          ? vv.height
          : Number(window.innerHeight) || 0;
      if (visibleHeight <= 0) return;
      const visibleBottom = visibleTop + visibleHeight;
      const probeRect = vvProbe.getBoundingClientRect();
      const probeBottom =
        probeRect && Number.isFinite(probeRect.bottom) && probeRect.bottom > 0
          ? probeRect.bottom
          : visibleBottom;
      const extraBottom = Math.max(0, Math.ceil(probeBottom - visibleBottom));
      const clampedHeight = Math.max(240, Math.floor(visibleHeight));
      if (wrapEl.style && typeof wrapEl.style.setProperty === 'function') {
        wrapEl.style.setProperty('--kvz-vv-offset', extraBottom + 'px');
        wrapEl.style.setProperty('--kvz-vv-height', clampedHeight + 'px');
      }
      const docEl = document && document.documentElement;
      if (docEl && docEl.style && typeof docEl.style.setProperty === 'function') {
        docEl.style.setProperty('--kvzalo-vv-offset', extraBottom + 'px');
        docEl.style.setProperty('--kvzalo-vv-height', clampedHeight + 'px');
      }
    };

    syncViewportOffset();
    window.addEventListener('resize', syncViewportOffset, { passive: true });
    window.addEventListener('orientationchange', syncViewportOffset, { passive: true });
    if (window.visualViewport && typeof window.visualViewport.addEventListener === 'function') {
      window.visualViewport.addEventListener('resize', syncViewportOffset, { passive: true });
      window.visualViewport.addEventListener('scroll', syncViewportOffset, { passive: true });
    }
    if (typeof window.ResizeObserver === 'function') {
      try {
        const ro = new window.ResizeObserver(() => syncViewportOffset());
        if (document.documentElement) ro.observe(document.documentElement);
        if (document.body) ro.observe(document.body);
      } catch {}
    }
    if (typeof window.MutationObserver === 'function' && typeof window.requestAnimationFrame === 'function' && document.body) {
      let vvRaf = false;
      const mo = new window.MutationObserver(() => {
        if (vvRaf) return;
        vvRaf = true;
        window.requestAnimationFrame(() => {
          vvRaf = false;
          syncViewportOffset();
        });
      });
      mo.observe(document.body, { childList: true, subtree: true });
    }

    const mediaCard = shadow.getElementById('mediaCard');
    const mcArt = shadow.getElementById('mcArt');
    const mcTitle = shadow.getElementById('mcTitle');
    const mcArtist = shadow.getElementById('mcArtist');
    const mcClose = shadow.getElementById('mcClose');
    const mcSeek = shadow.getElementById('mcSeek');
    const mcTime = shadow.getElementById('mcTime');
    const mcPrev = shadow.getElementById('mcPrev');
    const mcRew = shadow.getElementById('mcRew');
    const mcPlayPause = shadow.getElementById('mcPlayPause');
    const mcPlayIcon = shadow.getElementById('mcPlayIcon');
    const mcPauseIcon = shadow.getElementById('mcPauseIcon');
    const mcFwd = shadow.getElementById('mcFwd');
    const mcNext = shadow.getElementById('mcNext');
    const mcRate = shadow.getElementById('mcRate');
    const mcMute = shadow.getElementById('mcMute');
    const mcVolOn = shadow.getElementById('mcVolOn');
    const mcVolOff = shadow.getElementById('mcVolOff');
    const mcPip = shadow.getElementById('mcPip');

    let isDraggingSeek = false;
    let lastRenderedArtwork = '';

    const renderMediaCard = () => {
      syncViewportOffset();
      const st = mediaCtrl.getState();
      mediaCard.classList.toggle('visible', Boolean(st.visible));
      if (!st.visible) return;

      const meta = st.metadata || { title: 'Voice Message', artist: 'KV Zalo', artworkUrl: '' };
      mcTitle.textContent = meta.title || 'Voice Message';
      mcArtist.textContent = meta.artist || 'KV Zalo';

      const artSrc =
        meta.artworkUrl && meta.artworkUrl !== '/__olaz/logo.svg' ? meta.artworkUrl : '';
      if (artSrc !== lastRenderedArtwork) {
        lastRenderedArtwork = artSrc;
        if (artSrc) {
          const img = document.createElement('img');
          img.alt = '';
          img.src = artSrc;
          img.onerror = () => {
            lastRenderedArtwork = '';
            mcArt.innerHTML = renderKvZaloLogoSvg('media');
          };
          mcArt.innerHTML = '';
          mcArt.appendChild(img);
        } else {
          mcArt.innerHTML = renderKvZaloLogoSvg('media');
        }
      }

      const playing = !st.paused;
      mcPlayPause.setAttribute('aria-label', playing ? 'Pause' : 'Play');
      mcPlayIcon.classList.toggle('hidden', playing);
      mcPauseIcon.classList.toggle('hidden', !playing);

      if (!isDraggingSeek) {
        if (st.duration > 0) {
          mcSeek.disabled = false;
          mcSeek.value = String(Math.round((st.currentTime / st.duration) * 1000));
        } else {
          mcSeek.disabled = true;
          mcSeek.value = '0';
        }
      }
      mcTime.textContent =
        formatMediaClock(st.currentTime) +
        ' / ' +
        (st.duration > 0 ? formatMediaClock(st.duration) : '--:--');

      mcRate.textContent = st.playbackRate + '×';
      mcMute.setAttribute('aria-label', st.muted ? 'Unmute' : 'Mute');
      mcVolOn.classList.toggle('hidden', Boolean(st.muted));
      mcVolOff.classList.toggle('hidden', !st.muted);
      mcPip.classList.toggle('hidden', !st.pipAvailable);
    };
    renderMediaCardUiFn = renderMediaCard;

    const render = () => {
      syncViewportOffset();
      acctsEl.innerHTML = '';
      for (const item of loadKnownProfiles()) {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'acct' + (item.id === ACTIVE_SLOT ? ' active' : '');
        b.textContent = item.label;
        b.onclick = () => {
          if (item.id === ACTIVE_SLOT) return;
          const u = '/?profile=' + encodeURIComponent(item.id);
          window.location.assign(u);
        };
        acctsEl.appendChild(b);
      }
      antiRecallCb.checked = Boolean(privacyState.antiRecall);
      pinStateEl.textContent = getVaultMeta() ? 'AES-256-GCM' : 'Off';

      pwaSec.classList.toggle('hidden', !deferredPwaPrompt);

      const isMobileNow = Boolean(mobileViewportMql && mobileViewportMql.matches);
      const hasMainChat =
        window.location.pathname.endsWith('/index.html') ||
        Boolean(document.getElementById('container'));
      mobileNavSec.classList.toggle('hidden', !(isMobileNow && hasMainChat));
      if (isMobileNow && hasMainChat) {
        const curPane = getRootMobilePane() || 'list';
        mobilePaneLabel.textContent =
          curPane === 'chat' ? 'Mobile View: Switch to Chats' : 'Mobile View: Switch to Thread';
      }
      renderMediaCard();
    };
    refreshDeckUiFn = render;

    pill.addEventListener('click', () => {
      render();
      const isOpen = panel.classList.toggle('open');
      pill.setAttribute('aria-expanded', isOpen ? 'true' : 'false');
    });

    installPwaBtn.addEventListener('click', async () => {
      if (!deferredPwaPrompt) return;
      const promptEvent = deferredPwaPrompt;
      try {
        promptEvent.prompt();
        const choice = await promptEvent.userChoice;
        if (choice && choice.outcome === 'accepted') {
          deferredPwaPrompt = null;
        }
      } catch {
        deferredPwaPrompt = null;
      }
      render();
    });

    mobilePaneBtn.addEventListener('click', () => {
      const curPane = getRootMobilePane() || 'list';
      setMobilePane(curPane === 'chat' ? 'list' : 'chat');
      panel.classList.remove('open');
      pill.setAttribute('aria-expanded', 'false');
    });

    addBtn.addEventListener('click', () => {
      const profiles = loadKnownProfiles();
      let n = profiles.length + 1;
      while (profiles.some((p) => p.id === 'acc-' + n)) n++;
      const newSlot = 'acc-' + n;
      profiles.push({ id: newSlot, label: 'Account ' + n });
      saveKnownProfiles(profiles);
      window.open('/?profile=' + encodeURIComponent(newSlot), '_blank', 'noopener');
      render();
    });

    antiRecallCb.addEventListener('change', () => {
      privacyState.antiRecall = antiRecallCb.checked;
      persistPrivacySettings();
    });

    pinBtn.addEventListener('click', async () => {
      const hasPin = Boolean(getVaultMeta());
      const promptText = hasPin
        ? 'Enter a new Local Vault PIN (or leave empty to remove PIN encryption):'
        : 'Set a Local Browser Vault PIN (encrypts zpw_sek & E2EE keys with SubtleCrypto AES-256-GCM):';
      const entered = window.prompt(promptText, '');
      if (entered === null) return;
      await configureVaultPin(entered.trim());
      render();
    });

    wipeBtn.addEventListener('click', async () => {
      if (window.confirm('Wipe all local browser IndexedDB/cookies/keys for "' + ACTIVE_SLOT + '"?')) {
        await wipeLocalSlotSession();
      }
    });

    // Media Card interactive transport bindings
    mcClose.addEventListener('click', () => mediaCtrl.stop());
    mcPrev.addEventListener('click', () => mediaCtrl.previousTrack());
    mcRew.addEventListener('click', () => mediaCtrl.seekBy(-10));
    mcPlayPause.addEventListener('click', () => mediaCtrl.toggle());
    mcFwd.addEventListener('click', () => mediaCtrl.seekBy(10));
    mcNext.addEventListener('click', () => mediaCtrl.nextTrack());
    mcRate.addEventListener('click', () => mediaCtrl.cycleRate());
    mcMute.addEventListener('click', () => mediaCtrl.toggleMute());
    mcPip.addEventListener('click', () => mediaCtrl.togglePip());

    mcSeek.addEventListener('pointerdown', () => {
      isDraggingSeek = true;
    });
    mcSeek.addEventListener('pointerup', () => {
      isDraggingSeek = false;
    });
    mcSeek.addEventListener('input', () => {
      const st = mediaCtrl.getState();
      if (st.duration > 0) {
        const targetTime = (Number(mcSeek.value) / 1000) * st.duration;
        mediaCtrl.seekTo(targetTime);
      }
    });
    mcSeek.addEventListener('change', () => {
      isDraggingSeek = false;
      const st = mediaCtrl.getState();
      if (st.duration > 0) {
        const targetTime = (Number(mcSeek.value) / 1000) * st.duration;
        mediaCtrl.seekTo(targetTime);
      }
    });

    render();
  }

  // ==========================================================================
  // 9. Two-Phase `<head>` Bootstrapper + Deferred Zalo Bundle Execution
  // ==========================================================================

  async function hydrateBrowserVault() {
    const meta = getVaultMeta();
    if (meta) {
      activeVaultCryptoKey = await promptForVaultUnlockModal(meta);
    }

    const hostDb = await openHostIdb();
    if (hostDb) {
      const rows = await idbGetAllKv(hostDb);
      hostDb.close();
      for (const row of rows) {
        if (!row || typeof row.k !== 'string') continue;
        try {
          if (row.k === 'cookies') {
            const opened = await openWithVaultKey(activeVaultCryptoKey, row.v);
            cookieJar.importJson(opened);
          } else if (row.k === 'keychain') {
            const opened = await openWithVaultKey(activeVaultCryptoKey, row.v);
            if (opened && typeof opened === 'object') {
              for (const [acc, b64] of Object.entries(opened)) {
                if (typeof b64 === 'string') {
                  keychainRam.set(acc, core.decodeBase64Forgiving(b64));
                }
              }
            }
          } else if (row.k === 'imei' && typeof row.v === 'string' && row.v) {
            hostImei = row.v;
          } else if (row.k === 'privacy' && row.v && typeof row.v === 'object') {
            Object.assign(privacyState, row.v);
          } else if (row.k.startsWith('store:')) {
            const name = row.k.slice('store:'.length);
            const opened =
              name === 'zsafe-storage'
                ? await openWithVaultKey(activeVaultCryptoKey, row.v)
                : row.v;
            if (opened && typeof opened === 'object') {
              hostStoresRam.set(
                name,
                name === 'database-config' ? core.normalizeDatabaseConfig(opened) : opened
              );
            }
          }
        } catch {}
      }
    }

    for (const storeName of ['config', 'default']) {
      const stObj = hostStoresRam.get(storeName);
      if (stObj && typeof stObj === 'object') {
        let fixed = false;
        for (const key of ['sh_app_ver', 'appVersion']) {
          if (
            stObj[key] === '0.1.0' ||
            stObj[key] === 'null' ||
            stObj[key] === 'undefined' ||
            stObj[key] === ''
          ) {
            stObj[key] = ZALO_APP_VERSION;
            fixed = true;
          }
        }
        if (fixed) {
          writeNamedStoreSync(storeName, stObj);
        }
      }
    }

    if (!hostImei) {
      hostImei = generateDefaultImei();
      idbPutKv('imei', hostImei);
    }

    try {
      const vfsDb = await openVfsIdb();
      if (vfsDb) {
        const files = await new Promise((res) => {
          const tx = vfsDb.transaction('files', 'readonly');
          const r = tx.objectStore('files').getAll();
          r.onsuccess = () => res(r.result || []);
          r.onerror = () => res([]);
        });
        vfsDb.close();
        for (const f of files) {
          if (f && typeof f.path === 'string') {
            vfsEntriesRam.set(f.path, {
              path: f.path,
              isDir: Boolean(f.isDir),
              size: f.size || 0,
              mtimeMs: f.mtimeMs || Date.now(),
              data: f.data instanceof Uint8Array ? f.data : f.data ? new Uint8Array(f.data) : null,
              mime: f.mime || '',
            });
          }
        }
      }
    } catch {}
  }

  async function runDeferredZaloScripts() {
    const placeholders = Array.from(document.querySelectorAll('script[data-olaz-defer-src]'));
    for (const ph of placeholders) {
      const src = ph.getAttribute('data-olaz-defer-src');
      if (!src) continue;
      await new Promise((resolve) => {
        const s = document.createElement('script');
        for (const attr of Array.from(ph.attributes)) {
          if (attr.name === 'data-olaz-defer-src' || attr.name === 'type') continue;
          s.setAttribute(attr.name, attr.value);
        }
        s.src = appendProfileParam(src);
        s.async = false;
        s.onload = () => resolve();
        s.onerror = () => resolve();
        ph.replaceWith(s);
      });
    }
    // Notify any DOMContentLoaded / load listeners registered dynamically by deferred bundles
    try {
      if (document.readyState === 'interactive' || document.readyState === 'complete') {
        document.dispatchEvent(new Event('DOMContentLoaded', { bubbles: true }));
      }
      if (document.readyState === 'complete') {
        window.dispatchEvent(new Event('load'));
      }
    } catch {}
  }

  // Install `$z*` globals & initial mobile pane attribute immediately in `<head>`
  installPreloadGlobals();
  try {
    if (
      !isAuxiliaryFrame &&
      (window.location.pathname.endsWith('/index.html') || window.location.pathname === '/')
    ) {
      setRootMobilePane('list');
    }
  } catch {}

  window.__olazBootReady = (async () => {
    await hydrateBrowserVault();
    seedZaloClientIdentity();

    // Smart client-side boot routing: if user visits `/login.html` with an active `zpw_sek`
    // in their browser vault (and didn't explicitly log out), go straight to `/index.html`!
    const pathname = window.location.pathname;
    const params = new URLSearchParams(window.location.search);
    if (pathname.endsWith('/login.html') && cookieJar.hasZpwSek() && params.get('logout') !== '1') {
      const q = buildZaloPageQuery({ is_main: 'true', already_login: 'y' });
      window.location.replace('/index.html?' + q.toString());
      return;
    }

    const startBundles = async () => {
      mountAccountAndPrivacyPill();
      if (pathname.endsWith('/index.html')) {
        ensureSharedWorkerFrame();
      }
      await runDeferredZaloScripts();
      emitLocalIpcEvent('visibility-change', ['show']);
      emitLocalIpcEvent('visibility-change', ['focus', '1']);
    };

    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', () => startBundles(), { once: true });
    } else {
      await startBundles();
    }
  })();
}

bootstrapSessionRuntime(
  typeof globalThis !== 'undefined' ? globalThis : typeof self !== 'undefined' ? self : window
);
