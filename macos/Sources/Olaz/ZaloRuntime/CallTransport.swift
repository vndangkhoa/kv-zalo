import Foundation

// Wire format between the Zalo main process and its native call helper (`ZaloCall`, zcall-v2).
// Source: `main-dist/main.js` modules "3Zc2" (call transport), "vqv6" (encrypt / decrypt) and
// "4KIH" (buildListMsgs); key: module "5NUA" `ZPC_SECRET_VALUES.callV2TransportAesKey`.

enum CallTransportCrypto {
    /// `callV2TransportAesKey` (base64 of a 16-byte AES key), a constant of the official client
    /// (read by `zsecureSecrets().getCallV2TransportAesKey()` in main).
    private static let keyBase64 = "yjAF9oqMWl6XfXYJn9mA7w=="

    static var defaultKey: Data { Data(base64Encoded: keyBase64) ?? Data() }
    /// `Buffer.from("0".repeat(32), "hex")`: sixteen zero bytes.
    private static let iv = Data(count: 16)

    /// `encrypt(text, key)`: AES-128-CBC (key = base64-decoded secret, zero IV, PKCS#7), lower-case hex.
    static func encrypt(_ text: String, key: Data = defaultKey) throws -> String {
        let sealed = try AESCBC.crypt(Data(text.utf8), key: key, iv: iv, encrypt: true)
        return sealed.map { String(format: "%02x", $0) }.joined()
    }

    /// `decrypt(hex, key)`: the inverse; nil when the frame is not valid hex / padding / UTF-8.
    static func decrypt(_ hex: String, key: Data = defaultKey) -> String? {
        guard let bytes = hexData(hex), !bytes.isEmpty, bytes.count % 16 == 0,
              let plain = try? AESCBC.crypt(bytes, key: key, iv: iv, encrypt: false) else { return nil }
        return String(data: plain, encoding: .utf8)
    }

    static func hexData(_ hex: String) -> Data? {
        let chars = Array(hex.utf8)
        guard chars.count % 2 == 0 else { return nil }
        var out = Data(capacity: chars.count / 2)
        func nibble(_ c: UInt8) -> UInt8? {
            switch c {
            case 48...57: return c - 48
            case 97...102: return c - 87
            case 65...70: return c - 55
            default: return nil
            }
        }
        var i = 0
        while i < chars.count {
            guard let hi = nibble(chars[i]), let lo = nibble(chars[i + 1]) else { return nil }
            out.append(hi << 4 | lo)
            i += 2
        }
        return out
    }
}

/// `buildListMsgs`: payloads longer than 4000 characters are cut into
/// `<chunk>#<sequence>#<count>#<index>#$` pieces; a single piece is `<payload>$`.
struct CallChunker {
    static let chunkSize = 4000
    /// `r` in module 4KIH: starts at 1 and becomes `max(r + 1, Date.now())` per message.
    private var sequence = 1

    init() {}

    mutating func chunks(of payload: String, now: Date = .now) -> [String] {
        let count = (payload.utf8.count + Self.chunkSize - 1) / Self.chunkSize
        sequence = max(sequence + 1, Int(now.timeIntervalSince1970 * 1000))
        // `payload` is lower-case hex (ASCII): UTF-16 offsets equal byte offsets.
        if count <= 1 { return [payload + "$"] }
        let bytes = Array(payload.utf8)
        return (0..<count).map { index in
            let slice = bytes[(index * Self.chunkSize)..<min(bytes.count, (index + 1) * Self.chunkSize)]
            return String(decoding: slice, as: UTF8.self) + "#\(sequence)#\(count)#\(index)#$"
        }
    }
}

/// Splits the native → main byte stream on `$` (the official side does `data.toString().split(/\$/)`, but a
/// frame may straddle two reads, so incomplete tails are buffered until their `$` arrives).
struct CallFrameDecoder {
    private var buffer = Data()
    private let terminator = UInt8(ascii: "$")

    /// Messages (decrypted JSON objects) completed by `data`. Frames that fail to decrypt or parse are dropped
    /// (`cannot parse payload`); `dropped` counts them.
    mutating func feed(_ data: Data, key: Data = CallTransportCrypto.defaultKey, dropped: inout Int) -> [[String: Any]] {
        buffer.append(data)
        var messages: [[String: Any]] = []
        while let end = buffer.firstIndex(of: terminator) {
            let frame = buffer[buffer.startIndex..<end]
            buffer.removeSubrange(buffer.startIndex...end)
            guard !frame.isEmpty else { continue }
            if let message = Self.parse(frame, key: key) { messages.append(message) } else { dropped += 1 }
        }
        // An unterminated tail waits for its `$`.
        if buffer.count > 4_000_000 {
            buffer.removeAll()
            dropped += 1
        }
        return messages
    }

    mutating func reset() { buffer.removeAll() }

    /// `decrypt(frame, R)` → strip NULs → `JSON.parse`.
    static func parse(_ frame: some DataProtocol, key: Data) -> [String: Any]? {
        let text = String(decoding: Array(frame), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        guard let plain = CallTransportCrypto.decrypt(text, key: key) else { return nil }
        let cleaned = plain.replacingOccurrences(of: "\0", with: "")
        guard let data = cleaned.data(using: .utf8),
              let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return nil }
        return object
    }
}
