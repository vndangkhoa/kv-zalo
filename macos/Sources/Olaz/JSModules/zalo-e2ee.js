/*
 * zalo-e2ee.js — Olaz glue around the vendored libsignal-protocol build.
 *
 * Loaded after vendor/libsignal-protocol.js inside JSRuntime (JavaScriptCore).
 * Publishes `globalThis.ZaloE2EE`; every function returns a Promise and takes or
 * returns only strings, numbers and plain objects so Swift can marshal them.
 *
 * Each local identity (the logged-in user plus demo peers) gets its own in-memory
 * SignalProtocolStore. A real engine would persist the stores and fetch prekey
 * bundles from the server; here bundles are read straight out of the peer's store.
 */
(function (g) {
  'use strict';
  const signal = g.libsignal;
  if (!signal) throw new Error('libsignal-protocol must be loaded before zalo-e2ee.js');

  const DEVICE_ID = 1;
  // Small batches keep first use fast; refilled when a peer drains them.
  const PREKEY_BATCH = 5;
  const SIGNED_PREKEY_ID = 1;
  const FINGERPRINT_ITERATIONS = 5200;

  // MARK: bytes helpers

  // libsignal hands keys around as ArrayBuffers, views, ByteBuffers *or* binary
  // strings (session records are stringified), so normalize before comparing.
  const toBytes = (k) => {
    if (k instanceof ArrayBuffer) return new Uint8Array(k);
    if (ArrayBuffer.isView(k)) return new Uint8Array(k.buffer, k.byteOffset, k.byteLength);
    if (typeof k === 'string') return Uint8Array.from(k, (c) => c.charCodeAt(0) & 0xff);
    if (k && typeof k.toArrayBuffer === 'function') return new Uint8Array(k.toArrayBuffer());
    throw new TypeError('Unsupported key representation');
  };
  const bytesEqual = (a, b) => {
    if (a === undefined || b === undefined) return false;
    const x = toBytes(a), y = toBytes(b);
    if (x.length !== y.length) return false;
    for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
    return true;
  };
  const utf8 = new TextEncoder();
  const utf8d = new TextDecoder();
  const base64ToBuffer = (b64) => {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out.buffer;
  };

  // MARK: in-memory store (libsignal-protocol-javascript test/InMemorySignalProtocolStore)

  class MemoryStore {
    constructor() { this.store = {}; }
    get Direction() { return { SENDING: 1, RECEIVING: 2 }; }
    put(key, value) {
      if (key === undefined || value === undefined || key === null || value === null) {
        throw new Error('Tried to store undefined/null');
      }
      this.store[key] = value;
    }
    get(key, fallback) { return key in this.store ? this.store[key] : fallback; }
    remove(key) { delete this.store[key]; }

    getIdentityKeyPair() { return Promise.resolve(this.get('identityKey')); }
    getLocalRegistrationId() { return Promise.resolve(this.get('registrationId')); }

    isTrustedIdentity(identifier, identityKey /*, direction */) {
      if (identifier === null || identifier === undefined) {
        return Promise.reject(new Error('Tried to check identity key for undefined/null key'));
      }
      if (identityKey === undefined || identityKey === null) {
        return Promise.reject(new Error('Expected an identity key'));
      }
      const trusted = this.get('identityKey' + identifier);
      // Trust on first use, then pin.
      return Promise.resolve(trusted === undefined || bytesEqual(identityKey, trusted));
    }
    loadIdentityKey(identifier) {
      return Promise.resolve(this.get('identityKey' + identifier));
    }
    saveIdentity(identifier, identityKey) {
      const name = signal.SignalProtocolAddress.fromString(identifier).getName();
      const existing = this.get('identityKey' + name);
      this.put('identityKey' + name, toBytes(identityKey).slice().buffer);
      return Promise.resolve(existing !== undefined && !bytesEqual(existing, identityKey));
    }

    loadPreKey(keyId) {
      const res = this.get('25519KeypreKey' + keyId);
      return Promise.resolve(res === undefined ? undefined : { pubKey: res.pubKey, privKey: res.privKey });
    }
    storePreKey(keyId, keyPair) { return Promise.resolve(this.put('25519KeypreKey' + keyId, keyPair)); }
    removePreKey(keyId) { return Promise.resolve(this.remove('25519KeypreKey' + keyId)); }

    loadSignedPreKey(keyId) {
      const res = this.get('25519KeysignedKey' + keyId);
      return Promise.resolve(res === undefined ? undefined : { pubKey: res.pubKey, privKey: res.privKey });
    }
    storeSignedPreKey(keyId, keyPair) { return Promise.resolve(this.put('25519KeysignedKey' + keyId, keyPair)); }
    removeSignedPreKey(keyId) { return Promise.resolve(this.remove('25519KeysignedKey' + keyId)); }

    loadSession(identifier) { return Promise.resolve(this.get('session' + identifier)); }
    storeSession(identifier, record) { return Promise.resolve(this.put('session' + identifier, record)); }
    removeSession(identifier) { return Promise.resolve(this.remove('session' + identifier)); }

    // Zalo fork addition (synchronous): last session-reset timestamp for a peer.
    getSessionTs(/* name */) { return undefined; }
  }

  // MARK: parties

  const parties = new Map(); // id -> { store, registrationId, identity, signedPreKey, preKeys: [{keyId, keyPair}] }
  const pendingRegistrations = new Map();

  const party = (id) => {
    const p = parties.get(id);
    if (!p) throw new Error('Unknown identity "' + id + '" — call registerIdentity first');
    return p;
  };

  async function createParty(id) {
    const store = new MemoryStore();
    const KeyHelper = signal.KeyHelper;
    const registrationId = KeyHelper.generateRegistrationId();
    const identity = await KeyHelper.generateIdentityKeyPair();
    store.put('identityKey', identity);
    store.put('registrationId', registrationId);

    const signedPreKey = await KeyHelper.generateSignedPreKey(identity, SIGNED_PREKEY_ID);
    await store.storeSignedPreKey(signedPreKey.keyId, signedPreKey.keyPair);

    const p = { id, store, registrationId, identity, signedPreKey, preKeys: [], nextPreKeyId: 1 };
    await refillPreKeys(p);
    return p;
  }

  async function refillPreKeys(p) {
    for (let i = 0; i < PREKEY_BATCH; i++) {
      const preKey = await signal.KeyHelper.generatePreKey(p.nextPreKeyId++);
      await p.store.storePreKey(preKey.keyId, preKey.keyPair);
      p.preKeys.push(preKey);
    }
  }

  // What a server would hand out: identity + signed prekey + one one-time prekey.
  async function takePreKeyBundle(p) {
    if (p.preKeys.length === 0) await refillPreKeys(p);
    const preKey = p.preKeys.shift(); // one-time: each bundle consumes one
    const bundle = {
      identityKey: p.identity.pubKey,
      registrationId: p.registrationId,
      signedPreKey: {
        keyId: p.signedPreKey.keyId,
        publicKey: p.signedPreKey.keyPair.pubKey,
        signature: p.signedPreKey.signature,
      },
    };
    bundle.preKey = { keyId: preKey.keyId, publicKey: preKey.keyPair.pubKey };
    return bundle;
  }

  const address = (id) => new signal.SignalProtocolAddress(id, DEVICE_ID);
  const logger = {
    info: (...a) => console.debug('[libsignal]', ...a),
    debug: () => {},
    warn: (...a) => console.warn('[libsignal]', ...a),
    error: (...a) => console.error('[libsignal]', ...a),
  };

  // MARK: API

  g.ZaloE2EE = {
    version: signal.VERSION,

    /** Idempotent. Resolves to the identity's registration id. */
    registerIdentity(id) {
      if (typeof id !== 'string' || !id) return Promise.reject(new TypeError('identity id must be a non-empty string'));
      if (parties.has(id)) return Promise.resolve(parties.get(id).registrationId);
      if (!pendingRegistrations.has(id)) {
        pendingRegistrations.set(id, createParty(id).then((p) => {
          parties.set(id, p);
          pendingRegistrations.delete(id);
          return p.registrationId;
        }, (err) => { pendingRegistrations.delete(id); throw err; }));
      }
      return pendingRegistrations.get(id);
    },

    hasSession(from, to) {
      return new signal.SessionCipher(party(from).store, address(to), { logger }).hasOpenSession();
    },

    /** -> { type: 3 (PreKeyWhisper) | 1 (Whisper), body: base64, registrationId } */
    async encrypt(plaintext, from, to) {
      const sender = party(from);
      const recipientAddress = address(to);
      const cipher = new signal.SessionCipher(sender.store, recipientAddress, { logger });
      if (!(await cipher.hasOpenSession())) {
        const builder = new signal.SessionBuilder(sender.store, recipientAddress, { logger });
        await builder.processPreKey(await takePreKeyBundle(party(to)));
      }
      const message = await cipher.encrypt(utf8.encode(String(plaintext)).buffer);
      return { type: message.type, body: btoa(message.body), registrationId: message.registrationId };
    },

    /** Resolves to the UTF-8 plaintext; rejects on bad MAC / unknown session. */
    async decrypt(envelope, at, from) {
      const recipient = party(at);
      const cipher = new signal.SessionCipher(recipient.store, address(from), { logger });
      const body = base64ToBuffer(envelope.body);
      let plaintext;
      if (envelope.type === 3) plaintext = await cipher.decryptPreKeyWhisperMessage(body);
      else if (envelope.type === 1) plaintext = await cipher.decryptWhisperMessage(body);
      else throw new Error('Unknown Signal message type ' + envelope.type);
      return utf8d.decode(plaintext);
    },

    /** 60-digit safety number, identical from both sides, grouped in 5s. */
    async safetyNumber(a, b) {
      const pa = party(a), pb = party(b);
      const generator = new signal.FingerprintGenerator(FINGERPRINT_ITERATIONS);
      const digits = await generator.createFor(a, pa.identity.pubKey, b, pb.identity.pubKey);
      return digits.match(/.{1,5}/g).join(' ');
    },
  };
})(globalThis);
