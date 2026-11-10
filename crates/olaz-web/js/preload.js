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
    'zalo.vn',
    'zalo.ai',
    'zaloapp.com',
    'zadn.vn',
    'zdn.vn',
    'z-cdn.me',
    'dlfl.vn',
    'dlfl.me',
    'dlfl.info',
    'flchat.vn',
    'pchat.vn',
    'dlmd.me',
    'mdchat.me',
    'zavi.me',
    'zapps.vn',
    'zing.vn',
    'zingcdn.me',
    'zingmp3.vn',
    'zingmp3.com',
    'zingtv.vn',
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
  function rawSsGet(k) {
    if (!rawSessionStorage || !rawStorageMethods) return null;
    return rawStorageMethods.getItem.call(rawSessionStorage, k);
  }
  function rawSsSet(k, v) {
    if (!rawSessionStorage || !rawStorageMethods) return;
    rawStorageMethods.setItem.call(rawSessionStorage, k, String(v));
  }
  function rawSsRemove(k) {
    if (!rawSessionStorage || !rawStorageMethods) return;
    rawStorageMethods.removeItem.call(rawSessionStorage, k);
  }

  function isOlazProtectedHostNode(node) {
    if (!node || typeof node !== 'object') return false;
    const id = node.id;
    return id === '__olaz-privacy-pill-host' || id === '__olaz-vault-unlock-overlay';
  }

  function isInsideOlazUiEvent(ev) {
    if (!ev || typeof ev !== 'object') return false;
    try {
      if (isOlazProtectedHostNode(ev.target)) return true;
      if (typeof ev.composedPath === 'function') {
        const path = ev.composedPath();
        if (Array.isArray(path)) {
          for (let i = 0; i < path.length; i++) {
            if (isOlazProtectedHostNode(path[i])) return true;
          }
        }
      }
    } catch {}
    return false;
  }

  function shieldShadowRootEvents(shadowRoot) {
    if (!shadowRoot || typeof shadowRoot.addEventListener !== 'function') return;
    const stopBubble = (ev) => {
      if (ev && typeof ev.stopPropagation === 'function') {
        ev.stopPropagation();
      }
    };
    stopBubble.__olazInternal = true;
    for (const evType of ['keydown', 'keyup', 'keypress', 'input', 'paste', 'copy', 'cut']) {
      shadowRoot.addEventListener(evType, stopBubble, false);
    }
  }

  function installHeadInputFocusCaptureShield() {
    if (!IS_WINDOW) return;
    const shieldedTypes = new Set(['keydown', 'keyup', 'keypress', 'focus', 'focusin']);
    for (const targetObj of [window, document]) {
      if (
        !targetObj ||
        typeof targetObj.addEventListener !== 'function' ||
        typeof targetObj.removeEventListener !== 'function' ||
        targetObj.addEventListener.__olazShielded
      ) {
        continue;
      }
      const origAdd = targetObj.addEventListener;
      const origRemove = targetObj.removeEventListener;
      const wrapperMap = typeof WeakMap === 'function' ? new WeakMap() : null;

      const wrapListener = (listener, createIfMissing) => {
        if (!listener || (typeof listener !== 'function' && typeof listener !== 'object')) {
          return listener;
        }
        if (listener.__olazInternal === true) return listener;
        if (!wrapperMap) return listener;
        let wrapped = wrapperMap.get(listener);
        if (!wrapped && createIfMissing) {
          wrapped = function (ev) {
            if (ev && shieldedTypes.has(ev.type) && isInsideOlazUiEvent(ev)) {
              return;
            }
            if (typeof listener === 'function') {
              return listener.call(this, ev);
            }
            if (listener && typeof listener.handleEvent === 'function') {
              return listener.handleEvent(ev);
            }
          };
          wrapperMap.set(listener, wrapped);
        }
        return wrapped || listener;
      };

      const shieldedAdd = function (type, listener, options) {
        if (shieldedTypes.has(String(type)) && listener && !listener.__olazInternal) {
          return origAdd.call(this, type, wrapListener(listener, true), options);
        }
        return origAdd.call(this, type, listener, options);
      };
      shieldedAdd.__olazShielded = true;

      const shieldedRemove = function (type, listener, options) {
        if (shieldedTypes.has(String(type)) && listener && wrapperMap && wrapperMap.has(listener)) {
          origRemove.call(this, type, wrapperMap.get(listener), options);
        }
        return origRemove.call(this, type, listener, options);
      };
      shieldedRemove.__olazShielded = true;

      try {
        targetObj.addEventListener = shieldedAdd;
        targetObj.removeEventListener = shieldedRemove;
      } catch {}
    }
  }

  installHeadInputFocusCaptureShield();

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
      const w =
        options !== undefined ? new NativeWorker(nextUrl, options) : new NativeWorker(nextUrl);
      try {
        w.addEventListener('message', (ev) => {
          const d = ev && ev.data;
          if (d && typeof d === 'object' && typeof d.__olazLog === 'string') {
            ev.stopImmediatePropagation();
          }
        });
      } catch {}
      return w;
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

  function createZnodeOs(existingInfo) {
    const osInfo = existingInfo || {
      release: '22.6.0',
      version: 'Darwin Kernel Version 22.6.0',
      arch: 'x64',
      hostname: 'OlazWeb',
      homedir: '/Users/olaz',
      tmpdir: VFS_TEMP,
      totalmem: 8 * 1024 * 1024 * 1024,
      freemem: 4 * 1024 * 1024 * 1024,
      cpus: (globalScope.navigator && globalScope.navigator.hardwareConcurrency) || 4,
      uptime: 3600,
      username: 'olaz',
    };
    return {
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
  }

  const znodeUrl = {
    URL,
    URLSearchParams,
    pathToFileURL: (p) => new URL('file://' + normPosixPath(p)),
    fileURLToPath: (u) => normPosixPath(typeof u === 'string' ? u : u.pathname),
  };

  const znodeCrypto = {
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

  const olazHost = (globalScope.__olazHost = {
    core,
    IS_WINDOW,
    IS_WORKER,
    ACTIVE_SLOT,
    CONTEXT_ID,
    HOST_DB_NAME,
    VFS_DB_NAME,
    ZALO_APP_VERSION,
    ZALO_USER_AGENT,
    ZALO_RELAY_SUFFIXES,
    NativeFetch,
    NativeXHR,
    NativeWebSocket,
    NativeBroadcastChannel,
    ipcBus,
    cookieJar,
    cryptoBridge,
    nativeIdb,
    rawIdbOpen,
    rawIdbPut,
    rawIdbDeleteRec,
    rawIdbDelete,
    openRawIdb,
    rawLocalStorage,
    rawStorageMethods,
    rawLsGet,
    rawLsSet,
    rawLsRemove,
    rawSsGet,
    rawSsSet,
    rawSsRemove,
    isZaloHost,
    parseZaloTargetUrl,
    appendProfileParam,
    buildZaloPageQuery,
    decodeBridgeWireResult,
    asyncIpcHandlers: new Map(), // channel -> async (args) => result
    syncIpcHandlers: new Map(), // channel -> (args) => result
    httpFallbackResolvers: [], // Array<async (urlHref, status) => Response | Blob | null>
    ipcBusListeners: [], // Array<(msg) => void>
    webpackModuleHooks: [], // Array<(req) => void>
    diRegistrationHooks: [], // Array<(reg) => void>
    popupWindowHooks: [], // Array<(childWin, url) => void>
    deckMountHooks: [], // Array<(ctx) => void>
    privacyListeners: [], // Array<(privacyState) => void>
  });

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
    if (
      /^zfile:/i.test(raw) ||
      /^file:/i.test(raw) ||
      /^zalo-app:\/\/local\/__olaz\/zfile\//i.test(raw) ||
      /^\/(Users\/olaz|tmp\/olaz|olaz-vfs)(\/|$)/.test(raw)
    ) {
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
      if (typeof globalScope.__olazResolveVfsBlobUrl === 'function') {
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
      if (status >= 400 && olazHost.httpFallbackResolvers.length > 0 && typeof Response === 'function') {
        for (const resolver of olazHost.httpFallbackResolvers) {
          try {
            const fb = await resolver(targetUrl.href, status);
            if (!fb) continue;
            if (fb instanceof Response) {
              return fb;
            }
            if (
              (typeof Blob !== 'undefined' && fb instanceof Blob) ||
              fb instanceof Uint8Array ||
              fb instanceof ArrayBuffer
            ) {
              const fbResp = new Response(fb, {
                status: 200,
                headers: {
                  'Content-Type': (fb && fb.type) || 'application/octet-stream',
                },
              });
              try {
                Object.defineProperty(fbResp, 'url', { value: targetUrl.href });
              } catch {}
              return fbResp;
            }
          } catch {}
        }
      }
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

  // If running inside a Web Worker (`dal-worker` / `sync-v2-sub-worker` / `zd-worker` / `opfs-worker`),
  // expose worker-side `$znode`, OPFS slot isolation, crypto & cookie sync and return cleanly!
  if (IS_WORKER) {
    if (
      ACTIVE_SLOT !== 'default' &&
      globalScope.navigator &&
      globalScope.navigator.storage &&
      typeof globalScope.navigator.storage.getDirectory === 'function' &&
      !globalScope.navigator.storage.getDirectory.__olazScoped
    ) {
      try {
        const storageObj = globalScope.navigator.storage;
        const origGetDir = storageObj.getDirectory.bind(storageObj);
        const scopedGetDir = async function () {
          const rootHandle = await origGetDir();
          if (rootHandle && typeof rootHandle.getDirectoryHandle === 'function') {
            return rootHandle.getDirectoryHandle('olaz_slot_' + ACTIVE_SLOT, { create: true });
          }
          return rootHandle;
        };
        scopedGetDir.__olazScoped = true;
        storageObj.getDirectory = scopedGetDir;
      } catch {}
    }
    if (ipcBus) {
      ipcBus.addEventListener('message', (ev) => {
        const msg = ev && ev.data;
        if (!msg || msg.sender === CONTEXT_ID) return;
        if (msg.__olazType === 'cookie-sync') {
          cookieJar.importJson(msg.cookies);
        }
        for (const fn of olazHost.ipcBusListeners) {
          try {
            fn(msg);
          } catch {}
        }
      });
      try {
        ipcBus.postMessage({ __olazType: 'cookie-request', sender: CONTEXT_ID });
      } catch {}
    }
    globalScope.$znode = Object.assign(globalScope.$znode || {}, {
      path: posixPath,
      url: znodeUrl,
      os: createZnodeOs(globalScope.$znode && globalScope.$znode.os && globalScope.$znode.os.__info),
      crypto: znodeCrypto,
    });
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
  const ANTI_RECALL_LS_KEY = 'olaz:__anti_recall:' + ACTIVE_SLOT;
  const initialAntiRecallRaw = rawLsGet(ANTI_RECALL_LS_KEY);
  const privacyState = {
    antiRecall:
      initialAntiRecallRaw === null
        ? true
        : initialAntiRecallRaw !== '0' && initialAntiRecallRaw !== 'false',
    blockTyping: false,
    blockSeen: false,
  };
  let activeVaultCryptoKey = null; // non-extractable SubtleCrypto AES-GCM key when PIN lock is active
  let vaultDirtyWhileLocked = false;
  let pendingUnlockPromise = null;
  let dismissUnlockOverlayFn = null;

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

  const pendingHostWrites = new Set();
  function trackHostWrite(promise) {
    if (!promise || typeof promise.then !== 'function') return promise;
    pendingHostWrites.add(promise);
    const done = () => pendingHostWrites.delete(promise);
    promise.then(done, done);
    return promise;
  }

  function idbPutKv(k, v) {
    return trackHostWrite(
      (async () => {
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
            tx.onabort = () => {
              db.close();
              resolve();
            };
          });
        } catch {}
      })()
    );
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

  async function deriveVaultKeyFromPin(pinStr, saltBytes, iterOverride) {
    const pin = String(pinStr == null ? '' : pinStr);
    if (!pin.length) {
      throw new Error('PIN cannot be empty');
    }
    const pinBytes = new TextEncoder().encode(pin);
    const subtle = globalScope.crypto && globalScope.crypto.subtle;
    const hasExplicitIter = Number.isFinite(Number(iterOverride)) && Number(iterOverride) > 0;
    const targetIter = hasExplicitIter ? Math.floor(Number(iterOverride)) : PBKDF2_ITERATIONS;
    if (subtle && typeof subtle.importKey === 'function' && typeof subtle.deriveKey === 'function') {
      try {
        const baseKey = await subtle.importKey('raw', pinBytes, 'PBKDF2', false, ['deriveKey']);
        const derivedKey = await subtle.deriveKey(
          {
            name: 'PBKDF2',
            salt: saltBytes,
            iterations: targetIter,
            hash: 'SHA-256',
          },
          baseKey,
          { name: 'AES-GCM', length: 256 },
          false, // non-extractable!
          ['encrypt', 'decrypt']
        );
        return { key: derivedKey, iter: targetIter };
      } catch {}
    }
    const fallbackIter = hasExplicitIter ? targetIter : Math.min(PBKDF2_ITERATIONS, 100000);
    return {
      key: { __rawAes256Key: pbkdf2HmacSha256Sync(pinBytes, saltBytes, fallbackIter) },
      iter: fallbackIter,
    };
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

  function isVaultLocked() {
    return Boolean(getVaultMeta() && !activeVaultCryptoKey);
  }

  async function verifyVaultPin(pinStr, meta) {
    const activeMeta = meta || getVaultMeta();
    if (!activeMeta) {
      throw new Error('No Vault PIN configured');
    }
    const salt = core.decodeBase64Forgiving(activeMeta.saltB64);
    const { key } = await deriveVaultKeyFromPin(
      pinStr,
      salt,
      activeMeta.iter || PBKDF2_ITERATIONS
    );
    const iv = core.decodeBase64Forgiving(activeMeta.ivB64);
    const ct = core.decodeBase64Forgiving(activeMeta.verifierB64);
    let ptBytes;
    try {
      ptBytes = await aesGcmDecryptCompat(key, iv, ct);
    } catch {
      throw new Error('Invalid PIN');
    }
    const txt = new TextDecoder().decode(ptBytes);
    if (txt !== 'olaz-vault-ok-v1:' + ACTIVE_SLOT) {
      throw new Error('Invalid PIN');
    }
    return key;
  }

  async function hydrateSealedRecordsWithKey(cryptoKey) {
    if (!cryptoKey) return;
    try {
      const hostDb = await openHostIdb();
      if (!hostDb) return;
      const rows = await idbGetAllKv(hostDb);
      hostDb.close();
      for (const row of rows) {
        if (!row || typeof row.k !== 'string') continue;
        if (!row.v || typeof row.v !== 'object' || row.v.__sealedVaultV1 !== 1) continue;
        try {
          if (row.k === 'cookies' && !cookieJar.exportJson().length) {
            const opened = await openWithVaultKey(cryptoKey, row.v);
            cookieJar.importJson(opened);
          } else if (row.k === 'keychain' && keychainRam.size === 0) {
            const opened = await openWithVaultKey(cryptoKey, row.v);
            if (opened && typeof opened === 'object') {
              for (const [acc, b64] of Object.entries(opened)) {
                if (typeof b64 === 'string' && !keychainRam.has(acc)) {
                  keychainRam.set(acc, core.decodeBase64Forgiving(b64));
                }
              }
            }
          } else if (row.k === 'store:zsafe-storage' && !hostStoresRam.has('zsafe-storage')) {
            const opened = await openWithVaultKey(cryptoKey, row.v);
            if (opened && typeof opened === 'object') {
              hostStoresRam.set('zsafe-storage', opened);
            }
          }
        } catch {}
      }
    } catch {}
  }

  function postVaultIpcMessage(type, extra) {
    if (!ipcBus) return;
    const msg = Object.assign({ __olazType: type, sender: CONTEXT_ID }, extra || {});
    try {
      ipcBus.postMessage(msg);
    } catch {
      if (msg.key) {
        const slim = Object.assign({}, msg);
        delete slim.key;
        try {
          ipcBus.postMessage(slim);
        } catch {}
      }
    }
  }

  async function flushVaultDirtyAfterUnlock() {
    if (!vaultDirtyWhileLocked || isVaultLocked()) return;
    vaultDirtyWhileLocked = false;
    await persistCookiesNow();
    if (hostStoresRam.has('zsafe-storage')) {
      await persistStoreToIdb('zsafe-storage', hostStoresRam.get('zsafe-storage'));
    }
    if (keychainRam.size > 0) {
      await persistKeychainToIdb();
    }
  }

  async function configureVaultPin(newPinStr, currentPinOrOpts) {
    const existingMeta = getVaultMeta();
    const currentPin =
      typeof currentPinOrOpts === 'string'
        ? currentPinOrOpts
        : currentPinOrOpts && typeof currentPinOrOpts === 'object' && typeof currentPinOrOpts.currentPin === 'string'
          ? currentPinOrOpts.currentPin
          : '';

    if (existingMeta) {
      const verifiedOldKey = await verifyVaultPin(currentPin, existingMeta);
      await hydrateSealedRecordsWithKey(verifiedOldKey);
    }

    const cleanNewPin = String(newPinStr == null ? '' : newPinStr).trim();
    if (!cleanNewPin) {
      activeVaultCryptoKey = null;
      vaultDirtyWhileLocked = false;
      rawLsRemove(VAULT_META_KEY);
    } else {
      if (cleanNewPin.length < 4) {
        throw new Error('PIN must be at least 4 characters');
      }
      const salt = core.csprngBytes(16);
      const iv = core.csprngBytes(12);
      const { key, iter } = await deriveVaultKeyFromPin(cleanNewPin, salt);
      const verifierPt = new TextEncoder().encode('olaz-vault-ok-v1:' + ACTIVE_SLOT);
      const verifierCt = await aesGcmEncryptCompat(key, iv, verifierPt);
      rawLsSet(
        VAULT_META_KEY,
        JSON.stringify({
          v: 1,
          iter,
          saltB64: core.bytesToBase64(salt, false),
          ivB64: core.bytesToBase64(iv, false),
          verifierB64: core.bytesToBase64(verifierCt, false),
        })
      );
      activeVaultCryptoKey = key;
      vaultDirtyWhileLocked = false;
    }

    // Re-persist sensitive records (`cookies`, `zsafe-storage`, `keychain`) encrypted (or decrypted if cleared)
    await persistCookiesNow();
    if (hostStoresRam.has('zsafe-storage')) {
      await persistStoreToIdb('zsafe-storage', hostStoresRam.get('zsafe-storage'));
    }
    await persistKeychainToIdb();
    await flushAllPendingWrites();

    if (dismissUnlockOverlayFn && (!cleanNewPin || activeVaultCryptoKey)) {
      dismissUnlockOverlayFn(activeVaultCryptoKey);
    }
    postVaultIpcMessage('vault-pin-updated', {
      hasPin: Boolean(cleanNewPin),
      key: activeVaultCryptoKey || null,
    });
    if (refreshDeckUiFn) refreshDeckUiFn();
  }

  function promptForVaultUnlockModal(meta) {
    const activeMeta = meta || getVaultMeta();
    if (!activeMeta) return Promise.resolve(null);
    if (activeVaultCryptoKey) return Promise.resolve(activeVaultCryptoKey);

    // Same-origin child iframe (`shared-worker.html`) inherits parent's unlocked CryptoKey!
    try {
      if (window.parent && window.parent !== window && window.parent.__olazGetActiveVaultKey) {
        const parentKey = window.parent.__olazGetActiveVaultKey(ACTIVE_SLOT);
        if (parentKey) {
          activeVaultCryptoKey = parentKey;
          return Promise.resolve(parentKey);
        }
      }
    } catch {}

    if (pendingUnlockPromise) return pendingUnlockPromise;

    pendingUnlockPromise = new Promise((resolve) => {
      const mount = () => {
        const host = document.createElement('div');
        host.id = '__olaz-vault-unlock-overlay';
        const shadow = host.attachShadow({ mode: 'open' });
        shieldShadowRootEvents(shadow);
        const roundsLabel = Number(activeMeta.iter || PBKDF2_ITERATIONS).toLocaleString('en-US');
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
              width: 368px; max-width: 92vw; padding: 28px; border-radius: 18px;
              background: rgba(30, 41, 59, 0.94);
              border: 1px solid rgba(148, 163, 184, 0.24);
              box-shadow: 0 24px 60px rgba(0, 0, 0, 0.65);
            }
            h2 { margin: 0 0 6px; font-size: 18px; font-weight: 600; }
            p { margin: 0 0 18px; font-size: 13px; color: #94a3b8; line-height: 1.45; }
            input {
              width: 100%; box-sizing: border-box; padding: 11px 14px;
              border-radius: 10px; border: 1px solid #334155;
              background: #0f172a; color: #f8fafc; font-size: 16px;
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
            .secondary { background: rgba(51, 65, 85, 0.72); color: #e2e8f0; flex: 0 0 auto; }
            .danger { background: rgba(239, 68, 68, 0.16); color: #fca5a5; flex: 0 0 auto; }
            .danger-solid { background: #dc2626; color: #fff; flex: 0 0 auto; }
            .hidden { display: none !important; }
          </style>
          <div class="backdrop">
            <form class="card" id="f">
              <h2>Unlock Browser Vault (${ACTIVE_SLOT})</h2>
              <p>Your Zalo session &amp; E2EE keys are encrypted locally with AES-256-GCM (PBKDF2 ${roundsLabel} rounds). Zero data is stored on the server.</p>
              <input id="pin" type="password" placeholder="Enter Vault PIN" autocomplete="current-password" autofocus />
              <div class="err" id="err"></div>
              <div class="actions">
                <button type="submit" class="primary" id="btn">Unlock Session</button>
                <button type="button" class="danger" id="wipe">Wipe</button>
                <button type="button" class="danger-solid hidden" id="wipeConfirm">Confirm Wipe?</button>
                <button type="button" class="secondary hidden" id="wipeCancel">Cancel</button>
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
        const wipeConfirmBtn = shadow.getElementById('wipeConfirm');
        const wipeCancelBtn = shadow.getElementById('wipeCancel');

        const setWipeStep = (confirming) => {
          wipeBtn.classList.toggle('hidden', confirming);
          wipeConfirmBtn.classList.toggle('hidden', !confirming);
          wipeCancelBtn.classList.toggle('hidden', !confirming);
          errEl.textContent = confirming
            ? 'Permanently wipe local session & PIN for "' + ACTIVE_SLOT + '"?'
            : '';
        };

        dismissUnlockOverlayFn = (unlockedKey) => {
          pendingUnlockPromise = null;
          dismissUnlockOverlayFn = null;
          try {
            host.remove();
          } catch {}
          resolve(unlockedKey);
        };

        const focusTimer = setTimeout(() => {
          if (input && typeof input.focus === 'function') input.focus();
        }, 20);
        if (focusTimer && typeof focusTimer.unref === 'function') focusTimer.unref();

        form.addEventListener('submit', async (ev) => {
          if (ev && typeof ev.preventDefault === 'function') ev.preventDefault();
          setWipeStep(false);
          errEl.textContent = '';
          btn.disabled = true;
          btn.textContent = 'Deriving Key…';
          try {
            const latestMeta = getVaultMeta() || activeMeta;
            const key = await verifyVaultPin(input.value, latestMeta);
            activeVaultCryptoKey = key;
            if (dismissUnlockOverlayFn) dismissUnlockOverlayFn(key);
            postVaultIpcMessage('vault-unlocked', { key });
            await flushVaultDirtyAfterUnlock();
            if (refreshDeckUiFn) refreshDeckUiFn();
          } catch {
            errEl.textContent = 'Incorrect PIN. Please try again.';
            btn.disabled = false;
            btn.textContent = 'Unlock Session';
            if (input && typeof input.select === 'function') input.select();
          }
        });

        wipeBtn.addEventListener('click', () => {
          setWipeStep(true);
        });
        wipeCancelBtn.addEventListener('click', () => {
          setWipeStep(false);
        });
        wipeConfirmBtn.addEventListener('click', async () => {
          wipeConfirmBtn.disabled = true;
          wipeCancelBtn.disabled = true;
          await wipeLocalSlotSession();
        });
      };

      if (document.documentElement || document.body) mount();
      else document.addEventListener('DOMContentLoaded', mount, { once: true });
    });

    return pendingUnlockPromise;
  }

  async function unlockVaultWithPin(pinStr) {
    const meta = getVaultMeta();
    if (!meta) return null;
    const key = await verifyVaultPin(pinStr, meta);
    activeVaultCryptoKey = key;
    if (dismissUnlockOverlayFn) {
      dismissUnlockOverlayFn(key);
    }
    postVaultIpcMessage('vault-unlocked', { key });
    await flushVaultDirtyAfterUnlock();
    if (refreshDeckUiFn) refreshDeckUiFn();
    return key;
  }

  async function lockVaultNow() {
    const meta = getVaultMeta();
    if (!meta) return false;
    await flushAllPendingWrites();
    activeVaultCryptoKey = null;
    postVaultIpcMessage('vault-lock', {});
    if (refreshDeckUiFn) refreshDeckUiFn();
    const unlockedKey = await promptForVaultUnlockModal(meta);
    activeVaultCryptoKey = unlockedKey;
    await flushVaultDirtyAfterUnlock();
    if (refreshDeckUiFn) refreshDeckUiFn();
    return true;
  }

  window.__olazGetActiveVaultKey = (slot) =>
    slot === ACTIVE_SLOT ? activeVaultCryptoKey : null;

  window.__olazVault = {
    hasPin: () => Boolean(getVaultMeta()),
    isLocked: () => isVaultLocked(),
    configurePin: configureVaultPin,
    verifyPin: (pin) => verifyVaultPin(pin, getVaultMeta()),
    lockNow: lockVaultNow,
    unlockWithPin: unlockVaultWithPin,
  };

  function persistCookiesNow() {
    if (isVaultLocked()) {
      vaultDirtyWhileLocked = true;
      return Promise.resolve();
    }
    return trackHostWrite(
      (async () => {
        if (isVaultLocked()) {
          vaultDirtyWhileLocked = true;
          return;
        }
        const exported = cookieJar.exportJson();
        const payload = activeVaultCryptoKey
          ? await sealWithVaultKey(activeVaultCryptoKey, exported)
          : exported;
        await idbPutKv('cookies', payload);
      })()
    );
  }
  persistCookiesFn = () => {
    persistCookiesNow().catch(() => {});
  };

  function persistStoreToIdb(storeName, storeObj) {
    const isSensitive = storeName === 'zsafe-storage';
    if (isSensitive && isVaultLocked()) {
      vaultDirtyWhileLocked = true;
      return Promise.resolve();
    }
    return trackHostWrite(
      (async () => {
        if (isSensitive && isVaultLocked()) {
          vaultDirtyWhileLocked = true;
          return;
        }
        const payload =
          isSensitive && activeVaultCryptoKey
            ? await sealWithVaultKey(activeVaultCryptoKey, storeObj)
            : storeObj;
        await idbPutKv('store:' + storeName, payload);
      })()
    );
  }

  function persistKeychainToIdb() {
    if (isVaultLocked()) {
      vaultDirtyWhileLocked = true;
      return Promise.resolve();
    }
    return trackHostWrite(
      (async () => {
        if (isVaultLocked()) {
          vaultDirtyWhileLocked = true;
          return;
        }
        const obj = {};
        for (const [k, u8] of keychainRam.entries()) {
          obj[k] = core.bytesToBase64(u8, false);
        }
        const payload = activeVaultCryptoKey
          ? await sealWithVaultKey(activeVaultCryptoKey, obj)
          : obj;
        await idbPutKv('keychain', payload);
      })()
    );
  }

  async function flushAllPendingWrites() {
    try {
      await persistCookiesNow();
    } catch {}
    let guard = 0;
    while (pendingHostWrites.size > 0 && guard++ < 8) {
      await Promise.allSettled(Array.from(pendingHostWrites));
    }
  }

  async function persistPrivacySettings() {
    await idbPutKv('privacy', Object.assign({}, privacyState));
  }

  function notifyPrivacyListeners() {
    const snapshot = Object.assign({}, privacyState);
    for (const fn of olazHost.privacyListeners) {
      try {
        fn(snapshot);
      } catch {}
    }
  }

  function setAntiRecallState(enabled, opts) {
    const options = opts && typeof opts === 'object' ? opts : {};
    const boolVal = Boolean(enabled);
    privacyState.antiRecall = boolVal;
    if (options.persist !== false) {
      rawLsSet(ANTI_RECALL_LS_KEY, boolVal ? '1' : '0');
      persistPrivacySettings();
    }
    if (options.broadcast !== false && ipcBus) {
      try {
        ipcBus.postMessage({
          __olazType: 'privacy-sync',
          sender: CONTEXT_ID,
          privacy: Object.assign({}, privacyState),
        });
      } catch {}
    }
    notifyPrivacyListeners();
    if (refreshDeckUiFn) refreshDeckUiFn();
    return boolVal;
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

  function persistVfsEntry(entry) {
    return trackHostWrite(
      (async () => {
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
            tx.onabort = () => {
              db.close();
              resolve();
            };
          });
        } catch {}
        // Best-effort OPFS mirror when available
        try {
          if (
            !entry.isDir &&
            entry.data &&
            navigator.storage &&
            typeof navigator.storage.getDirectory === 'function'
          ) {
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
      })()
    );
  }

  function removeVfsEntry(normPath) {
    vfsEntriesRam.delete(normPath);
    const prevBlob = vfsBlobUrlCache.get(normPath);
    if (prevBlob) {
      vfsBlobUrlCache.delete(normPath);
      if (typeof URL !== 'undefined' && typeof URL.revokeObjectURL === 'function') {
        try {
          URL.revokeObjectURL(prevBlob.url);
        } catch {}
      }
    }
    if (ipcBus) {
      try {
        ipcBus.postMessage({
          __olazType: 'vfs-sync',
          sender: CONTEXT_ID,
          op: 'rm',
          path: normPath,
        });
      } catch {}
    }
    return trackHostWrite(
      (async () => {
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
            tx.onabort = () => {
              db.close();
              resolve();
            };
          });
        } catch {}
      })()
    );
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
    if (ipcBus && entry.size <= 16 * 1024 * 1024) {
      try {
        ipcBus.postMessage({
          __olazType: 'vfs-sync',
          sender: CONTEXT_ID,
          op: 'put',
          entry: {
            path: entry.path,
            isDir: false,
            size: entry.size,
            mtimeMs: entry.mtimeMs,
            data: entry.data,
            mime: entry.mime || '',
          },
        });
      } catch {}
    }
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

  Object.assign(olazHost, {
    vfsEntriesRam,
    vfsBlobUrlCache,
    VFS_USER_DATA,
    VFS_DOCUMENTS,
    VFS_DOWNLOADS,
    VFS_TEMP,
    normPosixPath,
    guessMimeFromExt,
    ensureVfsDirsInRam,
    openVfsIdb,
    openHostIdb,
    writeVfsFileSync,
    removeVfsEntry,
    persistVfsEntry,
    formatStatEntry,
    makeFsErr,
    rewriteLocalZfileUrl,
    shieldShadowRootEvents,
    getPrivacyState: () => Object.assign({}, privacyState),
    setAntiRecall: (val, opts) => setAntiRecallState(val, opts),
  });

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

  // In-browser `ZaloMainProcess` cross-window state & `SharedWorkerRouter`
  // (`crates/olaz-app/src/main_process.rs` parity across `login.html`, `index.html`, and `shared-worker.html`).
  const IS_SHARED_WORKER_FRAME =
    window.location.pathname.endsWith('/shared-worker.html') ||
    new URLSearchParams(window.location.search).get('is_main') === 'false';
  const MP_STATE_SS_KEY = 'olaz:__mp_state:' + ACTIVE_SLOT;
  const MP_PUSH_LOGIN_SS_KEY = 'olaz:__mp_push_login_success:' + ACTIVE_SLOT;

  const mpState = {
    session: null,
    announcedSession: null,
    viewerKey: '',
    remoteConfig: null,
  };

  (function hydrateMpStateFromSession() {
    try {
      const savedRaw = rawSsGet(MP_STATE_SS_KEY);
      if (savedRaw) {
        const parsed = JSON.parse(savedRaw);
        if (parsed && typeof parsed === 'object') {
          if (parsed.session !== undefined) mpState.session = parsed.session;
          if (parsed.announcedSession !== undefined) mpState.announcedSession = parsed.announcedSession;
          if (typeof parsed.viewerKey === 'string') mpState.viewerKey = parsed.viewerKey;
          if (parsed.remoteConfig !== undefined) mpState.remoteConfig = parsed.remoteConfig;
        }
      }
    } catch {}
    try {
      if (window.parent && window.parent !== window && window.parent.__olazGetMainProcessState) {
        const parentSt = window.parent.__olazGetMainProcessState(ACTIVE_SLOT);
        if (parentSt && typeof parentSt === 'object') {
          if (parentSt.session !== null && parentSt.session !== undefined) mpState.session = parentSt.session;
          if (parentSt.announcedSession !== null && parentSt.announcedSession !== undefined) {
            mpState.announcedSession = parentSt.announcedSession;
          }
          if (parentSt.viewerKey) mpState.viewerKey = parentSt.viewerKey;
          if (parentSt.remoteConfig !== null && parentSt.remoteConfig !== undefined) {
            mpState.remoteConfig = parentSt.remoteConfig;
          }
        }
      }
    } catch {}
  })();

  window.__olazGetMainProcessState = (slot) =>
    slot === ACTIVE_SLOT ? Object.assign({}, mpState) : null;
  window.__olazApplyMainProcessPatch = (slot, patch) => {
    if (slot === ACTIVE_SLOT && patch && typeof patch === 'object') {
      Object.assign(mpState, patch);
    }
  };

  function saveMpState(patch, broadcast = true) {
    if (patch && typeof patch === 'object') {
      Object.assign(mpState, patch);
    }
    try {
      rawSsSet(MP_STATE_SS_KEY, JSON.stringify(mpState));
    } catch {}
    try {
      if (window.parent && window.parent !== window && window.parent.__olazApplyMainProcessPatch) {
        window.parent.__olazApplyMainProcessPatch(ACTIVE_SLOT, patch);
      }
    } catch {}
    if (broadcast && ipcBus) {
      try {
        ipcBus.postMessage({
          __olazType: 'mp-state-sync',
          sender: CONTEXT_ID,
          state: Object.assign({}, mpState),
        });
      } catch {}
    }
  }

  Object.assign(olazHost, {
    emitLocalIpcEvent,
    addIpcSub,
    removeIpcSub,
    ipcSubs,
    mpState,
    saveMpState,
  });

  let sharedWorkerIframe = null;
  let sharedWorkerBootDone = false;
  const swHandlers = new Map(); // taskType -> handlerContextId
  const swLocalHandlers = new Set(); // taskTypes registered locally by this window
  const swPending = new Map(); // routedKey -> { origId, taskType, handlerCtx }
  const swQueuedRuns = []; // Array<{ task: object, timer: any }>
  let closeDbsResolvers = [];

  function valueIdStr(v) {
    if (v === undefined || v === null) return 'null';
    if (typeof v === 'string') return v;
    if (typeof v === 'number' || typeof v === 'boolean') return String(v);
    try {
      return JSON.stringify(v);
    } catch {
      return String(v);
    }
  }

  function respondSharedWorkerEntry(entry, hasResult, resultVal, hasError, errorVal) {
    const out = { id: entry.origId };
    if (hasResult) out.result = resultVal;
    if (hasError) out.error = errorVal;
    setTimeout(() => emitLocalIpcEvent('shared-worker-resp', [out]), 0);
  }

  function routeSharedWorkerTaskNow(task) {
    const origId = task.id;
    const taskType = typeof task.type === 'string' ? task.type : '';
    const handlerCtx = swHandlers.get(taskType);
    if (!handlerCtx) {
      setTimeout(() => {
        emitLocalIpcEvent('shared-worker-resp', [
          { id: origId, error: 'Task type is not supported' },
        ]);
      }, 0);
      return;
    }
    const routedKey = CONTEXT_ID + '_' + valueIdStr(origId);
    swPending.set(routedKey, { origId, taskType, handlerCtx });
    const routedTask = Object.assign({}, task, { id: routedKey });
    if (handlerCtx === CONTEXT_ID) {
      setTimeout(() => emitLocalIpcEvent('shared-worker-run', [routedTask]), 0);
    } else if (ipcBus) {
      ipcBus.postMessage({
        __olazType: 'sw-run',
        sender: CONTEXT_ID,
        target: handlerCtx,
        task: routedTask,
      });
    } else {
      swPending.delete(routedKey);
      setTimeout(() => {
        emitLocalIpcEvent('shared-worker-resp', [{ id: origId, error: 'Worker is not ready' }]);
      }, 0);
    }
  }

  function flushQueuedSharedWorkerRuns() {
    if (!swQueuedRuns.length) return;
    const remaining = [];
    while (swQueuedRuns.length > 0) {
      const item = swQueuedRuns.shift();
      const taskType = typeof item.task.type === 'string' ? item.task.type : '';
      if (swHandlers.has(taskType)) {
        clearTimeout(item.timer);
        routeSharedWorkerTaskNow(item.task);
      } else if (sharedWorkerBootDone) {
        clearTimeout(item.timer);
        // Give async post-load handler registration a brief 200ms grace window before failing
        setTimeout(() => {
          if (swHandlers.has(taskType)) {
            routeSharedWorkerTaskNow(item.task);
          } else {
            emitLocalIpcEvent('shared-worker-resp', [
              { id: item.task.id, error: 'Task type is not supported' },
            ]);
          }
        }, 200);
      } else {
        remaining.push(item);
      }
    }
    for (const r of remaining) swQueuedRuns.push(r);
  }

  function interruptStaleWorkerPending(newHandlerCtx) {
    const beforeKeys = Array.from(swPending.keys());
    if (!beforeKeys.length) return;
    setTimeout(() => {
      for (const k of beforeKeys) {
        const entry = swPending.get(k);
        if (entry && entry.handlerCtx !== newHandlerCtx) {
          swPending.delete(k);
          respondSharedWorkerEntry(entry, false, undefined, true, {
            name: 'WorkerInterrupted',
            message: 'Worker is interrupted',
          });
        }
      }
    }, 100);
  }

  function resetMpState(broadcast = true) {
    mpState.session = null;
    mpState.announcedSession = null;
    mpState.viewerKey = '';
    mpState.remoteConfig = null;
    rawSsRemove(MP_STATE_SS_KEY);
    rawSsRemove(MP_PUSH_LOGIN_SS_KEY);
    swHandlers.clear();
    swLocalHandlers.clear();
    swPending.clear();
    while (swQueuedRuns.length > 0) {
      const it = swQueuedRuns.shift();
      clearTimeout(it.timer);
    }
    sharedWorkerBootDone = false;
    if (sharedWorkerIframe) {
      try {
        sharedWorkerIframe.remove();
      } catch {}
      sharedWorkerIframe = null;
    }
    if (broadcast && ipcBus) {
      try {
        ipcBus.postMessage({ __olazType: 'mp-state-reset', sender: CONTEXT_ID });
      } catch {}
    }
  }

  function notifySharedWorkerBootComplete() {
    sharedWorkerBootDone = true;
    if (mpState.remoteConfig !== null && mpState.remoteConfig !== undefined) {
      emitLocalIpcEvent('shared-worker-remote-config-change', [mpState.remoteConfig]);
    }
    if (mpState.viewerKey) {
      emitLocalIpcEvent('receive-viewerkey', [mpState.viewerKey]);
    }
    if (ipcBus) {
      try {
        ipcBus.postMessage({
          __olazType: 'sw-worker-loaded',
          sender: CONTEXT_ID,
          handlers: Array.from(swLocalHandlers),
        });
      } catch {}
    }
    flushQueuedSharedWorkerRuns();
  }

  function ensureSharedWorkerFrame() {
    if (IS_SHARED_WORKER_FRAME || sharedWorkerIframe) return;
    if (!window.location.pathname.endsWith('/index.html')) return;
    const mount = () => {
      if (sharedWorkerIframe || !document.body) return;
      const ifr = document.createElement('iframe');
      ifr.style.cssText = 'position:fixed;width:0;height:0;border:0;visibility:hidden;pointer-events:none;';
      ifr.setAttribute('aria-hidden', 'true');
      const q = buildZaloPageQuery({ is_main: 'false', already_login: 'y' });
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
        case 'mp-state-sync':
          if (msg.state && typeof msg.state === 'object') {
            Object.assign(mpState, msg.state);
          }
          break;
        case 'mp-state-reset':
          resetMpState(false);
          break;
        case 'ipc-broadcast':
          if (
            msg.channel === 'shared-worker-remote-config-change' &&
            !IS_SHARED_WORKER_FRAME
          ) {
            break;
          }
          emitLocalIpcEvent(msg.channel, msg.args || []);
          break;
        case 'sw-sync-req':
          if (swLocalHandlers.size > 0 || (IS_SHARED_WORKER_FRAME && sharedWorkerBootDone)) {
            ipcBus.postMessage({
              __olazType: 'sw-sync-ack',
              sender: CONTEXT_ID,
              isWorkerLoaded: IS_SHARED_WORKER_FRAME && sharedWorkerBootDone,
              handlers: Array.from(swLocalHandlers),
            });
          }
          break;
        case 'sw-sync-ack':
          if (Array.isArray(msg.handlers)) {
            for (const t of msg.handlers) {
              if (typeof t === 'string') swHandlers.set(t, msg.sender);
            }
          }
          if (msg.isWorkerLoaded) {
            sharedWorkerBootDone = true;
          }
          flushQueuedSharedWorkerRuns();
          break;
        case 'sw-worker-loaded':
          if (Array.isArray(msg.handlers)) {
            for (const t of msg.handlers) {
              if (typeof t === 'string') swHandlers.set(t, msg.sender);
            }
          }
          sharedWorkerBootDone = true;
          flushQueuedSharedWorkerRuns();
          break;
        case 'sw-register':
          if (typeof msg.taskType === 'string') {
            swHandlers.set(msg.taskType, msg.sender);
          }
          if (msg.isWorkerFrame) {
            sharedWorkerBootDone = true;
          }
          flushQueuedSharedWorkerRuns();
          break;
        case 'sw-start':
          interruptStaleWorkerPending(msg.sender);
          break;
        case 'sw-run':
          if ((!msg.target && IS_SHARED_WORKER_FRAME) || msg.target === CONTEXT_ID) {
            emitLocalIpcEvent('shared-worker-run', [msg.task]);
          }
          break;
        case 'sw-resp': {
          const p = msg.payload;
          const routedKey = p && p.id !== undefined ? valueIdStr(p.id) : '';
          if (routedKey && swPending.has(routedKey)) {
            const entry = swPending.get(routedKey);
            swPending.delete(routedKey);
            respondSharedWorkerEntry(
              entry,
              Object.prototype.hasOwnProperty.call(p, 'result'),
              p.result,
              Object.prototype.hasOwnProperty.call(p, 'error'),
              p.error
            );
          }
          break;
        }
        case 'sw-progress': {
          const p = msg.payload;
          const routedKey = p && p.id !== undefined ? valueIdStr(p.id) : '';
          if (routedKey && swPending.has(routedKey)) {
            const entry = swPending.get(routedKey);
            const fwd = { id: entry.origId, type: entry.taskType };
            if (Object.prototype.hasOwnProperty.call(p, 'progress')) fwd.progress = p.progress;
            if (Object.prototype.hasOwnProperty.call(p, 'checkpoint')) fwd.checkpoint = p.checkpoint;
            emitLocalIpcEvent('shared-worker-progress', [fwd]);
          }
          break;
        }
        case 'sw-abort':
          if ((!msg.target && IS_SHARED_WORKER_FRAME) || msg.target === CONTEXT_ID) {
            emitLocalIpcEvent('shared-worker-abort', [msg.payload]);
          }
          break;
        case 'sw-update-params':
          if ((!msg.target && IS_SHARED_WORKER_FRAME) || msg.target === CONTEXT_ID) {
            emitLocalIpcEvent('shared-worker-update-params', [msg.payload]);
          }
          break;
        case 'vfs-sync': {
          if (msg.op === 'put' && msg.entry && typeof msg.entry.path === 'string') {
            const e = msg.entry;
            ensureVfsDirsInRam(e.path);
            vfsEntriesRam.set(e.path, {
              path: e.path,
              isDir: Boolean(e.isDir),
              size: e.size || 0,
              mtimeMs: e.mtimeMs || Date.now(),
              data: e.data instanceof Uint8Array ? e.data : e.data ? new Uint8Array(e.data) : null,
              mime: e.mime || '',
            });
          } else if (msg.op === 'rm' && typeof msg.path === 'string') {
            vfsEntriesRam.delete(msg.path);
            const prevBlob = vfsBlobUrlCache.get(msg.path);
            if (prevBlob) {
              vfsBlobUrlCache.delete(msg.path);
              if (typeof URL !== 'undefined' && typeof URL.revokeObjectURL === 'function') {
                try {
                  URL.revokeObjectURL(prevBlob.url);
                } catch {}
              }
            }
          }
          break;
        }
        case 'privacy-sync':
          if (msg.privacy && typeof msg.privacy === 'object') {
            Object.assign(privacyState, msg.privacy);
            notifyPrivacyListeners();
            if (refreshDeckUiFn) refreshDeckUiFn();
          }
          break;
        case 'vault-lock': {
          activeVaultCryptoKey = null;
          if (refreshDeckUiFn) refreshDeckUiFn();
          const curMeta = getVaultMeta();
          if (curMeta && !isAuxiliaryFrame) {
            promptForVaultUnlockModal(curMeta).catch(() => {});
          }
          break;
        }
        case 'vault-unlocked':
          if (msg.key) {
            activeVaultCryptoKey = msg.key;
            if (dismissUnlockOverlayFn) dismissUnlockOverlayFn(msg.key);
            flushVaultDirtyAfterUnlock().catch(() => {});
          }
          if (refreshDeckUiFn) refreshDeckUiFn();
          break;
        case 'vault-pin-updated':
          if (!msg.hasPin) {
            activeVaultCryptoKey = null;
            vaultDirtyWhileLocked = false;
            if (dismissUnlockOverlayFn) dismissUnlockOverlayFn(null);
          } else if (msg.key) {
            activeVaultCryptoKey = msg.key;
            if (dismissUnlockOverlayFn) dismissUnlockOverlayFn(msg.key);
            flushVaultDirtyAfterUnlock().catch(() => {});
          }
          if (refreshDeckUiFn) refreshDeckUiFn();
          break;
        default:
          for (const fn of olazHost.ipcBusListeners) {
            try {
              fn(msg);
            } catch {}
          }
          break;
      }
    });
    try {
      ipcBus.postMessage({ __olazType: 'sw-sync-req', sender: CONTEXT_ID });
    } catch {}
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
    if (olazHost.syncIpcHandlers.has(channel)) {
      return olazHost.syncIpcHandlers.get(channel)(args);
    }
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
    if (olazHost.asyncIpcHandlers.has(channel)) {
      return olazHost.asyncIpcHandlers.get(channel)(args);
    }
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
        rawSsSet(MP_PUSH_LOGIN_SS_KEY, '1');
        await flushAllPendingWrites();
        const q = buildZaloPageQuery({ is_main: 'true', already_login: 'y' });
        window.location.assign('/index.html?' + q.toString());
        return undefined;
      }
      case 'login': {
        cookieJar.clearCookies();
        resetMpState();
        await flushAllPendingWrites();
        const q = buildZaloPageQuery({ logout: '1' });
        window.location.assign('/login.html?' + q.toString());
        return undefined;
      }
      case 'new-session': {
        const payload = args[0];
        const isEmpty =
          payload === undefined ||
          payload === null ||
          (typeof payload === 'object' &&
            !Array.isArray(payload) &&
            Object.keys(payload).length === 0);
        if (isEmpty) {
          saveMpState({ session: payload || null, announcedSession: null });
        } else {
          saveMpState({ session: payload, announcedSession: payload });
          if (ipcBus) {
            ipcBus.postMessage({
              __olazType: 'ipc-broadcast',
              sender: CONTEXT_ID,
              channel: 'new-session',
              args: [payload],
            });
          }
        }
        return undefined;
      }
      case 'script-connected': {
        const announced = mpState.announcedSession;
        if (announced !== null && announced !== undefined) {
          setTimeout(() => emitLocalIpcEvent('new-session', [announced]), 0);
        }
        return undefined;
      }
      case 'db-authenticated': {
        const sess = args[0];
        saveMpState({ session: sess !== undefined ? sess : null });
        if (sess !== undefined && sess !== null) {
          emitLocalIpcEvent('db-authenticated', [sess]);
          if (ipcBus) {
            ipcBus.postMessage({
              __olazType: 'ipc-broadcast',
              sender: CONTEXT_ID,
              channel: 'db-authenticated',
              args: [sess],
            });
          }
        }
        return undefined;
      }
      case 'db-client-connected': {
        const sess = mpState.session;
        if (sess !== undefined && sess !== null) {
          setTimeout(() => emitLocalIpcEvent('db-authenticated', [sess]), 0);
        }
        return undefined;
      }
      case 'shared-worker-handle': {
        if (typeof args[0] === 'string' && args[0]) {
          swLocalHandlers.add(args[0]);
          swHandlers.set(args[0], CONTEXT_ID);
          if (IS_SHARED_WORKER_FRAME) {
            sharedWorkerBootDone = true;
          }
          if (ipcBus) {
            ipcBus.postMessage({
              __olazType: 'sw-register',
              sender: CONTEXT_ID,
              taskType: args[0],
              isWorkerFrame: IS_SHARED_WORKER_FRAME,
            });
          }
          flushQueuedSharedWorkerRuns();
        }
        return undefined;
      }
      case 'shared-worker-start': {
        interruptStaleWorkerPending(CONTEXT_ID);
        if (ipcBus) {
          ipcBus.postMessage({ __olazType: 'sw-start', sender: CONTEXT_ID });
        }
        return undefined;
      }
      case 'shared-worker-run': {
        ensureSharedWorkerFrame();
        const task = args[0];
        if (!task || typeof task !== 'object') return undefined;
        const taskType = typeof task.type === 'string' ? task.type : '';
        if (swHandlers.has(taskType)) {
          routeSharedWorkerTaskNow(task);
          return undefined;
        }
        if (sharedWorkerBootDone) {
          // Allow a brief 200ms async settle window before reporting unsupported task type
          setTimeout(() => {
            if (swHandlers.has(taskType)) {
              routeSharedWorkerTaskNow(task);
            } else {
              emitLocalIpcEvent('shared-worker-resp', [
                { id: task.id, error: 'Task type is not supported' },
              ]);
            }
          }, 200);
          return undefined;
        }
        // Queue task while hidden `shared-worker.html` iframe downloads & executes its bundles
        const entry = { task, timer: null };
        entry.timer = setTimeout(() => {
          const idx = swQueuedRuns.indexOf(entry);
          if (idx >= 0) swQueuedRuns.splice(idx, 1);
          if (swHandlers.has(taskType)) {
            routeSharedWorkerTaskNow(task);
          } else {
            emitLocalIpcEvent('shared-worker-resp', [
              { id: task.id, error: 'Worker is not ready' },
            ]);
          }
        }, 20000);
        swQueuedRuns.push(entry);
        return undefined;
      }
      case 'shared-worker-resp': {
        const p = args[0];
        if (p && typeof p === 'object') {
          const routedKey = p.id !== undefined ? valueIdStr(p.id) : '';
          if (routedKey && swPending.has(routedKey)) {
            const entry = swPending.get(routedKey);
            swPending.delete(routedKey);
            respondSharedWorkerEntry(
              entry,
              Object.prototype.hasOwnProperty.call(p, 'result'),
              p.result,
              Object.prototype.hasOwnProperty.call(p, 'error'),
              p.error
            );
          } else if (ipcBus) {
            ipcBus.postMessage({ __olazType: 'sw-resp', sender: CONTEXT_ID, payload: p });
          }
        }
        return undefined;
      }
      case 'shared-worker-progress': {
        const p = args[0];
        if (p && typeof p === 'object') {
          const routedKey = p.id !== undefined ? valueIdStr(p.id) : '';
          if (routedKey && swPending.has(routedKey)) {
            const entry = swPending.get(routedKey);
            const fwd = { id: entry.origId, type: entry.taskType };
            if (Object.prototype.hasOwnProperty.call(p, 'progress')) fwd.progress = p.progress;
            if (Object.prototype.hasOwnProperty.call(p, 'checkpoint')) fwd.checkpoint = p.checkpoint;
            emitLocalIpcEvent('shared-worker-progress', [fwd]);
          } else if (ipcBus) {
            ipcBus.postMessage({ __olazType: 'sw-progress', sender: CONTEXT_ID, payload: p });
          }
        }
        return undefined;
      }
      case 'shared-worker-abort': {
        const p = args[0];
        if (p && typeof p === 'object') {
          const routedKey = CONTEXT_ID + '_' + valueIdStr(p.id);
          if (swPending.has(routedKey)) {
            const entry = swPending.get(routedKey);
            swPending.delete(routedKey);
            respondSharedWorkerEntry(entry, false, undefined, true, 'Aborted');
          }
          const taskType = typeof p.type === 'string' ? p.type : '';
          const handlerCtx = swHandlers.get(taskType);
          const abortPayload = { id: routedKey };
          if (handlerCtx === CONTEXT_ID) {
            emitLocalIpcEvent('shared-worker-abort', [abortPayload]);
          } else if (ipcBus) {
            ipcBus.postMessage({
              __olazType: 'sw-abort',
              sender: CONTEXT_ID,
              target: handlerCtx || null,
              payload: abortPayload,
            });
          }
        }
        return undefined;
      }
      case 'shared-worker-update-params': {
        const p = args[0];
        if (p && typeof p === 'object' && typeof p.type === 'string') {
          const routedKey = CONTEXT_ID + '_' + valueIdStr(p.id);
          const handlerCtx = swHandlers.get(p.type);
          const updPayload = {
            id: routedKey,
            type: p.type,
            params: p.params !== undefined ? p.params : null,
          };
          if (handlerCtx === CONTEXT_ID) {
            emitLocalIpcEvent('shared-worker-update-params', [updPayload]);
          } else if (ipcBus && handlerCtx) {
            ipcBus.postMessage({
              __olazType: 'sw-update-params',
              sender: CONTEXT_ID,
              target: handlerCtx,
              payload: updPayload,
            });
          }
        }
        return undefined;
      }
      case 'shared-worker-remote-config-change': {
        const a0 = args[0];
        const cfg =
          a0 && typeof a0 === 'object' && Object.prototype.hasOwnProperty.call(a0, 'config')
            ? a0.config
            : a0;
        if (cfg !== undefined) {
          saveMpState({ remoteConfig: cfg });
          if (IS_SHARED_WORKER_FRAME) {
            emitLocalIpcEvent('shared-worker-remote-config-change', [cfg]);
          } else if (ipcBus) {
            ipcBus.postMessage({
              __olazType: 'ipc-broadcast',
              sender: CONTEXT_ID,
              channel: 'shared-worker-remote-config-change',
              args: [cfg],
            });
          }
        }
        return undefined;
      }
      case 'show-shared-worker':
      case 'hide-shared-worker':
      case 'shared-worker-ping':
      case 'shared-worker-dispose':
        return undefined;
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
        await flushAllPendingWrites();
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
          saveMpState({ viewerKey: String(args[0]) });
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
      case 'db-start-close-dbs': {
        const subs = ipcSubs.get('db-start-close-dbs');
        if (!subs || subs.size === 0) return undefined;
        emitLocalIpcEvent('db-start-close-dbs', args);
        await new Promise((resolve) => {
          let done = false;
          const finish = () => {
            if (done) return;
            done = true;
            resolve();
          };
          closeDbsResolvers.push(finish);
          setTimeout(finish, 3000);
        });
        return undefined;
      }
      case 'db-finish-close-dbs': {
        const waiters = closeDbsResolvers.splice(0, closeDbsResolvers.length);
        for (const fn of waiters) {
          try {
            fn();
          } catch {}
        }
        return undefined;
      }
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
    znode.path = posixPath;
    znode.os = createZnodeOs(znode.os && znode.os.__info);
    znode.url = znodeUrl;
    znode.crypto = znodeCrypto;

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
      getViewerkey: async () => mpState.viewerKey || '',
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

    // Rewrite DOM `<img>` / `<video>` / `<audio>` `zfile://`, bare VFS paths & cross-origin media URLs
    const rewriteMediaUrl = (u) => {
      if (typeof u !== 'string') return u;
      if (
        /^(zfile:|file:)/i.test(u) ||
        /^zalo-app:\/\/local\/__olaz\/zfile\//i.test(u) ||
        /^\/(Users\/olaz|tmp\/olaz|olaz-vfs)(\/|$)/.test(u)
      ) {
        return rewriteLocalZfileUrl(u);
      }
      const target = parseZaloTargetUrl(u);
      if (target) return '/__olaz/media?u=' + encodeURIComponent(target.href);
      return u;
    };
    olazHost.rewriteMediaUrl = rewriteMediaUrl;
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
        const finalUrl = appendProfileParam(url);
        const childWin = origOpen.call(this, finalUrl, target, features);
        for (const fn of olazHost.popupWindowHooks) {
          try {
            fn(childWin, finalUrl);
          } catch {}
        }
        return childWin;
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

  function checkIsAuxiliaryFrame() {
    return (
      IS_SHARED_WORKER_FRAME ||
      window.__ZaBUNDLENAME__ === 'popup-viewer-pc' ||
      /\/(child|popup-viewer|znotification|sqlite)\.html$/i.test(window.location.pathname)
    );
  }
  const isAuxiliaryFrame = checkIsAuxiliaryFrame();

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
    resetMpState();
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
    if (checkIsAuxiliaryFrame() || window.__olazMobileController) return;
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

  const PILL_POS_KEY = 'olaz:__pill_pos_v1';

  function mountAccountAndPrivacyPill() {
    try {
      syncSlotWebManifest();
    } catch {}
    const mediaCtrl = installMediaSessionController();
    installMobilePaneController();

    if (checkIsAuxiliaryFrame()) return;
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
    host.setAttribute('data-dragging', 'false');

    const shadow = host.attachShadow({ mode: 'open' });
    shieldShadowRootEvents(shadow);
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
          bottom: 112px;
          z-index: 2147483640;
          display: inline-block;
          font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
          color: #f8fafc;
          user-select: none;
          -webkit-user-select: none;
        }
        .float-stack {
          position: absolute;
          bottom: 48px;
          right: 0;
          display: flex;
          flex-direction: column;
          align-items: flex-end;
          gap: 10px;
          pointer-events: none;
        }
        .float-stack > * {
          pointer-events: auto;
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
          cursor: grab;
          touch-action: none;
          font-size: 12px;
          font-weight: 600;
          color: #f8fafc;
          backdrop-filter: blur(12px);
          -webkit-backdrop-filter: blur(12px);
          transition: border-color 0.14s ease;
        }
        :host([data-dragging="true"]) .pill,
        :host([data-dragging="true"]) .brand-hdr,
        :host([data-dragging="true"]) .mc-top,
        :host([data-dragging="true"]) .call-rec-bar {
          cursor: grabbing !important;
        }
        .pill:hover {
          border-color: rgba(96, 165, 250, 0.72);
        }
        .pill:focus-visible,
        .btn:focus-visible,
        .hdr-icon-btn:focus-visible,
        .acct:focus-visible,
        .mc-btn:focus-visible,
        .mc-seek:focus-visible,
        .pin-input:focus-visible {
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
          width: 320px;
          max-width: calc(100vw - 16px);
          max-width: calc(100dvw - 16px);
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
          gap: 8px;
          margin-bottom: 8px;
          cursor: grab;
          touch-action: none;
        }
        .drag-grip {
          color: #64748b;
          font-size: 13px;
          line-height: 1;
          flex: none;
          letter-spacing: -1px;
        }
        .brand-hdr-icon {
          width: 30px;
          height: 30px;
          flex: none;
        }
        .brand-hdr-icon svg {
          width: 100%;
          height: 100%;
          display: block;
        }
        .brand-hdr-txt {
          flex: 1 1 auto;
          min-width: 0;
        }
        .hdr-actions {
          display: inline-flex;
          align-items: center;
          gap: 4px;
          flex: none;
        }
        .hdr-icon-btn {
          width: 26px;
          height: 26px;
          border-radius: 7px;
          border: 1px solid rgba(148, 163, 184, 0.2);
          background: rgba(30, 41, 59, 0.8);
          color: #cbd5e1;
          font-size: 14px;
          line-height: 1;
          display: inline-flex;
          align-items: center;
          justify-content: center;
          cursor: pointer;
          padding: 0;
        }
        .hdr-icon-btn:hover {
          background: rgba(51, 65, 85, 0.95);
          color: #ffffff;
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
        .btn-row {
          display: flex;
          gap: 6px;
          margin-top: 6px;
        }
        .btn-row .btn {
          margin-top: 0;
          flex: 1 1 0;
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
        .btn.danger-solid {
          border-color: rgba(248, 113, 113, 0.6);
          color: #ffffff;
          background: #dc2626;
        }
        .btn svg {
          width: 15px;
          height: 15px;
          flex: none;
        }
        .pin-form {
          margin-top: 8px;
          padding: 10px;
          border-radius: 11px;
          background: rgba(2, 6, 23, 0.55);
          border: 1px solid rgba(148, 163, 184, 0.18);
        }
        .pin-input {
          width: 100%;
          padding: 8px 10px;
          margin-bottom: 6px;
          border-radius: 8px;
          border: 1px solid #334155;
          background: #0f172a;
          color: #f8fafc;
          font-size: 16px;
          outline: none;
          user-select: text;
          -webkit-user-select: text;
        }
        .pin-input:focus {
          border-color: #38bdf8;
        }
        .pin-feedback {
          font-size: 11.5px;
          line-height: 1.35;
          margin-top: 6px;
          min-height: 0;
        }
        .pin-feedback.err {
          color: #f87171;
        }
        .pin-feedback.ok {
          color: #34d399;
        }
        .wipe-confirm-box {
          margin-top: 6px;
          padding: 9px;
          border-radius: 10px;
          background: rgba(127, 29, 29, 0.25);
          border: 1px solid rgba(239, 68, 68, 0.35);
        }
        .wipe-confirm-lbl {
          font-size: 11.5px;
          color: #fecaca;
          margin-bottom: 6px;
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
          width: min(340px, calc(100vw - 16px));
          width: min(340px, calc(100dvw - 16px));
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
          cursor: grab;
          touch-action: none;
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
          .panel {
            width: calc(100vw - 16px);
            width: calc(100dvw - 16px);
            max-width: 340px;
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
        <div class="float-stack" id="floatStack">
          <div class="panel" id="panel" role="region" aria-label="KV Zalo Vault and Accounts">
            <div class="brand-hdr" id="brandHdr">
              <span class="drag-grip" aria-hidden="true">⋮⋮</span>
              <span class="brand-hdr-icon">${renderKvZaloLogoSvg('panel')}</span>
              <div class="brand-hdr-txt">
                <div class="hdr">KV Zalo Vault &amp; Accounts</div>
                <div class="sub">100% Browser IndexedDB · 0 Bytes on Server</div>
              </div>
              <div class="hdr-actions">
                <button type="button" class="hdr-icon-btn" id="resetPosBtn" title="Reset floating position" aria-label="Reset floating position">↺</button>
                <button type="button" class="hdr-icon-btn" id="closePanelBtn" title="Close panel" aria-label="Close panel">×</button>
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
              <button type="button" class="btn" id="pinBtn">Set Local PIN</button>
              <div class="btn-row hidden" id="pinActiveRow">
                <button type="button" class="btn" id="lockVaultBtn">Lock Now</button>
                <button type="button" class="btn danger" id="clearPinBtn">Remove PIN</button>
              </div>
              <form class="pin-form hidden" id="pinForm">
                <input type="password" class="pin-input hidden" id="pinCurrentInput" placeholder="Current PIN" autocomplete="current-password" />
                <input type="password" class="pin-input hidden" id="pinNewInput" placeholder="New PIN (min 4 chars)" autocomplete="new-password" />
                <input type="password" class="pin-input hidden" id="pinConfirmInput" placeholder="Confirm PIN" autocomplete="new-password" />
                <div class="btn-row">
                  <button type="submit" class="btn primary" id="pinSubmitBtn">Save PIN</button>
                  <button type="button" class="btn" id="pinCancelBtn">Cancel</button>
                </div>
              </form>
              <div class="pin-feedback hidden" id="pinFeedback"></div>
            </div>

            <div class="sec">
              <button type="button" class="btn danger" id="wipeBtn">Wipe Local Session (${ACTIVE_SLOT})</button>
              <div class="wipe-confirm-box hidden" id="wipeConfirmBox">
                <div class="wipe-confirm-lbl">Wipe all local IndexedDB, cookies &amp; keys for "${ACTIVE_SLOT}"?</div>
                <div class="btn-row">
                  <button type="button" class="btn danger-solid" id="wipeConfirmYes">Yes, Wipe</button>
                  <button type="button" class="btn" id="wipeConfirmCancel">Cancel</button>
                </div>
              </div>
            </div>
          </div>

          <section class="media-card" id="mediaCard" aria-label="Now Playing Media Controls">
            <div class="mc-top" id="mcTop">
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
        </div>

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
    const floatStack = shadow.getElementById('floatStack');
    const pill = shadow.getElementById('pill');
    const panel = shadow.getElementById('panel');
    const brandHdr = shadow.getElementById('brandHdr');
    const resetPosBtn = shadow.getElementById('resetPosBtn');
    const closePanelBtn = shadow.getElementById('closePanelBtn');
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
    const pinActiveRow = shadow.getElementById('pinActiveRow');
    const lockVaultBtn = shadow.getElementById('lockVaultBtn');
    const clearPinBtn = shadow.getElementById('clearPinBtn');
    const pinForm = shadow.getElementById('pinForm');
    const pinCurrentInput = shadow.getElementById('pinCurrentInput');
    const pinNewInput = shadow.getElementById('pinNewInput');
    const pinConfirmInput = shadow.getElementById('pinConfirmInput');
    const pinSubmitBtn = shadow.getElementById('pinSubmitBtn');
    const pinCancelBtn = shadow.getElementById('pinCancelBtn');
    const pinFeedback = shadow.getElementById('pinFeedback');
    const wipeBtn = shadow.getElementById('wipeBtn');
    const wipeConfirmBox = shadow.getElementById('wipeConfirmBox');
    const wipeConfirmYes = shadow.getElementById('wipeConfirmYes');
    const wipeConfirmCancel = shadow.getElementById('wipeConfirmCancel');

    const mediaCard = shadow.getElementById('mediaCard');
    const mcTop = shadow.getElementById('mcTop');
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

    let isPanelOpen = false;
    let pinFormMode = null; // null | 'set' | 'change' | 'clear'
    let isWipeConfirmOpen = false;
    let suppressNextPillClick = false;
    let currentStackDir = 'above'; // 'above' | 'below'

    const EDGE_PAD = 8;
    const STACK_GAP = 10;
    const DRAG_SLOP_PX = 5;

    const loadSavedPillAnchor = () => {
      try {
        const raw = rawLsGet(PILL_POS_KEY);
        if (!raw) return null;
        const parsed = JSON.parse(raw);
        if (
          parsed &&
          (parsed.edgeX === 'left' || parsed.edgeX === 'right') &&
          (parsed.edgeY === 'top' || parsed.edgeY === 'bottom') &&
          Number.isFinite(Number(parsed.offsetX)) &&
          Number.isFinite(Number(parsed.offsetY))
        ) {
          return {
            edgeX: parsed.edgeX,
            offsetX: Math.max(4, Math.round(Number(parsed.offsetX))),
            edgeY: parsed.edgeY,
            offsetY: Math.max(4, Math.round(Number(parsed.offsetY))),
          };
        }
      } catch {}
      return null;
    };

    let customAnchor = loadSavedPillAnchor();

    const getViewportMetrics = () => {
      const vv = window.visualViewport;
      const vw =
        vv && Number(vv.width) > 0
          ? Number(vv.width)
          : Number(window.innerWidth) > 0
            ? Number(window.innerWidth)
            : 0;
      const vh =
        vv && Number(vv.height) > 0
          ? Number(vv.height)
          : Number(window.innerHeight) > 0
            ? Number(window.innerHeight)
            : 0;
      const left = vv && Number.isFinite(Number(vv.offsetLeft)) ? Math.max(0, Number(vv.offsetLeft)) : 0;
      const top = vv && Number.isFinite(Number(vv.offsetTop)) ? Math.max(0, Number(vv.offsetTop)) : 0;
      return { vw, vh, left, top, right: left + vw, bottom: top + vh };
    };

    const getDefaultPillAnchor = (vwHint) => {
      const isMobile =
        Boolean(mobileViewportMql && mobileViewportMql.matches) ||
        (vwHint > 0 && vwHint < 768);
      const isLogin = /\/login\.html$/i.test(window.location.pathname || '');
      if (isMobile) {
        if (!isLogin && (getRootMobilePane() || 'list') === 'chat') {
          return { edgeX: 'right', offsetX: 10, edgeY: 'top', offsetY: 58 };
        }
        return { edgeX: 'right', offsetX: 12, edgeY: 'bottom', offsetY: isLogin ? 18 : 66 };
      }
      if (isLogin) {
        return { edgeX: 'right', offsetX: 16, edgeY: 'bottom', offsetY: 18 };
      }
      return { edgeX: 'right', offsetX: 16, edgeY: 'bottom', offsetY: 112 };
    };

    const syncViewportCssVars = (vp) => {
      if (!vvProbe || !wrapEl || typeof vvProbe.getBoundingClientRect !== 'function') return;
      if (vp.vh <= 0) return;
      const probeRect = vvProbe.getBoundingClientRect();
      const probeBottom =
        probeRect && Number.isFinite(probeRect.bottom) && probeRect.bottom > 0
          ? probeRect.bottom
          : vp.bottom;
      const extraBottom = Math.max(0, Math.ceil(probeBottom - vp.bottom));
      const clampedHeight = Math.max(240, Math.floor(vp.vh));
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

    const layoutFloatingDeck = (lockedOrientation) => {
      const vp = getViewportMetrics();
      syncViewportCssVars(vp);
      const anchor = customAnchor || getDefaultPillAnchor(vp.vw);

      if (vp.vw <= 0 || vp.vh <= 0) {
        if (anchor.edgeX === 'left') {
          wrapEl.style.left = anchor.offsetX + 'px';
          wrapEl.style.right = 'auto';
        } else {
          wrapEl.style.right = anchor.offsetX + 'px';
          wrapEl.style.left = 'auto';
        }
        if (anchor.edgeY === 'top') {
          wrapEl.style.top = anchor.offsetY + 'px';
          wrapEl.style.bottom = 'auto';
        } else {
          wrapEl.style.bottom = anchor.offsetY + 'px';
          wrapEl.style.top = 'auto';
        }
        return;
      }

      const pillRect = typeof pill.getBoundingClientRect === 'function' ? pill.getBoundingClientRect() : null;
      const pillW = pillRect && pillRect.width > 0 ? pillRect.width : 148;
      const pillH = pillRect && pillRect.height > 0 ? pillRect.height : 38;

      const rawX =
        anchor.edgeX === 'left'
          ? vp.left + anchor.offsetX
          : vp.right - pillW - anchor.offsetX;
      const rawY =
        anchor.edgeY === 'top'
          ? vp.top + anchor.offsetY
          : vp.bottom - pillH - anchor.offsetY;

      const maxLeft = Math.max(vp.left + EDGE_PAD, vp.right - pillW - EDGE_PAD);
      const maxTop = Math.max(vp.top + EDGE_PAD, vp.bottom - pillH - EDGE_PAD);
      const pillLeft = Math.min(maxLeft, Math.max(vp.left + EDGE_PAD, rawX));
      const pillTop = Math.min(maxTop, Math.max(vp.top + EDGE_PAD, rawY));

      wrapEl.style.left = Math.round(pillLeft) + 'px';
      wrapEl.style.top = Math.round(pillTop) + 'px';
      wrapEl.style.right = 'auto';
      wrapEl.style.bottom = 'auto';

      const spaceAbove = pillTop - vp.top - EDGE_PAD - STACK_GAP;
      const spaceBelow = vp.bottom - (pillTop + pillH) - EDGE_PAD - STACK_GAP;
      const placeBelow = lockedOrientation
        ? lockedOrientation === 'below'
        : spaceBelow > spaceAbove && spaceAbove < 340;
      currentStackDir = placeBelow ? 'below' : 'above';

      if (placeBelow) {
        floatStack.style.top = Math.round(pillH + STACK_GAP) + 'px';
        floatStack.style.bottom = 'auto';
        floatStack.style.flexDirection = 'column-reverse';
      } else {
        floatStack.style.bottom = Math.round(pillH + STACK_GAP) + 'px';
        floatStack.style.top = 'auto';
        floatStack.style.flexDirection = 'column';
      }

      const availStackH = Math.max(160, Math.floor(placeBelow ? spaceBelow : spaceAbove));
      let siblingHeight = 0;
      if (floatStack.children && floatStack.children.length) {
        for (let i = 0; i < floatStack.children.length; i++) {
          const ch = floatStack.children[i];
          if (!ch || ch === panel) continue;
          if (typeof ch.getBoundingClientRect === 'function') {
            const cr = ch.getBoundingClientRect();
            if (cr && cr.height > 0) siblingHeight += cr.height + STACK_GAP;
          }
        }
      }
      panel.style.maxHeight = Math.max(150, Math.floor(availStackH - siblingHeight)) + 'px';

      const stackRect =
        typeof floatStack.getBoundingClientRect === 'function'
          ? floatStack.getBoundingClientRect()
          : null;
      const fallbackStackW = Math.min(isPanelOpen ? 320 : 340, Math.max(200, vp.vw - EDGE_PAD * 2));
      const stackW = stackRect && stackRect.width > 0 ? stackRect.width : fallbackStackW;

      const isLeftHalf = pillLeft + pillW / 2 <= vp.left + vp.vw / 2;
      floatStack.style.alignItems = isLeftHalf ? 'flex-start' : 'flex-end';
      const desiredScreenLeft = isLeftHalf ? pillLeft : pillLeft + pillW - stackW;
      const maxStackScreenLeft = Math.max(vp.left + EDGE_PAD, vp.right - stackW - EDGE_PAD);
      const clampedScreenLeft = Math.min(
        maxStackScreenLeft,
        Math.max(vp.left + EDGE_PAD, desiredScreenLeft)
      );
      floatStack.style.left = Math.round(clampedScreenLeft - pillLeft) + 'px';
      floatStack.style.right = 'auto';
    };

    const attachDragHandle = (handleEl, opts) => {
      if (!handleEl || typeof handleEl.addEventListener !== 'function') return;
      const lockOrientationOnDrag = Boolean(opts && opts.lockOrientationOnDrag);

      handleEl.addEventListener('pointerdown', (ev) => {
        if (!ev || (ev.button !== undefined && ev.button !== 0)) return;
        const t = ev.target;
        if (
          t &&
          t !== handleEl &&
          typeof t.closest === 'function' &&
          t.closest('button, input, select, textarea, a, label')
        ) {
          return;
        }
        const vpStart = getViewportMetrics();
        if (vpStart.vw <= 0 || vpStart.vh <= 0) return;

        const wrapRect =
          typeof wrapEl.getBoundingClientRect === 'function' ? wrapEl.getBoundingClientRect() : null;
        const pillRect =
          typeof pill.getBoundingClientRect === 'function' ? pill.getBoundingClientRect() : null;
        const pillW = pillRect && pillRect.width > 0 ? pillRect.width : 148;
        const pillH = pillRect && pillRect.height > 0 ? pillRect.height : 38;
        const startLeft = wrapRect ? wrapRect.left : parseFloat(wrapEl.style.left) || 0;
        const startTop = wrapRect ? wrapRect.top : parseFloat(wrapEl.style.top) || 0;
        const startClientX = Number(ev.clientX) || 0;
        const startClientY = Number(ev.clientY) || 0;
        const lockedDir = lockOrientationOnDrag ? currentStackDir : null;
        let dragging = false;

        const onMove = (moveEv) => {
          const dx = (Number(moveEv.clientX) || 0) - startClientX;
          const dy = (Number(moveEv.clientY) || 0) - startClientY;
          if (!dragging) {
            if (Math.hypot(dx, dy) < DRAG_SLOP_PX) return;
            dragging = true;
            suppressNextPillClick = true;
            host.setAttribute('data-dragging', 'true');
            if (typeof handleEl.setPointerCapture === 'function' && ev.pointerId !== undefined) {
              try {
                handleEl.setPointerCapture(ev.pointerId);
              } catch {}
            }
          }
          if (typeof moveEv.preventDefault === 'function') moveEv.preventDefault();
          const vp = getViewportMetrics();
          if (vp.vw <= 0 || vp.vh <= 0) return;
          const maxL = Math.max(vp.left + EDGE_PAD, vp.right - pillW - EDGE_PAD);
          const maxT = Math.max(vp.top + EDGE_PAD, vp.bottom - pillH - EDGE_PAD);
          const nextL = Math.min(maxL, Math.max(vp.left + EDGE_PAD, startLeft + dx));
          const nextT = Math.min(maxT, Math.max(vp.top + EDGE_PAD, startTop + dy));
          const edgeX = nextL + pillW / 2 < vp.left + vp.vw / 2 ? 'left' : 'right';
          const edgeY = nextT + pillH / 2 < vp.top + vp.vh / 2 ? 'top' : 'bottom';
          const offsetX = Math.max(
            EDGE_PAD,
            Math.round(edgeX === 'left' ? nextL - vp.left : vp.right - (nextL + pillW))
          );
          const offsetY = Math.max(
            EDGE_PAD,
            Math.round(edgeY === 'top' ? nextT - vp.top : vp.bottom - (nextT + pillH))
          );
          customAnchor = { edgeX, offsetX, edgeY, offsetY };
          layoutFloatingDeck(lockedDir);
        };
        onMove.__olazInternal = true;

        const onUp = () => {
          if (typeof window.removeEventListener === 'function') {
            window.removeEventListener('pointermove', onMove, true);
            window.removeEventListener('pointerup', onUp, true);
            window.removeEventListener('pointercancel', onUp, true);
          }
          host.setAttribute('data-dragging', 'false');
          if (dragging && customAnchor) {
            rawLsSet(PILL_POS_KEY, JSON.stringify(customAnchor));
            layoutFloatingDeck(null);
            const clearClickGuard = setTimeout(() => {
              suppressNextPillClick = false;
            }, 120);
            if (clearClickGuard && typeof clearClickGuard.unref === 'function') {
              clearClickGuard.unref();
            }
          }
        };
        onUp.__olazInternal = true;

        if (typeof window.addEventListener === 'function') {
          window.addEventListener('pointermove', onMove, true);
          window.addEventListener('pointerup', onUp, true);
          window.addEventListener('pointercancel', onUp, true);
        }
      });
    };

    attachDragHandle(pill, { lockOrientationOnDrag: false });
    attachDragHandle(brandHdr, { lockOrientationOnDrag: true });
    attachDragHandle(mcTop, { lockOrientationOnDrag: true });

    const onViewportResize = () => layoutFloatingDeck(null);
    onViewportResize.__olazInternal = true;
    window.addEventListener('resize', onViewportResize, { passive: true });
    window.addEventListener('orientationchange', onViewportResize, { passive: true });
    if (window.visualViewport && typeof window.visualViewport.addEventListener === 'function') {
      window.visualViewport.addEventListener('resize', onViewportResize, { passive: true });
      window.visualViewport.addEventListener('scroll', onViewportResize, { passive: true });
    }
    if (typeof window.ResizeObserver === 'function') {
      try {
        const ro = new window.ResizeObserver(() => layoutFloatingDeck(null));
        if (document.documentElement) ro.observe(document.documentElement);
        if (document.body) ro.observe(document.body);
      } catch {}
    }

    let isDraggingSeek = false;
    let lastRenderedArtwork = '';

    const setPinFeedback = (text, kind) => {
      pinFeedback.textContent = text || '';
      pinFeedback.className =
        'pin-feedback' + (text ? (kind === 'ok' ? ' ok' : ' err') : ' hidden');
    };

    const setPinFormMode = (mode) => {
      pinFormMode = mode;
      pinCurrentInput.value = '';
      pinNewInput.value = '';
      pinConfirmInput.value = '';
      if (!mode) {
        pinForm.classList.toggle('hidden', true);
        layoutFloatingDeck(null);
        return;
      }
      pinForm.classList.toggle('hidden', false);
      const needCurrent = mode === 'change' || mode === 'clear';
      const needNew = mode === 'set' || mode === 'change';
      pinCurrentInput.classList.toggle('hidden', !needCurrent);
      pinNewInput.classList.toggle('hidden', !needNew);
      pinConfirmInput.classList.toggle('hidden', !needNew);
      pinNewInput.placeholder = mode === 'change' ? 'New PIN (min 4 chars)' : 'New PIN (min 4 chars)';
      pinConfirmInput.placeholder = mode === 'change' ? 'Confirm New PIN' : 'Confirm PIN';
      pinSubmitBtn.textContent =
        mode === 'set' ? 'Save PIN' : mode === 'change' ? 'Update PIN' : 'Remove PIN';
      setPinFeedback('', '');
      layoutFloatingDeck(null);
      const firstInput = needCurrent ? pinCurrentInput : pinNewInput;
      const ft = setTimeout(() => {
        if (firstInput && typeof firstInput.focus === 'function') firstInput.focus();
      }, 15);
      if (ft && typeof ft.unref === 'function') ft.unref();
    };

    const setPanelOpen = (open) => {
      isPanelOpen = Boolean(open);
      panel.classList.toggle('open', isPanelOpen);
      pill.setAttribute('aria-expanded', isPanelOpen ? 'true' : 'false');
      if (!isPanelOpen) {
        setPinFormMode(null);
        isWipeConfirmOpen = false;
        wipeConfirmBox.classList.toggle('hidden', true);
      }
      layoutFloatingDeck(null);
    };

    const renderMediaCard = () => {
      const st = mediaCtrl.getState();
      mediaCard.classList.toggle('visible', Boolean(st.visible));
      if (!st.visible) {
        layoutFloatingDeck(null);
        return;
      }

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
      layoutFloatingDeck(null);
    };
    renderMediaCardUiFn = renderMediaCard;

    const render = () => {
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
      const hasPin = Boolean(getVaultMeta());
      pinStateEl.textContent = hasPin ? 'AES-256-GCM' : 'Off';
      pinBtn.textContent = hasPin ? 'Change PIN' : 'Set Local PIN';
      pinActiveRow.classList.toggle('hidden', !hasPin);

      pwaSec.classList.toggle('hidden', !deferredPwaPrompt);

      const isMobileNow = Boolean(mobileViewportMql && mobileViewportMql.matches);
      const hasMainChat =
        window.location.pathname.endsWith('/index.html') ||
        Boolean(typeof document.getElementById === 'function' && document.getElementById('container'));
      mobileNavSec.classList.toggle('hidden', !(isMobileNow && hasMainChat));
      if (isMobileNow && hasMainChat) {
        const curPane = getRootMobilePane() || 'list';
        mobilePaneLabel.textContent =
          curPane === 'chat' ? 'Mobile View: Switch to Chats' : 'Mobile View: Switch to Thread';
      }
      renderMediaCard();
    };
    refreshDeckUiFn = render;

    pill.addEventListener('click', (ev) => {
      if (suppressNextPillClick) {
        suppressNextPillClick = false;
        if (ev && typeof ev.preventDefault === 'function') ev.preventDefault();
        if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
        return;
      }
      render();
      setPanelOpen(!isPanelOpen);
    });

    resetPosBtn.addEventListener('click', () => {
      customAnchor = null;
      rawLsRemove(PILL_POS_KEY);
      layoutFloatingDeck(null);
    });

    closePanelBtn.addEventListener('click', () => {
      setPanelOpen(false);
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
      setPanelOpen(false);
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
      setAntiRecallState(antiRecallCb.checked);
    });

    pinBtn.addEventListener('click', () => {
      const hasPin = Boolean(getVaultMeta());
      const targetMode = hasPin ? 'change' : 'set';
      setPinFormMode(pinFormMode === targetMode ? null : targetMode);
    });

    clearPinBtn.addEventListener('click', () => {
      setPinFormMode(pinFormMode === 'clear' ? null : 'clear');
    });

    lockVaultBtn.addEventListener('click', () => {
      setPanelOpen(false);
      lockVaultNow().catch(() => {});
    });

    pinCancelBtn.addEventListener('click', () => {
      setPinFormMode(null);
    });

    pinForm.addEventListener('submit', async (ev) => {
      if (ev && typeof ev.preventDefault === 'function') ev.preventDefault();
      if (!pinFormMode) return;
      const mode = pinFormMode;
      const curPin = String(pinCurrentInput.value || '');
      const nextPin = String(pinNewInput.value || '').trim();
      const confPin = String(pinConfirmInput.value || '').trim();

      if (mode === 'set' || mode === 'change') {
        if (nextPin.length < 4) {
          setPinFeedback('PIN must be at least 4 characters', 'err');
          return;
        }
        if (nextPin !== confPin) {
          setPinFeedback('PINs do not match', 'err');
          return;
        }
      }
      if ((mode === 'change' || mode === 'clear') && !curPin.length) {
        setPinFeedback('Incorrect current PIN', 'err');
        return;
      }

      pinSubmitBtn.disabled = true;
      try {
        if (mode === 'clear') {
          await configureVaultPin('', curPin);
          setPinFormMode(null);
          setPinFeedback('Vault PIN removed', 'ok');
        } else if (mode === 'change') {
          await configureVaultPin(nextPin, curPin);
          setPinFormMode(null);
          setPinFeedback('Vault PIN updated (AES-256-GCM)', 'ok');
        } else {
          await configureVaultPin(nextPin);
          setPinFormMode(null);
          setPinFeedback('Vault PIN enabled (AES-256-GCM)', 'ok');
        }
        render();
      } catch (err) {
        const msg = String((err && err.message) || err);
        setPinFeedback(
          /Invalid PIN/i.test(msg) ? 'Incorrect current PIN' : msg,
          'err'
        );
      } finally {
        pinSubmitBtn.disabled = false;
      }
    });

    wipeBtn.addEventListener('click', () => {
      isWipeConfirmOpen = !isWipeConfirmOpen;
      wipeConfirmBox.classList.toggle('hidden', !isWipeConfirmOpen);
      layoutFloatingDeck(null);
    });

    wipeConfirmCancel.addEventListener('click', () => {
      isWipeConfirmOpen = false;
      wipeConfirmBox.classList.toggle('hidden', true);
      layoutFloatingDeck(null);
    });

    wipeConfirmYes.addEventListener('click', async () => {
      wipeConfirmYes.disabled = true;
      wipeConfirmCancel.disabled = true;
      await wipeLocalSlotSession();
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

    const deckContext = {
      host,
      shadow,
      wrapEl,
      floatStack,
      panel,
      pill,
      mediaCard,
      refreshDeck: render,
      requestLayout: () => layoutFloatingDeck(null),
      attachDragHandle,
    };
    olazHost.deckContext = deckContext;
    for (const fn of olazHost.deckMountHooks) {
      try {
        fn(deckContext);
      } catch {}
    }
    const origDeckPush = olazHost.deckMountHooks.push.bind(olazHost.deckMountHooks);
    olazHost.deckMountHooks.push = function (...fns) {
      const res = origDeckPush(...fns);
      for (const fn of fns) {
        if (typeof fn === 'function') {
          try {
            fn(deckContext);
          } catch {}
        }
      }
      layoutFloatingDeck(null);
      return res;
    };
    layoutFloatingDeck(null);
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
            const lsAnti = rawLsGet(ANTI_RECALL_LS_KEY);
            if (lsAnti !== null) {
              privacyState.antiRecall = lsAnti !== '0' && lsAnti !== 'false';
            } else {
              rawLsSet(ANTI_RECALL_LS_KEY, privacyState.antiRecall ? '1' : '0');
            }
          } else if (row.k.startsWith('store:')) {
            const name = row.k.slice('store:'.length);
            const opened =
              name === 'zsafe-storage'
                ? await openWithVaultKey(activeVaultCryptoKey, row.v)
                : row.v;
            if (opened && typeof opened === 'object') {
              hostStoresRam.set(name, opened);
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

    if (hostStoresRam.has('database-config')) {
      const prevDbCfg = hostStoresRam.get('database-config');
      const nextDbCfg = readNamedStoreSync('database-config');
      let dbCfgChanged = false;
      try {
        dbCfgChanged = JSON.stringify(prevDbCfg) !== JSON.stringify(nextDbCfg);
      } catch {
        dbCfgChanged = true;
      }
      if (dbCfgChanged) {
        writeNamedStoreSync('database-config', nextDbCfg);
      } else {
        hostStoresRam.set('database-config', nextDbCfg);
      }
    }

    if (IS_WINDOW && rawLocalStorage && rawStorageMethods) {
      try {
        const lsKeys = [];
        for (let i = 0; i < rawLocalStorage.length; i++) {
          const k = rawStorageMethods.key.call(rawLocalStorage, i);
          if (typeof k === 'string') lsKeys.push(k);
        }
        const staleSuffixes = ['db4-ftu-required', 'sf-mg-gate', 'ismigtin', 'sqlite_m_d'];
        for (const k of lsKeys) {
          if (k.startsWith('olaz:__')) continue;
          const kl = k.toLowerCase();
          if (staleSuffixes.some((s) => kl === s || kl.endsWith(s))) {
            rawLsRemove(k);
          } else if (kl.endsWith('adp-st')) {
            const rawVal = rawLsGet(k);
            if (typeof rawVal === 'string') {
              try {
                const parsed = JSON.parse(rawVal);
                if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
                  let mutated = false;
                  for (const innerK of Object.keys(parsed)) {
                    if (parsed[innerK] !== 0) {
                      parsed[innerK] = 0;
                      mutated = true;
                    }
                  }
                  if (mutated) {
                    rawLsSet(k, JSON.stringify(parsed));
                  }
                  continue;
                }
              } catch {}
              if (rawVal !== '0') {
                rawLsSet(k, '0');
              }
            }
          }
        }
      } catch {}
    }

    if (!hostImei) {
      hostImei = generateDefaultImei();
      idbPutKv('imei', hostImei);
    }

    notifyPrivacyListeners();

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

  let tryApplyWebpackRuntimePatchesNow = null;

  function installBrowserWebpackRuntimePatches() {
    if (window.__olazWebpackPatchesInstalled) return;
    window.__olazWebpackPatchesInstalled = true;

    const eachExport = (req, visit) => {
      if (!req || !req.c) return;
      for (const [id, mod] of Object.entries(req.c)) {
        const ex = mod && mod.exports;
        if (!ex) continue;
        try {
          visit(ex, id);
        } catch {}
        if (typeof ex === 'object') {
          for (const k of Object.keys(ex)) {
            try {
              if (ex[k]) visit(ex[k], id);
            } catch {}
          }
        }
      }
    };

    const diRegistrations = (req) => {
      const out = [];
      const seen = new Set();
      eachExport(req, (ex) => {
        if (!ex || typeof ex !== 'object' || seen.has(ex)) return;
        const registry = ex._registry && ex._registry._registryMap;
        if (typeof ex.resolve !== 'function' || !(registry instanceof Map)) return;
        seen.add(ex);
        for (const regs of registry.values()) {
          for (const r of regs || []) out.push(r);
        }
      });
      return out;
    };

    window.__zaloEachExport = eachExport;
    window.__zaloDI = diRegistrations;

    let gidSvc = null;
    const findGidSvc = () => {
      if (gidSvc) return gidSvc;
      const req = window.__zaloRequire;
      if (!req) return null;
      eachExport(req, (ex) => {
        if (gidSvc || !ex || typeof ex !== 'object') return;
        try {
          if (
            ex._userIdMap instanceof Map &&
            ex._globalIdMap instanceof Map &&
            typeof ex.getGlobalId === 'function' &&
            typeof ex.getGlobalIdFromServer === 'function'
          ) {
            gidSvc = ex;
          }
        } catch {}
      });
      return gidSvc;
    };

    if (typeof window.__swzGlobalIdFallback !== 'function') {
      window.__swzGlobalIdFallback = async (uids) => {
        const svc = findGidSvc();
        const ids = Array.isArray(uids)
          ? [...new Set(uids.filter((u) => typeof u === 'string' && u && !u.startsWith('g')))]
          : [];
        const out = {};
        if (!svc || !ids.length) return out;
        for (let i = 0; i < ids.length; i += 100) {
          const part = ids.slice(i, i + 100);
          const m = await svc.getGlobalId(part);
          for (const id of part) {
            if (m && typeof m[id] === 'string' && m[id]) out[id] = m[id];
          }
        }
        return out;
      };
    }

    const patchSingleRegistration = (reg) => {
      if (!reg || typeof reg !== 'object') return false;
      let done = false;
      for (const c of [reg.instance, reg.provider && reg.provider.useClass, reg.provider]) {
        const proto =
          typeof c === 'function'
            ? c.prototype
            : c && typeof c === 'object'
              ? Object.getPrototypeOf(c)
              : null;
        if (!proto || typeof proto.waitForDBMigrate !== 'function' || proto.waitForDBMigrate.__olaz) {
          continue;
        }
        const original = proto.waitForDBMigrate;
        proto.waitForDBMigrate = function (session, config, diskConfig) {
          return original.call(
            this,
            session,
            Object.assign({}, config || {}, { enable: false }),
            diskConfig
          );
        };
        proto.waitForDBMigrate.__olaz = true;
        done = true;
      }
      return done;
    };

    let migratePatched = false;
    let resizerPinned = false;
    let resizerMicrotaskQueued = false;

    const queueResizerPinCheck = () => {
      if (resizerPinned || resizerMicrotaskQueued) return;
      resizerMicrotaskQueued = true;
      Promise.resolve().then(() => {
        resizerMicrotaskQueued = false;
        if (!resizerPinned && window.__zaloRequire && pinResizer(window.__zaloRequire)) {
          resizerPinned = true;
        }
      });
    };

    const seenDiRegs = typeof WeakSet === 'function' ? new WeakSet() : new Set();
    const notifyExternalDiHooks = (reg) => {
      if (!reg || typeof reg !== 'object' || seenDiRegs.has(reg)) return;
      seenDiRegs.add(reg);
      for (const fn of olazHost.diRegistrationHooks) {
        try {
          fn(reg);
        } catch {}
      }
    };

    const runExternalWebpackHooks = (req) => {
      if (!req || !olazHost.webpackModuleHooks.length) return;
      for (const fn of olazHost.webpackModuleHooks) {
        try {
          fn(req);
        } catch {}
      }
    };

    const hookDiRegistries = (req) => {
      eachExport(req, (ex) => {
        if (!ex || typeof ex !== 'object') return;
        const regObj = ex._registry;
        if (
          !regObj ||
          typeof regObj !== 'object' ||
          !(regObj._registryMap instanceof Map) ||
          typeof ex.resolve !== 'function'
        ) {
          return;
        }
        for (const regs of regObj._registryMap.values()) {
          for (const r of regs || []) {
            notifyExternalDiHooks(r);
          }
        }
        if (regObj.__olazHooked) return;
        try {
          Object.defineProperty(regObj, '__olazHooked', { value: true });
          if (typeof regObj.set === 'function') {
            const origSet = regObj.set;
            regObj.set = function (token, registration) {
              const r = origSet.apply(this, arguments);
              try {
                if (patchSingleRegistration(registration)) {
                  migratePatched = true;
                }
                if (!resizerPinned) {
                  queueResizerPinCheck();
                }
                notifyExternalDiHooks(registration);
              } catch {}
              return r;
            };
          }
        } catch {}
      });
    };

    const patchWaitForDbMigrate = (req) => {
      hookDiRegistries(req);
      let done = false;
      for (const reg of diRegistrations(req)) {
        if (patchSingleRegistration(reg)) {
          done = true;
        }
      }
      return done;
    };

    const pinResizer = (req) => {
      let pinned = false;
      eachExport(req, (ex) => {
        if (pinned || !ex || typeof ex !== 'object' || ex === window.$capp) return;
        try {
          if (
            Object.prototype.hasOwnProperty.call(ex, 'stickerUrl') &&
            Object.prototype.hasOwnProperty.call(ex, 'sendToMeId')
          ) {
            if (!ex.__olazResizer) {
              let value = ex.image_resizer;
              const patched = (v) =>
                v && typeof v === 'object' && !Array.isArray(v)
                  ? Object.assign({}, v, { enable_libvips_macos: 0 })
                  : v;
              Object.defineProperty(ex, 'image_resizer', {
                configurable: true,
                enumerable: true,
                get: () => patched(value),
                set: (v) => {
                  value = v;
                },
              });
              Object.defineProperty(ex, '__olazResizer', { value: true });
            }
            pinned = true;
          }
        } catch {}
      });
      return pinned;
    };

    let inStep = false;
    const step = () => {
      if (inStep) return false;
      inStep = true;
      try {
        if (!window.__zaloRequire) {
          const jsonp = window.webpackJsonp;
          if (!jsonp || jsonp.push === Array.prototype.push) return false;
          const probeId = '__olaz_web_probe_' + Date.now();
          try {
            jsonp.push([
              [probeId],
              {
                [probeId]: (_m, _e, r) => {
                  window.__zaloRequire = r;
                },
              },
              [[probeId]],
            ]);
          } catch {}
        }
        const req = window.__zaloRequire;
        if (!req) return false;
        hookDiRegistries(req);
        if (!migratePatched && patchWaitForDbMigrate(req)) {
          migratePatched = true;
        }
        if (!resizerPinned && pinResizer(req)) {
          resizerPinned = true;
        }
        runExternalWebpackHooks(req);
        return migratePatched && resizerPinned;
      } finally {
        inStep = false;
      }
    };

    const makeWrappedPush = (origPush) => {
      if (
        typeof origPush !== 'function' ||
        origPush === Array.prototype.push ||
        origPush.__olazWrapped
      ) {
        return origPush;
      }
      const wrapped = function (...args) {
        const res = origPush.apply(this, args);
        try {
          step();
        } catch {}
        if (!(migratePatched && resizerPinned) || olazHost.webpackModuleHooks.length > 0) {
          Promise.resolve()
            .then(() => Promise.resolve())
            .then(() => Promise.resolve())
            .then(() => {
              try {
                step();
              } catch {}
            });
        }
        return res;
      };
      Object.defineProperty(wrapped, '__olazWrapped', { value: true });
      return wrapped;
    };

    const wrapJsonpArray = (arr) => {
      if (!Array.isArray(arr)) return;
      if (arr.push !== Array.prototype.push && !arr.push.__olazWrapped) {
        try {
          arr.push = makeWrappedPush(arr.push);
        } catch {}
      }
      if (!arr.__olazPushWatched) {
        try {
          let currentPush = arr.push;
          Object.defineProperty(arr, '__olazPushWatched', { value: true });
          Object.defineProperty(arr, 'push', {
            configurable: true,
            enumerable: false,
            get() {
              return currentPush;
            },
            set(fn) {
              currentPush = makeWrappedPush(fn);
            },
          });
        } catch {}
      }
    };

    if (window.webpackJsonp) {
      wrapJsonpArray(window.webpackJsonp);
    }
    try {
      const desc = Object.getOwnPropertyDescriptor(window, 'webpackJsonp');
      if (!desc || desc.configurable) {
        let jsonpVal = window.webpackJsonp;
        Object.defineProperty(window, 'webpackJsonp', {
          configurable: true,
          enumerable: true,
          get() {
            return jsonpVal;
          },
          set(v) {
            jsonpVal = v;
            wrapJsonpArray(v);
          },
        });
      }
    } catch {}

    tryApplyWebpackRuntimePatchesNow = () => {
      if (window.webpackJsonp) wrapJsonpArray(window.webpackJsonp);
      return step();
    };
    if (!tryApplyWebpackRuntimePatchesNow()) {
      const timer = setInterval(() => {
        if (tryApplyWebpackRuntimePatchesNow()) clearInterval(timer);
      }, 200);
      if (timer && typeof timer.unref === 'function') timer.unref();
      const stopTimer = setTimeout(() => clearInterval(timer), 60000);
      if (stopTimer && typeof stopTimer.unref === 'function') stopTimer.unref();
    }
  }

  async function runDeferredZaloScripts(onEachScriptLoaded) {
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
        s.onload = () => {
          if (typeof onEachScriptLoaded === 'function') {
            try {
              onEachScriptLoaded();
            } catch {}
          }
          resolve();
        };
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
      if (
        pathname.endsWith('/index.html') ||
        IS_SHARED_WORKER_FRAME ||
        /\/popup-viewer\.html$/i.test(pathname) ||
        window.__ZaBUNDLENAME__ === 'popup-viewer-pc'
      ) {
        installBrowserWebpackRuntimePatches();
      }
      await runDeferredZaloScripts(() => {
        if (tryApplyWebpackRuntimePatchesNow) tryApplyWebpackRuntimePatchesNow();
      });
      if (IS_SHARED_WORKER_FRAME) {
        notifySharedWorkerBootComplete();
      } else {
        emitLocalIpcEvent('visibility-change', ['show']);
        emitLocalIpcEvent('visibility-change', ['focus', '1']);
        if (pathname.endsWith('/index.html') && rawSsGet(MP_PUSH_LOGIN_SS_KEY) === '1') {
          rawSsRemove(MP_PUSH_LOGIN_SS_KEY);
          emitLocalIpcEvent('login-success', []);
        }
      }
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
