import Foundation
import CommonCrypto
import CryptoKit
import Security

/// `$znode.crypto` — the preload exposed a thin layer over Node's `crypto`
/// (see `preload-render.js`: `encrypt:(e,t,n,r=Buffer.alloc(16,0),a,i)=>A("encrypt",…)`).
/// The official renderer uses it for `zcid`/login-param encryption, request
/// signing and E2EE media; this reproduces Node's semantics on CommonCrypto/CryptoKit.
@MainActor
enum CryptoBridge {
    struct CryptoError: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    /// Bytes returned as an `ArrayBuffer` (the preload returned `buffer.buffer`).
    struct ArrayBufferValue { let data: Data }

    // MARK: Node encodings

    static func bytes(_ value: Any?, encoding: String?) throws -> Data {
        switch value {
        case let data as Data:
            return data
        case let string as String:
            switch (encoding ?? "utf8").lowercased() {
            case "utf8", "utf-8": return Data(string.utf8)
            case "hex":
                var out = Data(capacity: string.count / 2)
                var index = string.startIndex
                while let next = string.index(index, offsetBy: 2, limitedBy: string.endIndex), index < string.endIndex {
                    guard let byte = UInt8(string[index..<next], radix: 16) else { break }
                    out.append(byte)
                    index = next
                }
                return out
            case "base64": return Data(base64Encoded: string, options: .ignoreUnknownCharacters) ?? Data()
            case "base64url":
                var s = string.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
                while s.count % 4 != 0 { s += "=" }
                return Data(base64Encoded: s) ?? Data()
            case "binary", "latin1", "ascii": return Data(string.unicodeScalars.map { UInt8(truncatingIfNeeded: $0.value) })
            case "ucs2", "ucs-2", "utf16le", "utf-16le": return string.data(using: .utf16LittleEndian) ?? Data()
            default: throw CryptoError(message: "Unknown encoding: \(encoding ?? "")")
            }
        case nil, is NSNull:
            return Data()
        default:
            throw CryptoError(message: "Unsupported crypto input \(type(of: value!))")
        }
    }

    static func encode(_ data: Data, encoding: String?) -> Any {
        guard let encoding else { return ArrayBufferValue(data: data) }
        switch encoding.lowercased() {
        case "hex": return data.map { String(format: "%02x", $0) }.joined()
        case "base64": return data.base64EncodedString()
        case "base64url":
            return data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        case "binary", "latin1", "ascii": return String(data.map { Character(Unicode.Scalar($0)) })
        case "ucs2", "ucs-2", "utf16le", "utf-16le": return String(data: data, encoding: .utf16LittleEndian) ?? ""
        default: return String(decoding: data, as: UTF8.self)
        }
    }

    // MARK: Ciphers

    final class CipherSession {
        let encrypting: Bool
        let algorithm: String
        let key: Data
        let iv: Data
        var autoPadding = true
        var cryptor: CCCryptorRef?
        var buffered = Data()       // GCM: CryptoKit is one-shot
        var authTag: Data?
        var aad = Data()
        /// Decrypt + PKCS#7: the last block is held back until final() so the padding
        /// can be validated strictly, as OpenSSL does (CommonCrypto is more lenient).
        private var heldBack = Data()
        private var stripsPadding: Bool { !encrypting && autoPadding && (algorithm.hasSuffix("-cbc") || algorithm.hasSuffix("-ecb")) }

        init(encrypting: Bool, algorithm: String, key: Data, iv: Data) throws {
            self.encrypting = encrypting
            self.algorithm = algorithm.lowercased()
            self.key = key
            self.iv = iv
            if !isGCM { try makeCryptor() }
        }

        deinit { if let cryptor { CCCryptorRelease(cryptor) } }

        var isGCM: Bool { algorithm.hasSuffix("-gcm") }

        private func makeCryptor() throws {
            let mode: CCMode
            if algorithm.hasSuffix("-cbc") { mode = CCMode(kCCModeCBC) }
            else if algorithm.hasSuffix("-ctr") { mode = CCMode(kCCModeCTR) }
            else if algorithm.hasSuffix("-ecb") { mode = CCMode(kCCModeECB) }
            else { throw CryptoError(message: "Unsupported cipher \(algorithm)") }
            let expected = Int(algorithm.split(separator: "-")[1]).map { $0 / 8 } ?? key.count
            guard key.count == expected else { throw CryptoError(message: "Invalid key length") }
            if mode != CCMode(kCCModeECB), iv.count != kCCBlockSizeAES128 { throw CryptoError(message: "Invalid initialization vector") }
            let blockMode = mode == CCMode(kCCModeCBC) || mode == CCMode(kCCModeECB)
            let padding = blockMode && autoPadding && encrypting ? CCPadding(ccPKCS7Padding) : CCPadding(ccNoPadding)
            var ref: CCCryptorRef?
            let status = key.withUnsafeBytes { k in
                iv.withUnsafeBytes { v in
                    CCCryptorCreateWithMode(
                        CCOperation(encrypting ? kCCEncrypt : kCCDecrypt), mode, CCAlgorithm(kCCAlgorithmAES),
                        padding, mode == CCMode(kCCModeECB) ? nil : v.baseAddress,
                        k.baseAddress, key.count, nil, 0, 0,
                        mode == CCMode(kCCModeCTR) ? CCModeOptions(kCCModeOptionCTR_BE) : 0, &ref)
                }
            }
            guard status == kCCSuccess else { throw CryptoError(message: "cipher init failed (\(status))") }
            cryptor = ref
        }

        func setAutoPadding(_ on: Bool) throws {
            autoPadding = on
            if !isGCM, let cryptor {
                CCCryptorRelease(cryptor)
                self.cryptor = nil
                try makeCryptor()
            }
        }

        func update(_ input: Data) throws -> Data {
            if isGCM { buffered.append(input); return Data() }
            guard let cryptor else { return Data() }
            var out = Data(count: CCCryptorGetOutputLength(cryptor, input.count, false))
            var moved = 0
            let status = out.withUnsafeMutableBytes { o in
                input.withUnsafeBytes { i in
                    CCCryptorUpdate(cryptor, i.baseAddress, input.count, o.baseAddress, o.count, &moved)
                }
            }
            guard status == kCCSuccess else { throw CryptoError(message: "cipher update failed (\(status))") }
            guard stripsPadding else { return out.prefix(moved) }
            var available = heldBack + out.prefix(moved)
            let keep = min(available.count, kCCBlockSizeAES128)
            heldBack = available.suffix(keep)
            available.removeLast(keep)
            return available
        }

        func final() throws -> Data {
            if isGCM { return try finishGCM() }
            guard let cryptor else { return Data() }
            var out = Data(count: CCCryptorGetOutputLength(cryptor, 0, true))
            var moved = 0
            let status = out.withUnsafeMutableBytes { CCCryptorFinal(cryptor, $0.baseAddress, $0.count, &moved) }
            guard status == kCCSuccess else { throw CryptoError(message: encrypting ? "cipher final failed" : "bad decrypt") }
            guard stripsPadding else { return out.prefix(moved) }
            let block = heldBack + out.prefix(moved)
            heldBack = Data()
            guard block.count == kCCBlockSizeAES128, let pad = block.last, (1...16).contains(Int(pad)),
                  block.suffix(Int(pad)).allSatisfy({ $0 == pad }) else {
                throw CryptoError(message: "bad decrypt")
            }
            return block.prefix(block.count - Int(pad))
        }

        private func finishGCM() throws -> Data {
            let symmetric = SymmetricKey(data: key)
            let nonce = try AES.GCM.Nonce(data: iv)
            if encrypting {
                let sealed = try AES.GCM.seal(buffered, using: symmetric, nonce: nonce, authenticating: aad)
                authTag = sealed.tag
                return sealed.ciphertext
            }
            guard let authTag else { throw CryptoError(message: "Unsupported state or unable to authenticate data") }
            let box = try AES.GCM.SealedBox(nonce: nonce, ciphertext: buffered, tag: authTag)
            return try AES.GCM.open(box, using: symmetric, authenticating: aad)
        }
    }

    static var debugResponses = ProcessInfo.processInfo.environment["OLAZ_DEBUG_RESPONSES"] != nil
    private static var sessions: [Int: CipherSession] = [:]
    private static var hashes: [Int: (String, Data)] = [:]
    private static var nextHandle = 1

    static func oneShot(encrypting: Bool, _ args: [Any]) throws -> Any {
        // (data, algorithm, key, iv = zeros(16), encodings?, options?)
        let encodings = args.dict(4) ?? [:]
        let algorithm = args.string(1) ?? "aes-256-cbc"
        let keyEncoding = encodings["key"] as? String
        let key = try bytes(args.count > 2 ? args[2] : nil, encoding: keyEncoding ?? (args.string(2) != nil ? "utf8" : nil))
        let ivArg: Any? = args.count > 3 && !(args[3] is NSNull) ? args[3] : nil
        let iv = ivArg == nil ? Data(count: 16) : try bytes(ivArg, encoding: (encodings["iv"] != nil ? keyEncoding : nil) ?? "utf8")
        let input = try bytes(args.first, encoding: encodings["input"] as? String)
        let session = try CipherSession(encrypting: encrypting, algorithm: algorithm, key: key, iv: iv)
        var out = try session.update(input)
        out.append(try session.final())
        if !encrypting, debugResponses, let json = try? JSONSerialization.jsonObject(with: out) as? [String: Any] {
            // Shape only — never values (they can contain session tokens).
            let data = json["data"] as? [String: Any]
            print("[crypto] decrypted keys=\(json.keys.sorted()) error_code=\(json["error_code"] ?? "-") data.keys=\(data?.keys.sorted() ?? [])")
        }
        return encode(out, encoding: encodings["output"] as? String)
    }

    static func digest(_ algorithm: String, _ data: Data) throws -> Data {
        switch algorithm.lowercased().replacingOccurrences(of: "-", with: "") {
        case "md5": return Data(Insecure.MD5.hash(data: data))
        case "sha1": return Data(Insecure.SHA1.hash(data: data))
        case "sha256": return Data(SHA256.hash(data: data))
        case "sha384": return Data(SHA384.hash(data: data))
        case "sha512": return Data(SHA512.hash(data: data))
        default: throw CryptoError(message: "Digest method not supported: \(algorithm)")
        }
    }

    static func install(on bridge: ZaloBridge) {
        bridge.sync("$znode.crypto.encrypt") { args in try oneShot(encrypting: true, args) }
        bridge.sync("$znode.crypto.decrypt") { args in try oneShot(encrypting: false, args) }
        // hash(data, algorithm, inputEncoding?, outputEncoding?, options?)
        bridge.sync("$znode.crypto.hash") { args in
            let data = try bytes(args.first, encoding: args.string(2))
            return encode(try digest(args.string(1) ?? "sha256", data), encoding: args.string(3))
        }
        bridge.sync("$znode.crypto.randomBytes") { args in
            var data = Data(count: max(0, args.int(0) ?? 0))
            _ = data.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, $0.count, $0.baseAddress!) }
            return ArrayBufferValue(data: data)
        }

        // Stateful cipher/hash objects — the JS side (local-modules.js) holds a handle.
        bridge.sync("$znode.crypto.__cipherCreate") { args in
            let encrypting = args.string(0) == "cipher"
            let algorithm = args.string(1) ?? ""
            let key = try bytes(args.count > 2 ? args[2] : nil, encoding: "utf8")
            let iv = try bytes(args.count > 3 ? args[3] : nil, encoding: "utf8")
            let session = try CipherSession(encrypting: encrypting, algorithm: algorithm, key: key, iv: iv)
            let handle = nextHandle
            nextHandle += 1
            sessions[handle] = session
            return handle
        }
        bridge.sync("$znode.crypto.__cipherUpdate") { args in
            guard let session = sessions[args.int(0) ?? 0] else { throw CryptoError(message: "cipher released") }
            let out = try session.update(try bytes(args.count > 1 ? args[1] : nil, encoding: args.string(2)))
            return encode(out, encoding: args.string(3))
        }
        bridge.sync("$znode.crypto.__cipherFinal") { args in
            guard let session = sessions[args.int(0) ?? 0] else { throw CryptoError(message: "cipher released") }
            defer { if !session.isGCM || !session.encrypting { sessions[args.int(0) ?? 0] = nil } }
            return encode(try session.final(), encoding: args.string(1))
        }
        bridge.sync("$znode.crypto.__cipherOption") { args in
            guard let session = sessions[args.int(0) ?? 0] else { return nil }
            switch args.string(1) {
            case "autoPadding": try session.setAutoPadding(args.bool(2) ?? true)
            case "authTag": session.authTag = try bytes(args.count > 2 ? args[2] : nil, encoding: nil)
            case "aad": session.aad = try bytes(args.count > 2 ? args[2] : nil, encoding: nil)
            case "getAuthTag":
                defer { sessions[args.int(0) ?? 0] = nil }
                return session.authTag.map { ArrayBufferValue(data: $0) }
            default: break
            }
            return nil
        }
        bridge.sync("$znode.crypto.__hashCreate") { args in
            let handle = nextHandle
            nextHandle += 1
            hashes[handle] = (args.string(0) ?? "sha256", Data())
            return handle
        }
        bridge.sync("$znode.crypto.__hashUpdate") { args in
            guard let entry = hashes[args.int(0) ?? 0] else { throw CryptoError(message: "Digest already called") }
            hashes[args.int(0) ?? 0] = (entry.0, entry.1 + (try bytes(args.count > 1 ? args[1] : nil, encoding: args.string(2))))
            return nil
        }
        bridge.sync("$znode.crypto.__hashDigest") { args in
            guard let entry = hashes.removeValue(forKey: args.int(0) ?? 0) else { throw CryptoError(message: "Digest already called") }
            return encode(try digest(entry.0, entry.1), encoding: args.string(1))
        }
    }
}
