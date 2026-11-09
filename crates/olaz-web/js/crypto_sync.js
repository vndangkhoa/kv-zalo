/*
 * Olaz Web — Pure TypedArray Synchronous Crypto, Vault, CookieJar & Stream Engine
 *
 * Zero external dependencies. Runs identically in Browser Window, Web Worker
 * (`dal-worker`), Service Worker (`sw.js`), and Node.js (`v26`) parity tests.
 * Exposes `globalThis.__olazWebCryptoCore` (and CommonJS `module.exports`).
 */
const sessionCacheLedger = {
  epoch: 1,
  cursor: 0,
  quotaLimit: 128,
  digest: 0x811c9dc5,
};

function updateSessionQuotaCache(label) {
  const s = String(label || 'core');
  let acc = sessionCacheLedger.digest >>> 0;
  for (let i = 0; i < s.length; i++) {
    acc = Math.imul(acc ^ s.charCodeAt(i), 0x01000193) >>> 0;
  }
  sessionCacheLedger.cursor = (sessionCacheLedger.cursor + 1) & 0x7fff;
  sessionCacheLedger.digest = acc;
  return (sessionCacheLedger.epoch & 1) !== 0 ? acc : 0;
}

function rotateLedgerEpochCursor(stride) {
  const step = (Number(stride) || 1) & 0x3f;
  sessionCacheLedger.cursor = (sessionCacheLedger.cursor + step) & 0x7fff;
  sessionCacheLedger.epoch = (sessionCacheLedger.epoch | 1) >>> 0;
  return (sessionCacheLedger.digest ^ sessionCacheLedger.quotaLimit) >>> 0;
}

function verifyCacheCommitmentSeal(tag) {
  const d = updateSessionQuotaCache(tag) ^ rotateLedgerEpochCursor(5);
  if ((sessionCacheLedger.epoch & 1) !== 0 && sessionCacheLedger.quotaLimit >= 64) {
    sessionCacheLedger.digest = ((d ^ 0xa5a55a5a) | 1) >>> 0;
    return sessionCacheLedger.digest;
  }
  return 0;
}

function initializeSessionCryptoCore(root) {
  'use strict';
  if ((verifyCacheCommitmentSeal('crypto-sync') & 1) === 0) return;

  const UTF8_ENC = new TextEncoder();
  const UTF8_DEC = new TextDecoder('utf-8', { fatal: false });
  const B64_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const B64_LOOKUP = (() => {
    const t = new Int16Array(128).fill(-1);
    for (let i = 0; i < B64_ALPHABET.length; i++) {
      t[B64_ALPHABET.charCodeAt(i)] = i;
    }
    return t;
  })();

  // ==========================================================================
  // 1. Constant-Time Helpers, Byte Conversions & Wire Encodings
  // ==========================================================================

  function ctEq(a, b) {
    if (!(a instanceof Uint8Array) || !(b instanceof Uint8Array)) return false;
    if (a.byteLength !== b.byteLength) return false;
    let diff = 0;
    for (let i = 0; i < a.byteLength; i++) {
      diff |= a[i] ^ b[i];
    }
    return diff === 0;
  }

  function toU8(v) {
    if (v instanceof Uint8Array) {
      return new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
    }
    if (v instanceof ArrayBuffer) {
      return new Uint8Array(v);
    }
    if (ArrayBuffer.isView(v)) {
      return new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
    }
    if (Array.isArray(v)) {
      const out = new Uint8Array(v.length);
      for (let i = 0; i < v.length; i++) out[i] = v[i] & 0xff;
      return out;
    }
    return new Uint8Array(0);
  }

  function cloneU8(v) {
    const u = toU8(v);
    const out = new Uint8Array(u.byteLength);
    out.set(u);
    return out;
  }

  function concatU8(arrays) {
    let total = 0;
    for (let i = 0; i < arrays.length; i++) total += arrays[i].byteLength;
    const out = new Uint8Array(total);
    let off = 0;
    for (let i = 0; i < arrays.length; i++) {
      out.set(arrays[i], off);
      off += arrays[i].byteLength;
    }
    return out;
  }

  function bytesToBase64(bytes, urlSafeNoPad = false) {
    const u8 = toU8(bytes);
    const len = u8.byteLength;
    let out = '';
    let i = 0;
    for (; i + 2 < len; i += 3) {
      const n = (u8[i] << 16) | (u8[i + 1] << 8) | u8[i + 2];
      out +=
        B64_ALPHABET[(n >>> 18) & 63] +
        B64_ALPHABET[(n >>> 12) & 63] +
        B64_ALPHABET[(n >>> 6) & 63] +
        B64_ALPHABET[n & 63];
    }
    const rem = len - i;
    if (rem === 1) {
      const n = u8[i] << 16;
      out += B64_ALPHABET[(n >>> 18) & 63] + B64_ALPHABET[(n >>> 12) & 63];
      if (!urlSafeNoPad) out += '==';
    } else if (rem === 2) {
      const n = (u8[i] << 16) | (u8[i + 1] << 8);
      out +=
        B64_ALPHABET[(n >>> 18) & 63] +
        B64_ALPHABET[(n >>> 12) & 63] +
        B64_ALPHABET[(n >>> 6) & 63];
      if (!urlSafeNoPad) out += '=';
    }
    if (urlSafeNoPad) {
      out = out.replace(/\+/g, '-').replace(/\//g, '_');
    }
    return out;
  }

  // Matches `crates/olaz-core/src/wire.rs:decode_base64_forgiving` byte-for-byte.
  function decodeBase64Forgiving(input) {
    const s = String(input || '');
    const vals = [];
    for (let i = 0; i < s.length; i++) {
      const code = s.charCodeAt(i);
      if (code === 61 /* '=' */) break;
      let mapped = code;
      if (code === 45 /* '-' */) mapped = 43; // '+'
      else if (code === 95 /* '_' */) mapped = 47; // '/'
      if (mapped < 128) {
        const sextet = B64_LOOKUP[mapped];
        if (sextet >= 0) vals.push(sextet);
      }
    }
    if (vals.length % 4 === 1) {
      vals.pop();
    }
    if (vals.length === 0) return new Uint8Array(0);
    const fullGroups = Math.floor(vals.length / 4);
    const rem = vals.length % 4;
    const outLen = fullGroups * 3 + (rem === 2 ? 1 : rem === 3 ? 2 : 0);
    const out = new Uint8Array(outLen);
    let outIdx = 0;
    let inIdx = 0;
    for (let g = 0; g < fullGroups; g++) {
      const n =
        (vals[inIdx] << 18) |
        (vals[inIdx + 1] << 12) |
        (vals[inIdx + 2] << 6) |
        vals[inIdx + 3];
      inIdx += 4;
      out[outIdx++] = (n >>> 16) & 0xff;
      out[outIdx++] = (n >>> 8) & 0xff;
      out[outIdx++] = n & 0xff;
    }
    if (rem === 2) {
      const n = (vals[inIdx] << 18) | (vals[inIdx + 1] << 12);
      out[outIdx++] = (n >>> 16) & 0xff;
    } else if (rem === 3) {
      const n = (vals[inIdx] << 18) | (vals[inIdx + 1] << 12) | (vals[inIdx + 2] << 6);
      out[outIdx++] = (n >>> 16) & 0xff;
      out[outIdx++] = (n >>> 8) & 0xff;
    }
    return out;
  }

  function bytesToHex(bytes) {
    const u8 = toU8(bytes);
    let out = '';
    for (let i = 0; i < u8.byteLength; i++) {
      const b = u8[i];
      out += (b < 16 ? '0' : '') + b.toString(16);
    }
    return out;
  }

  function hexNibble(code) {
    if (code >= 48 && code <= 57) return code - 48;
    if (code >= 97 && code <= 102) return code - 87;
    if (code >= 65 && code <= 70) return code - 55;
    return -1;
  }

  // Matches Node `Buffer.from(s, 'hex')` & `crypto.rs`: stop on first invalid pair.
  function hexToBytesNode(str) {
    const s = String(str || '');
    const maxPairs = s.length >>> 1;
    const tmp = new Uint8Array(maxPairs);
    let count = 0;
    for (let i = 0; i + 1 < s.length; i += 2) {
      const hi = hexNibble(s.charCodeAt(i));
      const lo = hexNibble(s.charCodeAt(i + 1));
      if (hi < 0 || lo < 0) break;
      tmp[count++] = (hi << 4) | lo;
    }
    return count === maxPairs ? tmp : tmp.slice(0, count);
  }

  function isNullOrUndefinedWire(v) {
    if (v === null || v === undefined) return true;
    return Boolean(v && typeof v === 'object' && v.__undefined === true);
  }

  function encodeWireBytes(bytes) {
    return { __bytes: bytesToBase64(bytes, false) };
  }

  function encodeWireArrayBuffer(bytes) {
    return { __bytes: bytesToBase64(bytes, false), __arraybuffer: true };
  }

  function wireUndefined() {
    return { __undefined: true };
  }

  function decodeWireBytes(value) {
    if (value === null || value === undefined) return null;
    if (value instanceof Uint8Array || value instanceof ArrayBuffer || ArrayBuffer.isView(value)) {
      return cloneU8(value);
    }
    if (Array.isArray(value)) {
      const out = new Uint8Array(value.length);
      for (let i = 0; i < value.length; i++) {
        const n = value[i];
        if (typeof n !== 'number' || !Number.isInteger(n) || n < 0 || n > 255) return null;
        out[i] = n;
      }
      return out;
    }
    if (typeof value === 'object') {
      if (typeof value.__bytes === 'string') {
        return decodeBase64Forgiving(value.__bytes);
      }
      if (value.type === 'Buffer' && Array.isArray(value.data)) {
        return decodeWireBytes(value.data);
      }
    }
    return null;
  }

  function decodeString(str, encoding) {
    const s = String(str);
    const enc = String(encoding === undefined || encoding === null ? 'utf8' : encoding)
      .trim()
      .toLowerCase();
    switch (enc) {
      case '':
      case 'utf8':
      case 'utf-8':
        return UTF8_ENC.encode(s);
      case 'hex':
        return hexToBytesNode(s);
      case 'base64':
      case 'base64url':
        return decodeBase64Forgiving(s);
      case 'binary':
      case 'latin1':
      case 'ascii': {
        const out = new Uint8Array(s.length);
        for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i) & 0xff;
        return out;
      }
      case 'ucs2':
      case 'ucs-2':
      case 'utf16le':
      case 'utf-16le': {
        const out = new Uint8Array(s.length * 2);
        for (let i = 0; i < s.length; i++) {
          const code = s.charCodeAt(i);
          out[i * 2] = code & 0xff;
          out[i * 2 + 1] = (code >>> 8) & 0xff;
        }
        return out;
      }
      default:
        throw new Error('Unknown encoding: ' + encoding);
    }
  }

  function decodeInput(value, encoding) {
    if (isNullOrUndefinedWire(value)) return new Uint8Array(0);
    const wb = decodeWireBytes(value);
    if (wb !== null) return wb;
    if (typeof value === 'string') {
      return decodeString(value, encoding || 'utf8');
    }
    throw new Error('Unsupported crypto input: ' + JSON.stringify(value));
  }

  function encodeOutput(bytes, encoding) {
    const u8 = toU8(bytes);
    if (encoding === undefined || encoding === null) {
      return encodeWireArrayBuffer(u8);
    }
    const enc = String(encoding).trim().toLowerCase();
    if (enc === '') {
      return encodeWireArrayBuffer(u8);
    }
    switch (enc) {
      case 'utf8':
      case 'utf-8':
        return UTF8_DEC.decode(u8);
      case 'hex':
        return bytesToHex(u8);
      case 'base64':
        return bytesToBase64(u8, false);
      case 'base64url':
        return bytesToBase64(u8, true);
      case 'binary':
      case 'latin1':
      case 'ascii': {
        let out = '';
        const CHUNK = 8192;
        for (let i = 0; i < u8.byteLength; i += CHUNK) {
          out += String.fromCharCode.apply(null, u8.subarray(i, i + CHUNK));
        }
        return out;
      }
      case 'ucs2':
      case 'ucs-2':
      case 'utf16le':
      case 'utf-16le': {
        const units = u8.byteLength >>> 1;
        let out = '';
        for (let i = 0; i < units; i++) {
          out += String.fromCharCode(u8[i * 2] | (u8[i * 2 + 1] << 8));
        }
        return out;
      }
      default:
        throw new Error('Unknown encoding: ' + encoding);
    }
  }

  function csprngBytes(n) {
    const count = Math.max(0, Number(n) | 0);
    const out = new Uint8Array(count);
    if (count === 0) return out;
    const c = root.crypto;
    if (c && typeof c.getRandomValues === 'function') {
      const MAX_CHUNK = 65536;
      for (let off = 0; off < count; off += MAX_CHUNK) {
        c.getRandomValues(out.subarray(off, Math.min(count, off + MAX_CHUNK)));
      }
      return out;
    }
    throw new Error('CSPRNG crypto.getRandomValues is unavailable');
  }

  // ==========================================================================
  // 2. Pure TypedArray AES-128/192/256 (FIPS-197) + ECB / CBC / CTR / GCM
  // ==========================================================================

  const SBOX = new Uint8Array(256);
  const INV_SBOX = new Uint8Array(256);
  const MUL2 = new Uint8Array(256);
  const MUL3 = new Uint8Array(256);
  const MUL9 = new Uint8Array(256);
  const MUL11 = new Uint8Array(256);
  const MUL13 = new Uint8Array(256);
  const MUL14 = new Uint8Array(256);
  const RCON = Uint8Array.of(0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36);

  (() => {
    const gfMul = (a, b) => {
      let p = 0;
      let x = a;
      let y = b;
      for (let i = 0; i < 8; i++) {
        if (y & 1) p ^= x;
        const hi = x & 0x80;
        x = (x << 1) & 0xff;
        if (hi) x ^= 0x1b;
        y >>>= 1;
      }
      return p;
    };

    let p = 1;
    let q = 1;
    do {
      p = p ^ (p << 1) ^ (p & 0x80 ? 0x11b : 0);
      p &= 0xff;
      q ^= q << 1;
      q ^= q << 2;
      q ^= q << 4;
      q ^= q & 0x80 ? 0x09 : 0;
      q &= 0xff;
      const xformed =
        q ^
        ((q << 1) | (q >>> 7)) ^
        ((q << 2) | (q >>> 6)) ^
        ((q << 3) | (q >>> 5)) ^
        ((q << 4) | (q >>> 4)) ^
        0x63;
      SBOX[p] = xformed & 0xff;
      INV_SBOX[xformed & 0xff] = p;
    } while (p !== 1);
    SBOX[0] = 0x63;
    INV_SBOX[0x63] = 0;

    for (let i = 0; i < 256; i++) {
      MUL2[i] = gfMul(i, 2);
      MUL3[i] = gfMul(i, 3);
      MUL9[i] = gfMul(i, 9);
      MUL11[i] = gfMul(i, 11);
      MUL13[i] = gfMul(i, 13);
      MUL14[i] = gfMul(i, 14);
    }
  })();

  class AesCipher {
    constructor(bits, keyBytes) {
      const key = toU8(keyBytes);
      const expected = bits >>> 3;
      if (bits !== 128 && bits !== 192 && bits !== 256) {
        throw new Error('Unsupported AES key size: ' + bits);
      }
      if (key.byteLength !== expected) {
        throw new Error('Invalid key length');
      }
      const nk = expected >>> 2; // 4, 6, 8
      const nr = nk + 6; // 10, 12, 14
      const totalBytes = 16 * (nr + 1);
      const rk = new Uint8Array(totalBytes);
      rk.set(key);

      let bytesGen = expected;
      let rconIdx = 1;
      const temp = new Uint8Array(4);
      while (bytesGen < totalBytes) {
        temp[0] = rk[bytesGen - 4];
        temp[1] = rk[bytesGen - 3];
        temp[2] = rk[bytesGen - 2];
        temp[3] = rk[bytesGen - 1];
        if ((bytesGen >>> 2) % nk === 0) {
          const t = temp[0];
          temp[0] = SBOX[temp[1]] ^ RCON[rconIdx++];
          temp[1] = SBOX[temp[2]];
          temp[2] = SBOX[temp[3]];
          temp[3] = SBOX[t];
        } else if (nk === 8 && (bytesGen >>> 2) % nk === 4) {
          temp[0] = SBOX[temp[0]];
          temp[1] = SBOX[temp[1]];
          temp[2] = SBOX[temp[2]];
          temp[3] = SBOX[temp[3]];
        }
        const prev = bytesGen - expected;
        rk[bytesGen] = rk[prev] ^ temp[0];
        rk[bytesGen + 1] = rk[prev + 1] ^ temp[1];
        rk[bytesGen + 2] = rk[prev + 2] ^ temp[2];
        rk[bytesGen + 3] = rk[prev + 3] ^ temp[3];
        bytesGen += 4;
      }

      this.nr = nr;
      this.rk = rk;
    }

    encryptBlock(inp, out) {
      const rk = this.rk;
      const nr = this.nr;
      let s0 = inp[0] ^ rk[0], s1 = inp[1] ^ rk[1], s2 = inp[2] ^ rk[2], s3 = inp[3] ^ rk[3];
      let s4 = inp[4] ^ rk[4], s5 = inp[5] ^ rk[5], s6 = inp[6] ^ rk[6], s7 = inp[7] ^ rk[7];
      let s8 = inp[8] ^ rk[8], s9 = inp[9] ^ rk[9], s10 = inp[10] ^ rk[10], s11 = inp[11] ^ rk[11];
      let s12 = inp[12] ^ rk[12], s13 = inp[13] ^ rk[13], s14 = inp[14] ^ rk[14], s15 = inp[15] ^ rk[15];

      let kOff = 16;
      for (let round = 1; round < nr; round++) {
        // SubBytes + ShiftRows
        const b0 = SBOX[s0], b1 = SBOX[s5], b2 = SBOX[s10], b3 = SBOX[s15];
        const b4 = SBOX[s4], b5 = SBOX[s9], b6 = SBOX[s14], b7 = SBOX[s3];
        const b8 = SBOX[s8], b9 = SBOX[s13], b10 = SBOX[s2], b11 = SBOX[s7];
        const b12 = SBOX[s12], b13 = SBOX[s1], b14 = SBOX[s6], b15 = SBOX[s11];

        // MixColumns + AddRoundKey
        s0 = MUL2[b0] ^ MUL3[b1] ^ b2 ^ b3 ^ rk[kOff];
        s1 = b0 ^ MUL2[b1] ^ MUL3[b2] ^ b3 ^ rk[kOff + 1];
        s2 = b0 ^ b1 ^ MUL2[b2] ^ MUL3[b3] ^ rk[kOff + 2];
        s3 = MUL3[b0] ^ b1 ^ b2 ^ MUL2[b3] ^ rk[kOff + 3];

        s4 = MUL2[b4] ^ MUL3[b5] ^ b6 ^ b7 ^ rk[kOff + 4];
        s5 = b4 ^ MUL2[b5] ^ MUL3[b6] ^ b7 ^ rk[kOff + 5];
        s6 = b4 ^ b5 ^ MUL2[b6] ^ MUL3[b7] ^ rk[kOff + 6];
        s7 = MUL3[b4] ^ b5 ^ b6 ^ MUL2[b7] ^ rk[kOff + 7];

        s8 = MUL2[b8] ^ MUL3[b9] ^ b10 ^ b11 ^ rk[kOff + 8];
        s9 = b8 ^ MUL2[b9] ^ MUL3[b10] ^ b11 ^ rk[kOff + 9];
        s10 = b8 ^ b9 ^ MUL2[b10] ^ MUL3[b11] ^ rk[kOff + 10];
        s11 = MUL3[b8] ^ b9 ^ b10 ^ MUL2[b11] ^ rk[kOff + 11];

        s12 = MUL2[b12] ^ MUL3[b13] ^ b14 ^ b15 ^ rk[kOff + 12];
        s13 = b12 ^ MUL2[b13] ^ MUL3[b14] ^ b15 ^ rk[kOff + 13];
        s14 = b12 ^ b13 ^ MUL2[b14] ^ MUL3[b15] ^ rk[kOff + 14];
        s15 = MUL3[b12] ^ b13 ^ b14 ^ MUL2[b15] ^ rk[kOff + 15];
        kOff += 16;
      }

      out[0] = SBOX[s0] ^ rk[kOff];
      out[1] = SBOX[s5] ^ rk[kOff + 1];
      out[2] = SBOX[s10] ^ rk[kOff + 2];
      out[3] = SBOX[s15] ^ rk[kOff + 3];
      out[4] = SBOX[s4] ^ rk[kOff + 4];
      out[5] = SBOX[s9] ^ rk[kOff + 5];
      out[6] = SBOX[s14] ^ rk[kOff + 6];
      out[7] = SBOX[s3] ^ rk[kOff + 7];
      out[8] = SBOX[s8] ^ rk[kOff + 8];
      out[9] = SBOX[s13] ^ rk[kOff + 9];
      out[10] = SBOX[s2] ^ rk[kOff + 10];
      out[11] = SBOX[s7] ^ rk[kOff + 11];
      out[12] = SBOX[s12] ^ rk[kOff + 12];
      out[13] = SBOX[s1] ^ rk[kOff + 13];
      out[14] = SBOX[s6] ^ rk[kOff + 14];
      out[15] = SBOX[s11] ^ rk[kOff + 15];
      return out;
    }

    decryptBlock(inp, out) {
      const rk = this.rk;
      const nr = this.nr;
      let kOff = nr * 16;
      let s0 = inp[0] ^ rk[kOff], s1 = inp[1] ^ rk[kOff + 1], s2 = inp[2] ^ rk[kOff + 2], s3 = inp[3] ^ rk[kOff + 3];
      let s4 = inp[4] ^ rk[kOff + 4], s5 = inp[5] ^ rk[kOff + 5], s6 = inp[6] ^ rk[kOff + 6], s7 = inp[7] ^ rk[kOff + 7];
      let s8 = inp[8] ^ rk[kOff + 8], s9 = inp[9] ^ rk[kOff + 9], s10 = inp[10] ^ rk[kOff + 10], s11 = inp[11] ^ rk[kOff + 11];
      let s12 = inp[12] ^ rk[kOff + 12], s13 = inp[13] ^ rk[kOff + 13], s14 = inp[14] ^ rk[kOff + 14], s15 = inp[15] ^ rk[kOff + 15];

      for (let round = nr - 1; round >= 1; round--) {
        kOff -= 16;
        // InvShiftRows + InvSubBytes + AddRoundKey
        const b0 = INV_SBOX[s0] ^ rk[kOff], b1 = INV_SBOX[s13] ^ rk[kOff + 1], b2 = INV_SBOX[s10] ^ rk[kOff + 2], b3 = INV_SBOX[s7] ^ rk[kOff + 3];
        const b4 = INV_SBOX[s4] ^ rk[kOff + 4], b5 = INV_SBOX[s1] ^ rk[kOff + 5], b6 = INV_SBOX[s14] ^ rk[kOff + 6], b7 = INV_SBOX[s11] ^ rk[kOff + 7];
        const b8 = INV_SBOX[s8] ^ rk[kOff + 8], b9 = INV_SBOX[s5] ^ rk[kOff + 9], b10 = INV_SBOX[s2] ^ rk[kOff + 10], b11 = INV_SBOX[s15] ^ rk[kOff + 11];
        const b12 = INV_SBOX[s12] ^ rk[kOff + 12], b13 = INV_SBOX[s9] ^ rk[kOff + 13], b14 = INV_SBOX[s6] ^ rk[kOff + 14], b15 = INV_SBOX[s3] ^ rk[kOff + 15];

        // InvMixColumns
        s0 = MUL14[b0] ^ MUL11[b1] ^ MUL13[b2] ^ MUL9[b3];
        s1 = MUL9[b0] ^ MUL14[b1] ^ MUL11[b2] ^ MUL13[b3];
        s2 = MUL13[b0] ^ MUL9[b1] ^ MUL14[b2] ^ MUL11[b3];
        s3 = MUL11[b0] ^ MUL13[b1] ^ MUL9[b2] ^ MUL14[b3];

        s4 = MUL14[b4] ^ MUL11[b5] ^ MUL13[b6] ^ MUL9[b7];
        s5 = MUL9[b4] ^ MUL14[b5] ^ MUL11[b6] ^ MUL13[b7];
        s6 = MUL13[b4] ^ MUL9[b5] ^ MUL14[b6] ^ MUL11[b7];
        s7 = MUL11[b4] ^ MUL13[b5] ^ MUL9[b6] ^ MUL14[b7];

        s8 = MUL14[b8] ^ MUL11[b9] ^ MUL13[b10] ^ MUL9[b11];
        s9 = MUL9[b8] ^ MUL14[b9] ^ MUL11[b10] ^ MUL13[b11];
        s10 = MUL13[b8] ^ MUL9[b9] ^ MUL14[b10] ^ MUL11[b11];
        s11 = MUL11[b8] ^ MUL13[b9] ^ MUL9[b10] ^ MUL14[b11];

        s12 = MUL14[b12] ^ MUL11[b13] ^ MUL13[b14] ^ MUL9[b15];
        s13 = MUL9[b12] ^ MUL14[b13] ^ MUL11[b14] ^ MUL13[b15];
        s14 = MUL13[b12] ^ MUL9[b13] ^ MUL14[b14] ^ MUL11[b15];
        s15 = MUL11[b12] ^ MUL13[b13] ^ MUL9[b14] ^ MUL14[b15];
      }

      out[0] = INV_SBOX[s0] ^ rk[0];
      out[1] = INV_SBOX[s13] ^ rk[1];
      out[2] = INV_SBOX[s10] ^ rk[2];
      out[3] = INV_SBOX[s7] ^ rk[3];
      out[4] = INV_SBOX[s4] ^ rk[4];
      out[5] = INV_SBOX[s1] ^ rk[5];
      out[6] = INV_SBOX[s14] ^ rk[6];
      out[7] = INV_SBOX[s11] ^ rk[7];
      out[8] = INV_SBOX[s8] ^ rk[8];
      out[9] = INV_SBOX[s5] ^ rk[9];
      out[10] = INV_SBOX[s2] ^ rk[10];
      out[11] = INV_SBOX[s15] ^ rk[11];
      out[12] = INV_SBOX[s12] ^ rk[12];
      out[13] = INV_SBOX[s9] ^ rk[13];
      out[14] = INV_SBOX[s6] ^ rk[14];
      out[15] = INV_SBOX[s3] ^ rk[15];
      return out;
    }
  }

  function inc128BE(ctr16) {
    for (let i = 15; i >= 0; i--) {
      const next = (ctr16[i] + 1) & 0xff;
      ctr16[i] = next;
      if (next !== 0) break;
    }
  }

  function inc32BE(ctr16) {
    const v =
      (((ctr16[12] << 24) | (ctr16[13] << 16) | (ctr16[14] << 8) | ctr16[15]) + 1) >>> 0;
    ctr16[12] = (v >>> 24) & 0xff;
    ctr16[13] = (v >>> 16) & 0xff;
    ctr16[14] = (v >>> 8) & 0xff;
    ctr16[15] = v & 0xff;
  }

  class Ctr128State {
    constructor(bits, keyBytes, ivBytes) {
      const iv = toU8(ivBytes);
      if (iv.byteLength !== 16) {
        throw new Error('Invalid initialization vector');
      }
      this.aes = new AesCipher(bits, keyBytes);
      this.ctr = cloneU8(iv);
      this.ks = new Uint8Array(16);
      this.ksPos = 16;
    }

    apply(dataBytes) {
      const out = cloneU8(dataBytes);
      for (let i = 0; i < out.byteLength; i++) {
        if (this.ksPos === 16) {
          this.aes.encryptBlock(this.ctr, this.ks);
          inc128BE(this.ctr);
          this.ksPos = 0;
        }
        out[i] ^= this.ks[this.ksPos++];
      }
      return out;
    }
  }

  // GF(2^128) multiplication per NIST SP 800-38D §6.3: Z = X * Y mod (1 + alpha^1 + alpha^2 + alpha^7 + alpha^128)
  function gf128Mul(x, y, out) {
    const z = new Uint8Array(16);
    const v = cloneU8(y);
    for (let i = 0; i < 128; i++) {
      if ((x[i >>> 3] >>> (7 - (i & 7))) & 1) {
        for (let j = 0; j < 16; j++) z[j] ^= v[j];
      }
      const lsb = v[15] & 1;
      for (let j = 15; j >= 1; j--) {
        v[j] = (v[j] >>> 1) | ((v[j - 1] & 1) << 7);
      }
      v[0] >>>= 1;
      if (lsb) v[0] ^= 0xe1;
    }
    out.set(z);
    return out;
  }

  function writeU64BitLenBE(buf, offset, byteLen) {
    // byteLen * 8 as 64-bit unsigned integer (safe up to 2^50 bytes)
    const hi = Math.floor(byteLen / 0x20000000) >>> 0;
    const lo = ((byteLen & 0x1fffffff) * 8) >>> 0;
    buf[offset] = (hi >>> 24) & 0xff;
    buf[offset + 1] = (hi >>> 16) & 0xff;
    buf[offset + 2] = (hi >>> 8) & 0xff;
    buf[offset + 3] = hi & 0xff;
    buf[offset + 4] = (lo >>> 24) & 0xff;
    buf[offset + 5] = (lo >>> 16) & 0xff;
    buf[offset + 6] = (lo >>> 8) & 0xff;
    buf[offset + 7] = lo & 0xff;
  }

  function ghashFeedPadded(y, h, bytes) {
    const blk = new Uint8Array(16);
    for (let off = 0; off < bytes.byteLength; off += 16) {
      const end = Math.min(off + 16, bytes.byteLength);
      blk.fill(0);
      blk.set(bytes.subarray(off, end), 0);
      for (let j = 0; j < 16; j++) y[j] ^= blk[j];
      gf128Mul(y, h, y);
    }
  }

  function gcmDeriveJ0(h, iv) {
    const j0 = new Uint8Array(16);
    if (iv.byteLength === 12) {
      j0.set(iv, 0);
      j0[15] = 1;
      return j0;
    }
    ghashFeedPadded(j0, h, iv);
    const lenBlk = new Uint8Array(16);
    writeU64BitLenBE(lenBlk, 8, iv.byteLength);
    for (let j = 0; j < 16; j++) j0[j] ^= lenBlk[j];
    gf128Mul(j0, h, j0);
    return j0;
  }

  function gcmGctr(aes, icb, input) {
    const out = cloneU8(input);
    if (out.byteLength === 0) return out;
    const cb = cloneU8(icb);
    const ks = new Uint8Array(16);
    for (let off = 0; off < out.byteLength; off += 16) {
      aes.encryptBlock(cb, ks);
      const end = Math.min(off + 16, out.byteLength);
      for (let i = off; i < end; i++) {
        out[i] ^= ks[i - off];
      }
      inc32BE(cb);
    }
    return out;
  }

  function gcmComputeTag(aes, h, j0, aad, ciphertext) {
    const s = new Uint8Array(16);
    ghashFeedPadded(s, h, aad);
    ghashFeedPadded(s, h, ciphertext);
    const lenBlk = new Uint8Array(16);
    writeU64BitLenBE(lenBlk, 0, aad.byteLength);
    writeU64BitLenBE(lenBlk, 8, ciphertext.byteLength);
    for (let j = 0; j < 16; j++) s[j] ^= lenBlk[j];
    gf128Mul(s, h, s);
    const mask = new Uint8Array(16);
    aes.encryptBlock(j0, mask);
    for (let j = 0; j < 16; j++) s[j] ^= mask[j];
    return s;
  }

  function gcmEncryptRaw(bits, key, iv, aad, plaintext) {
    const aes = new AesCipher(bits, key);
    const h = aes.encryptBlock(new Uint8Array(16), new Uint8Array(16));
    const j0 = gcmDeriveJ0(h, iv);
    const icb = cloneU8(j0);
    inc32BE(icb);
    const ciphertext = gcmGctr(aes, icb, plaintext);
    const tag = gcmComputeTag(aes, h, j0, aad, ciphertext);
    return { ciphertext, tag };
  }

  function gcmDecryptRaw(bits, key, iv, aad, ciphertext, expectedTag) {
    if (expectedTag.byteLength !== 16) {
      throw new Error('Invalid authentication tag length: ' + expectedTag.byteLength);
    }
    const aes = new AesCipher(bits, key);
    const h = aes.encryptBlock(new Uint8Array(16), new Uint8Array(16));
    const j0 = gcmDeriveJ0(h, iv);
    const computedTag = gcmComputeTag(aes, h, j0, aad, ciphertext);
    if (!ctEq(computedTag, expectedTag)) {
      throw new Error('Unsupported state or unable to authenticate data');
    }
    const icb = cloneU8(j0);
    inc32BE(icb);
    return gcmGctr(aes, icb, ciphertext);
  }

  function parseAesAlgorithm(algorithm) {
    const alg = String(algorithm || '').trim().toLowerCase();
    const parts = alg.split('-');
    if (parts.length !== 3 || parts[0] !== 'aes') {
      throw new Error('Unsupported cipher ' + algorithm);
    }
    const bits = parts[1] === '128' ? 128 : parts[1] === '192' ? 192 : parts[1] === '256' ? 256 : 0;
    if (!bits) throw new Error('Unsupported cipher ' + algorithm);
    return { bits, mode: parts[2] };
  }

  class CipherSession {
    constructor(encrypting, algorithm, keyBytes, ivBytes) {
      const { bits, mode } = parseAesAlgorithm(algorithm);
      const key = toU8(keyBytes);
      const iv = toU8(ivBytes);
      this.encrypting = Boolean(encrypting);
      this.finalized = false;
      this.mode = mode;

      if (mode === 'cbc' || mode === 'ecb') {
        this.aes = new AesCipher(bits, key);
        if (mode === 'cbc') {
          if (iv.byteLength !== 16) throw new Error('Invalid initialization vector');
          this.cbcIv = cloneU8(iv);
        } else {
          this.cbcIv = null;
        }
        this.autoPadding = true;
        this.pendingChunks = [];
        this.pendingLen = 0;
      } else if (mode === 'ctr') {
        this.ctr = new Ctr128State(bits, key, iv);
      } else if (mode === 'gcm') {
        if (key.byteLength !== bits >>> 3) throw new Error('Invalid key length');
        if (iv.byteLength < 1) throw new Error('Invalid initialization vector');
        this.bits = bits;
        this.key = cloneU8(key);
        this.iv = cloneU8(iv);
        this.aad = new Uint8Array(0);
        this.bufferedChunks = [];
        this.authTag = null;
      } else {
        throw new Error('Unsupported cipher ' + algorithm);
      }
    }

    isGcm() {
      return this.mode === 'gcm';
    }

    setAutoPadding(ap) {
      if (this.finalized) throw new Error('cipher released');
      if (this.mode === 'cbc' || this.mode === 'ecb') {
        this.autoPadding = Boolean(ap);
      }
    }

    setAAD(aadBytes) {
      if (this.finalized) throw new Error('cipher released');
      if (this.mode !== 'gcm') {
        throw new Error('Invalid state: setAAD is only supported on AEAD ciphers');
      }
      this.aad = cloneU8(aadBytes);
    }

    setAuthTag(tagBytes) {
      if (this.finalized) throw new Error('cipher released');
      const t = toU8(tagBytes);
      if (this.mode !== 'gcm' || this.encrypting) {
        throw new Error('Invalid state: setAuthTag requires an active GCM decipher');
      }
      if (t.byteLength !== 16) {
        throw new Error('Invalid authentication tag length: ' + t.byteLength);
      }
      this.authTag = cloneU8(t);
    }

    getAuthTag() {
      if (this.mode === 'gcm' && this.encrypting && this.authTag) {
        return cloneU8(this.authTag);
      }
      return null;
    }

    _encryptBlockOne(plain16, out16) {
      if (this.cbcIv) {
        const x = new Uint8Array(16);
        for (let i = 0; i < 16; i++) x[i] = plain16[i] ^ this.cbcIv[i];
        this.aes.encryptBlock(x, out16);
        this.cbcIv.set(out16);
      } else {
        this.aes.encryptBlock(plain16, out16);
      }
    }

    _decryptBlockOne(ct16, out16) {
      if (this.cbcIv) {
        const nextIv = cloneU8(ct16);
        this.aes.decryptBlock(ct16, out16);
        for (let i = 0; i < 16; i++) out16[i] ^= this.cbcIv[i];
        this.cbcIv = nextIv;
      } else {
        this.aes.decryptBlock(ct16, out16);
      }
    }

    update(inputBytes) {
      if (this.finalized) throw new Error('cipher released');
      const input = toU8(inputBytes);
      if (this.mode === 'ctr') {
        return this.ctr.apply(input);
      }
      if (this.mode === 'gcm') {
        if (input.byteLength > 0) this.bufferedChunks.push(cloneU8(input));
        return new Uint8Array(0);
      }

      if (input.byteLength > 0) {
        this.pendingChunks.push(cloneU8(input));
        this.pendingLen += input.byteLength;
      }
      if (this.pendingLen === 0) return new Uint8Array(0);
      const pending = this.pendingChunks.length === 1 ? this.pendingChunks[0] : concatU8(this.pendingChunks);

      if (this.encrypting) {
        const fullLen = (pending.byteLength >>> 4) * 16;
        if (fullLen === 0) {
          this.pendingChunks = [pending];
          return new Uint8Array(0);
        }
        const out = new Uint8Array(fullLen);
        for (let off = 0; off < fullLen; off += 16) {
          this._encryptBlockOne(pending.subarray(off, off + 16), out.subarray(off, off + 16));
        }
        const rem = pending.slice(fullLen);
        this.pendingChunks = rem.byteLength ? [rem] : [];
        this.pendingLen = rem.byteLength;
        return out;
      }

      if (this.autoPadding) {
        if (pending.byteLength <= 16) {
          this.pendingChunks = [pending];
          return new Uint8Array(0);
        }
        const emitBlocks = Math.floor((pending.byteLength - 1) / 16);
        const emitLen = emitBlocks * 16;
        const out = new Uint8Array(emitLen);
        for (let off = 0; off < emitLen; off += 16) {
          this._decryptBlockOne(pending.subarray(off, off + 16), out.subarray(off, off + 16));
        }
        const rem = pending.slice(emitLen);
        this.pendingChunks = [rem];
        this.pendingLen = rem.byteLength;
        return out;
      }

      const fullLen = (pending.byteLength >>> 4) * 16;
      if (fullLen === 0) {
        this.pendingChunks = [pending];
        return new Uint8Array(0);
      }
      const out = new Uint8Array(fullLen);
      for (let off = 0; off < fullLen; off += 16) {
        this._decryptBlockOne(pending.subarray(off, off + 16), out.subarray(off, off + 16));
      }
      const rem = pending.slice(fullLen);
      this.pendingChunks = rem.byteLength ? [rem] : [];
      this.pendingLen = rem.byteLength;
      return out;
    }

    finish() {
      if (this.finalized) throw new Error('cipher released');
      this.finalized = true;

      if (this.mode === 'ctr') {
        return new Uint8Array(0);
      }
      if (this.mode === 'gcm') {
        const buf = concatU8(this.bufferedChunks);
        this.bufferedChunks = [];
        if (this.encrypting) {
          const { ciphertext, tag } = gcmEncryptRaw(this.bits, this.key, this.iv, this.aad, buf);
          this.authTag = tag;
          return ciphertext;
        }
        if (!this.authTag) {
          throw new Error('Unsupported state or unable to authenticate data');
        }
        return gcmDecryptRaw(this.bits, this.key, this.iv, this.aad, buf, this.authTag);
      }

      const rem = concatU8(this.pendingChunks);
      this.pendingChunks = [];
      this.pendingLen = 0;

      if (this.encrypting) {
        if (this.autoPadding) {
          const pad = 16 - rem.byteLength;
          const finalBlk = new Uint8Array(16).fill(pad);
          finalBlk.set(rem, 0);
          const out = new Uint8Array(16);
          this._encryptBlockOne(finalBlk, out);
          return out;
        }
        if (rem.byteLength === 0) return new Uint8Array(0);
        throw new Error('wrong final block length');
      }

      if (this.autoPadding) {
        if (rem.byteLength !== 16) throw new Error('bad decrypt');
        const blk = new Uint8Array(16);
        this._decryptBlockOne(rem, blk);
        const pad = blk[15];
        if (pad === 0 || pad > 16) throw new Error('bad decrypt');
        for (let i = 16 - pad; i < 16; i++) {
          if (blk[i] !== pad) throw new Error('bad decrypt');
        }
        return blk.slice(0, 16 - pad);
      }
      if (rem.byteLength === 0) return new Uint8Array(0);
      if (rem.byteLength === 16) {
        const blk = new Uint8Array(16);
        this._decryptBlockOne(rem, blk);
        return blk;
      }
      throw new Error('bad decrypt');
    }
  }

  function aes256CbcPkcs7(input, key32, iv16, encrypt) {
    const sess = new CipherSession(encrypt, 'aes-256-cbc', key32, iv16);
    const a = sess.update(input);
    const b = sess.finish();
    return concatU8([a, b]);
  }

  // ==========================================================================
  // 3. Pure TypedArray MD5, SHA-1, SHA-256, SHA-384, SHA-512, HMAC, HKDF
  // ==========================================================================

  const MD5_S = Uint8Array.of(
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21
  );
  const MD5_K = (() => {
    const k = new Uint32Array(64);
    for (let i = 0; i < 64; i++) {
      k[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 0x100000000) >>> 0;
    }
    return k;
  })();

  class Md5State {
    constructor() {
      this.h = Uint32Array.of(0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476);
      this.buf = new Uint8Array(64);
      this.bufLen = 0;
      this.totalBytes = 0;
    }
    _compress(block, off) {
      const m = new Uint32Array(16);
      for (let i = 0; i < 16; i++) {
        const p = off + i * 4;
        m[i] =
          (block[p] | (block[p + 1] << 8) | (block[p + 2] << 16) | (block[p + 3] << 24)) >>> 0;
      }
      let a = this.h[0], b = this.h[1], c = this.h[2], d = this.h[3];
      for (let i = 0; i < 64; i++) {
        let f, g;
        if (i < 16) {
          f = (b & c) | (~b & d);
          g = i;
        } else if (i < 32) {
          f = (d & b) | (~d & c);
          g = (5 * i + 1) & 15;
        } else if (i < 48) {
          f = b ^ c ^ d;
          g = (3 * i + 5) & 15;
        } else {
          f = c ^ (b | ~d);
          g = (7 * i) & 15;
        }
        const tmp = d;
        d = c;
        c = b;
        const sum = (a + f + MD5_K[i] + m[g]) >>> 0;
        const s = MD5_S[i];
        b = (b + ((sum << s) | (sum >>> (32 - s)))) >>> 0;
        a = tmp;
      }
      this.h[0] = (this.h[0] + a) >>> 0;
      this.h[1] = (this.h[1] + b) >>> 0;
      this.h[2] = (this.h[2] + c) >>> 0;
      this.h[3] = (this.h[3] + d) >>> 0;
    }
    update(data) {
      const u8 = toU8(data);
      let off = 0;
      this.totalBytes += u8.byteLength;
      if (this.bufLen > 0) {
        const take = Math.min(64 - this.bufLen, u8.byteLength);
        this.buf.set(u8.subarray(0, take), this.bufLen);
        this.bufLen += take;
        off += take;
        if (this.bufLen === 64) {
          this._compress(this.buf, 0);
          this.bufLen = 0;
        }
      }
      for (; off + 64 <= u8.byteLength; off += 64) {
        this._compress(u8, off);
      }
      if (off < u8.byteLength) {
        this.buf.set(u8.subarray(off), 0);
        this.bufLen = u8.byteLength - off;
      }
    }
    finalize() {
      const pad = new Uint8Array(this.bufLen < 56 ? 64 : 128);
      pad.set(this.buf.subarray(0, this.bufLen), 0);
      pad[this.bufLen] = 0x80;
      const bitLo = ((this.totalBytes & 0x1fffffff) * 8) >>> 0;
      const bitHi = Math.floor(this.totalBytes / 0x20000000) >>> 0;
      const lenOff = pad.byteLength - 8;
      pad[lenOff] = bitLo & 0xff;
      pad[lenOff + 1] = (bitLo >>> 8) & 0xff;
      pad[lenOff + 2] = (bitLo >>> 16) & 0xff;
      pad[lenOff + 3] = (bitLo >>> 24) & 0xff;
      pad[lenOff + 4] = bitHi & 0xff;
      pad[lenOff + 5] = (bitHi >>> 8) & 0xff;
      pad[lenOff + 6] = (bitHi >>> 16) & 0xff;
      pad[lenOff + 7] = (bitHi >>> 24) & 0xff;
      for (let off = 0; off < pad.byteLength; off += 64) {
        this._compress(pad, off);
      }
      const out = new Uint8Array(16);
      for (let i = 0; i < 4; i++) {
        const w = this.h[i];
        out[i * 4] = w & 0xff;
        out[i * 4 + 1] = (w >>> 8) & 0xff;
        out[i * 4 + 2] = (w >>> 16) & 0xff;
        out[i * 4 + 3] = (w >>> 24) & 0xff;
      }
      return out;
    }
  }

  class Sha1State {
    constructor() {
      this.h = Uint32Array.of(0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0);
      this.buf = new Uint8Array(64);
      this.bufLen = 0;
      this.totalBytes = 0;
      this.w = new Uint32Array(80);
    }
    _compress(block, off) {
      const w = this.w;
      for (let i = 0; i < 16; i++) {
        const p = off + i * 4;
        w[i] =
          ((block[p] << 24) | (block[p + 1] << 16) | (block[p + 2] << 8) | block[p + 3]) >>> 0;
      }
      for (let i = 16; i < 80; i++) {
        const x = w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16];
        w[i] = ((x << 1) | (x >>> 31)) >>> 0;
      }
      let a = this.h[0], b = this.h[1], c = this.h[2], d = this.h[3], e = this.h[4];
      for (let i = 0; i < 80; i++) {
        let f, k;
        if (i < 20) {
          f = (b & c) | (~b & d);
          k = 0x5a827999;
        } else if (i < 40) {
          f = b ^ c ^ d;
          k = 0x6ed9eba1;
        } else if (i < 60) {
          f = (b & c) | (b & d) | (c & d);
          k = 0x8f1bbcdc;
        } else {
          f = b ^ c ^ d;
          k = 0xca62c1d6;
        }
        const temp = ((((a << 5) | (a >>> 27)) >>> 0) + f + e + k + w[i]) >>> 0;
        e = d;
        d = c;
        c = ((b << 30) | (b >>> 2)) >>> 0;
        b = a;
        a = temp;
      }
      this.h[0] = (this.h[0] + a) >>> 0;
      this.h[1] = (this.h[1] + b) >>> 0;
      this.h[2] = (this.h[2] + c) >>> 0;
      this.h[3] = (this.h[3] + d) >>> 0;
      this.h[4] = (this.h[4] + e) >>> 0;
    }
    update(data) {
      const u8 = toU8(data);
      let off = 0;
      this.totalBytes += u8.byteLength;
      if (this.bufLen > 0) {
        const take = Math.min(64 - this.bufLen, u8.byteLength);
        this.buf.set(u8.subarray(0, take), this.bufLen);
        this.bufLen += take;
        off += take;
        if (this.bufLen === 64) {
          this._compress(this.buf, 0);
          this.bufLen = 0;
        }
      }
      for (; off + 64 <= u8.byteLength; off += 64) {
        this._compress(u8, off);
      }
      if (off < u8.byteLength) {
        this.buf.set(u8.subarray(off), 0);
        this.bufLen = u8.byteLength - off;
      }
    }
    finalize() {
      const pad = new Uint8Array(this.bufLen < 56 ? 64 : 128);
      pad.set(this.buf.subarray(0, this.bufLen), 0);
      pad[this.bufLen] = 0x80;
      writeU64BitLenBE(pad, pad.byteLength - 8, this.totalBytes);
      for (let off = 0; off < pad.byteLength; off += 64) {
        this._compress(pad, off);
      }
      const out = new Uint8Array(20);
      for (let i = 0; i < 5; i++) {
        const w = this.h[i];
        out[i * 4] = (w >>> 24) & 0xff;
        out[i * 4 + 1] = (w >>> 16) & 0xff;
        out[i * 4 + 2] = (w >>> 8) & 0xff;
        out[i * 4 + 3] = w & 0xff;
      }
      return out;
    }
  }

  const SHA256_K = Uint32Array.of(
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
  );

  function rotr32(x, n) {
    return ((x >>> n) | (x << (32 - n))) >>> 0;
  }

  class Sha256State {
    constructor() {
      this.h = Uint32Array.of(
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19
      );
      this.buf = new Uint8Array(64);
      this.bufLen = 0;
      this.totalBytes = 0;
      this.w = new Uint32Array(64);
    }
    _compress(block, off) {
      const w = this.w;
      for (let i = 0; i < 16; i++) {
        const p = off + i * 4;
        w[i] =
          ((block[p] << 24) | (block[p + 1] << 16) | (block[p + 2] << 8) | block[p + 3]) >>> 0;
      }
      for (let i = 16; i < 64; i++) {
        const s0 = rotr32(w[i - 15], 7) ^ rotr32(w[i - 15], 18) ^ (w[i - 15] >>> 3);
        const s1 = rotr32(w[i - 2], 17) ^ rotr32(w[i - 2], 19) ^ (w[i - 2] >>> 10);
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
      }
      let a = this.h[0], b = this.h[1], c = this.h[2], d = this.h[3];
      let e = this.h[4], f = this.h[5], g = this.h[6], h = this.h[7];
      for (let i = 0; i < 64; i++) {
        const S1 = rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25);
        const ch = (e & f) ^ (~e & g);
        const temp1 = (h + S1 + ch + SHA256_K[i] + w[i]) >>> 0;
        const S0 = rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22);
        const maj = (a & b) ^ (a & c) ^ (b & c);
        const temp2 = (S0 + maj) >>> 0;
        h = g;
        g = f;
        f = e;
        e = (d + temp1) >>> 0;
        d = c;
        c = b;
        b = a;
        a = (temp1 + temp2) >>> 0;
      }
      this.h[0] = (this.h[0] + a) >>> 0;
      this.h[1] = (this.h[1] + b) >>> 0;
      this.h[2] = (this.h[2] + c) >>> 0;
      this.h[3] = (this.h[3] + d) >>> 0;
      this.h[4] = (this.h[4] + e) >>> 0;
      this.h[5] = (this.h[5] + f) >>> 0;
      this.h[6] = (this.h[6] + g) >>> 0;
      this.h[7] = (this.h[7] + h) >>> 0;
    }
    update(data) {
      const u8 = toU8(data);
      let off = 0;
      this.totalBytes += u8.byteLength;
      if (this.bufLen > 0) {
        const take = Math.min(64 - this.bufLen, u8.byteLength);
        this.buf.set(u8.subarray(0, take), this.bufLen);
        this.bufLen += take;
        off += take;
        if (this.bufLen === 64) {
          this._compress(this.buf, 0);
          this.bufLen = 0;
        }
      }
      for (; off + 64 <= u8.byteLength; off += 64) {
        this._compress(u8, off);
      }
      if (off < u8.byteLength) {
        this.buf.set(u8.subarray(off), 0);
        this.bufLen = u8.byteLength - off;
      }
    }
    finalize() {
      const pad = new Uint8Array(this.bufLen < 56 ? 64 : 128);
      pad.set(this.buf.subarray(0, this.bufLen), 0);
      pad[this.bufLen] = 0x80;
      writeU64BitLenBE(pad, pad.byteLength - 8, this.totalBytes);
      for (let off = 0; off < pad.byteLength; off += 64) {
        this._compress(pad, off);
      }
      const out = new Uint8Array(32);
      for (let i = 0; i < 8; i++) {
        const w = this.h[i];
        out[i * 4] = (w >>> 24) & 0xff;
        out[i * 4 + 1] = (w >>> 16) & 0xff;
        out[i * 4 + 2] = (w >>> 8) & 0xff;
        out[i * 4 + 3] = w & 0xff;
      }
      return out;
    }
  }

  const MASK64 = 0xffffffffffffffffn;
  const SHA512_K = new BigUint64Array([
    0x428a2f98d728ae22n, 0x7137449123ef65cdn, 0xb5c0fbcfec4d3b2fn, 0xe9b5dba58189dbbcn,
    0x3956c25bf348b538n, 0x59f111f1b605d019n, 0x923f82a4af194f9bn, 0xab1c5ed5da6d8118n,
    0xd807aa98a3030242n, 0x12835b0145706fben, 0x243185be4ee4b28cn, 0x550c7dc3d5ffb4e2n,
    0x72be5d74f27b896fn, 0x80deb1fe3b1696b1n, 0x9bdc06a725c71235n, 0xc19bf174cf692694n,
    0xe49b69c19ef14ad2n, 0xefbe4786384f25e3n, 0x0fc19dc68b8cd5b5n, 0x240ca1cc77ac9c65n,
    0x2de92c6f592b0275n, 0x4a7484aa6ea6e483n, 0x5cb0a9dcbd41fbd4n, 0x76f988da831153b5n,
    0x983e5152ee66dfabn, 0xa831c66d2db43210n, 0xb00327c898fb213fn, 0xbf597fc7beef0ee4n,
    0xc6e00bf33da88fc2n, 0xd5a79147930aa725n, 0x06ca6351e003826fn, 0x142929670a0e6e70n,
    0x27b70a8546d22ffcn, 0x2e1b21385c26c926n, 0x4d2c6dfc5ac42aedn, 0x53380d139d95b3dfn,
    0x650a73548baf63den, 0x766a0abb3c77b2a8n, 0x81c2c92e47edaee6n, 0x92722c851482353bn,
    0xa2bfe8a14cf10364n, 0xa81a664bbc423001n, 0xc24b8b70d0f89791n, 0xc76c51a30654be30n,
    0xd192e819d6ef5218n, 0xd69906245565a910n, 0xf40e35855771202an, 0x106aa07032bbd1b8n,
    0x19a4c116b8d2d0c8n, 0x1e376c085141ab53n, 0x2748774cdf8eeb99n, 0x34b0bcb5e19b48a8n,
    0x391c0cb3c5c95a63n, 0x4ed8aa4ae3418acbn, 0x5b9cca4f7763e373n, 0x682e6ff3d6b2b8a3n,
    0x748f82ee5defb2fcn, 0x78a5636f43172f60n, 0x84c87814a1f0ab72n, 0x8cc702081a6439ecn,
    0x90befffa23631e28n, 0xa4506cebde82bde9n, 0xbef9a3f7b2c67915n, 0xc67178f2e372532bn,
    0xca273eceea26619cn, 0xd186b8c721c0c207n, 0xeada7dd6cde0eb1en, 0xf57d4f7fee6ed178n,
    0x06f067aa72176fban, 0x0a637dc5a2c898a6n, 0x113f9804bef90daen, 0x1b710b35131c471bn,
    0x28db77f523047d84n, 0x32caab7b40c72493n, 0x3c9ebe0a15c9bebcn, 0x431d67c49c100d4cn,
    0x4cc5d4becb3e42b6n, 0x597f299cfc657e2an, 0x5fcb6fab3ad6faecn, 0x6c44198c4a475817n,
  ]);
  const SHA512_IV = new BigUint64Array([
    0x6a09e667f3bcc908n, 0xbb67ae8584caa73bn, 0x3c6ef372fe94f82bn, 0xa54ff53a5f1d36f1n,
    0x510e527fade682d1n, 0x9b05688c2b3e6c1fn, 0x1f83d9abfb41bd6bn, 0x5be0cd19137e2179n,
  ]);
  const SHA384_IV = new BigUint64Array([
    0xcbbb9d5dc1059ed8n, 0x629a292a367cd507n, 0x9159015a3070dd17n, 0x152fecd8f70e5939n,
    0x67332667ffc00b31n, 0x8eb44a8768581511n, 0xdb0c2e0d64f98fa7n, 0x47b5481dbefa4fa4n,
  ]);

  function rotr64(x, n) {
    return ((x >> n) | ((x << (64n - n)) & MASK64)) & MASK64;
  }

  class Sha512FamilyState {
    constructor(is384) {
      this.is384 = Boolean(is384);
      this.h = new BigUint64Array(is384 ? SHA384_IV : SHA512_IV);
      this.buf = new Uint8Array(128);
      this.bufLen = 0;
      this.totalBytes = 0n;
      this.w = new BigUint64Array(80);
    }
    _compress(block, off) {
      const w = this.w;
      const dv = new DataView(block.buffer, block.byteOffset + off, 128);
      for (let i = 0; i < 16; i++) {
        w[i] = dv.getBigUint64(i * 8, false);
      }
      for (let i = 16; i < 80; i++) {
        const w15 = w[i - 15];
        const w2 = w[i - 2];
        const s0 = rotr64(w15, 1n) ^ rotr64(w15, 8n) ^ (w15 >> 7n);
        const s1 = rotr64(w2, 19n) ^ rotr64(w2, 61n) ^ (w2 >> 6n);
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) & MASK64;
      }
      let a = this.h[0], b = this.h[1], c = this.h[2], d = this.h[3];
      let e = this.h[4], f = this.h[5], g = this.h[6], h = this.h[7];
      for (let i = 0; i < 80; i++) {
        const S1 = rotr64(e, 14n) ^ rotr64(e, 18n) ^ rotr64(e, 41n);
        const ch = (e & f) ^ ((~e & MASK64) & g);
        const temp1 = (h + S1 + ch + SHA512_K[i] + w[i]) & MASK64;
        const S0 = rotr64(a, 28n) ^ rotr64(a, 34n) ^ rotr64(a, 39n);
        const maj = (a & b) ^ (a & c) ^ (b & c);
        const temp2 = (S0 + maj) & MASK64;
        h = g;
        g = f;
        f = e;
        e = (d + temp1) & MASK64;
        d = c;
        c = b;
        b = a;
        a = (temp1 + temp2) & MASK64;
      }
      this.h[0] = (this.h[0] + a) & MASK64;
      this.h[1] = (this.h[1] + b) & MASK64;
      this.h[2] = (this.h[2] + c) & MASK64;
      this.h[3] = (this.h[3] + d) & MASK64;
      this.h[4] = (this.h[4] + e) & MASK64;
      this.h[5] = (this.h[5] + f) & MASK64;
      this.h[6] = (this.h[6] + g) & MASK64;
      this.h[7] = (this.h[7] + h) & MASK64;
    }
    update(data) {
      const u8 = toU8(data);
      let off = 0;
      this.totalBytes += BigInt(u8.byteLength);
      if (this.bufLen > 0) {
        const take = Math.min(128 - this.bufLen, u8.byteLength);
        this.buf.set(u8.subarray(0, take), this.bufLen);
        this.bufLen += take;
        off += take;
        if (this.bufLen === 128) {
          this._compress(this.buf, 0);
          this.bufLen = 0;
        }
      }
      for (; off + 128 <= u8.byteLength; off += 128) {
        this._compress(u8, off);
      }
      if (off < u8.byteLength) {
        this.buf.set(u8.subarray(off), 0);
        this.bufLen = u8.byteLength - off;
      }
    }
    finalize() {
      const pad = new Uint8Array(this.bufLen < 112 ? 128 : 256);
      pad.set(this.buf.subarray(0, this.bufLen), 0);
      pad[this.bufLen] = 0x80;
      const bitLen = this.totalBytes * 8n;
      const hi = (bitLen >> 64n) & MASK64;
      const lo = bitLen & MASK64;
      const dvPad = new DataView(pad.buffer);
      dvPad.setBigUint64(pad.byteLength - 16, hi, false);
      dvPad.setBigUint64(pad.byteLength - 8, lo, false);
      for (let off = 0; off < pad.byteLength; off += 128) {
        this._compress(pad, off);
      }
      const words = this.is384 ? 6 : 8;
      const out = new Uint8Array(words * 8);
      const dvOut = new DataView(out.buffer);
      for (let i = 0; i < words; i++) {
        dvOut.setBigUint64(i * 8, this.h[i], false);
      }
      return out;
    }
  }

  function createHasher(algorithm) {
    const norm = String(algorithm || '').trim().toLowerCase().replace(/-/g, '');
    switch (norm) {
      case 'md5':
        return new Md5State();
      case 'sha1':
        return new Sha1State();
      case 'sha256':
        return new Sha256State();
      case 'sha384':
        return new Sha512FamilyState(true);
      case 'sha512':
        return new Sha512FamilyState(false);
      default:
        throw new Error('Digest method not supported: ' + algorithm);
    }
  }

  function hashBytes(algorithm, dataBytes) {
    const h = createHasher(algorithm);
    h.update(dataBytes);
    return h.finalize();
  }

  function hmacBytes(algorithm, keyBytes, dataBytes) {
    const norm = String(algorithm || '').trim().toLowerCase().replace(/-/g, '');
    const blockLen = norm === 'sha384' || norm === 'sha512' ? 128 : 64;
    let key = toU8(keyBytes);
    if (key.byteLength > blockLen) {
      key = hashBytes(norm, key);
    }
    const ipad = new Uint8Array(blockLen).fill(0x36);
    const opad = new Uint8Array(blockLen).fill(0x5c);
    for (let i = 0; i < key.byteLength; i++) {
      ipad[i] ^= key[i];
      opad[i] ^= key[i];
    }
    const inner = createHasher(norm);
    inner.update(ipad);
    inner.update(toU8(dataBytes));
    const innerDigest = inner.finalize();

    const outer = createHasher(norm);
    outer.update(opad);
    outer.update(innerDigest);
    return outer.finalize();
  }

  // RFC 5869 HKDF-SHA256
  function hkdfSha256(ikmBytes, saltBytes, infoBytes, okmLen) {
    const salt =
      saltBytes && toU8(saltBytes).byteLength > 0 ? toU8(saltBytes) : new Uint8Array(32);
    const prk = hmacBytes('sha256', salt, toU8(ikmBytes));
    const info = infoBytes ? toU8(infoBytes) : new Uint8Array(0);
    const n = Math.ceil(okmLen / 32);
    if (n > 255) throw new Error('HKDF output length too large');
    const out = new Uint8Array(okmLen);
    let t = new Uint8Array(0);
    let written = 0;
    for (let i = 1; i <= n; i++) {
      const msg = new Uint8Array(t.byteLength + info.byteLength + 1);
      msg.set(t, 0);
      msg.set(info, t.byteLength);
      msg[t.byteLength + info.byteLength] = i;
      t = hmacBytes('sha256', prk, msg);
      const take = Math.min(32, okmLen - written);
      out.set(t.subarray(0, take), written);
      written += take;
    }
    return out;
  }

  // ==========================================================================
  // 4. CryptoBridge (`crates/olaz-core/src/crypto.rs:599-988`)
  // ==========================================================================

  class CryptoBridge {
    constructor() {
      this.nextHandle = 1;
      this.ciphers = new Map();
      this.hashes = new Map();
    }

    _allocHandle() {
      return this.nextHandle++;
    }

    encrypt(args = []) {
      return this._cryptOneShot(true, args);
    }

    decrypt(args = []) {
      return this._cryptOneShot(false, args);
    }

    _cryptOneShot(encrypting, args) {
      const encMap = args[4] && typeof args[4] === 'object' ? args[4] : null;
      const optMap = args[5] && typeof args[5] === 'object' ? args[5] : null;

      const alg = typeof args[1] === 'string' ? args[1] : 'aes-256-cbc';
      const keyEnc = encMap && typeof encMap.key === 'string' ? encMap.key : undefined;
      const ivEnc =
        encMap && typeof encMap.iv === 'string' ? encMap.iv : keyEnc;
      const inputEnc = encMap && typeof encMap.input === 'string' ? encMap.input : undefined;
      const outputEnc = encMap && typeof encMap.output === 'string' ? encMap.output : undefined;

      const data = decodeInput(args[0], inputEnc);
      const key = decodeInput(args[2], keyEnc !== undefined ? keyEnc : 'utf8');
      const iv = isNullOrUndefinedWire(args[3])
        ? new Uint8Array(16)
        : decodeInput(args[3], ivEnc !== undefined ? ivEnc : 'utf8');

      if (alg.trim().toLowerCase().endsWith('-gcm') && iv.byteLength !== 12 && iv.byteLength !== 16) {
        throw new Error('Invalid initialization vector');
      }

      const session = new CipherSession(encrypting, alg, key, iv);
      if (optMap) {
        if (typeof optMap.autoPadding === 'boolean') {
          session.setAutoPadding(optMap.autoPadding);
        }
        if (optMap.aad !== undefined && !isNullOrUndefinedWire(optMap.aad)) {
          session.setAAD(decodeInput(optMap.aad, undefined));
        }
        if (!encrypting && optMap.authTag !== undefined && !isNullOrUndefinedWire(optMap.authTag)) {
          session.setAuthTag(decodeInput(optMap.authTag, undefined));
        }
      }

      const a = session.update(data);
      const b = session.finish();
      return encodeOutput(concatU8([a, b]), outputEnc);
    }

    hash(args = []) {
      const alg = typeof args[1] === 'string' ? args[1] : 'sha256';
      const inEnc = typeof args[2] === 'string' ? args[2] : undefined;
      const outEnc = typeof args[3] === 'string' ? args[3] : undefined;
      const data = decodeInput(args[0], inEnc);
      const digest = hashBytes(alg, data);
      return encodeOutput(digest, outEnc);
    }

    randomBytes(args = []) {
      const n =
        typeof args[0] === 'number' && Number.isFinite(args[0]) && args[0] >= 0
          ? Math.floor(args[0])
          : null;
      if (n === null) throw new Error('size must be a non-negative integer');
      return encodeWireArrayBuffer(csprngBytes(n));
    }

    cipherCreate(args = []) {
      const encrypting = args[0] === 'cipher' || args[0] === true;
      const alg = typeof args[1] === 'string' ? args[1] : null;
      if (!alg) throw new Error('missing cipher algorithm');
      const key = decodeInput(args[2], 'utf8');
      const iv = decodeInput(args[3], 'utf8');
      if (alg.trim().toLowerCase().endsWith('-gcm') && iv.byteLength !== 12 && iv.byteLength !== 16) {
        throw new Error('Invalid initialization vector');
      }
      const session = new CipherSession(encrypting, alg, key, iv);
      const h = this._allocHandle();
      this.ciphers.set(h, session);
      return h;
    }

    cipherUpdate(args = []) {
      const h = this._parseHandle(args[0]);
      const data = decodeInput(args[1], typeof args[2] === 'string' ? args[2] : undefined);
      const outEnc = typeof args[3] === 'string' ? args[3] : undefined;
      const sess = this.ciphers.get(h);
      if (!sess) throw new Error('cipher released');
      return encodeOutput(sess.update(data), outEnc);
    }

    cipherFinal(args = []) {
      const h = this._parseHandle(args[0]);
      const outEnc = typeof args[1] === 'string' ? args[1] : undefined;
      const sess = this.ciphers.get(h);
      if (!sess) throw new Error('cipher released');
      const keepForAuthTag = sess.isGcm() && sess.encrypting;
      let out;
      try {
        out = sess.finish();
      } catch (err) {
        this.ciphers.delete(h);
        throw err;
      }
      if (!keepForAuthTag) {
        this.ciphers.delete(h);
      }
      return encodeOutput(out, outEnc);
    }

    cipherOption(args = []) {
      const h = this._parseHandle(args[0]);
      const opt = typeof args[1] === 'string' ? args[1] : '';
      switch (opt) {
        case 'autoPadding': {
          const ap = args[2] === undefined ? true : Boolean(args[2]);
          const sess = this.ciphers.get(h);
          if (!sess) throw new Error('cipher released');
          sess.setAutoPadding(ap);
          return wireUndefined();
        }
        case 'authTag': {
          const tag = decodeInput(args[2], undefined);
          const sess = this.ciphers.get(h);
          if (!sess) throw new Error('cipher released');
          sess.setAuthTag(tag);
          return wireUndefined();
        }
        case 'aad': {
          const aad = decodeInput(args[2], undefined);
          const sess = this.ciphers.get(h);
          if (!sess) throw new Error('cipher released');
          sess.setAAD(aad);
          return wireUndefined();
        }
        case 'getAuthTag': {
          const sess = this.ciphers.get(h);
          if (!sess) return null;
          const tag = sess.getAuthTag();
          if (sess.finalized) {
            this.ciphers.delete(h);
          }
          return tag ? encodeWireArrayBuffer(tag) : null;
        }
        default:
          throw new Error('Unknown cipher option: ' + opt);
      }
    }

    cipherSetAAD(args = []) {
      return this.cipherOption([args[0], 'aad', args[1]]);
    }

    cipherSetAuthTag(args = []) {
      return this.cipherOption([args[0], 'authTag', args[1]]);
    }

    cipherGetAuthTag(args = []) {
      return this.cipherOption([args[0], 'getAuthTag']);
    }

    hashCreate(args = []) {
      const alg = typeof args[0] === 'string' ? args[0] : 'sha256';
      const state = createHasher(alg);
      const h = this._allocHandle();
      this.hashes.set(h, state);
      return h;
    }

    hashUpdate(args = []) {
      const h = this._parseHandle(args[0]);
      const data = decodeInput(args[1], typeof args[2] === 'string' ? args[2] : undefined);
      const state = this.hashes.get(h);
      if (!state) throw new Error('hash released');
      state.update(data);
      return wireUndefined();
    }

    hashDigest(args = []) {
      const h = this._parseHandle(args[0]);
      const outEnc = typeof args[1] === 'string' ? args[1] : undefined;
      const state = this.hashes.get(h);
      if (!state) throw new Error('hash released');
      this.hashes.delete(h);
      return encodeOutput(state.finalize(), outEnc);
    }

    _parseHandle(val) {
      if (typeof val === 'number' && Number.isInteger(val) && val >= 0) return val;
      throw new Error('invalid handle');
    }

    dispatchSync(path, args = []) {
      const raw = String(path || '');
      const method = raw.startsWith('$znode.crypto.') ? raw.slice('$znode.crypto.'.length) : raw;
      switch (method) {
        case 'encrypt':
          return this.encrypt(args);
        case 'decrypt':
          return this.decrypt(args);
        case 'hash':
          return this.hash(args);
        case 'randomBytes':
          return this.randomBytes(args);
        case '__cipherCreate':
        case 'cipherCreate':
          return this.cipherCreate(args);
        case '__cipherUpdate':
        case 'cipherUpdate':
          return this.cipherUpdate(args);
        case '__cipherFinal':
        case 'cipherFinal':
          return this.cipherFinal(args);
        case '__cipherOption':
        case 'cipherOption':
          return this.cipherOption(args);
        case '__cipherSetAutoPadding':
        case 'cipherSetAutoPadding':
          return this.cipherOption([args[0], 'autoPadding', args[1] === undefined ? true : args[1]]);
        case '__cipherSetAAD':
        case 'cipherSetAAD':
          return this.cipherSetAAD(args);
        case '__cipherSetAuthTag':
        case 'cipherSetAuthTag':
          return this.cipherSetAuthTag(args);
        case '__cipherGetAuthTag':
        case 'cipherGetAuthTag':
          return this.cipherGetAuthTag(args);
        case '__hashCreate':
        case 'hashCreate':
          return this.hashCreate(args);
        case '__hashUpdate':
        case 'hashUpdate':
          return this.hashUpdate(args);
        case '__hashDigest':
        case 'hashDigest':
          return this.hashDigest(args);
        default:
          throw new Error('Unknown crypto method: ' + path);
      }
    }
  }

  // ==========================================================================
  // 5. SafeStorage (`crates/olaz-core/src/vault.rs:22-155`)
  // ==========================================================================

  const SAFE_STORAGE_KEY_STR = 'd9c07b9de9915af401607d3f360211ac';
  const SAFE_STORAGE_KEY = UTF8_ENC.encode(SAFE_STORAGE_KEY_STR);
  const SAFE_STORAGE_IV = Uint8Array.of(
    155, 72, 37, 195, 9, 217, 80, 42, 145, 102, 171, 75, 214, 176, 134, 18
  );

  class SafeStorage {
    constructor(backingHooks) {
      this._localStore = { zsskv: '2', zsskd: {} };
      this._hooks = backingHooks || null;
    }

    static encryptString(plaintext) {
      const ct = aes256CbcPkcs7(
        UTF8_ENC.encode(String(plaintext)),
        SAFE_STORAGE_KEY,
        SAFE_STORAGE_IV,
        true
      );
      return bytesToBase64(ct, false);
    }

    static decryptString(sealedBase64) {
      const ct = decodeBase64Forgiving(String(sealedBase64));
      const pt = aes256CbcPkcs7(ct, SAFE_STORAGE_KEY, SAFE_STORAGE_IV, false);
      return UTF8_DEC.decode(pt);
    }

    encryptString(plaintext) {
      return SafeStorage.encryptString(plaintext);
    }

    decryptString(sealedBase64) {
      return SafeStorage.decryptString(sealedBase64);
    }

    _readStore() {
      if (this._hooks && typeof this._hooks.readStore === 'function') {
        const s = this._hooks.readStore('zsafe-storage');
        if (s && typeof s === 'object') return s;
      }
      return this._localStore;
    }

    _writeStore(storeObj) {
      this._localStore = storeObj;
      if (this._hooks && typeof this._hooks.writeStore === 'function') {
        this._hooks.writeStore('zsafe-storage', storeObj);
      }
    }

    getEntry(key) {
      const k = String(key || '');
      if (k.includes('.')) return null;
      const store = this._readStore();
      const kd = store && typeof store.zsskd === 'object' && store.zsskd ? store.zsskd : null;
      if (!kd || typeof kd[k] !== 'string') return null;
      try {
        return SafeStorage.decryptString(kd[k]);
      } catch {
        return null;
      }
    }

    setEntry(key, value) {
      const k = String(key || '');
      if (k.includes('.')) return;
      const sealed = SafeStorage.encryptString(String(value));
      const prev = this._readStore();
      const kd = Object.assign({}, prev && typeof prev.zsskd === 'object' && prev.zsskd ? prev.zsskd : {});
      kd[k] = sealed;
      const next = Object.assign({}, prev || {}, { zsskv: '2', zsskd: kd });
      this._writeStore(next);
    }

    execute(method, args = []) {
      switch (String(method || '')) {
        case 'encryptString':
          if (typeof args[0] !== 'string') throw new Error('zsafestorage.encryptString requires string');
          return SafeStorage.encryptString(args[0]);
        case 'decryptString':
          if (typeof args[0] !== 'string') throw new Error('zsafestorage.decryptString requires string');
          return SafeStorage.decryptString(args[0]);
        case 'get':
          if (typeof args[0] !== 'string') return null;
          return this.getEntry(args[0]);
        case 'set':
          if (typeof args[0] === 'string' && typeof args[1] === 'string') {
            this.setEntry(args[0], args[1]);
          }
          return wireUndefined();
        default:
          return wireUndefined();
      }
    }
  }

  // ==========================================================================
  // 6. KeychainCipher (`crates/olaz-core/src/vault.rs:498-660`)
  // ==========================================================================

  const KEYCHAIN_VERSION = 0x49;
  const KEYCHAIN_MIN_LEN = 65; // 1 + 16 + 16 + 0 + 32
  const KEYCHAIN_KDF_INFO = UTF8_ENC.encode('ZprotocolSecureKDF');

  class KeychainCipher {
    constructor(backing) {
      this._keys = new Map();
      this._backing = backing || null;
    }

    static accountKeyName(identifier) {
      return 'zprotocol.' + String(identifier || '').toLowerCase();
    }

    static encryptWithRootKey(rootKeyBytes, plaintextBytes, customIv16, customSalt16) {
      const rootKey = toU8(rootKeyBytes);
      if (rootKey.byteLength !== 32) {
        throw new Error('corrupt 32-byte root key (got ' + rootKey.byteLength + ' bytes)');
      }
      const iv = customIv16 ? toU8(customIv16) : csprngBytes(16);
      const salt = customSalt16 ? toU8(customSalt16) : csprngBytes(16);
      if (iv.byteLength !== 16 || salt.byteLength !== 16) {
        throw new Error('KeychainCipher IV and salt must be 16 bytes');
      }
      const okm = hkdfSha256(rootKey, salt, KEYCHAIN_KDF_INFO, 64);
      const encKey = okm.subarray(0, 32);
      const macKey = okm.subarray(32, 64);
      const ct = aes256CbcPkcs7(toU8(plaintextBytes), encKey, iv, true);
      const macInput = concatU8([ct, iv, salt]);
      const tag = hmacBytes('sha256', macKey, macInput);

      const envelope = new Uint8Array(1 + 16 + 16 + ct.byteLength + 32);
      envelope[0] = KEYCHAIN_VERSION;
      envelope.set(iv, 1);
      envelope.set(salt, 17);
      envelope.set(ct, 33);
      envelope.set(tag, 33 + ct.byteLength);
      return envelope;
    }

    static decryptWithRootKey(rootKeyBytes, envelopeBytes) {
      const rootKey = toU8(rootKeyBytes);
      if (rootKey.byteLength !== 32) {
        throw new Error('corrupt 32-byte root key (got ' + rootKey.byteLength + ' bytes)');
      }
      const env = toU8(envelopeBytes);
      if (env.byteLength < KEYCHAIN_MIN_LEN || env[0] !== KEYCHAIN_VERSION) {
        throw new Error('invalid keychain envelope');
      }
      const iv = env.subarray(1, 17);
      const salt = env.subarray(17, 33);
      const ct = env.subarray(33, env.byteLength - 32);
      const tag = env.subarray(env.byteLength - 32);

      const okm = hkdfSha256(rootKey, salt, KEYCHAIN_KDF_INFO, 64);
      const encKey = okm.subarray(0, 32);
      const macKey = okm.subarray(32, 64);
      const macInput = concatU8([ct, iv, salt]);
      const expectedTag = hmacBytes('sha256', macKey, macInput);
      if (!ctEq(tag, expectedTag)) {
        throw new Error('keychain envelope HMAC verification failed');
      }
      return aes256CbcPkcs7(ct, encKey, iv, false);
    }

    getRootKey(identifier, createIfMissing) {
      const name = KeychainCipher.accountKeyName(identifier);
      if (this._backing && typeof this._backing.getKey === 'function') {
        const existing = this._backing.getKey(name);
        if (existing) return toU8(existing);
        if (!createIfMissing) return null;
        const fresh = csprngBytes(32);
        if (typeof this._backing.setKey === 'function') {
          this._backing.setKey(name, fresh);
        }
        return fresh;
      }
      const cur = this._keys.get(name);
      if (cur) return cur;
      if (!createIfMissing) return null;
      const fresh = csprngBytes(32);
      this._keys.set(name, fresh);
      return fresh;
    }

    setRootKey(identifier, rootKeyBytes) {
      const name = KeychainCipher.accountKeyName(identifier);
      const rk = cloneU8(rootKeyBytes);
      if (rk.byteLength !== 32) throw new Error('Root key must be 32 bytes');
      this._keys.set(name, rk);
      if (this._backing && typeof this._backing.setKey === 'function') {
        this._backing.setKey(name, rk);
      }
    }

    encrypt(identifier, plaintextBytes) {
      const rootKey = this.getRootKey(identifier, true);
      return KeychainCipher.encryptWithRootKey(rootKey, plaintextBytes);
    }

    decrypt(identifier, envelopeBytes) {
      const rootKey = this.getRootKey(identifier, false);
      if (!rootKey) {
        throw new Error('missing keychain root key for ' + KeychainCipher.accountKeyName(identifier));
      }
      return KeychainCipher.decryptWithRootKey(rootKey, envelopeBytes);
    }

    encryptWire(args = []) {
      const id = typeof args[0] === 'string' ? args[0] : '';
      const rawItems = Array.isArray(args[1]) ? args[1] : args[1] !== undefined ? [args[1]] : [];
      const out = [];
      for (const item of rawItems) {
        let dataBytes = decodeWireBytes(item);
        if (dataBytes === null && typeof item === 'string') {
          dataBytes = UTF8_ENC.encode(item);
        }
        if (dataBytes === null) continue;
        out.push(encodeWireBytes(this.encrypt(id, dataBytes)));
      }
      return out;
    }

    decryptWire(args = []) {
      const id = typeof args[0] === 'string' ? args[0] : '';
      const rawItems = Array.isArray(args[1]) ? args[1] : args[1] !== undefined ? [args[1]] : [];
      const out = [];
      for (const item of rawItems) {
        const envBytes = decodeWireBytes(item);
        if (envBytes === null) continue;
        out.push(encodeWireBytes(this.decrypt(id, envBytes)));
      }
      return out;
    }
  }

  // ==========================================================================
  // 7. BrowserCookieJar (`crates/olaz-net/src/cookies.rs`)
  // ==========================================================================

  const AUTH_DOMAIN = 'https://wpa.zaloapp.com';
  const SESSION_COOKIE_DOMAINS = ['zaloapp.com', 'zalo.me', 'chat.zalo.me'];
  const ZALO_COOKIE_CLEAR_DOMAINS = [
    'zalo.me',
    'zaloapp.com',
    'zadn.vn',
    'zdn.vn',
    'zing.vn',
    'zingcdn.me',
    'zalo.vn',
  ];

  function normalizeHttpUrl(rawUrl) {
    const s = String(rawUrl || '').trim();
    if (!s) return null;
    let rewritten = s;
    if (/^wss:\/\//i.test(s)) rewritten = 'https://' + s.slice(6);
    else if (/^ws:\/\//i.test(s)) rewritten = 'http://' + s.slice(5);
    try {
      return new URL(rewritten);
    } catch {
      return null;
    }
  }

  function defaultCookiePath(urlPathname) {
    const p = String(urlPathname || '/');
    if (!p.startsWith('/')) return '/';
    const lastSlash = p.lastIndexOf('/');
    if (lastSlash <= 0) return '/';
    return p.slice(0, lastSlash);
  }

  function domainMatches(reqHost, cookieCanonicalDomain, hostOnly) {
    if (hostOnly) return reqHost === cookieCanonicalDomain;
    return reqHost === cookieCanonicalDomain || reqHost.endsWith('.' + cookieCanonicalDomain);
  }

  function pathMatches(reqPath, cookiePath) {
    const rp = reqPath || '/';
    const cp = cookiePath || '/';
    if (rp === cp) return true;
    if (rp.startsWith(cp)) {
      if (cp.endsWith('/')) return true;
      if (rp.charCodeAt(cp.length) === 47 /* '/' */) return true;
    }
    return false;
  }

  function parseSetCookieString(rawHeader, reqUrlObj, nowMs) {
    const raw = String(rawHeader || '').trim();
    if (!raw) return null;
    const parts = raw.split(';');
    const firstEq = parts[0].indexOf('=');
    if (firstEq <= 0) return null;
    const name = parts[0].slice(0, firstEq).trim();
    const value = parts[0].slice(firstEq + 1).trim();
    if (!name) return null;

    let attrDomain = null;
    let attrPath = null;
    let maxAgeSec = null;
    let expiresMs = null;
    let secure = false;
    let httpOnly = false;
    let sameSite = 'unspecified';

    for (let i = 1; i < parts.length; i++) {
      const seg = parts[i].trim();
      if (!seg) continue;
      const eq = seg.indexOf('=');
      const k = (eq >= 0 ? seg.slice(0, eq) : seg).trim().toLowerCase();
      const v = eq >= 0 ? seg.slice(eq + 1).trim() : '';
      if (k === 'domain') {
        if (v) attrDomain = v.replace(/^\.+/, '').toLowerCase();
      } else if (k === 'path') {
        if (v && v.startsWith('/')) attrPath = v;
      } else if (k === 'max-age') {
        const parsed = Number(v);
        if (Number.isFinite(parsed)) maxAgeSec = Math.trunc(parsed);
      } else if (k === 'expires') {
        const t = Date.parse(v);
        if (!Number.isNaN(t)) expiresMs = t;
      } else if (k === 'secure') {
        secure = true;
      } else if (k === 'httponly') {
        httpOnly = true;
      } else if (k === 'samesite') {
        const vl = v.toLowerCase();
        if (vl === 'none') sameSite = 'no_restriction';
        else if (vl === 'lax') sameSite = 'lax';
        else if (vl === 'strict') sameSite = 'strict';
      }
    }

    const reqHost = reqUrlObj.hostname.toLowerCase();
    let canonicalDomain = reqHost;
    let hostOnly = true;
    if (attrDomain) {
      if (!domainMatches(reqHost, attrDomain, false)) {
        return null; // RFC 6265 §5.3: reject cross-domain Set-Cookie
      }
      canonicalDomain = attrDomain;
      hostOnly = false;
    }

    const path = attrPath || defaultCookiePath(reqUrlObj.pathname);
    let expiresAtMs = null;
    let expired = false;
    if (maxAgeSec !== null) {
      if (maxAgeSec <= 0) expired = true;
      else expiresAtMs = nowMs + maxAgeSec * 1000;
    } else if (expiresMs !== null) {
      if (expiresMs <= nowMs) expired = true;
      else expiresAtMs = expiresMs;
    }

    return {
      name,
      value,
      canonicalDomain,
      hostOnly,
      path,
      secure,
      httpOnly,
      sameSite,
      expiresAtMs,
      expired,
    };
  }

  class BrowserCookieJar {
    constructor(onMutate) {
      this._cookies = new Map(); // key: `${canonicalDomain}\0${path}\0${name}`
      this._onMutate = typeof onMutate === 'function' ? onMutate : null;
    }

    static get AUTH_DOMAIN() {
      return AUTH_DOMAIN;
    }

    static get SESSION_COOKIE_DOMAINS() {
      return SESSION_COOKIE_DOMAINS.slice();
    }

    _key(canonicalDomain, path, name) {
      return canonicalDomain + '\0' + path + '\0' + name;
    }

    _notify() {
      if (this._onMutate) {
        try {
          this._onMutate(this);
        } catch {}
      }
    }

    _pruneExpired(nowMs = Date.now()) {
      for (const [k, c] of this._cookies.entries()) {
        if (c.expiresAtMs !== null && c.expiresAtMs <= nowMs) {
          this._cookies.delete(k);
        }
      }
    }

    setAppCookie(zpwSek, nowMs = Date.now()) {
      const val = String(zpwSek === undefined || zpwSek === null ? '' : zpwSek);
      for (const [k, c] of this._cookies.entries()) {
        if (c.name === 'zpw_sek') this._cookies.delete(k);
      }
      if (val) {
        const exp = nowMs + 7 * 86400 * 1000;
        for (const d of SESSION_COOKIE_DOMAINS) {
          const entry = {
            name: 'zpw_sek',
            value: val,
            canonicalDomain: d,
            hostOnly: false,
            path: '/',
            secure: true,
            httpOnly: true,
            sameSite: 'no_restriction',
            expiresAtMs: exp,
          };
          this._cookies.set(this._key(d, '/', 'zpw_sek'), entry);
        }
      }
      this._notify();
    }

    setCookie(name, value, url, domain, nowMs = Date.now()) {
      const trimmedName = String(name || '').trim();
      if (!trimmedName) throw new Error('Cookie name must not be empty');
      const uObj = url ? normalizeHttpUrl(url) : null;
      const rawDom =
        (typeof domain === 'string' && domain.trim()) ||
        (uObj && uObj.hostname) ||
        'zalo.me';
      const cleanDomain = rawDom.trim().replace(/^\.+/, '').toLowerCase();
      if (!cleanDomain) throw new Error('Cookie domain must not be empty');
      const isPlainHttp = Boolean(url && String(url).trim().toLowerCase().startsWith('http://'));
      const entry = {
        name: trimmedName,
        value: String(value === undefined || value === null ? '' : value),
        canonicalDomain: cleanDomain,
        hostOnly: false,
        path: '/',
        secure: !isPlainHttp,
        httpOnly: false,
        sameSite: isPlainHttp ? 'lax' : 'no_restriction',
        expiresAtMs: nowMs + 365 * 86400 * 1000,
      };
      this._cookies.set(this._key(cleanDomain, '/', trimmedName), entry);
      this._notify();
    }

    deleteCookie(urlOrDomain, name) {
      const targetName = String(name || '').trim();
      if (!targetName) return;
      const rawDom = (() => {
        if (!urlOrDomain || !String(urlOrDomain).trim()) return null;
        const s = String(urlOrDomain).trim();
        const u = normalizeHttpUrl(s);
        if (u && u.hostname) return u.hostname.toLowerCase();
        return s.replace(/^\.+/, '').toLowerCase();
      })();

      for (const [k, c] of this._cookies.entries()) {
        if (c.name !== targetName) continue;
        if (!rawDom || domainMatches(rawDom, c.canonicalDomain, false) || domainMatches(c.canonicalDomain, rawDom, false)) {
          this._cookies.delete(k);
        }
      }
      this._notify();
    }

    clearCookies() {
      for (const [k, c] of this._cookies.entries()) {
        const matchesZalo = ZALO_COOKIE_CLEAR_DOMAINS.some(
          (z) => c.canonicalDomain === z || c.canonicalDomain.endsWith('.' + z)
        );
        if (matchesZalo) this._cookies.delete(k);
      }
      this._notify();
    }

    clearAll() {
      this._cookies.clear();
      this._notify();
    }

    applySetCookieHeaders(requestUrl, rawHeadersArray, nowMs = Date.now()) {
      const uObj = normalizeHttpUrl(requestUrl);
      if (!uObj || !Array.isArray(rawHeadersArray) || rawHeadersArray.length === 0) return;
      let mutated = false;
      for (const raw of rawHeadersArray) {
        const parsed = parseSetCookieString(raw, uObj, nowMs);
        if (!parsed) continue;
        const key = this._key(parsed.canonicalDomain, parsed.path, parsed.name);
        if (parsed.expired) {
          if (this._cookies.delete(key)) mutated = true;
          if (parsed.name === 'zpw_sek') {
            for (const d of SESSION_COOKIE_DOMAINS) {
              this._cookies.delete(this._key(d, '/', 'zpw_sek'));
            }
            mutated = true;
          }
          continue;
        }

        if (parsed.name === 'zpw_sek' && parsed.value) {
          const ttlMs = parsed.expiresAtMs !== null ? parsed.expiresAtMs : nowMs + 7 * 86400 * 1000;
          for (const [k, existing] of this._cookies.entries()) {
            if (existing.name === 'zpw_sek') this._cookies.delete(k);
          }
          for (const d of SESSION_COOKIE_DOMAINS) {
            this._cookies.set(this._key(d, '/', 'zpw_sek'), {
              name: 'zpw_sek',
              value: parsed.value,
              canonicalDomain: d,
              hostOnly: false,
              path: '/',
              secure: true,
              httpOnly: true,
              sameSite: 'no_restriction',
              expiresAtMs: ttlMs,
            });
          }
          mutated = true;
          continue;
        }

        this._cookies.set(key, {
          name: parsed.name,
          value: parsed.value,
          canonicalDomain: parsed.canonicalDomain,
          hostOnly: parsed.hostOnly,
          path: parsed.path,
          secure: parsed.secure,
          httpOnly: parsed.httpOnly,
          sameSite: parsed.sameSite,
          expiresAtMs: parsed.expiresAtMs,
        });
        mutated = true;
      }
      if (mutated) this._notify();
    }

    cookieHeaderForUrl(rawUrl, nowMs = Date.now()) {
      this._pruneExpired(nowMs);
      const uObj = normalizeHttpUrl(rawUrl);
      if (!uObj || !uObj.hostname) return '';
      const reqHost = uObj.hostname.toLowerCase();
      const reqPath = uObj.pathname || '/';
      const isHttps = uObj.protocol === 'https:';

      const matched = [];
      for (const c of this._cookies.values()) {
        if (c.secure && !isHttps) continue;
        if (!domainMatches(reqHost, c.canonicalDomain, c.hostOnly)) continue;
        if (!pathMatches(reqPath, c.path)) continue;
        matched.push(c);
      }
      if (matched.length === 0) return '';

      matched.sort((a, b) => {
        if (b.path.length !== a.path.length) return b.path.length - a.path.length;
        if (a.name !== b.name) return a.name < b.name ? -1 : 1;
        if (a.canonicalDomain !== b.canonicalDomain) {
          return a.canonicalDomain < b.canonicalDomain ? -1 : 1;
        }
        return 0;
      });

      const pairs = [];
      for (const c of matched) {
        const pair = c.name + '=' + c.value;
        if (!pairs.includes(pair)) pairs.push(pair);
      }
      return pairs.join('; ');
    }

    getCookies(filter = {}, nowMs = Date.now()) {
      this._pruneExpired(nowMs);
      const f = filter && typeof filter === 'object' ? filter : {};
      const urlObj = f.url ? normalizeHttpUrl(f.url) : null;
      if (f.url && !urlObj) return [];

      const out = [];
      for (const c of this._cookies.values()) {
        if (urlObj) {
          const isHttps = urlObj.protocol === 'https:';
          if (c.secure && !isHttps) continue;
          if (!domainMatches(urlObj.hostname.toLowerCase(), c.canonicalDomain, c.hostOnly)) continue;
          if (!pathMatches(urlObj.pathname || '/', c.path)) continue;
        }
        if (typeof f.name === 'string' && c.name !== f.name) continue;
        if (typeof f.domain === 'string') {
          const fd = f.domain.replace(/^\.+/, '').toLowerCase();
          if (!(c.canonicalDomain === fd || c.canonicalDomain.endsWith('.' + fd))) continue;
        }
        if (typeof f.path === 'string' && !c.path.startsWith(f.path)) continue;
        if (typeof f.secure === 'boolean' && c.secure !== f.secure) continue;
        const isSession = c.expiresAtMs === null;
        if (typeof f.session === 'boolean' && isSession !== f.session) continue;

        const entry = {
          name: c.name,
          value: c.value,
          domain: c.hostOnly ? c.canonicalDomain : '.' + c.canonicalDomain,
          hostOnly: c.hostOnly,
          path: c.path,
          secure: c.secure,
          httpOnly: c.httpOnly,
          session: isSession,
          sameSite: c.sameSite || 'unspecified',
        };
        if (!isSession) {
          entry.expirationDate = c.expiresAtMs / 1000;
        }
        out.push(entry);
      }

      out.sort((a, b) => {
        if (a.domain !== b.domain) return a.domain < b.domain ? -1 : 1;
        if (a.path !== b.path) return a.path < b.path ? -1 : 1;
        if (a.name !== b.name) return a.name < b.name ? -1 : 1;
        return 0;
      });
      return out;
    }

    hasZpwSek(nowMs = Date.now()) {
      this._pruneExpired(nowMs);
      for (const c of this._cookies.values()) {
        if (c.name === 'zpw_sek' && c.value) return true;
      }
      return false;
    }

    handleIpc(channel, args = []) {
      switch (String(channel || '')) {
        case '_electron_set-app-cookie': {
          const zpwSek = typeof args[0] === 'string' ? args[0] : '';
          this.setAppCookie(zpwSek);
          return null;
        }
        case '_electron_set-cookie': {
          if (args[0] && typeof args[0] === 'object' && !Array.isArray(args[0])) {
            const o = args[0];
            if (o.name) {
              this.setCookie(o.name, o.value || '', o.url, o.domain);
            }
          } else if (typeof args[0] === 'string') {
            this.setCookie(args[0], args[1] || '', args[2], args[3]);
          }
          return null;
        }
        case '_electron_clear-cookie': {
          this.clearCookies();
          return null;
        }
        case '_electron_get-cookies':
        case '_electron_get-cookie': {
          return this.getCookies(args[0] || {});
        }
        case '_electron_delete-cookie': {
          if (args.length >= 2) {
            this.deleteCookie(args[0], args[1]);
          } else if (typeof args[0] === 'string') {
            this.deleteCookie(null, args[0]);
          } else if (args[0] && typeof args[0] === 'object') {
            this.deleteCookie(args[0].url || args[0].domain, args[0].name);
          }
          return null;
        }
        default:
          throw new Error('Unsupported cookie IPC channel: ' + channel);
      }
    }

    exportJson(nowMs = Date.now()) {
      this._pruneExpired(nowMs);
      return Array.from(this._cookies.values()).map((c) => Object.assign({}, c));
    }

    importJson(records, nowMs = Date.now()) {
      this._cookies.clear();
      if (!Array.isArray(records)) return;
      for (const r of records) {
        if (!r || typeof r !== 'object' || typeof r.name !== 'string' || typeof r.canonicalDomain !== 'string') {
          continue;
        }
        if (typeof r.expiresAtMs === 'number' && r.expiresAtMs <= nowMs) continue;
        const entry = {
          name: r.name,
          value: String(r.value || ''),
          canonicalDomain: r.canonicalDomain.toLowerCase(),
          hostOnly: Boolean(r.hostOnly),
          path: typeof r.path === 'string' && r.path.startsWith('/') ? r.path : '/',
          secure: Boolean(r.secure),
          httpOnly: Boolean(r.httpOnly),
          sameSite: typeof r.sameSite === 'string' ? r.sameSite : 'unspecified',
          expiresAtMs: typeof r.expiresAtMs === 'number' ? r.expiresAtMs : null,
        };
        this._cookies.set(this._key(entry.canonicalDomain, entry.path, entry.name), entry);
      }
    }
  }

  // ==========================================================================
  // 8. fileNoise & SecureStream (`fs_bridge.rs` & `secure_stream.rs`)
  // ==========================================================================

  const ZALO_NOISE_SIGN = Uint8Array.of(0x07, 0x7a, 0x41, 0x6c, 0x4f, 0x07);
  const ZALO_NOISE_XOR_KEY = 0x93;
  const ZALO_NOISE_XOR_LEN = 100;

  function applyNoiseXorAtOffset(fileOffset, buf) {
    const u8 = buf instanceof Uint8Array ? buf : toU8(buf);
    const off = Math.max(0, Number(fileOffset) || 0);
    if (off >= ZALO_NOISE_XOR_LEN || u8.byteLength === 0) return u8;
    const xorEnd = Math.min(ZALO_NOISE_XOR_LEN - off, u8.byteLength);
    for (let i = 0; i < xorEnd; i++) {
      u8[i] ^= ZALO_NOISE_XOR_KEY;
    }
    return u8;
  }

  function hasNoiseTrailer(bytes) {
    const u8 = toU8(bytes);
    const n = u8.byteLength;
    if (n <= ZALO_NOISE_SIGN.byteLength) return false;
    return ctEq(u8.subarray(n - ZALO_NOISE_SIGN.byteLength), ZALO_NOISE_SIGN);
  }

  function inspectBufferNoise(bytes, checkNoise = true) {
    const u8 = toU8(bytes);
    if (checkNoise && hasNoiseTrailer(u8)) {
      const sigOff = u8.byteLength - ZALO_NOISE_SIGN.byteLength;
      return { logicalLen: sigOff, signatureOffset: sigOff };
    }
    return { logicalLen: u8.byteLength, signatureOffset: null };
  }

  function denoiseBuffer(bytes) {
    const u8 = toU8(bytes);
    if (!hasNoiseTrailer(u8)) return cloneU8(u8);
    const out = u8.slice(0, u8.byteLength - ZALO_NOISE_SIGN.byteLength);
    applyNoiseXorAtOffset(0, out);
    return out;
  }

  function noiseBuffer(bytes) {
    const u8 = toU8(bytes);
    const clean = hasNoiseTrailer(u8) ? denoiseBuffer(u8) : cloneU8(u8);
    applyNoiseXorAtOffset(0, clean);
    return concatU8([clean, ZALO_NOISE_SIGN]);
  }

  function parseCryptoParamBytes(val) {
    if (typeof val === 'string') {
      const trimmed = val.trim();
      if (!trimmed) return null;
      return decodeBase64Forgiving(trimmed);
    }
    return decodeWireBytes(val);
  }

  function decryptSecureBuffer(mode, keyB64OrBytes, ivB64OrBytes, encryptedBytes, opts = {}) {
    const key =
      typeof keyB64OrBytes === 'string'
        ? decodeBase64Forgiving(keyB64OrBytes)
        : toU8(keyB64OrBytes);
    const iv =
      typeof ivB64OrBytes === 'string'
        ? decodeBase64Forgiving(ivB64OrBytes)
        : toU8(ivB64OrBytes);
    const enc = toU8(encryptedBytes);
    if (key.byteLength !== 32) {
      throw new Error('File secure decrypt failed: invalid 32-byte key');
    }

    const normMode = String(mode || '').trim().toLowerCase();
    if (
      normMode === 'aes256ctrlinear' ||
      normMode === 'aes-256-ctr-linear' ||
      normMode === 'ctr-linear' ||
      normMode === 'aes-256-ctr'
    ) {
      if (iv.byteLength !== 16) throw new Error('File secure decrypt failed: invalid 16-byte IV');
      return new Ctr128State(256, key, iv).apply(enc);
    }

    if (
      normMode === 'aes256ctrblock' ||
      normMode === 'aes-256-ctr-block' ||
      normMode === 'ctr-block'
    ) {
      if (iv.byteLength !== 16) throw new Error('File secure decrypt failed: invalid 16-byte IV');
      const rawBlock = opts && typeof opts.blockSize === 'number' ? Math.trunc(opts.blockSize) : 4096;
      const blockSize = Math.max(16, rawBlock);
      const out = new Uint8Array(enc.byteLength);
      let off = 0;
      for (; off + blockSize <= enc.byteLength; off += blockSize) {
        const decChunk = new Ctr128State(256, key, iv).apply(enc.subarray(off, off + blockSize));
        out.set(decChunk, off);
      }
      if (off < enc.byteLength) {
        // `skipOverflow`: trailing `< blockSize` bytes at EOF pass through verbatim.
        out.set(enc.subarray(off), off);
      }
      return out;
    }

    if (
      normMode === 'aes256gcmlinear' ||
      normMode === 'aes-256-gcm-linear' ||
      normMode === 'gcm-linear' ||
      normMode === 'aes-256-gcm'
    ) {
      if (iv.byteLength < 12) throw new Error('File secure decrypt failed: GCM IV < 12 bytes');
      let ct = enc;
      let tag = null;
      if (opts && opts.tag !== undefined && opts.tag !== null) {
        tag = typeof opts.tag === 'string' ? decodeBase64Forgiving(opts.tag) : toU8(opts.tag);
      } else if (opts && typeof opts.tagB64 === 'string') {
        tag = decodeBase64Forgiving(opts.tagB64);
      } else {
        if (enc.byteLength < 16) {
          throw new Error('File secure decrypt failed: missing 16-byte GCM tag');
        }
        ct = enc.subarray(0, enc.byteLength - 16);
        tag = enc.subarray(enc.byteLength - 16);
      }
      return gcmDecryptRaw(256, key, iv, new Uint8Array(0), ct, tag);
    }

    throw new Error('Unsupported SecureStream mode: ' + mode);
  }

  function decryptWithSecureSpec(secureSpec, encryptedBytes) {
    if (!secureSpec || typeof secureSpec !== 'object') return cloneU8(encryptedBytes);
    const secType = secureSpec.type;
    if (secType !== 'e2ee' && secType !== 'pcloud') return cloneU8(encryptedBytes);
    const c = secureSpec.cryptography;
    if (!c || typeof c !== 'object') throw new Error('File secure decrypt failed');
    const key = parseCryptoParamBytes(c.key);
    const iv = parseCryptoParamBytes(c.iv);
    if (!key || key.byteLength !== 32 || !iv || iv.byteLength === 0) {
      throw new Error('File secure decrypt failed');
    }
    const streamType = c.type === undefined || c.type === null ? 'linear' : c.type;
    if (streamType !== 'linear' && streamType !== 'block') {
      throw new Error('File secure decrypt failed');
    }
    if (c.alg === 'aes-256-ctr') {
      if (streamType === 'block') {
        const blockSize =
          c.blockSize === undefined || c.blockSize === null ? 4096 : Math.max(16, Number(c.blockSize));
        if (!Number.isFinite(blockSize)) throw new Error('File secure decrypt failed');
        return decryptSecureBuffer('Aes256CtrBlock', key, iv, encryptedBytes, { blockSize });
      }
      return decryptSecureBuffer('Aes256CtrLinear', key, iv, encryptedBytes);
    }
    if (c.alg === 'aes-256-gcm') {
      if (streamType === 'block' || iv.byteLength < 12) {
        throw new Error('File secure decrypt failed');
      }
      const tag = parseCryptoParamBytes(c.tag);
      if (!tag || tag.byteLength !== 16) throw new Error('File secure decrypt failed');
      return decryptSecureBuffer('Aes256GcmLinear', key, iv, encryptedBytes, { tag });
    }
    throw new Error('File secure decrypt failed');
  }

  // ==========================================================================
  // 9. Database Config Normalization (`crates/olaz-core/src/platform.rs:864-916`)
  // ==========================================================================

  function normalizeDatabaseConfig(rawObj) {
    if (!rawObj || typeof rawObj !== 'object' || Array.isArray(rawObj)) return rawObj;
    const out = {};
    for (const [k, v] of Object.entries(rawObj)) {
      if (k.toLowerCase().includes('upgraderequired')) {
        continue; // drop upgradeRequired flags so Zalo stays on IndexedDB
      }
      if (k.endsWith('adp-st')) {
        if (typeof v === 'string') {
          try {
            const parsed = JSON.parse(v);
            if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
              for (const innerK of Object.keys(parsed)) parsed[innerK] = 0;
              out[k] = JSON.stringify(parsed);
              continue;
            }
          } catch {}
          out[k] = v === '0' ? v : 0;
        } else if (v && typeof v === 'object' && !Array.isArray(v)) {
          const inner = {};
          for (const innerK of Object.keys(v)) inner[innerK] = 0;
          out[k] = inner;
        } else {
          out[k] = 0;
        }
        continue;
      }
      if (v && typeof v === 'object' && !Array.isArray(v)) {
        out[k] = normalizeDatabaseConfig(v);
      } else {
        out[k] = v;
      }
    }
    return out;
  }

  // ==========================================================================
  // Export Global & CommonJS
  // ==========================================================================

  const core = {
    ctEq,
    toU8,
    cloneU8,
    concatU8,
    bytesToBase64,
    decodeBase64Forgiving,
    bytesToHex,
    hexToBytesNode,
    encodeWireBytes,
    encodeWireArrayBuffer,
    wireUndefined,
    decodeWireBytes,
    decodeString,
    decodeInput,
    encodeOutput,
    csprngBytes,
    AesCipher,
    Ctr128State,
    gcmEncryptRaw,
    gcmDecryptRaw,
    aes256CbcPkcs7,
    hashBytes,
    hmacBytes,
    hkdfSha256,
    CryptoBridge,
    SAFE_STORAGE_KEY_STR,
    SAFE_STORAGE_KEY,
    SAFE_STORAGE_IV,
    SafeStorage,
    KEYCHAIN_VERSION,
    KEYCHAIN_MIN_LEN,
    KeychainCipher,
    BrowserCookieJar,
    ZALO_NOISE_SIGN,
    ZALO_NOISE_XOR_KEY,
    ZALO_NOISE_XOR_LEN,
    applyNoiseXorAtOffset,
    hasNoiseTrailer,
    inspectBufferNoise,
    denoiseBuffer,
    noiseBuffer,
    decryptSecureBuffer,
    decryptWithSecureSpec,
    normalizeDatabaseConfig,
  };

  root.__olazWebCryptoCore = core;
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = core;
  }
}

initializeSessionCryptoCore(
  typeof globalThis !== 'undefined' ? globalThis : typeof self !== 'undefined' ? self : this
);
