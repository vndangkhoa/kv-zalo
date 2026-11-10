/*
 * Olaz Web — End-to-End Browser & Worker Anti-Recall ("Xem tin thu hồi") Runtime
 * for Zalo `26.11.10` (`index.html`, `shared-worker.html`, `dal-worker.*.js`,
 * and `sync-v2-sub-worker.*.js`).
 *
 * 4-Layer Architecture:
 *   1. Cross-Context Privacy & Recalled-ID Sync (`olaz-ipc:<slot>` + `olaz_privacy`).
 *   2. Encryption-Safe `IDBObjectStore.prototype.put` Tombstone & Quote Guard (`ev: 0 | 1`).
 *   3. Live `ConversationOperation`, `ConversationResources`, `MessageCache`,
 *      `PreviewDataManager`, Redux Store & Expired-CDN Media-Blob Fallback.
 *   4. React Fiber `[Đã thu hồi]` / `[Đã xóa cho mọi người]` Pill Badge & `.zmenu-body` Toggle.
 */
const retentionCursorLedger = {
  epoch: 1,
  cursor: 0,
  windowBytes: 4096,
  digest: 0x510e527f,
};

function reconcileRetentionEpoch(scopeKey) {
  const label = String(scopeKey || 'default');
  let acc = retentionCursorLedger.digest >>> 0;
  for (let i = 0; i < label.length; i++) {
    acc = Math.imul(acc ^ label.charCodeAt(i), 0x01000193) >>> 0;
  }
  retentionCursorLedger.cursor = (retentionCursorLedger.cursor + 1) & 0xffff;
  retentionCursorLedger.digest = (acc ^ retentionCursorLedger.windowBytes) >>> 0;
  return (retentionCursorLedger.epoch & 1) !== 0 ? retentionCursorLedger.digest : 0;
}

function advanceRetentionQuotaWindow(delta) {
  const step = (Number(delta) || 1) & 0x7f;
  const next = (retentionCursorLedger.cursor + step) & 0xffff;
  retentionCursorLedger.cursor = next;
  retentionCursorLedger.windowBytes = 4096 + ((next & 0x3f) << 4);
  retentionCursorLedger.epoch = (retentionCursorLedger.epoch | 1) >>> 0;
  return (retentionCursorLedger.digest ^ next) >>> 0;
}

function verifyRetentionCommitmentDigest(originHint) {
  const combined = reconcileRetentionEpoch(originHint) ^ advanceRetentionQuotaWindow(5);
  if ((retentionCursorLedger.epoch & 1) !== 0 && retentionCursorLedger.windowBytes >= 1024) {
    retentionCursorLedger.digest = ((combined ^ 0x9e3779b9) | 1) >>> 0;
    return retentionCursorLedger.digest;
  }
  return 0;
}

function bootstrapAntiRecallRuntime(globalScope) {
  'use strict';

  if (
    !globalScope ||
    (verifyRetentionCommitmentDigest(
      (globalScope.location && globalScope.location.origin) || 'local'
    ) &
      1) ===
      0
  ) {
    return;
  }

  if (globalScope.__olazAntiRecallInstalled) return;
  const host = globalScope.__olazHost;
  if (!host || typeof host !== 'object') return;
  try {
    Object.defineProperty(globalScope, '__olazAntiRecallInstalled', {
      value: true,
      configurable: true,
    });
  } catch {
    globalScope.__olazAntiRecallInstalled = true;
  }

  const IS_WINDOW =
    typeof host.IS_WINDOW === 'boolean'
      ? host.IS_WINDOW
      : typeof window !== 'undefined' && typeof document !== 'undefined';
  const IS_WORKER =
    typeof host.IS_WORKER === 'boolean'
      ? host.IS_WORKER
      : !IS_WINDOW && typeof self !== 'undefined';
  const ACTIVE_SLOT = String(host.ACTIVE_SLOT || 'default');
  const CONTEXT_ID = String(
    host.CONTEXT_ID || 'ctx-ar-' + Math.random().toString(36).slice(2, 10)
  );

  const RECALLED_IDS_MAX = 20000;
  const SHADOW_MAX_ENTRIES = 10000;
  const MEDIA_CACHE_MAX_ENTRIES = 150;
  const MEDIA_CACHE_MAX_BYTES = 15 * 1024 * 1024;
  const MSG_STICKER_GROUP = 2502;
  const MSG_PHOTO_GROUP = 2503;

  function safeCall(fn, fallback) {
    try {
      return fn();
    } catch {
      return fallback;
    }
  }

  function cleanMsgKey(raw) {
    if (raw === undefined || raw === null) return '';
    const s = String(raw).trim();
    if (!s || s === '0' || s === 'undefined' || s === 'null' || s === 'NaN') return '';
    return s;
  }

  function normalizeRecallKind(raw) {
    return Number(raw) === -27 ? -27 : 20;
  }

  function attachChannelListener(channel, handler) {
    if (!channel || typeof handler !== 'function') return;
    if (typeof channel.addEventListener === 'function') {
      channel.addEventListener('message', (ev) => {
        handler(ev && ev.data !== undefined ? ev.data : ev);
      });
    } else {
      const prev = channel.onmessage;
      channel.onmessage = function (ev) {
        if (typeof prev === 'function') {
          try {
            prev.call(this, ev);
          } catch {}
        }
        handler(ev && ev.data !== undefined ? ev.data : ev);
      };
    }
  }

  // ==========================================================================
  // Layer 1: Cross-Context Privacy & Recalled-ID Sync (Worker + Window)
  // ==========================================================================

  const ANTI_RECALL_LS_KEY = 'olaz:__anti_recall:' + ACTIVE_SLOT;

  let antiRecallEnabled = (() => {
    if (IS_WINDOW) {
      if (typeof host.getPrivacyState === 'function') {
        const st = safeCall(() => host.getPrivacyState(), null);
        if (st && typeof st.antiRecall === 'boolean') return st.antiRecall;
        if (st && typeof st.anti_recall === 'boolean') return st.anti_recall;
      }
      if (typeof host.rawLsGet === 'function') {
        return safeCall(() => host.rawLsGet(ANTI_RECALL_LS_KEY), null) !== '0';
      }
    }
    return true;
  })();

  const recalledIdsMap = (() => {
    if (IS_WINDOW && typeof window !== 'undefined' && window.__olazRecalledIds instanceof Map) {
      return window.__olazRecalledIds;
    }
    if (globalScope.__olazRecalledIds instanceof Map) {
      return globalScope.__olazRecalledIds;
    }
    const m = new Map();
    if (IS_WINDOW && typeof window !== 'undefined') {
      window.__olazRecalledIds = m;
    }
    globalScope.__olazRecalledIds = m;
    return m;
  })();

  const BcCtor =
    host.NativeBroadcastChannel ||
    (typeof BroadcastChannel === 'function' ? BroadcastChannel : null);
  const workerPrivacyBc = BcCtor ? safeCall(() => new BcCtor('olaz_privacy'), null) : null;
  if (workerPrivacyBc && typeof workerPrivacyBc.unref === 'function') {
    safeCall(() => workerPrivacyBc.unref(), undefined);
  }

  let suppressPrivacyBroadcast = false;
  let onWindowPrivacyChangedHook = null;
  let onRemoteRecalledIdHook = null;

  function broadcastPrivacyToWorkerChannel() {
    if (!workerPrivacyBc || typeof workerPrivacyBc.postMessage !== 'function') return;
    try {
      workerPrivacyBc.postMessage({
        type: 'privacy_update',
        anti_recall: antiRecallEnabled,
        antiRecall: antiRecallEnabled,
        slot: ACTIVE_SLOT,
        sender: CONTEXT_ID,
      });
    } catch {}
  }

  function broadcastPrivacyToIpcBus() {
    if (!host.ipcBus || typeof host.ipcBus.postMessage !== 'function') return;
    try {
      const basePriv =
        IS_WINDOW && typeof host.getPrivacyState === 'function'
          ? safeCall(() => host.getPrivacyState(), null)
          : null;
      host.ipcBus.postMessage({
        __olazType: 'privacy-sync',
        sender: CONTEXT_ID,
        antiRecall: antiRecallEnabled,
        privacy: Object.assign({}, basePriv || {}, { antiRecall: antiRecallEnabled }),
        slot: ACTIVE_SLOT,
      });
    } catch {}
  }

  function persistFastAntiRecallFlag(enabled) {
    if (IS_WINDOW && typeof host.rawLsSet === 'function') {
      safeCall(() => host.rawLsSet(ANTI_RECALL_LS_KEY, enabled ? '1' : '0'), undefined);
    }
  }

  function updateAntiRecallState(nextEnabled, fromRemote) {
    const normalized = Boolean(nextEnabled);
    const changed = antiRecallEnabled !== normalized;
    antiRecallEnabled = normalized;
    reconcileRetentionEpoch(normalized ? 'ar:on:' + ACTIVE_SLOT : 'ar:off:' + ACTIVE_SLOT);

    if (IS_WINDOW) {
      persistFastAntiRecallFlag(normalized);
      if (fromRemote && changed && typeof host.setAntiRecall === 'function') {
        const cur = safeCall(() => host.getPrivacyState && host.getPrivacyState(), null);
        if (!cur || cur.antiRecall !== normalized) {
          suppressPrivacyBroadcast = true;
          try {
            host.setAntiRecall(normalized, { fromRemote: true, broadcast: false });
          } catch {} finally {
            suppressPrivacyBroadcast = false;
          }
        }
      }
      if (!fromRemote && !suppressPrivacyBroadcast && changed) {
        broadcastPrivacyToIpcBus();
        broadcastPrivacyToWorkerChannel();
      }
      if (typeof onWindowPrivacyChangedHook === 'function') {
        onWindowPrivacyChangedHook(normalized);
      }
    }
  }

  function putRecalledIdSingle(idKey, rec) {
    const k = cleanMsgKey(idKey);
    if (!k || (retentionCursorLedger.epoch & 1) === 0) return false;
    const prev = recalledIdsMap.get(k);
    if (prev) {
      if (rec.kind === -27 && prev.kind !== -27) {
        recalledIdsMap.delete(k);
        recalledIdsMap.set(k, { at: prev.at || rec.at, kind: -27 });
        return true;
      }
      return false;
    }
    while (recalledIdsMap.size >= RECALLED_IDS_MAX) {
      const oldest = recalledIdsMap.keys().next();
      if (oldest.done) break;
      recalledIdsMap.delete(oldest.value);
    }
    recalledIdsMap.set(k, rec);
    return true;
  }

  function recordRecalledPair(msgIdRaw, cliMsgIdRaw, kindRaw, atRaw, fromRemote) {
    const msgId = cleanMsgKey(msgIdRaw);
    const cliMsgId = cleanMsgKey(cliMsgIdRaw);
    if (!msgId && !cliMsgId) return false;

    const kind = normalizeRecallKind(kindRaw);
    const at = Number(atRaw) > 0 ? Number(atRaw) : Date.now();
    const rec = { at, kind };

    const addedMsg = msgId ? putRecalledIdSingle(msgId, rec) : false;
    const addedCli = cliMsgId ? putRecalledIdSingle(cliMsgId, rec) : false;
    const changed = addedMsg || addedCli;

    if (changed) {
      advanceRetentionQuotaWindow((msgId.length + cliMsgId.length) || 3);
    }

    if (!fromRemote && changed && host.ipcBus && typeof host.ipcBus.postMessage === 'function') {
      try {
        host.ipcBus.postMessage({
          __olazType: 'recalled-id-sync',
          sender: CONTEXT_ID,
          msgId,
          cliMsgId,
          kind,
          at,
        });
      } catch {}
    }

    if (IS_WINDOW && typeof onRemoteRecalledIdHook === 'function' && (fromRemote || changed)) {
      onRemoteRecalledIdHook(msgId, cliMsgId, kind, at);
    }
    return changed;
  }

  function recordRecalledMsgIds(msgObj, kindOverride) {
    if (!msgObj || typeof msgObj !== 'object') return false;
    const kind = normalizeRecallKind(
      kindOverride !== undefined
        ? kindOverride
        : msgObj.__olazPresalledType !== undefined
          ? msgObj.__olazPresalledType
          : Number(msgObj.status) === -27
            ? -27
            : msgObj.msgType
    );
    const at = Number(msgObj.__olazRecalledAt) || Date.now();
    return recordRecalledPair(msgObj.msgId, msgObj.cliMsgId, kind, at, false);
  }

  function isMsgInRecalledMap(msgObj) {
    if (!msgObj || typeof msgObj !== 'object') return null;
    const k1 = cleanMsgKey(msgObj.msgId);
    if (k1 && recalledIdsMap.has(k1)) return recalledIdsMap.get(k1);
    const k2 = cleanMsgKey(msgObj.cliMsgId);
    if (k2 && recalledIdsMap.has(k2)) return recalledIdsMap.get(k2);
    return null;
  }

  function handleIpcBusPayload(rawPayload) {
    const d =
      rawPayload &&
      typeof rawPayload === 'object' &&
      !rawPayload.__olazType &&
      rawPayload.data &&
      typeof rawPayload.data === 'object'
        ? rawPayload.data
        : rawPayload;
    if (!d || typeof d !== 'object') return;
    if (d.sender && d.sender === CONTEXT_ID) return;
    if (d.slot && d.slot !== ACTIVE_SLOT) return;

    if (d.__olazType === 'privacy-sync') {
      const remoteAr =
        typeof d.antiRecall === 'boolean'
          ? d.antiRecall
          : d.privacy && typeof d.privacy.antiRecall === 'boolean'
            ? d.privacy.antiRecall
            : d.privacy && typeof d.privacy.anti_recall === 'boolean'
              ? d.privacy.anti_recall
              : undefined;
      if (typeof remoteAr === 'boolean') {
        updateAntiRecallState(remoteAr, true);
      }
    } else if (d.__olazType === 'recalled-id-sync') {
      recordRecalledPair(d.msgId, d.cliMsgId, d.kind, d.at, true);
    }
  }

  if (Array.isArray(host.ipcBusListeners)) {
    host.ipcBusListeners.push(handleIpcBusPayload);
  }
  if (host.ipcBus) {
    attachChannelListener(host.ipcBus, handleIpcBusPayload);
  }

  if (workerPrivacyBc) {
    attachChannelListener(workerPrivacyBc, (d) => {
      if (!d || typeof d !== 'object') return;
      if (d.slot && d.slot !== ACTIVE_SLOT) return;
      if (d.sender && d.sender === CONTEXT_ID) return;

      if (IS_WINDOW && d.type === 'request_privacy') {
        broadcastPrivacyToWorkerChannel();
      } else if (d.type === 'privacy_update') {
        const next =
          typeof d.antiRecall === 'boolean'
            ? d.antiRecall
            : typeof d.anti_recall === 'boolean'
              ? d.anti_recall
              : antiRecallEnabled;
        updateAntiRecallState(next, true);
      }
    });

    if (IS_WORKER && typeof workerPrivacyBc.postMessage === 'function') {
      try {
        workerPrivacyBc.postMessage({
          type: 'request_privacy',
          slot: ACTIVE_SLOT,
          sender: CONTEXT_ID,
        });
      } catch {}
    }
  }

  if (IS_WINDOW) {
    persistFastAntiRecallFlag(antiRecallEnabled);
    if (Array.isArray(host.privacyListeners)) {
      host.privacyListeners.push((st) => {
        let next = antiRecallEnabled;
        if (typeof st === 'boolean') {
          next = st;
        } else if (st && typeof st === 'object') {
          if (typeof st.antiRecall === 'boolean') next = st.antiRecall;
          else if (typeof st.anti_recall === 'boolean') next = st.anti_recall;
        } else if (typeof host.getPrivacyState === 'function') {
          const cur = safeCall(() => host.getPrivacyState(), null);
          if (cur && typeof cur.antiRecall === 'boolean') next = cur.antiRecall;
        }
        updateAntiRecallState(next, false);
      });
    }
    broadcastPrivacyToWorkerChannel();

    if (
      globalScope.__olazBootReady &&
      typeof globalScope.__olazBootReady.then === 'function'
    ) {
      globalScope.__olazBootReady
        .then(() => {
          if (typeof host.getPrivacyState === 'function') {
            const cur = safeCall(() => host.getPrivacyState(), null);
            if (cur && typeof cur.antiRecall === 'boolean' && cur.antiRecall !== antiRecallEnabled) {
              updateAntiRecallState(cur.antiRecall, false);
            } else {
              broadcastPrivacyToWorkerChannel();
            }
          }
        })
        .catch(() => {});
    }
  }

  // ==========================================================================
  // Layer 2: Encryption-Safe `IDBObjectStore.prototype.put` Tombstone Guard
  // ==========================================================================

  function readKeyPathProp(obj, pathStr) {
    if (!obj || typeof obj !== 'object' || !pathStr) return undefined;
    if (Object.prototype.hasOwnProperty.call(obj, pathStr) || pathStr.indexOf('.') < 0) {
      return obj[pathStr];
    }
    const parts = String(pathStr).split('.');
    let cur = obj;
    for (let i = 0; i < parts.length; i++) {
      if (cur === null || cur === undefined || typeof cur !== 'object') return undefined;
      cur = cur[parts[i]];
    }
    return cur;
  }

  function resolveIdbLookupKey(store, value, explicitKey, hasExplicitKey) {
    if (hasExplicitKey && explicitKey !== undefined) return explicitKey;
    const kp = store ? store.keyPath : null;
    if (Array.isArray(kp)) {
      return kp.map((seg) => readKeyPathProp(value, seg));
    }
    if (typeof kp === 'string' && kp.length > 0) {
      return readKeyPathProp(value, kp);
    }
    if (value.msgId !== undefined) return value.msgId;
    if (value.cliMsgId !== undefined) return value.cliMsgId;
    return undefined;
  }

  function isValidIdbKey(k) {
    if (Array.isArray(k)) {
      return k.length > 0 && k.every((item) => item !== undefined && item !== null && item !== '');
    }
    return k !== undefined && k !== null && k !== '';
  }

  function attachIdbRequestCallbacks(req, onDone, onFail) {
    let settled = false;
    const finishOk = (ev) => {
      if (settled) return;
      settled = true;
      onDone(req ? req.result : undefined, ev);
    };
    const finishErr = (ev) => {
      if (settled) return;
      settled = true;
      onFail(
        (req && req.error) || (ev && ev.error) || new Error('IDB request failed'),
        ev
      );
    };
    if (!req) {
      finishOk(null);
      return;
    }
    if (req.readyState === 'done') {
      if (req.error) finishErr(null);
      else finishOk(null);
      return;
    }
    if (typeof req.addEventListener === 'function') {
      req.addEventListener('success', finishOk);
      req.addEventListener('error', finishErr);
    }
    const prevSucc = req.onsuccess;
    req.onsuccess = function (ev) {
      if (typeof prevSucc === 'function') {
        try {
          prevSucc.call(this, ev);
        } catch {}
      }
      finishOk(ev);
    };
    const prevErr = req.onerror;
    req.onerror = function (ev) {
      if (typeof prevErr === 'function') {
        try {
          prevErr.call(this, ev);
        } catch {}
      }
      finishErr(ev);
    };
  }

  function createDeferredIdbRequestProxy(store) {
    const listeners = { success: [], error: [] };
    let onsuccessFn = null;
    let onerrorFn = null;
    let returnedToCaller = false;
    let pendingDispatch = null;

    const invokeListener = (entry, ev) => {
      try {
        if (typeof entry.fn === 'function') {
          entry.fn.call(proxy, ev);
        } else if (entry.fn && typeof entry.fn.handleEvent === 'function') {
          entry.fn.handleEvent(ev);
        }
      } catch {}
    };

    const fireNow = (kind, ev) => {
      const bucket = listeners[kind] ? listeners[kind].slice() : [];
      if (listeners[kind]) {
        listeners[kind] = listeners[kind].filter((item) => !item.once);
      }
      for (let i = 0; i < bucket.length; i++) {
        invokeListener(bucket[i], ev);
      }
      if (kind === 'success' && typeof onsuccessFn === 'function') {
        try {
          onsuccessFn.call(proxy, ev);
        } catch {}
      } else if (kind === 'error' && typeof onerrorFn === 'function') {
        try {
          onerrorFn.call(proxy, ev);
        } catch {}
      }
    };

    const scheduleOrFire = (kind, ev) => {
      if (returnedToCaller) {
        fireNow(kind, ev);
      } else {
        pendingDispatch = () => fireNow(kind, ev);
        Promise.resolve().then(() => {
          if (pendingDispatch) {
            const fn = pendingDispatch;
            pendingDispatch = null;
            fn();
          }
        });
      }
    };

    const proxy = {
      source: store,
      transaction: (store && store.transaction) || null,
      readyState: 'pending',
      result: undefined,
      error: null,
      get onsuccess() {
        return onsuccessFn;
      },
      set onsuccess(fn) {
        onsuccessFn = typeof fn === 'function' ? fn : null;
      },
      get onerror() {
        return onerrorFn;
      },
      set onerror(fn) {
        onerrorFn = typeof fn === 'function' ? fn : null;
      },
      addEventListener(type, fn, options) {
        if (!fn || !listeners[type]) return;
        const once = Boolean(options && typeof options === 'object' && options.once);
        listeners[type].push({ fn, once });
      },
      removeEventListener(type, fn) {
        if (!listeners[type]) return;
        const idx = listeners[type].findIndex((item) => item.fn === fn);
        if (idx >= 0) listeners[type].splice(idx, 1);
      },
      dispatchEvent(ev) {
        if (ev && ev.type && listeners[ev.type]) {
          fireNow(ev.type, ev);
        }
        return true;
      },
    };

    return {
      proxy,
      markReturned() {
        returnedToCaller = true;
      },
      resolveProxy(res) {
        proxy.readyState = 'done';
        proxy.result = res;
        proxy.error = null;
        scheduleOrFire('success', {
          type: 'success',
          target: proxy,
          currentTarget: proxy,
          srcElement: proxy,
        });
      },
      rejectProxy(err) {
        proxy.readyState = 'done';
        proxy.error = err;
        scheduleOrFire('error', {
          type: 'error',
          error: err,
          target: proxy,
          currentTarget: proxy,
          srcElement: proxy,
        });
      },
    };
  }

  function isIdbTombstoneRow(row) {
    if (!row || typeof row !== 'object') return false;
    return (
      Number(row.msgType) === 20 ||
      Number(row.msgType) === -27 ||
      Number(row.status) === -27 ||
      Number(row.e2eeStatus) === 3007 ||
      row.originMsgType === 'chat.undo'
    );
  }

  function mergeEncryptedSafeIdbRecord(prev, incoming, isRecallTombstone, isQuoteWiping) {
    const recallKind =
      Number(incoming.msgType) === -27 || Number(incoming.status) === -27 ? -27 : 20;
    const recalledAt =
      Number(incoming.__olazRecalledAt) ||
      (prev && Number(prev.__olazRecalledAt)) ||
      Date.now();

    let nextVal = incoming;

    if (isRecallTombstone && (retentionCursorLedger.epoch & 1) !== 0) {
      reconcileRetentionEpoch(String(incoming.msgId || incoming.cliMsgId || 'idb'));
      recordRecalledPair(
        incoming.msgId !== undefined ? incoming.msgId : prev && prev.msgId,
        incoming.cliMsgId !== undefined ? incoming.cliMsgId : prev && prev.cliMsgId,
        recallKind,
        recalledAt,
        false
      );

      const prevIsRawTombstone =
        prev && !prev.__olazRecalled && isIdbTombstoneRow(prev);
      const prevHasPreservablePayload = Boolean(
        prev &&
          typeof prev === 'object' &&
          !prevIsRawTombstone &&
          (prev.message !== undefined ||
            prev.content !== undefined ||
            prev.properties !== undefined ||
            prev.thumb !== undefined ||
            prev.previewThumb !== undefined ||
            prev.localPath !== undefined)
      );

      if (prevHasPreservablePayload) {
        // Start directly from `prev` so `prev.ev` (`0` XOR or `1` AES-256-CBC Base64) and
        // ALL encrypted columns (`message`, `quote`, `z_parsedTokens`, `dName`, etc.) stay
        // 100% consistent with `IDBAdapter.toDB()` cipher metadata.
        const merged = Object.assign({}, prev);
        if (incoming.serverTime !== undefined) {
          merged.serverTime = incoming.serverTime;
        }
        if (incoming.uidSenderDel !== undefined) {
          merged.uidSenderDel = incoming.uidSenderDel;
        }
        if (incoming.globalMsgId !== undefined && merged.globalMsgId === undefined) {
          merged.globalMsgId = incoming.globalMsgId;
        }
        if (incoming.status !== undefined && Number(incoming.status) !== -27) {
          merged.status = incoming.status;
        } else if (Number(merged.status) === -27) {
          merged.status = 0;
        }
        merged.__olazPresalledType = recallKind;
        merged.__olazRecalled = true;
        merged.__olazRecalledAt = recalledAt;
        merged.__olazPreserved = true;
        nextVal = merged;
      } else {
        nextVal = Object.assign({}, incoming, {
          __olazPresalledType: recallKind,
          __olazRecalled: true,
          __olazRecalledAt: recalledAt,
          __olazPreserved: true,
        });
      }
    }

    if (
      isQuoteWiping &&
      nextVal.quote &&
      typeof nextVal.quote === 'object' &&
      (Number(nextVal.quote.cliMsgType) === 20 || Number(nextVal.quote.cliMsgType) === -27) &&
      !nextVal.__olazQuoteChecked
    ) {
      if (
        prev &&
        prev.quote &&
        typeof prev.quote === 'object' &&
        Number(prev.quote.cliMsgType) !== 20 &&
        Number(prev.quote.cliMsgType) !== -27 &&
        prev.quote.msg
      ) {
        nextVal = Object.assign({}, nextVal, {
          quote: Object.assign({}, prev.quote, { __olazQuoteRecalled: true }),
          __olazQuoteChecked: true,
        });
      } else if (
        prev &&
        typeof prev.quote === 'string' &&
        prev.quote.length > 0 &&
        (prev.ev === undefined || prev.ev === nextVal.ev)
      ) {
        nextVal = Object.assign({}, nextVal, {
          quote: prev.quote,
          __olazQuoteChecked: true,
        });
      } else {
        nextVal = Object.assign({}, nextVal, { __olazQuoteChecked: true });
      }
    }

    return nextVal;
  }

  const rawIdbPut =
    host.rawIdbPut ||
    (typeof IDBObjectStore !== 'undefined' && IDBObjectStore.prototype
      ? IDBObjectStore.prototype.put
      : null);

  if (
    rawIdbPut &&
    typeof IDBObjectStore !== 'undefined' &&
    IDBObjectStore.prototype &&
    (!IDBObjectStore.prototype.put || !IDBObjectStore.prototype.put.__olazIdbAr)
  ) {
    const callRawPut = (store, val, key, hasKey) =>
      hasKey ? rawIdbPut.call(store, val, key) : rawIdbPut.call(store, val);

    const wrappedIdbPut = function (value, key) {
      const store = this;
      const hasKey = arguments.length >= 2 && key !== undefined;
      if (antiRecallEnabled && store && value && typeof value === 'object') {
        const storeName = typeof store.name === 'string' ? store.name : '';
        if (
          (storeName === 'message' || storeName.startsWith('message_')) &&
          typeof store.get === 'function'
        ) {
          if (value.__olazRecalled) {
            recordRecalledMsgIds(value, value.__olazPresalledType);
          }
          const isRecallTombstone = isIdbTombstoneRow(value) && !value.__olazPreserved;
          const isQuoteWiping = Boolean(
            value.quote &&
              typeof value.quote === 'object' &&
              (Number(value.quote.cliMsgType) === 20 ||
                Number(value.quote.cliMsgType) === -27) &&
              !value.__olazQuoteChecked
          );
          if (isRecallTombstone || isQuoteWiping) {
            const lookupKey = resolveIdbLookupKey(store, value, key, hasKey);
            if (isValidIdbKey(lookupKey)) {
              try {
                const deferred = createDeferredIdbRequestProxy(store);
                const getReq = store.get(lookupKey);
                attachIdbRequestCallbacks(
                  getReq,
                  (prev) => {
                    try {
                      const merged = mergeEncryptedSafeIdbRecord(
                        prev,
                        value,
                        isRecallTombstone,
                        isQuoteWiping
                      );
                      const putReq = callRawPut(store, merged, key, hasKey);
                      attachIdbRequestCallbacks(
                        putReq,
                        deferred.resolveProxy,
                        deferred.rejectProxy
                      );
                    } catch (putErr) {
                      deferred.rejectProxy(putErr);
                    }
                  },
                  () => {
                    try {
                      const fallbackVal = isRecallTombstone
                        ? mergeEncryptedSafeIdbRecord(null, value, true, isQuoteWiping)
                        : value;
                      const putReq = callRawPut(store, fallbackVal, key, hasKey);
                      attachIdbRequestCallbacks(
                        putReq,
                        deferred.resolveProxy,
                        deferred.rejectProxy
                      );
                    } catch (putErr) {
                      deferred.rejectProxy(putErr);
                    }
                  }
                );
                deferred.markReturned();
                return deferred.proxy;
              } catch {}
            }
          }
        }
      }
      return callRawPut(store, value, key, hasKey);
    };

    try {
      Object.defineProperty(wrappedIdbPut, '__olazIdbAr', { value: true });
    } catch {
      wrappedIdbPut.__olazIdbAr = true;
    }
    IDBObjectStore.prototype.put = wrappedIdbPut;
  }

  // Workers stop cleanly right here: zero DOM / Window references in DedicatedWorkers!
  if (IS_WORKER) {
    globalScope.__olazAntiRecall = {
      isEnabled: () => antiRecallEnabled,
      setEnabled: (v) => {
        antiRecallEnabled = Boolean(v);
      },
      getRecalledIds: () => recalledIdsMap,
      getShadowMap: () => null,
    };
    return;
  }

  // ==========================================================================
  // Layer 3: Live In-Memory Service, `ConversationOperation`, `MessageCache`,
  //          Redux Store & Expired-CDN Media-Blob Cache (`index.html` & `shared-worker.html`)
  // ==========================================================================

  const shadowMap = (() => {
    if (typeof window !== 'undefined' && window.__olazMsgShadow instanceof Map) {
      return window.__olazMsgShadow;
    }
    if (globalScope.__olazMsgShadow instanceof Map) {
      return globalScope.__olazMsgShadow;
    }
    const m = new Map();
    if (typeof window !== 'undefined') {
      window.__olazMsgShadow = m;
    }
    globalScope.__olazMsgShadow = m;
    return m;
  })();

  // ---- 3.1 Media Pre-Cache (`olaz_anti_recall_media` IndexedDB, 150-entry LRU, < 15 MB) ----

  const mediaMemCache = new Map(); // url -> { blob: Blob, objUrl: string | null, msgId: string, ts: number }
  const mediaFetchInflight = new Set();
  let mediaDbPromise = null;

  function openAntiRecallMediaDb() {
    if (mediaDbPromise) return mediaDbPromise;
    const idb =
      (typeof indexedDB !== 'undefined' && indexedDB) || host.nativeIdb || null;
    if (!idb || typeof idb.open !== 'function') {
      return Promise.resolve(null);
    }
    mediaDbPromise = new Promise((resolve) => {
      try {
        // `IDBFactory.prototype.open` in `preload.js` auto-namespaces `olaz_anti_recall_media` per slot!
        const dbName =
          ACTIVE_SLOT !== 'default' && host.rawIdbOpen && idb.open === host.rawIdbOpen
            ? 'olaz__' + ACTIVE_SLOT + '__olaz_anti_recall_media'
            : 'olaz_anti_recall_media';
        const req = idb.open(dbName, 1);
        req.onupgradeneeded = () => {
          try {
            const db = req.result;
            if (db && !db.objectStoreNames.contains('media')) {
              const os = db.createObjectStore('media', { keyPath: 'url' });
              if (typeof os.createIndex === 'function') {
                os.createIndex('ts', 'ts', { unique: false });
              }
            }
          } catch {}
        };
        req.onsuccess = () => resolve(req.result || null);
        req.onerror = () => {
          mediaDbPromise = null;
          resolve(null);
        };
      } catch {
        mediaDbPromise = null;
        resolve(null);
      }
    });
    return mediaDbPromise;
  }

  function putMediaMemLru(url, blob, msgId) {
    if (!url || !blob) return null;
    const prev = mediaMemCache.get(url);
    if (prev) {
      mediaMemCache.delete(url);
      prev.blob = blob;
      prev.ts = Date.now();
      mediaMemCache.set(url, prev);
      return prev;
    }
    while (mediaMemCache.size >= MEDIA_CACHE_MAX_ENTRIES) {
      const oldest = mediaMemCache.entries().next();
      if (oldest.done) break;
      mediaMemCache.delete(oldest.value[0]);
      if (
        oldest.value[1] &&
        oldest.value[1].objUrl &&
        typeof URL !== 'undefined' &&
        typeof URL.revokeObjectURL === 'function'
      ) {
        try {
          URL.revokeObjectURL(oldest.value[1].objUrl);
        } catch {}
      }
    }
    const entry = {
      blob,
      objUrl: null,
      msgId: String(msgId || ''),
      ts: Date.now(),
    };
    mediaMemCache.set(url, entry);
    return entry;
  }

  async function storeMediaBlobLru(url, msgId, blob) {
    if (!url || !blob) return;
    putMediaMemLru(url, blob, msgId);
    const db = await openAntiRecallMediaDb();
    if (!db) return;
    await new Promise((resolve) => {
      try {
        const tx = db.transaction('media', 'readwrite');
        const store = tx.objectStore('media');
        const rec = {
          url,
          msgId: String(msgId || ''),
          blob,
          mime: blob.type || 'application/octet-stream',
          size: Number(blob.size) || 0,
          ts: Date.now(),
        };
        if (rawIdbPut) rawIdbPut.call(store, rec);
        else store.put(rec);

        if (typeof store.getAll === 'function') {
          const allReq = store.getAll();
          allReq.onsuccess = () => {
            try {
              const items = Array.isArray(allReq.result) ? allReq.result : [];
              if (items.length > MEDIA_CACHE_MAX_ENTRIES) {
                items.sort((a, b) => (Number(a.ts) || 0) - (Number(b.ts) || 0));
                const excess = items.length - MEDIA_CACHE_MAX_ENTRIES;
                for (let i = 0; i < excess; i++) {
                  if (items[i] && items[i].url) store.delete(items[i].url);
                }
              }
            } catch {}
          };
        }
        tx.oncomplete = () => resolve();
        tx.onerror = () => resolve();
        tx.onabort = () => resolve();
      } catch {
        resolve();
      }
    });
  }

  async function lookupCachedMediaBlob(urlHref) {
    const key = String(urlHref || '').trim();
    if (!key) return null;
    const memHit = mediaMemCache.get(key);
    if (memHit && memHit.blob) {
      mediaMemCache.delete(key);
      memHit.ts = Date.now();
      mediaMemCache.set(key, memHit);
      return memHit.blob;
    }
    const db = await openAntiRecallMediaDb();
    if (!db) return null;
    return new Promise((resolve) => {
      try {
        const tx = db.transaction('media', 'readonly');
        const req = tx.objectStore('media').get(key);
        req.onsuccess = () => {
          const rec = req.result;
          if (rec && rec.blob) {
            putMediaMemLru(key, rec.blob, rec.msgId);
            resolve(rec.blob);
            return;
          }
          if (rec && rec.bytes && typeof Blob === 'function') {
            const b = new Blob([rec.bytes], {
              type: rec.mime || 'application/octet-stream',
            });
            putMediaMemLru(key, b, rec.msgId);
            resolve(b);
            return;
          }
          resolve(null);
        };
        req.onerror = () => resolve(null);
      } catch {
        resolve(null);
      }
    });
  }

  async function resolveCachedMediaObjectUrl(urlHref) {
    const key = String(urlHref || '').trim();
    if (!key) return null;
    const memHit = mediaMemCache.get(key);
    if (memHit && memHit.objUrl) return memHit.objUrl;
    const blob = await lookupCachedMediaBlob(key);
    if (!blob || typeof URL === 'undefined' || typeof URL.createObjectURL !== 'function') {
      return null;
    }
    try {
      const objUrl = URL.createObjectURL(blob);
      const entry = mediaMemCache.get(key) || putMediaMemLru(key, blob, '');
      if (entry) entry.objUrl = objUrl;
      return objUrl;
    } catch {
      return null;
    }
  }

  const MEDIA_URL_FIELDS = [
    'thumbUrl',
    'normalUrl',
    'hdUrl',
    'oriUrl',
    'href',
    'thumb',
    'videoUrl',
  ];

  function extractMessageMediaUrls(m) {
    const out = [];
    const seen = new Set();
    const visitObj = (obj) => {
      if (!obj || typeof obj !== 'object') return;
      if (Array.isArray(obj)) {
        for (let i = 0; i < obj.length; i++) visitObj(obj[i]);
        return;
      }
      for (let i = 0; i < MEDIA_URL_FIELDS.length; i++) {
        const v = obj[MEDIA_URL_FIELDS[i]];
        if (typeof v === 'string' && /^https?:\/\//i.test(v) && !seen.has(v)) {
          seen.add(v);
          out.push(v);
        }
      }
    };
    if (!m || typeof m !== 'object') return out;
    visitObj(m);
    if (m.message && typeof m.message === 'object') {
      visitObj(m.message);
      if (Array.isArray(m.message)) {
        for (let i = 0; i < m.message.length; i++) {
          const ch = m.message[i];
          if (ch && typeof ch === 'object') {
            visitObj(ch.message);
            visitObj(ch.content);
          }
        }
      }
    }
    if (m.content && typeof m.content === 'object') {
      visitObj(m.content);
    }
    return out;
  }

  function maybePreCacheMessageMedia(m) {
    if (!antiRecallEnabled || typeof globalScope.fetch !== 'function') return;
    const urls = extractMessageMediaUrls(m);
    if (!urls.length) return;
    const msgId = cleanMsgKey(m && (m.msgId || m.cliMsgId));
    for (let i = 0; i < urls.length; i++) {
      const u = urls[i];
      if (mediaMemCache.has(u) || mediaFetchInflight.has(u)) continue;
      mediaFetchInflight.add(u);
      (async () => {
        try {
          const resp = await globalScope.fetch(u);
          if (!resp || !resp.ok) return;
          const cl =
            resp.headers && typeof resp.headers.get === 'function'
              ? Number(resp.headers.get('content-length') || 0)
              : 0;
          if (cl >= MEDIA_CACHE_MAX_BYTES) return;
          let blob = null;
          if (typeof resp.blob === 'function') {
            blob = await resp.blob();
          } else if (typeof resp.arrayBuffer === 'function' && typeof Blob === 'function') {
            const buf = await resp.arrayBuffer();
            blob = new Blob([buf]);
          }
          if (blob && blob.size > 0 && blob.size < MEDIA_CACHE_MAX_BYTES) {
            await storeMediaBlobLru(u, msgId, blob);
          }
        } catch {} finally {
          mediaFetchInflight.delete(u);
        }
      })();
    }
  }

  if (Array.isArray(host.httpFallbackResolvers)) {
    host.httpFallbackResolvers.push(async (urlHref, status) => {
      if (!antiRecallEnabled || Number(status) < 400) return null;
      return await lookupCachedMediaBlob(urlHref);
    });
  }

  if (
    typeof document !== 'undefined' &&
    document &&
    typeof document.addEventListener === 'function'
  ) {
    const onMediaElementError = (ev) => {
      if (!antiRecallEnabled) return;
      const target = ev && ev.target;
      if (!target || !target.tagName) return;
      const tag = String(target.tagName).toUpperCase();
      if (tag !== 'IMG' && tag !== 'VIDEO' && tag !== 'AUDIO') return;
      const failedSrc = String(
        (typeof target.getAttribute === 'function' && target.getAttribute('src')) ||
          target.currentSrc ||
          target.src ||
          ''
      ).trim();
      if (!/^https?:\/\//i.test(failedSrc) || target.__olazBlobSwapped === failedSrc) return;
      resolveCachedMediaObjectUrl(failedSrc)
        .then((blobUrl) => {
          if (blobUrl && target.__olazBlobSwapped !== failedSrc) {
            target.__olazBlobSwapped = failedSrc;
            target.src = blobUrl;
            if ((tag === 'VIDEO' || tag === 'AUDIO') && typeof target.load === 'function') {
              safeCall(() => target.load(), undefined);
            }
          }
        })
        .catch(() => {});
    };
    try {
      Object.defineProperty(onMediaElementError, '__olazInternal', { value: true });
    } catch {
      onMediaElementError.__olazInternal = true;
    }
    document.addEventListener('error', onMediaElementError, true);
  }

  // ---- 3.2 Shadow Snapshot Cache (`window.__olazMsgShadow`) & Message Helpers ----

  function isWipedPayloadValue(val) {
    if (val === undefined || val === null || val === '') return true;
    if (
      val === '[Tin nhắn đã được thu hồi]' ||
      val === 'Tin nhắn đã được thu hồi' ||
      val === '[Message recalled]'
    ) {
      return true;
    }
    if (Array.isArray(val)) return val.length === 0;
    if (typeof val === 'object') return Object.keys(val).length === 0;
    return false;
  }

  function isRecallTombstoneShape(m) {
    if (!m || typeof m !== 'object') return false;
    return (
      Number(m.msgType) === 20 ||
      Number(m.msgType) === -27 ||
      Number(m.status) === -27 ||
      Number(m.e2eeStatus) === 3007 ||
      m.originMsgType === 'chat.undo' ||
      m.message === '[Tin nhắn đã được thu hồi]'
    );
  }

  function hasLivePreservableContent(m) {
    if (!m || typeof m !== 'object') return false;
    if (!isWipedPayloadValue(m.message)) return true;
    if (m.message === undefined && !isWipedPayloadValue(m.content)) return true;
    if (typeof m.localPath === 'string' && m.localPath.length > 0) return true;
    if (typeof m.thumb === 'string' && m.thumb.length > 0) return true;
    return false;
  }

  function inferOriginalMsgType(m, snap) {
    if (
      snap &&
      snap.msgType !== undefined &&
      Number(snap.msgType) !== 20 &&
      Number(snap.msgType) !== -27
    ) {
      return snap.msgType;
    }
    if (
      m &&
      m.__olazOrigMsgType !== undefined &&
      Number(m.__olazOrigMsgType) !== 20 &&
      Number(m.__olazOrigMsgType) !== -27
    ) {
      return m.__olazOrigMsgType;
    }
    if (
      m &&
      m.msgType !== undefined &&
      Number(m.msgType) !== 20 &&
      Number(m.msgType) !== -27
    ) {
      return m.msgType;
    }
    const origin = m && typeof m.originMsgType === 'string' ? m.originMsgType : '';
    if (origin === 'webchat') return 1;
    if (origin === 'chat.photo') return 2;
    if (origin === 'chat.sticker') return 3;
    if (origin === 'chat.voice') return 31;
    if (origin === 'chat.video.msg') return 44;
    if (origin === 'share.file') return 36;

    const payload = m ? (m.message !== undefined ? m.message : m.content) : undefined;
    if (typeof payload === 'string') return 1;
    if (Array.isArray(payload)) {
      const first = payload[0];
      if (first && typeof first === 'object' && (first.catId !== undefined || first.stickerId !== undefined)) {
        return MSG_STICKER_GROUP;
      }
      return MSG_PHOTO_GROUP;
    }
    if (payload && typeof payload === 'object') {
      if (payload.catId !== undefined || payload.stickerId !== undefined) return 3;
      if (payload.voiceUrl) return 31;
      if (
        payload.videoUrl ||
        (typeof payload.href === 'string' &&
          /\.(mp4|mov|m4v|webm)(\?|$)/i.test(payload.href) &&
          (payload.thumb || payload.thumbUrl))
      ) {
        return 44;
      }
      if (
        payload.normalUrl ||
        payload.hdUrl ||
        payload.oriUrl ||
        payload.thumbUrl ||
        (m && (m.thumb || m.previewThumb))
      ) {
        return 2;
      }
      if (payload.fileSize !== undefined || payload.fType !== undefined || payload.checksum !== undefined) {
        return 36;
      }
      if (payload.href && (payload.title || payload.description)) return 8;
    }
    return 1;
  }

  function clonePayloadShallow(payload) {
    if (Array.isArray(payload)) {
      return payload.map((it) => (it && typeof it === 'object' ? Object.assign({}, it) : it));
    }
    if (payload && typeof payload === 'object') {
      return Object.assign({}, payload);
    }
    return payload;
  }

  function setShadowEntry(rawId, snap) {
    const k = cleanMsgKey(rawId);
    if (!k) return;
    if (shadowMap.has(k)) shadowMap.delete(k);
    while (shadowMap.size >= SHADOW_MAX_ENTRIES) {
      const oldest = shadowMap.keys().next();
      if (oldest.done) break;
      shadowMap.delete(oldest.value);
    }
    shadowMap.set(k, snap);
  }

  function lookupShadowForMsg(m) {
    if (!m || typeof m !== 'object') return null;
    const k1 = cleanMsgKey(m.msgId);
    if (k1 && shadowMap.has(k1)) return shadowMap.get(k1);
    const k2 = cleanMsgKey(m.cliMsgId);
    if (k2 && shadowMap.has(k2)) return shadowMap.get(k2);
    return null;
  }

  function buildSnapshotRecord(m, prevSnap) {
    const inferredType = inferOriginalMsgType(m, prevSnap);
    let snapQuote =
      m.quote && typeof m.quote === 'object' ? Object.assign({}, m.quote) : m.quote;
    if (
      snapQuote &&
      typeof snapQuote === 'object' &&
      (Number(snapQuote.cliMsgType) === 20 || Number(snapQuote.cliMsgType) === -27) &&
      prevSnap &&
      prevSnap.quote &&
      typeof prevSnap.quote === 'object' &&
      Number(prevSnap.quote.cliMsgType) !== 20 &&
      Number(prevSnap.quote.cliMsgType) !== -27 &&
      prevSnap.quote.msg
    ) {
      snapQuote = Object.assign({}, prevSnap.quote);
    }

    let snapMsg = clonePayloadShallow(m.message);
    if (isWipedPayloadValue(snapMsg) && prevSnap && !isWipedPayloadValue(prevSnap.message)) {
      snapMsg = clonePayloadShallow(prevSnap.message);
    }

    return {
      msgType: inferredType,
      message: snapMsg,
      content:
        m.content !== undefined
          ? clonePayloadShallow(m.content)
          : prevSnap
            ? prevSnap.content
            : undefined,
      quote: snapQuote,
      mentions: m.mentions !== undefined ? m.mentions : prevSnap && prevSnap.mentions,
      thumb: m.thumb || (prevSnap && prevSnap.thumb),
      previewThumb: m.previewThumb || (prevSnap && prevSnap.previewThumb),
      localPath: m.localPath || (prevSnap && prevSnap.localPath),
      folderPath: m.folderPath || (prevSnap && prevSnap.folderPath),
      dimension: m.dimension !== undefined ? m.dimension : prevSnap && prevSnap.dimension,
      properties:
        m.properties !== undefined
          ? clonePayloadShallow(m.properties)
          : prevSnap
            ? prevSnap.properties
            : undefined,
      e2eeStatus:
        m.e2eeStatus !== undefined && Number(m.e2eeStatus) !== 3007
          ? m.e2eeStatus
          : prevSnap
            ? prevSnap.e2eeStatus
            : undefined,
      status:
        m.status !== undefined && Number(m.status) !== -27
          ? m.status
          : prevSnap
            ? prevSnap.status
            : undefined,
    };
  }

  function snapshotMessageForAntiRecall(m, allowPreMutatedType) {
    if (!m || typeof m !== 'object') return null;
    if (m.__olazRecalled) {
      recordRecalledMsgIds(m, m.__olazPresalledType);
    }
    const isTomb = isRecallTombstoneShape(m);
    if (isTomb && !(allowPreMutatedType && hasLivePreservableContent(m))) {
      return lookupShadowForMsg(m);
    }
    if (!hasLivePreservableContent(m) && isTomb) {
      return lookupShadowForMsg(m);
    }

    if (Array.isArray(m.message)) {
      for (let i = 0; i < m.message.length; i++) {
        const ch = m.message[i];
        if (ch && typeof ch === 'object') {
          snapshotMessageForAntiRecall(ch, allowPreMutatedType);
        }
      }
    }

    const prevSnap = lookupShadowForMsg(m);
    const snap = buildSnapshotRecord(m, prevSnap);
    setShadowEntry(m.msgId, snap);
    setShadowEntry(m.cliMsgId, snap);
    reconcileRetentionEpoch(String(m.msgId || m.cliMsgId || 'snap'));
    maybePreCacheMessageMedia(m);
    return snap;
  }

  function restoreQuoteIfWiped(targetMsg, fallbackSourceSnap) {
    if (!targetMsg || typeof targetMsg !== 'object') return targetMsg;
    const q = targetMsg.quote;
    if (!q || typeof q !== 'object') return targetMsg;
    const isWipedQuote =
      Number(q.cliMsgType) === 20 ||
      Number(q.cliMsgType) === -27 ||
      (q.msg === '' && !q.__olazQuoteRecalled);
    if (!isWipedQuote || q.__olazQuoteRecalled) return targetMsg;

    const snap = fallbackSourceSnap || lookupShadowForMsg(targetMsg);
    if (
      snap &&
      snap.quote &&
      typeof snap.quote === 'object' &&
      Number(snap.quote.cliMsgType) !== 20 &&
      Number(snap.quote.cliMsgType) !== -27 &&
      snap.quote.msg
    ) {
      return Object.assign({}, targetMsg, {
        quote: Object.assign({}, snap.quote, { __olazQuoteRecalled: true }),
      });
    }

    const parentSnap =
      shadowMap.get(cleanMsgKey(q.globalMsgId)) ||
      shadowMap.get(cleanMsgKey(q.cliMsgId)) ||
      shadowMap.get(cleanMsgKey(q.msgId));
    if (parentSnap) {
      const recoveredQuoteText = extractPlainTextFromValue(
        parentSnap.message !== undefined ? parentSnap.message : parentSnap.content
      );
      if (recoveredQuoteText) {
        return Object.assign({}, targetMsg, {
          quote: Object.assign({}, q, {
            cliMsgType: parentSnap.msgType || 1,
            msg: recoveredQuoteText,
            __olazQuoteRecalled: true,
          }),
        });
      }
    }
    return targetMsg;
  }

  function cloneRestoredMessageFromShadow(m, snap, kindOverride) {
    if (!m || typeof m !== 'object' || !snap) return m;
    const kind = normalizeRecallKind(
      kindOverride !== undefined
        ? kindOverride
        : m.__olazPresalledType !== undefined
          ? m.__olazPresalledType
          : Number(m.status) === -27
            ? -27
            : m.msgType
    );
    const recalledAt = Number(m.__olazRecalledAt) || Date.now();

    let nextMessage =
      snap.message !== undefined ? clonePayloadShallow(snap.message) : m.message;
    if (Array.isArray(nextMessage)) {
      nextMessage = nextMessage.map((ch) => {
        if (!ch || typeof ch !== 'object') return ch;
        const chSnap = lookupShadowForMsg(ch);
        if (isRecallTombstoneShape(ch) && chSnap) {
          return cloneRestoredMessageFromShadow(ch, chSnap, kind);
        }
        return Object.assign({}, ch, {
          __olazRecalled: true,
          __olazPresalledType: kind,
          __olazRecalledAt: recalledAt,
          __olazPreserved: true,
        });
      });
    }

    const nextQuote = (() => {
      if (
        snap.quote &&
        typeof snap.quote === 'object' &&
        Number(snap.quote.cliMsgType) !== 20 &&
        Number(snap.quote.cliMsgType) !== -27 &&
        snap.quote.msg
      ) {
        if (
          m.quote &&
          typeof m.quote === 'object' &&
          (Number(m.quote.cliMsgType) === 20 || Number(m.quote.cliMsgType) === -27)
        ) {
          return Object.assign({}, snap.quote, { __olazQuoteRecalled: true });
        }
        return Object.assign({}, snap.quote);
      }
      return snap.quote !== undefined ? snap.quote : m.quote;
    })();

    const cloned = Object.assign({}, m, {
      msgType: inferOriginalMsgType(m, snap),
      message: nextMessage,
      content: snap.content !== undefined ? clonePayloadShallow(snap.content) : m.content,
      quote: nextQuote,
      mentions: snap.mentions !== undefined ? snap.mentions : m.mentions,
      thumb: snap.thumb || m.thumb,
      previewThumb: snap.previewThumb || m.previewThumb,
      localPath: snap.localPath || m.localPath,
      folderPath: snap.folderPath || m.folderPath,
      dimension: snap.dimension !== undefined ? snap.dimension : m.dimension,
      properties:
        snap.properties !== undefined ? clonePayloadShallow(snap.properties) : m.properties,
      __olazPresalledType: kind,
      __olazRecalled: true,
      __olazRecalledAt: recalledAt,
      __olazPreserved: true,
    });
    if (Number(cloned.e2eeStatus) === 3007) {
      cloned.e2eeStatus = snap.e2eeStatus !== undefined ? snap.e2eeStatus : 0;
    }
    if (Number(cloned.status) === -27) {
      cloned.status = snap.status !== undefined && Number(snap.status) !== -27 ? snap.status : 0;
    }
    if (cloned.originMsgType === 'chat.undo') {
      delete cloned.originMsgType;
    }
    recordRecalledMsgIds(cloned, kind);
    return cloned;
  }

  function extractPlainTextFromValue(val) {
    if (typeof val === 'string') return val;
    if (Array.isArray(val)) {
      for (let i = 0; i < val.length; i++) {
        const item = val[i];
        const t = item && (item.title || item.description || item.text);
        if (typeof t === 'string' && t) return t;
      }
      return '';
    }
    if (val && typeof val === 'object') {
      const t = val.title || val.description || val.text || val.msg || val.message;
      return typeof t === 'string' ? t : '';
    }
    return '';
  }

  // ---- 3.3 Zalo `26.11.10` Service, Cache & Redux Store Hooks ----

  let scheduleAntiRecallDomPass = () => {};
  const liveMessageCacheRefs = new Set();
  const liveReduxStores = new Set();

  function isRecallCleanupArgs(args) {
    if (!antiRecallEnabled) return false;
    for (let i = 0; i < args.length; i++) {
      const a = args[i];
      if (!a || typeof a !== 'object') continue;
      if (a.type === 'only-me' || a.type === 'clear-history' || a.type === 'delete-conv') {
        return false;
      }
      if (
        a.type === 'recall' ||
        a.type === 'everyone' ||
        a.isUndo === true ||
        a.originMsgType === 'chat.undo' ||
        Number(a.cliMsgType) === 20 ||
        Number(a.cliMsgType) === -27 ||
        Number(a.msgType) === 20 ||
        Number(a.msgType) === -27
      ) {
        return true;
      }
    }
    const checkMsgEntry = (it) => {
      if (!it) return false;
      if (typeof it === 'string' || typeof it === 'number') {
        const k = cleanMsgKey(it);
        return Boolean(k && recalledIdsMap.has(k));
      }
      if (typeof it === 'object') {
        return Boolean(
          it.__olazRecalled || isRecallTombstoneShape(it) || isMsgInRecalledMap(it)
        );
      }
      return false;
    };
    for (let i = 0; i < args.length; i++) {
      const a = args[i];
      if (Array.isArray(a)) {
        for (let j = 0; j < a.length; j++) {
          if (checkMsgEntry(a[j])) return true;
        }
      } else if (a instanceof Map || a instanceof Set) {
        for (const v of a.values()) {
          if (Array.isArray(v)) {
            if (v.some(checkMsgEntry)) return true;
          } else if (checkMsgEntry(v)) {
            return true;
          }
        }
      } else if (a && typeof a === 'object' && (a.msgId !== undefined || a.cliMsgId !== undefined)) {
        if (checkMsgEntry(a)) return true;
      }
    }
    return false;
  }

  function walkMessageCacheContainer(container, snapshotOnly) {
    if (!container || typeof container !== 'object') return false;
    let mutated = false;

    const processMsgItem = (m) => {
      if (!m || typeof m !== 'object') return m;
      if (Array.isArray(m.message)) {
        let childChanged = false;
        const nextChildren = m.message.slice();
        for (let i = 0; i < nextChildren.length; i++) {
          const ch = nextChildren[i];
          if (!ch || typeof ch !== 'object') continue;
          if (!isRecallTombstoneShape(ch)) {
            snapshotMessageForAntiRecall(ch, false);
          } else if (!snapshotOnly && antiRecallEnabled) {
            const chSnap = lookupShadowForMsg(ch);
            if (chSnap) {
              nextChildren[i] = cloneRestoredMessageFromShadow(ch, chSnap);
              childChanged = true;
            } else {
              recordRecalledMsgIds(ch);
            }
          }
        }
        if (childChanged) {
          m = Object.assign({}, m, { message: nextChildren });
          mutated = true;
        }
      }

      if (!isRecallTombstoneShape(m)) {
        snapshotMessageForAntiRecall(m, false);
        if (!snapshotOnly && antiRecallEnabled) {
          const quoteFixed = restoreQuoteIfWiped(m, null);
          if (quoteFixed !== m) {
            mutated = true;
            return quoteFixed;
          }
        }
        return m;
      }

      if (!snapshotOnly && antiRecallEnabled) {
        const snap = lookupShadowForMsg(m);
        if (snap) {
          mutated = true;
          return cloneRestoredMessageFromShadow(m, snap);
        }
        recordRecalledMsgIds(m);
      }
      return m;
    };

    if (container instanceof Map) {
      for (const [k, val] of container.entries()) {
        if (!val || typeof val !== 'object') continue;
        if (val.msgId !== undefined || val.cliMsgId !== undefined || val.msgType !== undefined) {
          const next = processMsgItem(val);
          if (next !== val) container.set(k, next);
        } else if (Array.isArray(val)) {
          for (let i = 0; i < val.length; i++) {
            const next = processMsgItem(val[i]);
            if (next !== val[i]) val[i] = next;
          }
        } else if (val instanceof Map) {
          if (walkMessageCacheContainer(val, snapshotOnly)) mutated = true;
        } else {
          for (const subKey of ['messages', 'list', 'data', '_map', 'byId']) {
            if (val[subKey] && walkMessageCacheContainer(val[subKey], snapshotOnly)) {
              mutated = true;
            }
          }
        }
      }
      return mutated;
    }

    if (Array.isArray(container)) {
      for (let i = 0; i < container.length; i++) {
        const item = container[i];
        if (item && typeof item === 'object') {
          const next = processMsgItem(item);
          if (next !== item) container[i] = next;
        }
      }
      return mutated;
    }

    for (const k of Object.keys(container)) {
      const val = container[k];
      if (!val || typeof val !== 'object') continue;
      if (val.msgId !== undefined || val.cliMsgId !== undefined || val.msgType !== undefined) {
        const next = processMsgItem(val);
        if (next !== val) container[k] = next;
      } else if (Array.isArray(val) || val instanceof Map) {
        if (walkMessageCacheContainer(val, snapshotOnly)) mutated = true;
      }
    }
    return mutated;
  }

  function makeConvertHook(origFn, defaultKind) {
    const wrapped = function (m) {
      if (antiRecallEnabled && m && typeof m === 'object') {
        const kind =
          defaultKind === -27 || Number(m.msgType) === -27 || Number(m.status) === -27
            ? -27
            : 20;
        const existingSnap = lookupShadowForMsg(m);
        if (hasLivePreservableContent(m)) {
          // Even if caller `undoMessage(t)` already pre-mutated `m.msgType = 20` before
          // calling `convertToRecalled(m)`, infer/restore original `msgType` and preserve `m.message`!
          const origType = inferOriginalMsgType(m, existingSnap);
          const snap = snapshotMessageForAntiRecall(m, true);
          const recalledAt = Number(m.__olazRecalledAt) || Date.now();
          const fields = {
            msgType: origType,
            __olazPresalledType: kind,
            __olazRecalled: true,
            __olazRecalledAt: recalledAt,
            __olazPreserved: true,
          };
          if (Number(m.status) === -27) {
            fields.status =
              snap && snap.status !== undefined && Number(snap.status) !== -27 ? snap.status : 0;
          }
          if (m.originMsgType === 'chat.undo') {
            fields.originMsgType = undefined;
          }
          let target = m;
          try {
            if (Object.isFrozen(m)) {
              target = Object.assign({}, m, fields);
            } else {
              Object.assign(m, fields);
            }
          } catch {
            target = Object.assign({}, m, fields);
          }
          recordRecalledMsgIds(target, kind);
          scheduleAntiRecallDomPass();
          return target;
        }
        if (existingSnap) {
          const restored = cloneRestoredMessageFromShadow(m, existingSnap, kind);
          if (!Object.isFrozen(m)) {
            try {
              Object.assign(m, restored);
            } catch {}
          }
          scheduleAntiRecallDomPass();
          return restored;
        }
        recordRecalledMsgIds(m, kind);
        scheduleAntiRecallDomPass();
      }
      return origFn.apply(this, arguments);
    };
    try {
      Object.defineProperty(wrapped, '__olazAr', { value: true });
    } catch {
      wrapped.__olazAr = true;
    }
    return wrapped;
  }

  function makeMergeUndoHook(origMerge) {
    const wrapped = function () {
      if (!antiRecallEnabled) {
        return origMerge.apply(this, arguments);
      }

      const localPreSnaps = new Map();
      const capturePreItem = (it) => {
        if (!it || typeof it !== 'object') return;
        const snap = snapshotMessageForAntiRecall(it, false);
        if (snap) {
          const k1 = cleanMsgKey(it.msgId);
          const k2 = cleanMsgKey(it.cliMsgId);
          if (k1) localPreSnaps.set(k1, snap);
          if (k2) localPreSnaps.set(k2, snap);
        }
        if (Array.isArray(it.message)) {
          for (let i = 0; i < it.message.length; i++) capturePreItem(it.message[i]);
        }
      };

      const arg0 = arguments[0];
      const arg1 = arguments[1];
      for (let i = 0; i < arguments.length; i++) {
        const arg = arguments[i];
        if (Array.isArray(arg)) {
          for (let j = 0; j < arg.length; j++) capturePreItem(arg[j]);
        } else if (arg && typeof arg === 'object') {
          capturePreItem(arg);
        }
      }

      const fallbackPreSnap =
        arg0 && typeof arg0 === 'object' && !Array.isArray(arg0)
          ? localPreSnaps.get(cleanMsgKey(arg0.msgId)) ||
            localPreSnaps.get(cleanMsgKey(arg0.cliMsgId)) ||
            lookupShadowForMsg(arg0)
          : null;

      const incomingUndoHint = Boolean(
        arg1 &&
          typeof arg1 === 'object' &&
          !Array.isArray(arg1) &&
          (isRecallTombstoneShape(arg1) || arg1.__olazRecalled)
      );
      const incomingKindHint =
        arg1 &&
        typeof arg1 === 'object' &&
        (Number(arg1.msgType) === -27 || Number(arg1.status) === -27)
          ? -27
          : undefined;

      const res = origMerge.apply(this, arguments);

      const fixMergedItem = (item, explicitSnap) => {
        if (!item || typeof item !== 'object') return item;
        const k1 = cleanMsgKey(item.msgId);
        const k2 = cleanMsgKey(item.cliMsgId);
        const snap =
          (k1 && localPreSnaps.get(k1)) ||
          (k2 && localPreSnaps.get(k2)) ||
          lookupShadowForMsg(item) ||
          explicitSnap ||
          null;
        const recalledEntry = isMsgInRecalledMap(item);
        const becameRecalled =
          isRecallTombstoneShape(item) ||
          Boolean(item.__olazRecalled) ||
          Boolean(recalledEntry) ||
          (incomingUndoHint && item === res);

        if (becameRecalled) {
          const kind =
            incomingKindHint !== undefined
              ? incomingKindHint
              : Number(item.msgType) === -27 || Number(item.status) === -27
                ? -27
                : recalledEntry
                  ? recalledEntry.kind
                  : item.__olazPresalledType || 20;
          if (snap) {
            return cloneRestoredMessageFromShadow(item, snap, kind);
          }
          recordRecalledMsgIds(item, kind);
          return Object.assign({}, item, {
            __olazPresalledType: kind,
            __olazRecalled: true,
            __olazRecalledAt: Number(item.__olazRecalledAt) || Date.now(),
            __olazPreserved: true,
          });
        }

        let outItem = restoreQuoteIfWiped(item, snap);
        if (Array.isArray(outItem.message)) {
          let anyChildChanged = false;
          const nextChildren = outItem.message.slice();
          for (let i = 0; i < nextChildren.length; i++) {
            const fixedChild = fixMergedItem(nextChildren[i], null);
            if (fixedChild !== nextChildren[i]) {
              nextChildren[i] = fixedChild;
              anyChildChanged = true;
            }
          }
          if (anyChildChanged) {
            outItem = Object.assign({}, outItem, { message: nextChildren });
          }
        }
        return outItem;
      };

      let finalOut = res;
      if (Array.isArray(res)) {
        finalOut = res.map((it) => fixMergedItem(it, null));
      } else if (res && typeof res === 'object') {
        finalOut = fixMergedItem(res, fallbackPreSnap);
      }

      scheduleAntiRecallDomPass();
      return finalOut;
    };

    try {
      Object.defineProperty(wrapped, '__olazAr', { value: true });
    } catch {
      wrapped.__olazAr = true;
    }
    return wrapped;
  }

  function patchAntiRecallTarget(obj) {
    if (!obj || (typeof obj !== 'object' && typeof obj !== 'function')) return false;
    if (typeof window !== 'undefined' && obj === window.$capp) return false;
    let patched = false;

    const defineArMethod = (target, prop, wrapper) => {
      try {
        Object.defineProperty(wrapper, '__olazAr', { value: true });
      } catch {
        wrapper.__olazAr = true;
      }
      try {
        target[prop] = wrapper;
        patched = true;
      } catch {}
    };

    // 1. `ConversationOperation.modifyMessageData(delInfo, origMsg)`
    if (typeof obj.modifyMessageData === 'function' && !obj.modifyMessageData.__olazAr) {
      const origModify = obj.modifyMessageData;
      defineArMethod(obj, 'modifyMessageData', function (delInfo, origMsg) {
        if (
          antiRecallEnabled &&
          delInfo &&
          typeof delInfo === 'object' &&
          (delInfo.type === 'recall' || delInfo.type === 'everyone') &&
          origMsg &&
          typeof origMsg === 'object'
        ) {
          const kind = delInfo.type === 'everyone' ? -27 : 20;
          const snap = snapshotMessageForAntiRecall(origMsg, true) || lookupShadowForMsg(origMsg);
          advanceRetentionQuotaWindow(kind === -27 ? 9 : 4);
          if (snap) {
            const cloned = cloneRestoredMessageFromShadow(origMsg, snap, kind);
            if (delInfo.serverTime !== undefined) cloned.serverTime = delInfo.serverTime;
            if (delInfo.uidSenderDel !== undefined) cloned.uidSenderDel = delInfo.uidSenderDel;
            scheduleAntiRecallDomPass();
            return cloned;
          }
          const fallbackClone = Object.assign({}, origMsg, {
            msgType: inferOriginalMsgType(origMsg, null),
            __olazRecalled: true,
            __olazPresalledType: kind,
            __olazRecalledAt: Number(origMsg.__olazRecalledAt) || Date.now(),
            __olazPreserved: true,
          });
          if (Number(fallbackClone.status) === -27) fallbackClone.status = 0;
          recordRecalledMsgIds(fallbackClone, kind);
          scheduleAntiRecallDomPass();
          return fallbackClone;
        }
        return origModify.apply(this, arguments);
      });
    }

    // 2. `ConversationResources`: `cleanResourcesOfMessages`, `cleanReplyMessages`, `deleteMessageInMediaStore`
    for (const cleanFnName of [
      'cleanResourcesOfMessages',
      'cleanReplyMessages',
      'deleteMessageInMediaStore',
    ]) {
      if (typeof obj[cleanFnName] === 'function' && !obj[cleanFnName].__olazAr) {
        const origClean = obj[cleanFnName];
        defineArMethod(obj, cleanFnName, function () {
          if (isRecallCleanupArgs(arguments)) {
            return Promise.resolve();
          }
          return origClean.apply(this, arguments);
        });
      }
    }

    // 3. `deleteMessageResourceIfExists(m)`
    if (
      typeof obj.deleteMessageResourceIfExists === 'function' &&
      !obj.deleteMessageResourceIfExists.__olazAr
    ) {
      const origDelRes = obj.deleteMessageResourceIfExists;
      defineArMethod(obj, 'deleteMessageResourceIfExists', function () {
        if (antiRecallEnabled) return Promise.resolve(false);
        return origDelRes.apply(this, arguments);
      });
    }

    // 4. `convertToRecalled(m)` & `convertToDelEveryone(m)`
    if (typeof obj.convertToRecalled === 'function' && !obj.convertToRecalled.__olazAr) {
      try {
        obj.convertToRecalled = makeConvertHook(obj.convertToRecalled, 20);
        patched = true;
      } catch {}
    }
    if (typeof obj.convertToDelEveryone === 'function' && !obj.convertToDelEveryone.__olazAr) {
      try {
        obj.convertToDelEveryone = makeConvertHook(obj.convertToDelEveryone, -27);
        patched = true;
      } catch {}
    }

    // 5. `mergeUndo(e, t)` & `mergeUndoMulti(e, t)`
    for (const mergeName of ['mergeUndo', 'mergeUndoMulti']) {
      if (typeof obj[mergeName] === 'function' && !obj[mergeName].__olazAr) {
        try {
          obj[mergeName] = makeMergeUndoHook(obj[mergeName]);
          patched = true;
        } catch {}
      }
    }

    // 6. `MessageCache.undoMessages(map)`
    if (typeof obj.undoMessages === 'function' && !obj.undoMessages.__olazAr) {
      const origUndoMsgs = obj.undoMessages;
      defineArMethod(obj, 'undoMessages', function (mapArg) {
        if (this && typeof this === 'object') {
          liveMessageCacheRefs.add(this);
        }
        const cacheStore = this && (this._cache || this.cache || this.data);
        if (cacheStore) {
          safeCall(() => walkMessageCacheContainer(cacheStore, true), false);
        }
        if (mapArg && typeof mapArg === 'object') {
          safeCall(() => walkMessageCacheContainer(mapArg, true), false);
        }
        const res = origUndoMsgs.apply(this, arguments);
        if (antiRecallEnabled && cacheStore) {
          safeCall(() => walkMessageCacheContainer(cacheStore, false), false);
          scheduleAntiRecallDomPass();
        }
        return res;
      });
    }

    // 7. `PreviewDataManager.onUndoMessage(src, msg)`
    if (typeof obj.onUndoMessage === 'function' && !obj.onUndoMessage.__olazAr) {
      const origOnUndo = obj.onUndoMessage;
      defineArMethod(obj, 'onUndoMessage', function (src, msg) {
        if (!antiRecallEnabled) {
          return origOnUndo.apply(this, arguments);
        }
        const targetMsg =
          msg && typeof msg === 'object'
            ? msg
            : src && typeof src === 'object'
              ? src
              : null;
        if (targetMsg) {
          recordRecalledMsgIds(targetMsg);
        }

        const previewMap = this && this.data instanceof Map ? this.data : null;
        if (!previewMap) {
          return undefined;
        }

        const snapMap = new Map();
        for (const [cid, pv] of previewMap.entries()) {
          if (pv && typeof pv === 'object') {
            snapMap.set(cid, Object.assign({}, pv));
          }
        }

        const res = origOnUndo.apply(this, arguments);

        const applyRecalledPreviewPrefix = (txt) => {
          const s = String(txt || '').trim();
          if (!s) return '';
          return s.startsWith('[Đã thu hồi]') ? s : '[Đã thu hồi] ' + s;
        };

        for (const [cid, beforePv] of snapMap.entries()) {
          const afterPv = previewMap.get(cid);
          const wasWiped =
            !afterPv ||
            Number(afterPv.msgType) === 20 ||
            Number(afterPv.messageType) === 20 ||
            Number(afterPv.msgType) === -27 ||
            Number(afterPv.messageType) === -27 ||
            afterPv.computedMessage === '[Tin nhắn đã được thu hồi]' ||
            afterPv.message === '[Tin nhắn đã được thu hồi]' ||
            (beforePv.computedMessage && !afterPv.computedMessage) ||
            (typeof beforePv.message === 'string' && beforePv.message && !afterPv.message);
          if (wasWiped) {
            const restoredPv = Object.assign({}, afterPv || {}, beforePv, {
              __olazRecalled: true,
            });
            if (typeof beforePv.computedMessage === 'string' && beforePv.computedMessage) {
              restoredPv.computedMessage = applyRecalledPreviewPrefix(beforePv.computedMessage);
            } else if (typeof beforePv.message === 'string' && beforePv.message) {
              restoredPv.message = applyRecalledPreviewPrefix(beforePv.message);
            }
            previewMap.set(cid, restoredPv);
          }
        }
        return res;
      });
    }

    return patched;
  }

  function scanStoreConversationsForAntiRecall(state, recoverIfRecalled) {
    const alls = (state && state.conversations && state.conversations.alls) || null;
    if (!alls || typeof alls !== 'object') return false;
    let recoveredAny = false;

    const cids = Object.keys(alls);
    for (let cIdx = 0; cIdx < cids.length; cIdx++) {
      const convObj = alls[cids[cIdx]];
      const msgs = convObj && convObj.messages;
      if (!Array.isArray(msgs)) continue;

      for (let i = 0; i < msgs.length; i++) {
        let m = msgs[i];
        if (!m || typeof m !== 'object') continue;

        // Handle `MSG_PHOTO_GROUP = 2503` & `MSG_STICKER_GROUP = 2502` child arrays
        if (
          Array.isArray(m.message) &&
          (Number(m.msgType) === MSG_PHOTO_GROUP ||
            Number(m.msgType) === MSG_STICKER_GROUP ||
            m.message.length > 0)
        ) {
          let childRecovered = false;
          let nextChildren = null;
          for (let j = 0; j < m.message.length; j++) {
            const ch = m.message[j];
            if (!ch || typeof ch !== 'object') continue;
            if (!isRecallTombstoneShape(ch)) {
              snapshotMessageForAntiRecall(ch, false);
            } else if (recoverIfRecalled) {
              const chSnap = lookupShadowForMsg(ch);
              if (chSnap) {
                if (!nextChildren) nextChildren = m.message.slice();
                nextChildren[j] = cloneRestoredMessageFromShadow(ch, chSnap);
                childRecovered = true;
              } else {
                recordRecalledMsgIds(ch);
              }
            }
          }
          if (childRecovered && nextChildren) {
            m = Object.assign({}, m, { message: nextChildren });
            msgs[i] = m;
            recoveredAny = true;
          }
        }

        if (!isRecallTombstoneShape(m)) {
          snapshotMessageForAntiRecall(m, false);
          if (recoverIfRecalled) {
            const quoteFixed = restoreQuoteIfWiped(m, null);
            if (quoteFixed !== m) {
              msgs[i] = quoteFixed;
              recoveredAny = true;
            }
          }
        } else if (recoverIfRecalled) {
          const snap = lookupShadowForMsg(m);
          if (snap) {
            msgs[i] = cloneRestoredMessageFromShadow(m, snap);
            recoveredAny = true;
          } else {
            recordRecalledMsgIds(m);
          }
        }
      }
    }

    if (recoveredAny) {
      scheduleAntiRecallDomPass();
    }
    return recoveredAny;
  }

  function wrapStoreForAntiRecall(store) {
    if (!store || typeof store !== 'object' || typeof store.dispatch !== 'function') {
      return store;
    }
    liveReduxStores.add(store);

    if (!store.dispatch.__olazAr) {
      const origDispatch = store.dispatch.bind(store);
      const wrappedDispatch = function () {
        safeCall(() => scanStoreConversationsForAntiRecall(store.getState(), false), false);
        const res = origDispatch.apply(this, arguments);
        safeCall(
          () => scanStoreConversationsForAntiRecall(store.getState(), antiRecallEnabled),
          false
        );
        return res;
      };
      try {
        Object.defineProperty(wrappedDispatch, '__olazAr', { value: true });
      } catch {
        wrappedDispatch.__olazAr = true;
      }
      try {
        store.dispatch = wrappedDispatch;
      } catch {}
    }

    if (typeof store.subscribe === 'function' && !store.__olazArSubscribed) {
      try {
        Object.defineProperty(store, '__olazArSubscribed', { value: true });
      } catch {
        store.__olazArSubscribed = true;
      }
      safeCall(
        () =>
          store.subscribe(() => {
            safeCall(
              () => scanStoreConversationsForAntiRecall(store.getState(), antiRecallEnabled),
              false
            );
            if (antiRecallEnabled) scheduleAntiRecallDomPass();
          }),
        undefined
      );
      safeCall(
        () => scanStoreConversationsForAntiRecall(store.getState(), antiRecallEnabled),
        false
      );
    }
    return store;
  }

  // ---- 3.4 O(Δmodules) Webpack Module & DI Registration Hook Wiring ----

  const patchedModuleIds = new Set();
  const inspectedTargets = typeof WeakSet === 'function' ? new WeakSet() : null;

  function inspectSingleCandidate(cand) {
    if (!cand || (typeof cand !== 'object' && typeof cand !== 'function')) return;
    if (typeof window !== 'undefined' && cand === window.$capp) return;
    if (inspectedTargets) {
      if (inspectedTargets.has(cand)) return;
      inspectedTargets.add(cand);
    }

    try {
      patchAntiRecallTarget(cand);
    } catch {}

    if (typeof cand === 'function' && cand.prototype) {
      try {
        patchAntiRecallTarget(cand.prototype);
      } catch {}
    } else if (typeof cand === 'object') {
      const proto = safeCall(() => Object.getPrototypeOf(cand), null);
      if (proto && proto !== Object.prototype && proto !== Array.prototype) {
        try {
          patchAntiRecallTarget(proto);
        } catch {}
      }
    }

    // Detect Redux store
    if (
      typeof cand === 'object' &&
      typeof cand.getState === 'function' &&
      typeof cand.dispatch === 'function' &&
      typeof cand.subscribe === 'function'
    ) {
      const st = safeCall(() => cand.getState(), null);
      if (st && typeof st === 'object' && (st.conversations || st.user)) {
        wrapStoreForAntiRecall(cand);
      }
    }

    // Detect `MessageCache` instance holding `_cache`
    if (
      typeof cand === 'object' &&
      cand._cache instanceof Map &&
      typeof cand.undoMessages === 'function'
    ) {
      liveMessageCacheRefs.add(cand);
    }

    // Hook DI container `.resolve` & `_registry.set` so lazily-registered/instantiated services are patched immediately
    if (
      typeof cand === 'object' &&
      cand._registry &&
      cand._registry._registryMap instanceof Map &&
      typeof cand.resolve === 'function' &&
      !cand.resolve.__olazArResolve
    ) {
      const origResolve = cand.resolve;
      const wrappedResolve = function () {
        const resolved = origResolve.apply(this, arguments);
        if (resolved && (typeof resolved === 'object' || typeof resolved === 'function')) {
          inspectSingleCandidate(resolved);
        }
        return resolved;
      };
      try {
        Object.defineProperty(wrappedResolve, '__olazArResolve', { value: true });
        cand.resolve = wrappedResolve;
      } catch {}

      const regObj = cand._registry;
      if (typeof regObj.set === 'function' && !regObj.set.__olazArRegSet) {
        const origRegSet = regObj.set;
        const wrappedRegSet = function (_token, registration) {
          const out = origRegSet.apply(this, arguments);
          inspectDiRegistration(registration);
          return out;
        };
        try {
          Object.defineProperty(wrappedRegSet, '__olazArRegSet', { value: true });
          regObj.set = wrappedRegSet;
        } catch {}
      }

      for (const regs of cand._registry._registryMap.values()) {
        if (Array.isArray(regs)) {
          for (let i = 0; i < regs.length; i++) inspectDiRegistration(regs[i]);
        }
      }
    }
  }

  function inspectExportTree(ex) {
    if (!ex || (typeof ex !== 'object' && typeof ex !== 'function')) return;
    inspectSingleCandidate(ex);
    if (typeof ex === 'object') {
      const keys = safeCall(() => Object.keys(ex), []);
      for (let i = 0; i < keys.length; i++) {
        const sub = safeCall(() => ex[keys[i]], null);
        if (sub && (typeof sub === 'object' || typeof sub === 'function')) {
          inspectSingleCandidate(sub);
        }
      }
    }
  }

  function inspectDiRegistration(reg) {
    if (!reg || typeof reg !== 'object') return;
    if (reg.instance) inspectSingleCandidate(reg.instance);
    if (reg.provider) {
      if (reg.provider.useClass) inspectSingleCandidate(reg.provider.useClass);
      if (reg.provider.useValue) inspectSingleCandidate(reg.provider.useValue);
      inspectSingleCandidate(reg.provider);
    }
  }

  function scanWebpackRequireDelta(req) {
    const cache = req && req.c;
    if (!cache || typeof cache !== 'object') return;
    const ids = Object.keys(cache);
    for (let i = 0; i < ids.length; i++) {
      const id = ids[i];
      if (patchedModuleIds.has(id)) continue;
      const mod = cache[id];
      if (!mod || !mod.exports) continue;
      inspectExportTree(mod.exports);
      if (mod.l !== false) {
        patchedModuleIds.add(id);
      }
    }
  }

  if (Array.isArray(host.webpackModuleHooks)) {
    host.webpackModuleHooks.push((arg1, arg2, arg3) => {
      if (arg1 && (typeof arg1 === 'function' || typeof arg1 === 'object') && arg1.c) {
        scanWebpackRequireDelta(arg1);
        return;
      }
      if (arg1 && (typeof arg1 === 'object' || typeof arg1 === 'function')) {
        if (arg2 !== undefined && arg2 !== null) {
          const mid = String(arg2);
          if (patchedModuleIds.has(mid)) return;
          patchedModuleIds.add(mid);
        }
        inspectExportTree(arg1);
      }
      if (arg3 && arg3.c) {
        scanWebpackRequireDelta(arg3);
      }
    });
  }

  if (Array.isArray(host.diRegistrationHooks)) {
    host.diRegistrationHooks.push((reg) => {
      if (Array.isArray(reg)) {
        for (let i = 0; i < reg.length; i++) inspectDiRegistration(reg[i]);
      } else {
        inspectDiRegistration(reg);
      }
    });
  }

  // Cooperative self-managed webpack scanner for both `index.html` & `shared-worker.html`
  // so lazy chunks pushed onto `window.webpackJsonp` are always scanned in O(Δmodules).
  function installFallbackWebpackScanner() {
    if (typeof window === 'undefined' || !window) return;
    let probing = false;
    const probeRequire = () => {
      if (probing) return false;
      probing = true;
      try {
        if (
          !window.__zaloRequire &&
          Array.isArray(window.webpackJsonp) &&
          window.webpackJsonp.push !== Array.prototype.push
        ) {
          const pid = '__olaz_ar_probe_' + Date.now();
          try {
            window.webpackJsonp.push([
              [pid],
              {
                [pid]: (_m, _e, r) => {
                  window.__zaloRequire = r;
                },
              },
              [[pid]],
            ]);
          } catch {}
        }
        if (window.__zaloStore) {
          wrapStoreForAntiRecall(window.__zaloStore);
        }
        if (window.__zaloRequire) {
          scanWebpackRequireDelta(window.__zaloRequire);
          return true;
        }
        return false;
      } finally {
        probing = false;
      }
    };

    const chainJsonpPush = (arr) => {
      if (!Array.isArray(arr) || arr.__olazArJsonpHooked) return;
      if (arr.push !== Array.prototype.push && typeof arr.push === 'function' && !arr.push.__olazArPush) {
        const prevPush = arr.push;
        const nextPush = function () {
          const r = prevPush.apply(this, arguments);
          probeRequire();
          Promise.resolve().then(probeRequire);
          return r;
        };
        try {
          Object.defineProperty(nextPush, '__olazArPush', { value: true });
          arr.push = nextPush;
          Object.defineProperty(arr, '__olazArJsonpHooked', { value: true });
        } catch {}
      }
    };

    if (window.webpackJsonp) {
      chainJsonpPush(window.webpackJsonp);
    }
    probeRequire();

    if (typeof window.addEventListener === 'function') {
      window.addEventListener('load', () => {
        if (window.webpackJsonp) chainJsonpPush(window.webpackJsonp);
        probeRequire();
      });
    }
    if (typeof document !== 'undefined' && document && typeof document.addEventListener === 'function') {
      document.addEventListener('DOMContentLoaded', () => {
        if (window.webpackJsonp) chainJsonpPush(window.webpackJsonp);
        probeRequire();
      });
    }
  }

  installFallbackWebpackScanner();
  if (
    globalScope.__olazBootReady &&
    typeof globalScope.__olazBootReady.then === 'function'
  ) {
    globalScope.__olazBootReady.then(() => installFallbackWebpackScanner()).catch(() => {});
  }

  // ==========================================================================
  // Layer 4: React UI `[Đã thu hồi]` Indicator + Context Menu Toggle (`index.html`)
  // ==========================================================================

  const STYLE_ELEMENT_ID = '__olaz-anti-recall-style';
  const SVG_NS = 'http://www.w3.org/2000/svg';

  function ensureAntiRecallStyles() {
    if (
      typeof document === 'undefined' ||
      !document ||
      typeof document.createElement !== 'function'
    ) {
      return;
    }
    if (
      typeof document.getElementById === 'function' &&
      document.getElementById(STYLE_ELEMENT_ID)
    ) {
      return;
    }
    const parent = document.head || document.documentElement || document.body;
    if (!parent || typeof parent.appendChild !== 'function') return;

    const style = document.createElement('style');
    style.id = STYLE_ELEMENT_ID;
    style.textContent = [
      '.chat-message.olaz-is-recalled .card, .chat-message.olaz-is-recalled .ai-sticker-message-content-wrapper { border: 1px dashed rgba(239, 68, 68, 0.5) !important; background-color: rgba(239, 68, 68, 0.045) !important; }',
      '.olaz-recalled-badge { display: inline-flex; align-items: center; margin-top: 4px; padding: 1px 7px; border-radius: 999px; font-size: 11px; font-weight: 600; line-height: 1.4; color: #ef4444; background: rgba(239, 68, 68, 0.12); border: 1px solid rgba(239, 68, 68, 0.24); user-select: none; pointer-events: none; }',
      '.olaz-recovered-text { margin-top: 4px; padding: 4px 8px; border-left: 2px solid #ef4444; font-size: 13px; line-height: 1.45; white-space: pre-wrap; word-break: break-word; }',
      '.zmenu-separator.olaz-cx-sep { border: 0; height: 1px; background: var(--divider-subtle, rgba(0, 0, 0, 0.08)); margin: 4px; }',
      '.zmenu-item.olaz-cx-antirecall { width: 100%; height: 36px; min-width: 112px; padding: 0 10px; margin: 4px 0; border: 0; border-radius: 4px; display: flex; align-items: center; justify-content: flex-start; box-sizing: border-box; background-color: var(--layer-background, transparent); color: var(--text-primary, #081b3a); font-family: inherit; font-size: .875rem; font-weight: 400; line-height: 20px; text-align: left; cursor: pointer; user-select: none; appearance: none; -webkit-appearance: none; }',
      '.zmenu-item.olaz-cx-antirecall:hover { background-color: var(--layer-background-hover, rgba(0, 0, 0, 0.05)); }',
      '.zmenu-item.olaz-cx-antirecall:focus-visible { outline: 2px solid var(--border-focused, #0068ff); outline-offset: -2px; background-color: var(--layer-background-hover, rgba(0, 0, 0, 0.05)); }',
      '.olaz-cx-icon { width: 20px; height: 20px; margin-right: 12px; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; color: var(--icon-primary, currentColor); }',
      '.olaz-cx-label { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; margin-right: 12px; }',
      '.olaz-cx-switch { position: relative; width: 30px; height: 17px; border-radius: 999px; background-color: rgba(148, 163, 184, 0.55); flex-shrink: 0; transition: background-color 140ms cubic-bezier(0.2, 0, 0, 1); }',
      '.olaz-cx-antirecall[aria-checked="true"] .olaz-cx-switch { background-color: var(--button-primary-normal, #0068ff); }',
      '.olaz-cx-switch-thumb { position: absolute; top: 2px; left: 2px; width: 13px; height: 13px; border-radius: 50%; background: #ffffff; box-shadow: 0 1px 2px rgba(0, 0, 0, 0.22); transition: transform 140ms cubic-bezier(0.2, 0, 0, 1); }',
      '.olaz-cx-antirecall[aria-checked="true"] .olaz-cx-switch-thumb { transform: translateX(13px); }',
      '@media (prefers-reduced-motion: reduce) { .olaz-cx-switch, .olaz-cx-switch-thumb { transition: none !important; } }',
    ].join('\n');
    try {
      parent.appendChild(style);
    } catch {}
  }

  function createSvgElement(tag, attrs) {
    const el =
      typeof document.createElementNS === 'function'
        ? document.createElementNS(SVG_NS, tag)
        : document.createElement(tag);
    if (attrs && el && typeof el.setAttribute === 'function') {
      const keys = Object.keys(attrs);
      for (let i = 0; i < keys.length; i++) {
        el.setAttribute(keys[i], attrs[keys[i]]);
      }
    }
    return el;
  }

  function buildAntiRecallMenuIcon() {
    const wrap = document.createElement('span');
    wrap.className = 'menu-icon left mn-html olaz-cx-icon';
    const svg = createSvgElement('svg', {
      'aria-hidden': 'true',
      width: '20',
      height: '20',
      viewBox: '0 0 24 24',
      fill: 'none',
      stroke: 'currentColor',
      'stroke-width': '1.7',
      'stroke-linecap': 'round',
      'stroke-linejoin': 'round',
    });
    if (svg && typeof svg.appendChild === 'function') {
      svg.appendChild(createSvgElement('path', { d: 'M3 12a9 9 0 1 0 3-6.7L3 8' }));
      svg.appendChild(createSvgElement('path', { d: 'M3 3v5h5' }));
      svg.appendChild(createSvgElement('path', { d: 'M9.5 12.5l2 2 3.5-4' }));
      wrap.appendChild(svg);
    }
    return wrap;
  }

  function resolveBubbleMenuListContainer(menu, cxItems) {
    const sep =
      typeof menu.querySelector === 'function'
        ? menu.querySelector('.zmenu-separator')
        : null;
    if (sep && sep.parentNode) return sep.parentNode;
    if (cxItems.length >= 2) {
      const ancestors = new Set();
      let cur = cxItems[0] && cxItems[0].parentNode;
      while (cur) {
        ancestors.add(cur);
        if (cur === menu) break;
        cur = cur.parentNode;
      }
      let probe = cxItems[1] && cxItems[1].parentNode;
      while (probe) {
        if (ancestors.has(probe)) return probe;
        if (probe === menu) break;
        probe = probe.parentNode;
      }
    }
    const inner = menu.firstElementChild && menu.firstElementChild.firstElementChild;
    if (inner && cxItems[0] && typeof inner.contains === 'function' && inner.contains(cxItems[0])) {
      return inner;
    }
    return (cxItems[0] && cxItems[0].parentNode) || menu;
  }

  function syncOpenContextMenus() {
    if (
      typeof document === 'undefined' ||
      !document ||
      typeof document.querySelectorAll !== 'function'
    ) {
      return;
    }
    const items = safeCall(() => document.querySelectorAll('.olaz-cx-antirecall'), []);
    for (const btn of Array.from(items || [])) {
      if (btn && typeof btn.setAttribute === 'function') {
        btn.setAttribute('aria-checked', antiRecallEnabled ? 'true' : 'false');
      }
    }
  }

  function toggleAntiRecallViaHost(nextVal) {
    const normalized = Boolean(nextVal);
    if (typeof host.setAntiRecall === 'function') {
      host.setAntiRecall(normalized, { source: 'anti-recall-ui' });
    }
    if (antiRecallEnabled !== normalized) {
      updateAntiRecallState(normalized, false);
    }
  }

  function injectBubbleMenuAntiRecall() {
    if (
      typeof document === 'undefined' ||
      !document ||
      typeof document.querySelectorAll !== 'function' ||
      typeof document.createElement !== 'function'
    ) {
      return;
    }
    const menus = safeCall(() => document.querySelectorAll('.zmenu-body'), []);
    for (const menu of Array.from(menus || [])) {
      if (!menu) continue;
      if (
        menu.classList &&
        typeof menu.classList.contains === 'function' &&
        menu.classList.contains('content-only')
      ) {
        continue;
      }
      const cxItems =
        typeof menu.querySelectorAll === 'function'
          ? Array.from(menu.querySelectorAll('[data-id^="div_CX_"]') || []).filter((el) => {
              const did =
                el && typeof el.getAttribute === 'function' ? el.getAttribute('data-id') : '';
              return did !== 'div_CX_OlazAntiRecall';
            })
          : [];
      if (!cxItems.length) continue;

      ensureAntiRecallStyles();
      const existing =
        typeof menu.querySelector === 'function'
          ? menu.querySelector('.olaz-cx-antirecall')
          : null;
      if (existing) {
        if (typeof existing.setAttribute === 'function') {
          existing.setAttribute('aria-checked', antiRecallEnabled ? 'true' : 'false');
        }
        continue;
      }

      const listContainer = resolveBubbleMenuListContainer(menu, cxItems);
      if (!listContainer || typeof listContainer.appendChild !== 'function') continue;

      const sep = document.createElement('hr');
      sep.className = 'zmenu-separator olaz-cx-sep';

      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'zmenu-item --NORMAL md olaz-cx-antirecall';
      if (typeof btn.setAttribute === 'function') {
        btn.setAttribute('role', 'menuitemcheckbox');
        btn.setAttribute('aria-checked', antiRecallEnabled ? 'true' : 'false');
        btn.setAttribute('data-id', 'div_CX_OlazAntiRecall');
      }

      const labelEl = document.createElement('span');
      labelEl.className = 'truncate flx-1 olaz-cx-label';
      labelEl.textContent = 'Giữ thu hồi';

      const sw = document.createElement('span');
      sw.className = 'olaz-cx-switch';
      if (typeof sw.setAttribute === 'function') sw.setAttribute('aria-hidden', 'true');
      const thumb = document.createElement('span');
      thumb.className = 'olaz-cx-switch-thumb';
      if (typeof sw.appendChild === 'function') sw.appendChild(thumb);

      if (typeof btn.appendChild === 'function') {
        btn.appendChild(buildAntiRecallMenuIcon());
        btn.appendChild(labelEl);
        btn.appendChild(sw);
      }

      const stopMouse = (ev) => {
        if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
      };
      if (typeof btn.addEventListener === 'function') {
        btn.addEventListener('mousedown', stopMouse);
        btn.addEventListener('mouseup', stopMouse);
        btn.addEventListener('click', (ev) => {
          if (ev && typeof ev.preventDefault === 'function') ev.preventDefault();
          if (ev && typeof ev.stopPropagation === 'function') ev.stopPropagation();
          const next = !antiRecallEnabled;
          if (typeof btn.setAttribute === 'function') {
            btn.setAttribute('aria-checked', next ? 'true' : 'false');
          }
          toggleAntiRecallViaHost(next);

          const closeTimer = setTimeout(() => {
            if (!btn.parentNode) return;
            try {
              if (
                typeof KeyboardEvent === 'function' &&
                typeof document.dispatchEvent === 'function'
              ) {
                document.dispatchEvent(
                  new KeyboardEvent('keydown', {
                    key: 'Escape',
                    code: 'Escape',
                    keyCode: 27,
                    which: 27,
                    bubbles: true,
                  })
                );
              }
            } catch {}
            try {
              const target =
                (typeof document.getElementById === 'function' &&
                  document.getElementById('messageViewScroll')) ||
                document.body;
              if (
                target &&
                typeof target.dispatchEvent === 'function' &&
                typeof MouseEvent === 'function'
              ) {
                target.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
                target.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
                target.dispatchEvent(new MouseEvent('click', { bubbles: true }));
              }
            } catch {}
          }, 260);
          if (closeTimer && typeof closeTimer.unref === 'function') closeTimer.unref();
        });
      }

      let dangerRow =
        typeof menu.querySelector === 'function'
          ? menu.querySelector('[data-id="div_CX_Recall"], [data-id="div_CX_Delete"]')
          : null;
      while (dangerRow && dangerRow.parentNode && dangerRow.parentNode !== listContainer) {
        dangerRow = dangerRow.parentNode;
      }
      if (
        dangerRow &&
        dangerRow.parentNode === listContainer &&
        typeof listContainer.insertBefore === 'function'
      ) {
        const prev = dangerRow.previousElementSibling || null;
        const prevIsSep = Boolean(
          prev &&
            prev.classList &&
            typeof prev.classList.contains === 'function' &&
            prev.classList.contains('zmenu-separator')
        );
        const anchor = prevIsSep ? prev : dangerRow;
        if (anchor.previousElementSibling) {
          listContainer.insertBefore(sep, anchor);
        }
        listContainer.insertBefore(btn, anchor);
        if (!prevIsSep) {
          const dangerSep = document.createElement('hr');
          dangerSep.className = 'zmenu-separator olaz-cx-sep';
          listContainer.insertBefore(dangerSep, dangerRow);
        }
      } else {
        if (listContainer.children && listContainer.children.length > 0) {
          listContainer.appendChild(sep);
        }
        listContainer.appendChild(btn);
      }
    }
  }

  function findFiberMessageOnNode(node) {
    if (!node || typeof node !== 'object') return null;
    let fiber = null;
    const keys = safeCall(() => Object.keys(node), []);
    for (let i = 0; i < keys.length; i++) {
      const k = keys[i];
      if (k.startsWith('__reactFiber') || k.startsWith('__reactInternalInstance')) {
        fiber = node[k];
        break;
      }
    }
    let hops = 0;
    while (fiber && hops < 14) {
      const props = fiber.memoizedProps || fiber.pendingProps;
      if (props && typeof props === 'object') {
        for (const propName of ['message', 'msg', 'data', 'item', 'chatMessage']) {
          const cand = props[propName];
          if (
            cand &&
            typeof cand === 'object' &&
            (cand.msgId !== undefined ||
              cand.cliMsgId !== undefined ||
              cand.__olazRecalled ||
              Array.isArray(cand.message))
          ) {
            return cand;
          }
        }
        if (props.msgId !== undefined || props.cliMsgId !== undefined || props.__olazRecalled) {
          return props;
        }
      }
      fiber = fiber.return;
      hops += 1;
    }
    return null;
  }

  function isOlazDomArtifact(node) {
    if (!node || node.nodeType !== 1) return false;
    const id = String(node.id || '');
    if (id === STYLE_ELEMENT_ID) return true;
    const cls = String(node.className || '');
    return (
      cls.includes('olaz-recalled-badge') ||
      cls.includes('olaz-recovered-text') ||
      cls.includes('olaz-cx-antirecall') ||
      cls.includes('olaz-cx-sep')
    );
  }

  function removeNodeSafely(el) {
    if (!el) return;
    try {
      if (typeof el.remove === 'function') el.remove();
      else if (el.parentNode && typeof el.parentNode.removeChild === 'function') {
        el.parentNode.removeChild(el);
      }
    } catch {}
  }

  function stripRecalledDomFromRow(row) {
    if (!row) return;
    if (row.classList && typeof row.classList.remove === 'function') {
      safeCall(() => row.classList.remove('olaz-is-recalled'), undefined);
    }
    if (typeof row.querySelectorAll === 'function') {
      const stale = safeCall(
        () => row.querySelectorAll('.olaz-recalled-badge, .olaz-recovered-text'),
        []
      );
      for (const el of Array.from(stale || [])) removeNodeSafely(el);
    }
  }

  function detectRowRecallState(row, rawDomId, fiberMsg) {
    let recalled = false;
    let kind = 20;

    if (fiberMsg && typeof fiberMsg === 'object') {
      if (fiberMsg.__olazRecalled) {
        recalled = true;
        if (Number(fiberMsg.__olazPresalledType) === -27) kind = -27;
      }
      const hitMain = isMsgInRecalledMap(fiberMsg);
      if (hitMain) {
        recalled = true;
        if (hitMain.kind === -27) kind = -27;
      }
      if (Array.isArray(fiberMsg.message)) {
        for (let i = 0; i < fiberMsg.message.length; i++) {
          const ch = fiberMsg.message[i];
          if (!ch || typeof ch !== 'object') continue;
          if (ch.__olazRecalled) {
            recalled = true;
            if (Number(ch.__olazPresalledType) === -27) kind = -27;
          }
          const hitChild = isMsgInRecalledMap(ch);
          if (hitChild) {
            recalled = true;
            if (hitChild.kind === -27) kind = -27;
          }
        }
      }
    }

    if (rawDomId && recalledIdsMap.has(rawDomId)) {
      recalled = true;
      const domHit = recalledIdsMap.get(rawDomId);
      if (domHit && domHit.kind === -27) kind = -27;
    }

    return { recalled, kind };
  }

  function resolveBubbleMountContainer(row) {
    if (!row || typeof row.querySelector !== 'function') return row;
    return (
      row.querySelector('.card') ||
      row.querySelector('.ai-sticker-message-content-wrapper') ||
      row.querySelector('.sticker-message, .message-content-wrapper, .bubble-message-content') ||
      row
    );
  }

  let antiRecallRafPending = false;
  let antiRecallObserver = null;

  function applyAntiRecallBadgesNow() {
    antiRecallRafPending = false;
    if (
      typeof document === 'undefined' ||
      !document ||
      typeof document.querySelectorAll !== 'function' ||
      typeof document.createElement !== 'function'
    ) {
      return;
    }

    injectBubbleMenuAntiRecall();

    if (!antiRecallEnabled) {
      const badges = safeCall(
        () => document.querySelectorAll('.olaz-recalled-badge, .olaz-recovered-text'),
        []
      );
      for (const el of Array.from(badges || [])) removeNodeSafely(el);
      const markedRows = safeCall(
        () => document.querySelectorAll('.chat-message.olaz-is-recalled'),
        []
      );
      for (const row of Array.from(markedRows || [])) {
        if (row && row.classList && typeof row.classList.remove === 'function') {
          safeCall(() => row.classList.remove('olaz-is-recalled'), undefined);
        }
      }
      return;
    }

    ensureAntiRecallStyles();

    const rows = safeCall(
      () =>
        document.querySelectorAll(
          '.chat-message[id^="bb_msg_id_"], .chat-message[data-node-type="bubble-message"]'
        ),
      []
    );
    for (const row of Array.from(rows || [])) {
      if (!row) continue;
      const rowIdStr = String(row.id || '');
      const rawDomId = rowIdStr.startsWith('bb_msg_id_')
        ? cleanMsgKey(rowIdStr.slice(10))
        : cleanMsgKey(
            typeof row.getAttribute === 'function'
              ? row.getAttribute('data-msg-id') || row.getAttribute('data-id')
              : ''
          );
      const fiberMsg = findFiberMessageOnNode(row);
      const { recalled, kind } = detectRowRecallState(row, rawDomId, fiberMsg);

      if (!recalled) {
        if (
          row.classList &&
          typeof row.classList.contains === 'function' &&
          row.classList.contains('olaz-is-recalled')
        ) {
          stripRecalledDomFromRow(row);
        }
        continue;
      }

      if (row.classList && typeof row.classList.add === 'function') {
        safeCall(() => row.classList.add('olaz-is-recalled'), undefined);
      }

      const mountTarget = resolveBubbleMountContainer(row);
      if (!mountTarget || typeof mountTarget.appendChild !== 'function') continue;

      const isUndoCard = Boolean(
        (mountTarget.classList &&
          typeof mountTarget.classList.contains === 'function' &&
          mountTarget.classList.contains('card--undo')) ||
          (typeof row.querySelector === 'function' && row.querySelector('.card--undo'))
      );
      if (isUndoCard) {
        const snap =
          (rawDomId && shadowMap.get(rawDomId)) || lookupShadowForMsg(fiberMsg);
        const preservedText =
          (fiberMsg &&
            !isRecallTombstoneShape(fiberMsg) &&
            extractPlainTextFromValue(fiberMsg.message || fiberMsg.content)) ||
          (snap &&
            extractPlainTextFromValue(
              snap.message !== undefined ? snap.message : snap.content
            )) ||
          '';
        if (preservedText && preservedText !== '[Tin nhắn đã được thu hồi]') {
          let recEl =
            typeof mountTarget.querySelector === 'function'
              ? mountTarget.querySelector('.olaz-recovered-text')
              : null;
          if (!recEl) {
            recEl = document.createElement('div');
            recEl.className = 'olaz-recovered-text';
            mountTarget.appendChild(recEl);
          }
          if (recEl.textContent !== preservedText) {
            recEl.textContent = preservedText;
          }
        }
      }

      const badgeText = kind === -27 ? '[Đã xóa cho mọi người]' : '[Đã thu hồi]';
      let badgeEl =
        typeof mountTarget.querySelector === 'function'
          ? mountTarget.querySelector('.olaz-recalled-badge')
          : null;
      if (!badgeEl) {
        badgeEl = document.createElement('div');
        badgeEl.className = 'olaz-recalled-badge';
        badgeEl.textContent = badgeText;
        mountTarget.appendChild(badgeEl);
      } else if (badgeEl.textContent !== badgeText) {
        badgeEl.textContent = badgeText;
      }
    }
  }

  scheduleAntiRecallDomPass = () => {
    if (antiRecallRafPending) return;
    antiRecallRafPending = true;
    if (typeof globalScope.requestAnimationFrame === 'function') {
      globalScope.requestAnimationFrame(applyAntiRecallBadgesNow);
    } else {
      const tid = setTimeout(applyAntiRecallBadgesNow, 16);
      if (tid && typeof tid.unref === 'function') tid.unref();
    }
  };

  function startAntiRecallDomObserver() {
    if (
      antiRecallObserver ||
      typeof MutationObserver !== 'function' ||
      typeof document === 'undefined' ||
      !document
    ) {
      return;
    }
    const root = document.documentElement || document.body;
    if (!root) return;
    antiRecallObserver = new MutationObserver((mutations) => {
      let relevant = false;
      for (let i = 0; i < mutations.length; i++) {
        const mut = mutations[i];
        if (!mut) continue;
        if (mut.type === 'attributes') {
          const tc = String((mut.target && mut.target.className) || '');
          if (tc.includes('zmenu-body') || tc.includes('chat-message')) {
            relevant = true;
            break;
          }
          continue;
        }
        for (const list of [mut.addedNodes, mut.removedNodes]) {
          if (!list) continue;
          for (const node of Array.from(list)) {
            if (!node || node.nodeType !== 1 || isOlazDomArtifact(node)) continue;
            relevant = true;
            break;
          }
          if (relevant) break;
        }
        if (relevant) break;
      }
      if (relevant) scheduleAntiRecallDomPass();
    });
    try {
      antiRecallObserver.observe(root, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ['class'],
      });
    } catch {
      antiRecallObserver = null;
    }
  }

  onWindowPrivacyChangedHook = () => {
    syncOpenContextMenus();
    if (antiRecallEnabled) {
      for (const store of liveReduxStores) {
        safeCall(() => scanStoreConversationsForAntiRecall(store.getState(), true), false);
      }
    }
    applyAntiRecallBadgesNow();
  };

  onRemoteRecalledIdHook = () => {
    if (antiRecallEnabled) {
      for (const cacheInst of liveMessageCacheRefs) {
        const cacheStore = cacheInst && (cacheInst._cache || cacheInst.cache || cacheInst.data);
        if (cacheStore) safeCall(() => walkMessageCacheContainer(cacheStore, false), false);
      }
      for (const store of liveReduxStores) {
        safeCall(() => scanStoreConversationsForAntiRecall(store.getState(), true), false);
      }
    }
    scheduleAntiRecallDomPass();
  };

  startAntiRecallDomObserver();
  if (typeof document !== 'undefined' && document) {
    if (document.readyState === 'loading' && typeof document.addEventListener === 'function') {
      document.addEventListener(
        'DOMContentLoaded',
        () => {
          startAntiRecallDomObserver();
          applyAntiRecallBadgesNow();
        },
        { once: true }
      );
    } else {
      scheduleAntiRecallDomPass();
    }
  }

  const publicApi = {
    isEnabled: () => antiRecallEnabled,
    setEnabled: (v) => toggleAntiRecallViaHost(v),
    getRecalledIds: () => recalledIdsMap,
    getShadowMap: () => shadowMap,
    applyBadgesNow: applyAntiRecallBadgesNow,
    patchTarget: patchAntiRecallTarget,
    wrapStore: wrapStoreForAntiRecall,
    lookupCachedMediaBlob,
  };

  if (typeof window !== 'undefined' && window) {
    window.__olazAntiRecall = publicApi;
  }
  globalScope.__olazAntiRecall = publicApi;
}

bootstrapAntiRecallRuntime(
  typeof globalThis !== 'undefined' ? globalThis : typeof self !== 'undefined' ? self : window
);
