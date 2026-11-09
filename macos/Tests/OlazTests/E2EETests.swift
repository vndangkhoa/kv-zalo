import Testing
import Foundation
@testable import Olaz

@Suite struct JSRuntimeShims {
    @Test func webCryptoSubsetMatchesKnownVectors() async throws {
        let rt = try JSRuntime(name: "test")
        // SHA-256("abc") and HMAC-SHA256(key="key", "The quick brown fox jumps over the lazy dog")
        try await rt.evaluate("""
        globalThis.T = {
          hex: (b) => Array.from(new Uint8Array(b), (x) => x.toString(16).padStart(2, '0')).join(''),
          async sha() { return T.hex(await crypto.subtle.digest('SHA-256', new TextEncoder().encode('abc'))); },
          async mac() {
            const k = await crypto.subtle.importKey('raw', new TextEncoder().encode('key'),
              { name: 'HMAC', hash: { name: 'SHA-256' } }, false, ['sign']);
            return T.hex(await crypto.subtle.sign({ name: 'HMAC' }, k,
              new TextEncoder().encode('The quick brown fox jumps over the lazy dog')));
          },
          async aes() {
            const key = crypto.getRandomValues(new Uint8Array(32)), iv = crypto.getRandomValues(new Uint8Array(16));
            const k = await crypto.subtle.importKey('raw', key, { name: 'AES-CBC' }, false, ['encrypt', 'decrypt']);
            const ct = await crypto.subtle.encrypt({ name: 'AES-CBC', iv }, k, new TextEncoder().encode('xin chào 💙'));
            return new TextDecoder().decode(await crypto.subtle.decrypt({ name: 'AES-CBC', iv }, k, ct));
          },
          timer() { return new Promise((r) => setTimeout(() => r('fired'), 10)); },
        };
        """)
        #expect(try await rt.callAsync("T.sha") == .string("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"))
        #expect(try await rt.callAsync("T.mac") == .string("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"))
        #expect(try await rt.callAsync("T.aes") == .string("xin chào 💙"))
        #expect(try await rt.callAsync("T.timer") == .string("fired"))
        #expect(try await rt.evaluate("btoa(atob('AAEC/w=='))") == .string("AAEC/w=="))
    }

    @Test func bytesAndExceptionsCrossTheBridge() async throws {
        let rt = try JSRuntime(name: "test")
        try await rt.evaluate("""
        globalThis.T = {
          rev: (b) => new Uint8Array(b).reverse().subarray(1),   // offset view
          boom() { throw new RangeError('nope'); },
          reject: () => Promise.reject(new Error('later')),
        };
        """)
        #expect(try await rt.callAsync("T.rev", [.bytes(Data([1, 2, 3, 4]))]) == .bytes(Data([3, 2, 1])))
        await #expect(throws: JSRuntimeError.self) { try await rt.callAsync("T.boom") }
        await #expect(throws: JSRuntimeError.self) { try await rt.callAsync("T.reject") }
        await #expect(throws: JSRuntimeError.self) { try await rt.callAsync("T.missing") }
    }
}

@Suite(.serialized) struct SignalE2EETests {
    @Test func roundTripEstablishesSessionThenRatchets() async throws {
        let e2ee = try await SignalE2EE.load()
        try await e2ee.registerIdentity("alice")
        try await e2ee.registerIdentity("bob")
        try await e2ee.registerIdentity("alice") // idempotent

        let first = try await e2ee.encrypt("Chào Bob 👋", from: "alice", to: "bob")
        #expect(first.type == 3)
        #expect(Data(base64Encoded: first.body) != nil)
        #expect(try await e2ee.decrypt(first, at: "bob", from: "alice") == "Chào Bob 👋")

        let reply = try await e2ee.encrypt("Chào Alice!", from: "bob", to: "alice")
        #expect(reply.type == 1)
        #expect(try await e2ee.decrypt(reply, at: "alice", from: "bob") == "Chào Alice!")

        for i in 0..<5 {
            let text = "Tin nhắn số \(i) — mã hóa đầu cuối"
            let env = try await e2ee.encrypt(text, from: "alice", to: "bob")
            #expect(env.type == 1)
            #expect(try await e2ee.decrypt(env, at: "bob", from: "alice") == text)
        }
    }

    @Test func safetyNumberIsSixtyDigitsAndSymmetric() async throws {
        let e2ee = try await SignalE2EE.load()
        try await e2ee.registerIdentity("me")
        try await e2ee.registerIdentity("minh-anh")
        let ab = try await e2ee.safetyNumber("me", "minh-anh")
        let ba = try await e2ee.safetyNumber("minh-anh", "me")
        let digits = ab.filter { !$0.isWhitespace }
        #expect(digits.count == 60)
        #expect(digits.allSatisfy { $0.isNumber })
        #expect(ab.split(separator: " ").count == 12)
        #expect(ab == ba)
    }

    @Test func tamperedCiphertextIsRejected() async throws {
        let e2ee = try await SignalE2EE.load()
        try await e2ee.registerIdentity("carol")
        try await e2ee.registerIdentity("dave")
        let setup = try await e2ee.encrypt("hello", from: "carol", to: "dave")
        _ = try await e2ee.decrypt(setup, at: "dave", from: "carol")
        _ = try await e2ee.decrypt(try await e2ee.encrypt("ack", from: "dave", to: "carol"), at: "carol", from: "dave")

        var envelope = try await e2ee.encrypt("bí mật", from: "carol", to: "dave")
        #expect(envelope.type == 1)
        var bytes = try #require(Data(base64Encoded: envelope.body))
        bytes[bytes.count / 2] ^= 0x5A
        envelope.body = bytes.base64EncodedString()
        await #expect(throws: (any Error).self) {
            try await e2ee.decrypt(envelope, at: "dave", from: "carol")
        }
    }
}
