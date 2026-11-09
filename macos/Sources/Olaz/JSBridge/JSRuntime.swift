import Foundation
import JavaScriptCore
import CryptoKit
import CommonCrypto
import Security
import os

// MARK: - Bridge values

/// A Sendable value that crosses the Swift ↔ JavaScript boundary.
/// `ArrayBuffer` / typed arrays map to `.bytes`; `undefined` maps to `.null`.
enum JSBridgeValue: Sendable, Hashable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case bytes(Data)
    case array([JSBridgeValue])
    case object([String: JSBridgeValue])

    var stringValue: String? { if case .string(let s) = self { s } else { nil } }
    var numberValue: Double? { if case .number(let n) = self { n } else { nil } }
    var intValue: Int? { numberValue.map { Int($0) } }
    var boolValue: Bool? { if case .bool(let b) = self { b } else { nil } }
    var dataValue: Data? { if case .bytes(let d) = self { d } else { nil } }
    var arrayValue: [JSBridgeValue]? { if case .array(let a) = self { a } else { nil } }

    subscript(key: String) -> JSBridgeValue? {
        if case .object(let o) = self { o[key] } else { nil }
    }
}

struct JSRuntimeError: Error, LocalizedError, Sendable {
    let message: String
    var stack: String? = nil
    var errorDescription: String? { message }
}

// MARK: - Runtime

/// A JavaScriptCore host for the parts of the original client that only exist as JS
/// (libsignal-protocol today; zproto wasm or sync workers later).
///
/// **Threading:** every JSC call runs on one private serial queue, so the context,
/// timers and native callbacks never race. Public methods are `async` and hop onto
/// that queue; results come back as `JSBridgeValue` (never `JSValue`).
///
/// **Module convention:** a module is a classic script (not an ES module) under the
/// resource directory `JSModules/`, loaded with `loadModule("name.js")`. It runs in the
/// shared global scope (`window === self === globalThis`) and publishes its API on a
/// global namespace object, e.g. `globalThis.ZaloE2EE = { encrypt(...) {...} }`.
/// Swift calls it by dotted path: `callAsync("ZaloE2EE.encrypt", [...])`. Functions may
/// return a Promise; `callAsync` awaits it. Pass bytes as `.bytes` (arrives as an
/// `ArrayBuffer`) and receive `ArrayBuffer`/typed arrays back as `.bytes`.
///
/// **Host APIs provided:** `console.*` → os.Logger, `setTimeout`/`setInterval`/`clear*`,
/// `queueMicrotask`, `btoa`/`atob`, `TextEncoder`/`TextDecoder` (UTF-8), a `navigator`
/// stub, `crypto.getRandomValues` (SecRandomCopyBytes) and a WebCrypto `crypto.subtle`
/// subset backed by CryptoKit/CommonCrypto: `importKey("raw")`, AES-CBC (PKCS#7)
/// `encrypt`/`decrypt`, HMAC `sign`/`verify` (SHA-256/512), `digest` (SHA-1/256/384/512).
final class JSRuntime: @unchecked Sendable {
    let name: String
    private let queue: DispatchQueue
    private let log: Logger
    private let jsLog: Logger

    // Queue-confined state.
    private var context: JSContext!
    private var pendingException: JSValue?
    private var timers: [Int: (fn: JSValue, interval: Int?)] = [:]
    private var nextTimerID = 1

    init(name: String) throws {
        self.name = name
        queue = DispatchQueue(label: "app.olaz.js.\(name)", qos: .userInitiated)
        log = Logger(subsystem: "app.olaz", category: "js-runtime")
        jsLog = Logger(subsystem: "app.olaz", category: "js.\(name)")
        try queue.sync { try bootstrap() }
    }

    // MARK: Public API

    /// Evaluates a script in the global scope and returns its completion value.
    @discardableResult
    func evaluate(_ script: String, sourceURL: String = "inline.js") async throws -> JSBridgeValue {
        try await perform { rt in
            let value = rt.context.evaluateScript(script, withSourceURL: URL(string: sourceURL))
            try rt.throwPendingException()
            return rt.bridge(value)
        }
    }

    /// Loads `JSModules/<relativePath>` from the resource bundle.
    func loadModule(_ relativePath: String) async throws {
        guard let url = ResourceLocator.url(for: relativePath, in: "JSModules") else {
            throw JSRuntimeError(message: "JS module not found: JSModules/\(relativePath)")
        }
        try await loadScript(at: url)
    }

    /// Loads an arbitrary script file (useful for tests and dev overrides).
    func loadScript(at url: URL) async throws {
        let source = try String(contentsOf: url, encoding: .utf8)
        let start = ContinuousClock.now
        try await evaluate(source, sourceURL: url.lastPathComponent)
        let name = url.lastPathComponent
        log.info("loaded \(name, privacy: .public) in \(String(describing: ContinuousClock.now - start), privacy: .public)")
    }

    /// Calls the function at a dotted global path (`"ZaloE2EE.encrypt"`) with `this`
    /// bound to its parent object. If it returns a thenable, awaits settlement.
    func callAsync(_ path: String, _ args: [JSBridgeValue] = []) async throws -> JSBridgeValue {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<JSBridgeValue, Error>) in
            queue.async { [weak self] in
                let once = ResumeOnce(cont)
                guard let self else {
                    once.resume(.failure(JSRuntimeError(message: "JSRuntime was deallocated")))
                    return
                }
                do {
                    let result = try self.invoke(path, args)
                    if result.isObject, let then = result.objectForKeyedSubscript("then"), JSRuntime.isFunction(then) {
                        let ok: @convention(block) (JSValue) -> Void = { [weak self] value in
                            once.resume(.success(self?.bridge(value) ?? .null))
                        }
                        let fail: @convention(block) (JSValue) -> Void = { reason in
                            once.resume(.failure(JSRuntime.error(from: reason)))
                        }
                        result.invokeMethod("then", withArguments: [
                            JSValue(object: ok, in: self.context)!,
                            JSValue(object: fail, in: self.context)!,
                        ])
                        try self.throwPendingException()
                    } else {
                        once.resume(.success(self.bridge(result)))
                    }
                } catch {
                    once.resume(.failure(error))
                }
            }
        }
    }

    // MARK: Queue helpers

    private func perform<T: Sendable>(_ body: @escaping @Sendable (JSRuntime) throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { cont in
            queue.async {
                do { cont.resume(returning: try body(self)) } catch { cont.resume(throwing: error) }
            }
        }
    }

    private func invoke(_ path: String, _ args: [JSBridgeValue]) throws -> JSValue {
        let parts = path.split(separator: ".").map(String.init)
        guard let last = parts.last else { throw JSRuntimeError(message: "Empty JS path") }
        var owner: JSValue = context.globalObject
        for part in parts.dropLast() {
            guard let next = owner.objectForKeyedSubscript(part), next.isObject else {
                throw JSRuntimeError(message: "JS path not found: \(path)")
            }
            owner = next
        }
        guard let fn = owner.objectForKeyedSubscript(last), fn.isObject else {
            throw JSRuntimeError(message: "JS function not found: \(path)")
        }
        let jsArgs = args.map { toJS($0) }
        let result = owner.invokeMethod(last, withArguments: jsArgs) ?? JSValue(undefinedIn: context)!
        try throwPendingException()
        return result
    }

    private func throwPendingException() throws {
        guard let exception = pendingException else { return }
        pendingException = nil
        throw JSRuntime.error(from: exception)
    }

    static func error(from value: JSValue) -> JSRuntimeError {
        if value.isObject, let message = value.objectForKeyedSubscript("message"), message.isString {
            let name = value.objectForKeyedSubscript("name")?.toString() ?? "Error"
            let stack = value.objectForKeyedSubscript("stack")
            return JSRuntimeError(
                message: "\(name): \(message.toString() ?? "")",
                stack: stack?.isString == true ? stack?.toString() : nil
            )
        }
        return JSRuntimeError(message: value.toString() ?? "Unknown JS error")
    }

    // MARK: Marshalling (queue-confined)

    func toJS(_ value: JSBridgeValue) -> JSValue {
        switch value {
        case .null: return JSValue(nullIn: context)
        case .bool(let b): return JSValue(bool: b, in: context)
        case .number(let n): return JSValue(double: n, in: context)
        case .string(let s): return JSValue(object: s, in: context)
        case .bytes(let d): return makeArrayBuffer(d)
        case .array(let items):
            let array = JSValue(newArrayIn: context)!
            for (i, item) in items.enumerated() { array.setObject(toJS(item), atIndexedSubscript: i) }
            return array
        case .object(let dict):
            let object = JSValue(newObjectIn: context)!
            for (k, v) in dict { object.setObject(toJS(v), forKeyedSubscript: k as NSString) }
            return object
        }
    }

    func bridge(_ value: JSValue?) -> JSBridgeValue {
        guard let value, !value.isUndefined, !value.isNull else { return .null }
        if value.isBoolean { return .bool(value.toBool()) }
        if value.isNumber { return .number(value.toDouble()) }
        if value.isString { return .string(value.toString()) }
        if let data = Self.bytes(of: value) { return .bytes(data) }
        if value.isArray {
            let count = Int(value.objectForKeyedSubscript("length").toInt32())
            return .array((0..<count).map { bridge(value.atIndex($0)) })
        }
        if value.isObject {
            if Self.isFunction(value) { return .null }
            let keys = context.objectForKeyedSubscript("Object").invokeMethod("keys", withArguments: [value])
            let count = Int(keys?.objectForKeyedSubscript("length").toInt32() ?? 0)
            var dict: [String: JSBridgeValue] = [:]
            for i in 0..<count {
                guard let key = keys?.atIndex(i).toString(),
                      let child = value.objectForKeyedSubscript(key),
                      !Self.isFunction(child) else { continue }
                dict[key] = bridge(child)
            }
            return .object(dict)
        }
        return .string(value.toString() ?? "")
    }

    static func isFunction(_ value: JSValue) -> Bool {
        guard value.isObject, let ctx = value.context?.jsGlobalContextRef,
              let object = JSValueToObject(ctx, value.jsValueRef, nil) else { return false }
        return JSObjectIsFunction(ctx, object)
    }

    /// Copies the bytes out of an `ArrayBuffer` or typed array view.
    static func bytes(of value: JSValue) -> Data? {
        guard value.isObject, let ctx = value.context?.jsGlobalContextRef else { return nil }
        let ref = value.jsValueRef
        let type = JSValueGetTypedArrayType(ctx, ref, nil)
        if type == kJSTypedArrayTypeNone { return nil }
        guard let object = JSValueToObject(ctx, ref, nil) else { return nil }
        if type == kJSTypedArrayTypeArrayBuffer {
            let length = JSObjectGetArrayBufferByteLength(ctx, object, nil)
            guard length > 0, let ptr = JSObjectGetArrayBufferBytesPtr(ctx, object, nil) else { return Data() }
            return Data(bytes: ptr, count: length)
        }
        // JSObjectGetTypedArrayBytesPtr points at the *buffer* start; add the view offset.
        let length = JSObjectGetTypedArrayByteLength(ctx, object, nil)
        let offset = JSObjectGetTypedArrayByteOffset(ctx, object, nil)
        guard length > 0, let buffer = JSObjectGetTypedArrayBuffer(ctx, object, nil),
              let base = JSObjectGetArrayBufferBytesPtr(ctx, buffer, nil) else { return Data() }
        return Data(bytes: base.advanced(by: offset), count: length)
    }

    private func makeArrayBuffer(_ data: Data) -> JSValue {
        let buffer = context.objectForKeyedSubscript("ArrayBuffer").construct(withArguments: [data.count])!
        if !data.isEmpty, let ctx = context.jsGlobalContextRef,
           let object = JSValueToObject(ctx, buffer.jsValueRef, nil),
           let ptr = JSObjectGetArrayBufferBytesPtr(ctx, object, nil) {
            data.withUnsafeBytes { ptr.copyMemory(from: $0.baseAddress!, byteCount: data.count) }
        }
        return buffer
    }

    // MARK: Timers (queue-confined)

    private func addTimer(_ fn: JSValue, delay: Int, repeats: Bool) -> Int {
        let id = nextTimerID
        nextTimerID += 1
        timers[id] = (fn, repeats ? max(delay, 1) : nil)
        schedule(id, after: delay)
        return id
    }

    private func schedule(_ id: Int, after ms: Int) {
        queue.asyncAfter(deadline: .now() + .milliseconds(max(ms, 0))) { [weak self] in
            self?.fire(id)
        }
    }

    private func fire(_ id: Int) {
        guard let timer = timers[id] else { return }
        if let interval = timer.interval { schedule(id, after: interval) } else { timers[id] = nil }
        timer.fn.call(withArguments: [])
        if let exception = pendingException {
            pendingException = nil
            let message = JSRuntime.error(from: exception).message
            log.error("uncaught exception in timer: \(message, privacy: .public)")
        }
    }

    // MARK: Bootstrap

    private func bootstrap() throws {
        guard let ctx = JSContext(virtualMachine: JSVirtualMachine()) else {
            throw JSRuntimeError(message: "Could not create JSContext")
        }
        context = ctx
        ctx.name = "Olaz · \(name)"
        ctx.exceptionHandler = { [weak self] _, exception in
            self?.pendingException = exception
        }
        installHost()
        ctx.evaluateScript(Self.prelude, withSourceURL: URL(string: "olaz-prelude.js"))
        try throwPendingException()
    }

    /// Native primitives exposed as `globalThis.__zhost`. They never throw into JS;
    /// on failure they *return* an `Error` object which the prelude rethrows.
    private func installHost() {
        let host = JSValue(newObjectIn: context)!
        func set(_ key: String, _ block: Any) {
            host.setObject(block, forKeyedSubscript: key as NSString)
        }
        func fail(_ message: String) -> JSValue {
            JSValue(newErrorFromMessage: message, in: JSContext.current())
        }
        func buffer(_ data: Data) -> JSValue {
            let ctx = JSContext.current()!
            let ab = ctx.objectForKeyedSubscript("ArrayBuffer").construct(withArguments: [data.count])!
            if !data.isEmpty, let g = ctx.jsGlobalContextRef,
               let o = JSValueToObject(g, ab.jsValueRef, nil),
               let p = JSObjectGetArrayBufferBytesPtr(g, o, nil) {
                data.withUnsafeBytes { p.copyMemory(from: $0.baseAddress!, byteCount: data.count) }
            }
            return ab
        }

        let jsLog = self.jsLog
        let logFn: @convention(block) (String, String) -> Void = { level, message in
            switch level {
            case "error": jsLog.error("\(message, privacy: .public)")
            case "warn": jsLog.warning("\(message, privacy: .public)")
            case "debug": jsLog.debug("\(message, privacy: .public)")
            default: jsLog.info("\(message, privacy: .public)")
            }
        }
        set("log", logFn)

        let random: @convention(block) (Int) -> JSValue = { count in
            var bytes = [UInt8](repeating: 0, count: max(count, 0))
            guard count <= 65536 else { return fail("QuotaExceededError: getRandomValues limit is 65536 bytes") }
            if count > 0, SecRandomCopyBytes(kSecRandomDefault, count, &bytes) != errSecSuccess {
                return fail("SecRandomCopyBytes failed")
            }
            return buffer(Data(bytes))
        }
        set("randomBytes", random)

        let aes: @convention(block) (Bool, JSValue, JSValue, JSValue) -> JSValue = { encrypt, key, iv, data in
            guard let k = JSRuntime.bytes(of: key), let v = JSRuntime.bytes(of: iv), let d = JSRuntime.bytes(of: data) else {
                return fail("TypeError: AES-CBC expects BufferSource arguments")
            }
            guard [16, 24, 32].contains(k.count) else { return fail("DataError: invalid AES key length \(k.count)") }
            guard v.count == kCCBlockSizeAES128 else { return fail("OperationError: AES-CBC iv must be 16 bytes") }
            switch JSRuntime.aesCBC(encrypt: encrypt, key: k, iv: v, data: d) {
            case .success(let out): return buffer(out)
            case .failure(let error): return fail(error.message)
            }
        }
        set("aesCbc", aes)

        let hmac: @convention(block) (String, JSValue, JSValue) -> JSValue = { hash, key, data in
            guard let k = JSRuntime.bytes(of: key), let d = JSRuntime.bytes(of: data) else {
                return fail("TypeError: HMAC expects BufferSource arguments")
            }
            let symmetric = SymmetricKey(data: k)
            switch hash {
            case "SHA-256": return buffer(Data(HMAC<SHA256>.authenticationCode(for: d, using: symmetric)))
            case "SHA-384": return buffer(Data(HMAC<SHA384>.authenticationCode(for: d, using: symmetric)))
            case "SHA-512": return buffer(Data(HMAC<SHA512>.authenticationCode(for: d, using: symmetric)))
            default: return fail("NotSupportedError: HMAC hash \(hash)")
            }
        }
        set("hmac", hmac)

        let digest: @convention(block) (String, JSValue) -> JSValue = { algorithm, data in
            guard let d = JSRuntime.bytes(of: data) else { return fail("TypeError: digest expects a BufferSource") }
            switch algorithm {
            case "SHA-1": return buffer(Data(Insecure.SHA1.hash(data: d)))
            case "SHA-256": return buffer(Data(SHA256.hash(data: d)))
            case "SHA-384": return buffer(Data(SHA384.hash(data: d)))
            case "SHA-512": return buffer(Data(SHA512.hash(data: d)))
            default: return fail("NotSupportedError: digest \(algorithm)")
            }
        }
        set("digest", digest)

        let utf8Encode: @convention(block) (String) -> JSValue = { buffer(Data($0.utf8)) }
        set("utf8Encode", utf8Encode)
        let utf8Decode: @convention(block) (JSValue) -> JSValue = { value in
            guard let d = JSRuntime.bytes(of: value) else { return fail("TypeError: decode expects a BufferSource") }
            return JSValue(object: String(decoding: d, as: UTF8.self), in: JSContext.current())
        }
        set("utf8Decode", utf8Decode)

        // btoa/atob operate on "binary strings" (one byte per UTF-16 unit, ≤ 0xFF).
        let btoa: @convention(block) (String) -> JSValue = { s in
            var bytes = [UInt8]()
            bytes.reserveCapacity(s.utf16.count)
            for unit in s.utf16 {
                guard unit <= 0xFF else { return fail("InvalidCharacterError: btoa input is not Latin-1") }
                bytes.append(UInt8(unit))
            }
            return JSValue(object: Data(bytes).base64EncodedString(), in: JSContext.current())
        }
        set("btoa", btoa)
        let atob: @convention(block) (String) -> JSValue = { s in
            let cleaned = s.filter { !$0.isWhitespace }
            let padded = cleaned + String(repeating: "=", count: (4 - cleaned.count % 4) % 4)
            guard let d = Data(base64Encoded: padded) else { return fail("InvalidCharacterError: atob input is not base64") }
            let units = d.map { UInt16($0) }
            return JSValue(object: String(utf16CodeUnits: units, count: units.count), in: JSContext.current())
        }
        set("atob", atob)

        let setTimer: @convention(block) (JSValue, Double, Bool) -> Int = { [weak self] fn, delay, repeats in
            self?.addTimer(fn, delay: delay.isFinite ? Int(delay) : 0, repeats: repeats) ?? 0
        }
        set("setTimer", setTimer)
        let clearTimer: @convention(block) (Int) -> Void = { [weak self] id in
            self?.timers[id] = nil
        }
        set("clearTimer", clearTimer)

        context.setObject(host, forKeyedSubscript: "__zhost" as NSString)
    }

    static func aesCBC(encrypt: Bool, key: Data, iv: Data, data: Data) -> Result<Data, JSRuntimeError> {
        var out = Data(count: data.count + kCCBlockSizeAES128)
        let outCapacity = out.count
        var moved = 0
        let status = out.withUnsafeMutableBytes { outPtr in
            key.withUnsafeBytes { keyPtr in
                iv.withUnsafeBytes { ivPtr in
                    data.withUnsafeBytes { dataPtr in
                        CCCrypt(
                            CCOperation(encrypt ? kCCEncrypt : kCCDecrypt),
                            CCAlgorithm(kCCAlgorithmAES),
                            CCOptions(kCCOptionPKCS7Padding),
                            keyPtr.baseAddress, key.count,
                            ivPtr.baseAddress,
                            dataPtr.baseAddress, data.count,
                            outPtr.baseAddress, outCapacity,
                            &moved
                        )
                    }
                }
            }
        }
        guard status == kCCSuccess else {
            return .failure(JSRuntimeError(message: "OperationError: AES-CBC \(encrypt ? "encrypt" : "decrypt") failed (\(status))"))
        }
        out.count = moved
        return .success(out)
    }

    /// Browser-ish globals built on `__zhost`. Kept tiny: only what JS modules need.
    private static let prelude = #"""
    (function (g) {
      'use strict';
      const host = g.__zhost;
      const check = (r) => { if (r instanceof Error) throw r; return r; };
      g.window = g; g.self = g; g.global = g;
      g.navigator = { userAgent: 'Olaz JavaScriptCore', language: 'vi-VN', hardwareConcurrency: 1, onLine: true };
      g.location = { href: 'olaz://js/', protocol: 'olaz:', host: 'js', origin: 'olaz://js' };

      const fmt = (args) => args.map((a) => {
        if (typeof a === 'string') return a;
        if (a instanceof Error) return a.name + ': ' + a.message + (a.stack ? '\n' + a.stack : '');
        try { return JSON.stringify(a); } catch (_) { return String(a); }
      }).join(' ');
      g.console = {
        log: (...a) => host.log('info', fmt(a)), info: (...a) => host.log('info', fmt(a)),
        debug: (...a) => host.log('debug', fmt(a)), warn: (...a) => host.log('warn', fmt(a)),
        error: (...a) => host.log('error', fmt(a)), trace: (...a) => host.log('debug', fmt(a)),
      };

      g.setTimeout = (fn, ms, ...args) => host.setTimer(() => fn(...args), Number(ms) || 0, false);
      g.setInterval = (fn, ms, ...args) => host.setTimer(() => fn(...args), Number(ms) || 0, true);
      g.clearTimeout = g.clearInterval = (id) => { if (id) host.clearTimer(id); };
      g.setImmediate = (fn, ...args) => g.setTimeout(fn, 0, ...args);
      g.queueMicrotask = g.queueMicrotask || ((fn) => Promise.resolve().then(fn));

      g.btoa = (s) => check(host.btoa(String(s)));
      g.atob = (s) => check(host.atob(String(s)));

      // Copies any BufferSource into a standalone ArrayBuffer (offset-safe).
      const toBuffer = (d) => {
        if (d instanceof ArrayBuffer) return d;
        if (ArrayBuffer.isView(d)) return d.buffer.slice(d.byteOffset, d.byteOffset + d.byteLength);
        throw new TypeError('Expected an ArrayBuffer or ArrayBufferView');
      };

      g.TextEncoder = class TextEncoder {
        get encoding() { return 'utf-8'; }
        encode(s = '') { return new Uint8Array(check(host.utf8Encode(String(s)))); }
      };
      g.TextDecoder = class TextDecoder {
        constructor(label = 'utf-8') {
          if (!/^utf-?8$/i.test(label)) throw new RangeError('Only utf-8 is supported');
        }
        get encoding() { return 'utf-8'; }
        decode(d) { return d === undefined ? '' : check(host.utf8Decode(toBuffer(d))); }
      };

      const algName = (a) => (typeof a === 'string' ? a : a && a.name || '').toUpperCase();
      const hashName = (h) => algName(h || 'SHA-256');
      const run = (fn) => new Promise((resolve) => resolve(fn()));
      const keyOf = (k, name) => {
        if (!k || !k.__raw) throw new TypeError('Expected a CryptoKey');
        if (name && k.algorithm.name !== name) throw new Error('InvalidAccessError: key algorithm mismatch');
        return k.__raw;
      };
      const subtle = {
        importKey(format, keyData, algorithm, extractable, usages) {
          return run(() => {
            if (format !== 'raw') throw new Error('NotSupportedError: importKey format ' + format);
            const name = algName(algorithm);
            const alg = { name };
            if (name === 'HMAC') alg.hash = { name: hashName(algorithm.hash) };
            else if (name !== 'AES-CBC') throw new Error('NotSupportedError: importKey algorithm ' + name);
            return Object.freeze({ type: 'secret', algorithm: alg, extractable: !!extractable,
                                   usages: usages || [], __raw: toBuffer(keyData).slice(0) });
          });
        },
        exportKey(format, key) {
          return run(() => {
            if (format !== 'raw' || !key.extractable) throw new Error('InvalidAccessError: key is not extractable');
            return key.__raw.slice(0);
          });
        },
        encrypt(algorithm, key, data) {
          return run(() => check(host.aesCbc(true, keyOf(key, 'AES-CBC'), toBuffer(algorithm.iv), toBuffer(data))));
        },
        decrypt(algorithm, key, data) {
          return run(() => check(host.aesCbc(false, keyOf(key, 'AES-CBC'), toBuffer(algorithm.iv), toBuffer(data))));
        },
        sign(algorithm, key, data) {
          return run(() => check(host.hmac(key.algorithm.hash.name, keyOf(key, 'HMAC'), toBuffer(data))));
        },
        verify(algorithm, key, signature, data) {
          return run(() => {
            const a = new Uint8Array(check(host.hmac(key.algorithm.hash.name, keyOf(key, 'HMAC'), toBuffer(data))));
            const b = new Uint8Array(toBuffer(signature));
            if (a.length !== b.length) return false;
            let diff = 0;
            for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
            return diff === 0;
          });
        },
        digest(algorithm, data) {
          return run(() => check(host.digest(algName(algorithm), toBuffer(data))));
        },
      };
      g.crypto = {
        subtle,
        getRandomValues(arr) {
          if (!ArrayBuffer.isView(arr) || arr instanceof Float32Array || arr instanceof Float64Array) {
            throw new TypeError('TypeMismatchError: getRandomValues expects an integer typed array');
          }
          const bytes = new Uint8Array(check(host.randomBytes(arr.byteLength)));
          new Uint8Array(arr.buffer, arr.byteOffset, arr.byteLength).set(bytes);
          return arr;
        },
        randomUUID() {
          const b = this.getRandomValues(new Uint8Array(16));
          b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80;
          const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
          return h.slice(0, 8) + '-' + h.slice(8, 12) + '-' + h.slice(12, 16) + '-' + h.slice(16, 20) + '-' + h.slice(20);
        },
      };
    })(globalThis);
    """#
}

/// Resumes a continuation at most once (a Promise may settle only once, but a
/// synchronous throw after `then` is attached must not double-resume).
private final class ResumeOnce: @unchecked Sendable {
    private var continuation: CheckedContinuation<JSBridgeValue, Error>?
    private let lock = NSLock()

    init(_ continuation: CheckedContinuation<JSBridgeValue, Error>) {
        self.continuation = continuation
    }

    func resume(_ result: Result<JSBridgeValue, Error>) {
        lock.lock()
        let c = continuation
        continuation = nil
        lock.unlock()
        c?.resume(with: result)
    }
}
