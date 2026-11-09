import Foundation

/// A Signal-protocol ciphertext as produced by libsignal's `SessionCipher.encrypt`.
struct E2EEEnvelope: Codable, Hashable, Sendable {
    /// 3 = PreKeyWhisperMessage (session setup), 1 = WhisperMessage.
    var type: Int
    /// Base64 ciphertext.
    var body: String
    var registrationId: Int

    var isPreKeyMessage: Bool { type == 3 }
}

/// Real end-to-end encryption via the original client's libsignal-protocol build,
/// running in JavaScriptCore (`JSModules/vendor/libsignal-protocol.js` +
/// `JSModules/zalo-e2ee.js`). Crypto primitives come from CryptoKit/CommonCrypto
/// through `JSRuntime`'s WebCrypto shim; Curve25519 is libsignal's own asm.js.
///
/// Identities are plain strings (e.g. a user UUID string) and live in memory for the
/// lifetime of this instance.
actor SignalE2EE {
    private let runtime: JSRuntime

    private init(runtime: JSRuntime) {
        self.runtime = runtime
    }

    /// Boots a dedicated JS runtime and loads the vendored library plus glue.
    static func load() async throws -> SignalE2EE {
        let runtime = try JSRuntime(name: "signal")
        try await runtime.loadModule("vendor/libsignal-protocol.js")
        try await runtime.loadModule("zalo-e2ee.js")
        return SignalE2EE(runtime: runtime)
    }

    /// Idempotent: generates identity key, signed prekey and a batch of one-time prekeys.
    func registerIdentity(_ id: String) async throws {
        _ = try await runtime.callAsync("ZaloE2EE.registerIdentity", [.string(id)])
    }

    /// Encrypts from `from` to `to`, building a session from `to`'s prekey bundle
    /// when none is open (first message is then a type-3 PreKeyWhisperMessage).
    func encrypt(_ plaintext: String, from: String, to: String) async throws -> E2EEEnvelope {
        let result = try await runtime.callAsync(
            "ZaloE2EE.encrypt", [.string(plaintext), .string(from), .string(to)]
        )
        guard let type = result["type"]?.intValue,
              let body = result["body"]?.stringValue,
              let registrationId = result["registrationId"]?.intValue else {
            throw JSRuntimeError(message: "Malformed envelope from ZaloE2EE.encrypt: \(result)")
        }
        return E2EEEnvelope(type: type, body: body, registrationId: registrationId)
    }

    func decrypt(_ envelope: E2EEEnvelope, at recipient: String, from sender: String) async throws -> String {
        let payload: JSBridgeValue = .object([
            "type": .number(Double(envelope.type)),
            "body": .string(envelope.body),
            "registrationId": .number(Double(envelope.registrationId)),
        ])
        let result = try await runtime.callAsync(
            "ZaloE2EE.decrypt", [payload, .string(recipient), .string(sender)]
        )
        guard let text = result.stringValue else {
            throw JSRuntimeError(message: "ZaloE2EE.decrypt returned a non-string")
        }
        return text
    }

    /// Both identities must be registered. 60 digits in groups of five; symmetric.
    func safetyNumber(_ a: String, _ b: String) async throws -> String {
        let result = try await runtime.callAsync("ZaloE2EE.safetyNumber", [.string(a), .string(b)])
        guard let number = result.stringValue else {
            throw JSRuntimeError(message: "ZaloE2EE.safetyNumber returned a non-string")
        }
        return number
    }
}
