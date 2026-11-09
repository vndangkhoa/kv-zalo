import Testing
import Foundation
import CryptoKit
@testable import Olaz

@Suite struct MediaDiagnosticsTests {
    @Test func classifiesFailures() {
        #expect(MediaDiagnostics.classify(MediaLoader.LoadError.http(404)) == "http-404")
        #expect(MediaDiagnostics.classify(MediaLoader.LoadError.hostNotFound) == "dns")
        #expect(MediaDiagnostics.classify(URLError(.cannotFindHost)) == "dns")
        #expect(MediaDiagnostics.classify(URLError(.timedOut)) == "timeout")
        #expect(MediaDiagnostics.classify(URLError(.notConnectedToInternet)) == "offline")
        #expect(MediaDiagnostics.classify(URLError(.serverCertificateUntrusted)) == "tls")
        #expect(MediaDiagnostics.classify(MediaLoader.LoadError.undecodable) == "undecodable")
        #expect(MediaDiagnostics.classify(DoHResolver.ResolveError.notFound("x")) == "dns")
        #expect(MediaDiagnostics.classify(CancellationError()) == nil)
        #expect(MediaDiagnostics.classify(URLError(.cancelled)) == nil)
    }

    @Test func limiterDropsRepeatsAndCountsThem() {
        var limiter = MediaDiagnostics.Limiter(window: 60, maxPerMinute: 3)
        let t0 = Date(timeIntervalSince1970: 1_000)
        #expect(limiter.admit("a", now: t0) == 0)
        #expect(limiter.admit("a", now: t0.addingTimeInterval(1)) == nil)
        #expect(limiter.admit("a", now: t0.addingTimeInterval(2)) == nil)
        #expect(limiter.admit("a", now: t0.addingTimeInterval(61)) == 2)       // reports how many were dropped
        #expect(limiter.admit("b", now: t0.addingTimeInterval(62)) == 0)
        #expect(limiter.admit("c", now: t0.addingTimeInterval(63)) == 0)
        #expect(limiter.admit("d", now: t0.addingTimeInterval(64)) == nil)     // per-minute cap (3 since t0+0) reached
        #expect(limiter.admit("d", now: t0.addingTimeInterval(130)) == 1)      // new minute; one drop reported
    }

    @Test func lineCarriesHostAndClassOnly() {
        let line = MediaDiagnostics.line(kind: "image", host: "f28-zpc.zdn.vn", failureClass: "dns", suppressed: 4)
        #expect(line == "media fail kind=image host=f28-zpc.zdn.vn class=dns suppressed=4")
    }
}

private func tempDir(_ name: String = "t") throws -> URL {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("\(name)-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

private func hasQuarantine(_ url: URL) -> Bool { getxattr(url.path, "com.apple.quarantine", nil, 0, 0, 0) >= 0 }

@Suite struct DownloadManagerTests {
    @Test func errorMappingMatchesOfficialCodes() {
        #expect(DownloadManager.Failure.from(MediaLoader.LoadError.http(404)) == .http(404))
        #expect(DownloadManager.Failure.http(404).code == 75404)
        #expect(DownloadManager.Failure.http(404).name == "DOWNLOAD_SERVICE_NET_404")
        #expect(DownloadManager.Failure.from(MediaLoader.LoadError.hostNotFound).code == 30034)
        #expect(DownloadManager.Failure.from(URLError(.cannotFindHost)).name == "ERR_NAME_NOT_RESOLVED")
        #expect(DownloadManager.Failure.from(URLError(.timedOut)).code == 30047)
        #expect(DownloadManager.Failure.from(CancellationError()) == .aborted)
        #expect(DownloadManager.Failure.http(401).json["normalize"] as? Bool == true)
        // main-dist DownloadErrors: UI_OBJ_DOWNLOAD_INVALID_URL=9, MAIN_CONTROLLER_OBJ_DOWNLOAD_SAVE_DIR_INVALID=1012,
        // MAIN_CONTROLLER_OBJ_DOWNLOAD_ABORT=1009, SINGLE_DOWNLOAD_ON_EMPTY_ERR=5025 (+75000), SINGLE_FILE_SIZE_LIMIT_EXCEED=5026.
        #expect(DownloadManager.Failure.invalidURL.code == 9 && DownloadManager.Failure.invalidURL.name == "GENERAL_UI_OBJ_DOWNLOAD_INVALID_URL")
        #expect(DownloadManager.Failure.saveDirInvalid.code == 1012)
        #expect(DownloadManager.Failure.saveDirInvalid.name == "GENERAL_MAIN_CONTROLLER_OBJ_DOWNLOAD_SAVE_DIR_INVALID")
        #expect(DownloadManager.Failure.aborted.code == 1009)
        #expect(DownloadManager.Failure.unknown.code == 80025)
        #expect(DownloadManager.Failure.unknown.name == "DOWNLOAD_SERVICE_SINGLE_DOWNLOAD_ON_EMPTY_ERR")
        #expect(DownloadManager.Failure.from(MediaLoader.LoadError.tooLarge).code == 75000 + 5026)
        // A decrypt failure is a plain Error in main: normalizeError → INS_OF_ERROR_ERR (14998).
        #expect(DownloadManager.Failure.decryptFailed.code == 14998 && DownloadManager.Failure.decryptFailed.name == "INS_OF_ERROR_ERR")
    }

    @Test func destinationIsASingleComponent() {
        #expect(DownloadManager.destination(saveDir: "/tmp/x", fileName: "../../etc/passwd")?.path == "/tmp/x/passwd")
        #expect(DownloadManager.destination(saveDir: "/tmp/x", fileName: "")?.lastPathComponent == "no_name")
        #expect(DownloadManager.destination(saveDir: "relative", fileName: "a") == nil)
        #expect(DownloadManager.destination(saveDir: nil, fileName: "a") == nil)
    }

    @Test func headersFromOptionsAndExtraHeader() {
        let request: [String: Any] = [
            "options": ["headers": [["name": "cloud-viewer-key", "value": "k"], ["name": "", "value": "x"]]],
            "extraHeader": ["X-A": "1", "pending": ["code": 5]],
        ]
        #expect(DownloadManager.requestHeaders(request) == ["cloud-viewer-key": "k", "X-A": "1"])
    }

    @MainActor @Test func rejectsBadRequestsWithoutNetwork() async throws {
        let dir = try tempDir("dm")
        defer { try? FileManager.default.removeItem(at: dir) }
        let platform = ZaloPlatform(appVersion: "test", userData: dir)
        let manager = DownloadManager(platform: platform, tempDirectory: dir.appendingPathComponent("tmp"))
        let badURL = await manager.perform(["type": "Photo", "data": ["url": "ftp://x/y.jpg", "fileName": "a.jpg"], "options": ["saveDir": dir.path]])
        #expect(badURL.failure == .invalidURL)
        let outside = await manager.perform(["type": "Photo", "data": ["url": "https://example.invalid/a.jpg", "fileName": "a.jpg"], "options": ["saveDir": "/etc"]])
        #expect(outside.failure == .saveDirInvalid)
        // An existing non-empty file is reported as cached, no request made; an empty one is not.
        let target = dir.appendingPathComponent("ZaloDownloads/p", isDirectory: true)
        try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
        try Data("x".utf8).write(to: target.appendingPathComponent("a.jpg"))
        let cached = await manager.perform(["type": "Photo", "data": ["url": "https://example.invalid/a.jpg", "fileName": "a.jpg"], "options": ["saveDir": target.path]])
        #expect(cached.cached && cached.failure == nil)
        #expect(!DownloadManager.isUsableFile(target.appendingPathComponent("missing.jpg")))
        try Data().write(to: target.appendingPathComponent("empty.jpg"))
        #expect(!DownloadManager.isUsableFile(target.appendingPathComponent("empty.jpg")))
    }

    // 1: a `secure` the transformer cannot be built from must fail the download, never save ciphertext.
    @MainActor @Test func unparseableSecureFailsInsteadOfSavingCiphertext() async throws {
        let dir = try tempDir("dm")
        defer { try? FileManager.default.removeItem(at: dir) }
        let manager = DownloadManager(platform: ZaloPlatform(appVersion: "test", userData: dir), tempDirectory: dir.appendingPathComponent("tmp"))
        let target = dir.appendingPathComponent("ZaloDownloads/p", isDirectory: true)
        let key = Data(repeating: 1, count: 32), iv = Data(repeating: 2, count: 16)
        let bad: [[String: Any]] = [
            ["type": "pcloud", "cryptography": ["alg": "AES-GCM", "type": "linear", "key": key, "iv": iv]],          // WebCrypto spelling
            ["type": "pcloud", "cryptography": ["alg": "aes-256-ctr", "type": "block", "key": Data(count: 31), "iv": iv]],
            ["type": "e2ee", "cryptography": ["alg": "aes-256-ctr", "type": "block", "key": key]],                   // no iv
            ["type": "e2ee", "cryptography": ["alg": "aes-256-gcm", "type": "linear", "key": key, "iv": Data(count: 12)]],  // no tag
            ["type": "e2ee"],
        ]
        for secure in bad {
            let outcome = await manager.perform(["type": "PhotoOneOne", "secure": secure,
                                                 "data": ["url": "https://example.invalid/a.jpg", "fileName": "a.jpg"], "options": ["saveDir": target.path]])
            #expect(outcome.failure == .decryptFailed)
            #expect(outcome.savePath == nil)
            #expect(!FileManager.default.fileExists(atPath: target.appendingPathComponent("a.jpg").path))
        }
        // A type main does not decrypt means "no transformer": the bytes are saved as received.
        if case .none = SecureStream.parse(["type": "other"]) {} else { Issue.record("unknown type must not build a transformer") }
    }

    // 2 + 7: publication — quarantined, atomic, temp files gone, nothing left beside the user's file.
    @Test func finalizeQuarantinesPublishesAtomicallyAndCleansUp() throws {
        let dir = try tempDir("fin")
        defer { try? FileManager.default.removeItem(at: dir) }
        let save = dir.appendingPathComponent("save", isDirectory: true), tmp = dir.appendingPathComponent("tmp", isDirectory: true)
        func job(_ secure: SecureStream.Parsed) -> DownloadManager.Job {
            .init(url: URL(string: "https://f6-zpc.zdn.vn/a.jpg")!, destination: save.appendingPathComponent("a.jpg"), type: "PhotoOneOne",
                  headers: [:], secure: secure, ttfb: 0, tempDirectory: tmp)
        }
        // plain bytes
        let plain = Data("plain-image-bytes".utf8)
        let out = try DownloadManager.finalize(job(.none), .bytes(plain))
        #expect(out.failure == nil && out.savePath == save.appendingPathComponent("a.jpg").path)
        #expect(try Data(contentsOf: save.appendingPathComponent("a.jpg")) == plain)
        #expect(hasQuarantine(save.appendingPathComponent("a.jpg")))
        // streamed file + decrypt (CTR, block), replacing the existing file; owned raw file removed
        let stream = SecureStream(algorithm: .ctr, key: SecureStreamTests.key, iv: SecureStreamTests.iv, block: true, blockSize: 16)
        let raw = tmp.appendingPathComponent("raw.bin")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        try (SecureStreamTests.cipher + SecureStreamTests.cipher).write(to: raw)
        let out2 = try DownloadManager.finalize(job(.stream(stream)), .file(raw, ours: true))
        #expect(out2.failure == nil)
        #expect(try Data(contentsOf: save.appendingPathComponent("a.jpg")) == SecureStreamTests.plain + SecureStreamTests.plain)
        #expect(hasQuarantine(save.appendingPathComponent("a.jpg")))
        #expect(!FileManager.default.fileExists(atPath: raw.path))
        #expect((try FileManager.default.contentsOfDirectory(atPath: tmp.path)).isEmpty)               // no temp left
        #expect((try FileManager.default.contentsOfDirectory(atPath: save.path)) == ["a.jpg"])           // nothing beside the file
        // a shared cache file is copied, not consumed
        let cache = dir.appendingPathComponent("cache.bin")
        try Data("cached".utf8).write(to: cache)
        let out3 = try DownloadManager.finalize(job(.none), .file(cache, ours: false))
        #expect(out3.failure == nil && FileManager.default.fileExists(atPath: cache.path))
        // failed decryption publishes nothing new and leaves no temp
        let badGCM = SecureStream(algorithm: .gcm, key: Data(count: 32), iv: Data(count: 12), tag: Data(count: 16), block: false, blockSize: 4096)
        let raw2 = tmp.appendingPathComponent("raw2.bin")
        try Data("ciphertext".utf8).write(to: raw2)
        let before = try Data(contentsOf: save.appendingPathComponent("a.jpg"))
        let out4 = try DownloadManager.finalize(job(.stream(badGCM)), .file(raw2, ours: true))
        #expect(out4.failure == .decryptFailed)
        #expect(try Data(contentsOf: save.appendingPathComponent("a.jpg")) == before)
        #expect((try FileManager.default.contentsOfDirectory(atPath: tmp.path)).isEmpty)
    }

    @MainActor @Test func notFoundCacheIsBoundedAndResetClearsIt() throws {
        let dir = try tempDir("nf")
        defer { try? FileManager.default.removeItem(at: dir) }
        let manager = DownloadManager(platform: ZaloPlatform(appVersion: "test", userData: dir), tempDirectory: dir.appendingPathComponent("tmp"))
        for i in 0..<(DownloadManager.notFoundCapacity * 2) { manager.rememberNotFound("https://x.example/\(i)") }
        #expect(manager.notFoundCount <= DownloadManager.notFoundCapacity)
        manager.reset()
        #expect(manager.notFoundCount == 0)
    }

    @Test func staleTempsAreRemoved() throws {
        let dir = try tempDir("stale")
        defer { try? FileManager.default.removeItem(at: dir) }
        let old = dir.appendingPathComponent("old.part"), fresh = dir.appendingPathComponent("fresh.part")
        try Data("x".utf8).write(to: old); try Data("x".utf8).write(to: fresh)
        DownloadManager.removeStaleTemps(in: dir, now: Date().addingTimeInterval(3600))
        #expect(!FileManager.default.fileExists(atPath: old.path) && !FileManager.default.fileExists(atPath: fresh.path))
    }

    @MainActor @Test func isWritableResolvesSymlinks() throws {
        let dir = try tempDir("sym")
        defer { try? FileManager.default.removeItem(at: dir) }
        let platform = ZaloPlatform(appVersion: "test", userData: dir)
        try FileManager.default.createSymbolicLink(at: dir.appendingPathComponent("escape"), withDestinationURL: URL(fileURLWithPath: "/etc"))
        #expect(platform.isWritable(dir.appendingPathComponent("ok/file").path))
        #expect(!platform.isWritable(dir.appendingPathComponent("escape/x").path))
        #expect(!platform.isWritable("/etc/x"))
    }
}

@Suite struct HeaderSafetyTests {
    private let zalo = URL(string: "https://zcld.chat.zalo.me/a")!

    // 5
    @Test func sensitiveHeadersOnlyGoToZaloHosts() {
        let headers = ["cloud-viewer-key": "k", "Cookie": "c", "X-Ok": "1"]
        #expect(MediaLoader.sanitizedHeaders(headers, for: zalo) == headers)
        #expect(MediaLoader.sanitizedHeaders(headers, for: URL(string: "https://evil.example/a")!) == ["X-Ok": "1"])
        #expect(MediaLoader.sanitizedHeaders(headers, for: URL(string: "http://zcld.chat.zalo.me/a")!) == ["X-Ok": "1"])
    }

    @Test func rejectsInjectionAndHopByHopHeaders() {
        let headers = ["X-A": "ok", "X-B": "a\r\nX-Evil: 1", "X-C": "a\u{0}b", "Bad Name": "v", "X:D": "v", "": "v",
                       "Connection": "close", "Transfer-Encoding": "chunked", "Host": "evil", "Content-Length": "5", "X-Tab": "a\tb"]
        #expect(MediaLoader.sanitizedHeaders(headers, for: zalo) == ["X-A": "ok", "X-Tab": "a\tb"])
    }

    @Test func crossHostRedirectDropsSensitiveHeaders() {
        var request = URLRequest(url: URL(string: "https://other.example/x")!)
        request.setValue("k", forHTTPHeaderField: "cloud-viewer-key")
        request.setValue("c", forHTTPHeaderField: "Cookie")
        request.setValue("1", forHTTPHeaderField: "X-Ok")
        let cross = MediaLoader.redirectedRequest(request, originHost: "zcld.chat.zalo.me")
        #expect(cross.value(forHTTPHeaderField: "cloud-viewer-key") == nil && cross.value(forHTTPHeaderField: "Cookie") == nil)
        #expect(cross.value(forHTTPHeaderField: "X-Ok") == "1")
        var same = request
        same.url = URL(string: "https://zcld.chat.zalo.me/y")
        #expect(MediaLoader.redirectedRequest(same, originHost: "zcld.chat.zalo.me").value(forHTTPHeaderField: "cloud-viewer-key") == "k")
    }
}

@Suite struct SecureStreamTests {
    // NIST SP 800-38A F.5.5 (AES-256-CTR, first block).
    static let key = Data(hex: "603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4")
    static let iv = Data(hex: "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff")
    static let plain = Data(hex: "6bc1bee22e409f96e93d7e117393172a")
    static let cipher = Data(hex: "601ec313775789a5b7a7f504bbf3d228")

    @Test func ctrLinearMatchesNIST() throws {
        let stream = SecureStream(algorithm: .ctr, key: Self.key, iv: Self.iv, block: false, blockSize: 16)
        #expect(try stream.decrypt(Self.cipher) == Self.plain)
    }

    @Test func ctrBlockModeRestartsPerBlockAndPassesTailThrough() throws {
        let stream = SecureStream(algorithm: .ctr, key: Self.key, iv: Self.iv, block: true, blockSize: 16)
        let tail = Data([1, 2, 3, 4, 5])
        let out = try stream.decrypt(Self.cipher + Self.cipher + tail)
        #expect(out == Self.plain + Self.plain + tail)
    }

    @Test func parsesTheRendererPayload() throws {
        let secure: [String: Any] = ["type": "pcloud", "cryptography": ["alg": "aes-256-ctr", "type": "block", "blockSize": 4096,
                                                                       "key": Self.key, "iv": Self.iv]]
        guard case .stream(let stream) = SecureStream.parse(secure) else { Issue.record("expected a stream"); return }
        #expect(stream.block && stream.blockSize == 4096 && stream.algorithm == .ctr)
        if case .none = SecureStream.parse(["type": "unknown"]) {} else { Issue.record("unknown type → none") }
        if case .none = SecureStream.parse(nil) {} else { Issue.record("nil → none") }
        if case .invalid = SecureStream.parse(["type": "pcloud", "cryptography": ["alg": "AES-CTR", "key": Self.key, "iv": Self.iv]]) {} else { Issue.record("web alg name → invalid") }
    }

    @Test func gcmAuthenticates() throws {
        let key = SymmetricKey(size: .bits256)
        let nonce = AES.GCM.Nonce()
        let box = try AES.GCM.seal(Data("hello".utf8), using: key, nonce: nonce)
        let stream = SecureStream(algorithm: .gcm, key: key.withUnsafeBytes { Data($0) }, iv: Data(nonce),
                                  tag: box.tag, block: false, blockSize: 4096)
        #expect(try stream.decrypt(box.ciphertext) == Data("hello".utf8))
        let bad = SecureStream(algorithm: .gcm, key: key.withUnsafeBytes { Data($0) }, iv: Data(nonce),
                               tag: Data(repeating: 0, count: 16), block: false, blockSize: 4096)
        #expect(throws: (any Error).self) { try bad.decrypt(box.ciphertext) }
    }

    // 3: streaming GCM (own GHASH) agrees with CryptoKit across chunk boundaries and rejects bad tags / data.
    @Test func streamingGCMMatchesCryptoKit() throws {
        let dir = try tempDir("gcm")
        defer { try? FileManager.default.removeItem(at: dir) }
        let key = SymmetricKey(size: .bits256)
        let keyData = key.withUnsafeBytes { Data($0) }
        for size in [0, 1, 15, 16, 17, 4096, (1 << 20) - 1, (1 << 20), (1 << 20) + 17, 3 * (1 << 20) + 5] {
            var plain = Data(count: size)
            for i in 0..<size { plain[i] = UInt8(truncatingIfNeeded: i &* 31 &+ 7) }
            let nonce = AES.GCM.Nonce()
            let box = try AES.GCM.seal(plain, using: key, nonce: nonce)
            let src = dir.appendingPathComponent("c\(size)"), dst = dir.appendingPathComponent("p\(size)")
            try box.ciphertext.write(to: src)
            let stream = SecureStream(algorithm: .gcm, key: keyData, iv: Data(nonce), tag: box.tag, block: false, blockSize: 4096)
            try stream.decryptFile(at: src, to: dst)
            #expect(try Data(contentsOf: dst) == plain, "size \(size)")
            // flipped tag → failure and no output file
            var badTag = Data(box.tag); badTag[badTag.startIndex] ^= 1
            let bad = SecureStream(algorithm: .gcm, key: keyData, iv: Data(nonce), tag: badTag, block: false, blockSize: 4096)
            let out = dir.appendingPathComponent("bad\(size)")
            #expect(throws: (any Error).self) { try bad.decryptFile(at: src, to: out) }
            #expect(!FileManager.default.fileExists(atPath: out.path))
        }
    }

    @Test func decryptFileMatchesInMemory() throws {
        let dir = try tempDir("ss")
        defer { try? FileManager.default.removeItem(at: dir) }
        var big = Data()
        for _ in 0..<5000 { big.append(Self.cipher) }       // 80 000 bytes, not a multiple of 4096
        let stream = SecureStream(algorithm: .ctr, key: Self.key, iv: Self.iv, block: true, blockSize: 4096)
        let src = dir.appendingPathComponent("in"), dst = dir.appendingPathComponent("out")
        try big.write(to: src)
        try stream.decryptFile(at: src, to: dst)
        #expect(try Data(contentsOf: dst) == (try stream.decrypt(big)))
    }
}

@Suite struct MediaResolutionTests {
    @Test func parsesEngineJSON() {
        #expect(MediaResolution(json: ["state": "ok", "path": "/a/b.jpg"]) == .file(URL(fileURLWithPath: "/a/b.jpg")))
        #expect(MediaResolution(json: ["state": "ok"]) == .unavailable)
        #expect(MediaResolution(json: ["state": "expired"]) == .expired)
        #expect(MediaResolution(json: ["state": "unavailable", "reason": "x"]) == .unavailable)
        #expect(MediaResolution(json: nil) == .unavailable)
    }

    @MainActor @Test func resolverDedupesCachesAndForcesRetry() async {
        let resolver = MediaResolver(maxConcurrent: 2)
        var calls = 0
        let unavailable: @MainActor () async -> MediaResolution = { calls += 1; try? await Task.sleep(for: .milliseconds(30)); return .unavailable }
        async let a = resolver.resolve(key: "k", run: unavailable)
        async let b = resolver.resolve(key: "k", run: unavailable)
        _ = await (a, b)
        #expect(calls == 1)                                        // concurrent callers share one run
        _ = await resolver.resolve(key: "k", run: unavailable)
        #expect(calls == 1)                                        // "unavailable" is remembered briefly
        _ = await resolver.resolve(key: "k", force: true, run: unavailable)
        #expect(calls == 2)                                        // retry button bypasses it
        let expired = await resolver.resolve(key: "e") { .expired }
        let again = await resolver.resolve(key: "e") { .unavailable }
        #expect(expired == .expired && again == .expired)          // expired is final
    }

    // 1: a remembered file that the view could not decode is looked up again on retry.
    @MainActor @Test func forceDropsARememberedFileVerdict() async throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("bad-\(UUID().uuidString).jpg")
        try Data("not an image".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let resolver = MediaResolver()
        var calls = 0
        let first = await resolver.resolve(key: "f") { calls += 1; return .file(file) }
        #expect(first == .file(file) && resolver.known("f") == .file(file))
        _ = await resolver.resolve(key: "f") { calls += 1; return .unavailable }
        #expect(calls == 1)                                        // remembered while the file exists
        let retried = await resolver.resolve(key: "f", force: true) { calls += 1; return .expired }
        #expect(calls == 2 && retried == .expired)
    }

    @MainActor @Test func resolverLimitsConcurrency() async {
        let resolver = MediaResolver(maxConcurrent: 2)
        var running = 0, peak = 0
        var tasks: [Task<MediaResolution, Never>] = []
        for i in 0..<6 {
            tasks.append(Task { @MainActor in
                await resolver.resolve(key: "k\(i)") {
                    running += 1; peak = max(peak, running)
                    try? await Task.sleep(for: .milliseconds(20))
                    running -= 1
                    return .expired
                }
            })
        }
        for task in tasks { _ = await task.value }
        #expect(peak <= 2)
    }

    // 8: queued work whose views are gone is dropped, and the newest request is served first.
    @MainActor @Test func queueIsLIFOAndDropsCancelledWork() async {
        let resolver = MediaResolver(maxConcurrent: 1)
        var order: [String] = []
        let gate = Task { @MainActor in
            await resolver.resolve(key: "head") { try? await Task.sleep(for: .milliseconds(80)); order.append("head"); return .expired }
        }
        try? await Task.sleep(for: .milliseconds(20))             // head holds the only slot
        let stale = Task { @MainActor in await resolver.resolve(key: "stale") { order.append("stale"); return .expired } }
        let older = Task { @MainActor in await resolver.resolve(key: "older") { order.append("older"); return .expired } }
        let newest = Task { @MainActor in await resolver.resolve(key: "newest") { order.append("newest"); return .expired } }
        try? await Task.sleep(for: .milliseconds(20))
        stale.cancel()                                            // its view scrolled away while queued
        _ = await gate.value; _ = await older.value; _ = await newest.value; _ = await stale.value
        #expect(order == ["head", "newest", "older"])
    }
}

@Suite struct LocalMediaFileTests {
    @Test func onlyOurDirectoriesAreReadable() {
        let inside = ZaloPlatform.defaultUserData.appendingPathComponent("ZaloPC/1/ZaloDownloads/resource/c/picture/x.jpg")
        #expect(MediaLoader.isAllowedLocalFile(inside))
        #expect(!MediaLoader.isAllowedLocalFile(URL(fileURLWithPath: "/etc/hosts")))
        #expect(!MediaLoader.isAllowedLocalFile(inside.appendingPathComponent("../../../../../../../../etc/hosts")))
        #expect(!MediaLoader.isAllowedLocalFile(URL(string: "https://example.com/a.jpg")!))
    }

    @Test func hostNotFoundIsNotRetried() async {
        // `.invalid` never resolves; for a non-Zalo host no DoH is involved, so this only checks the error is surfaced.
        let loader = MediaLoader(cacheDirectory: FileManager.default.temporaryDirectory.appendingPathComponent("ml-\(UUID().uuidString)"))
        await #expect(throws: (any Error).self) { _ = try await loader.data(for: URL(string: "https://nonexistent-host.invalid/a.jpg")!, deadline: 8) }
    }
}

extension Data {
    init(hex: String) {
        var out = Data()
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            out.append(UInt8(hex[index..<next], radix: 16)!)
            index = next
        }
        self = out
    }
}

@Suite struct VideoPosterTests {
    @Test func detectsVideoSavedAsThumbnail() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let mp4 = dir.appendingPathComponent("a_t.jpg")
        try Data([0, 0, 0, 0x1c] + Array("ftypmp42".utf8) + [0, 0, 0, 1]).write(to: mp4)
        let jpeg = dir.appendingPathComponent("b_t.jpg")
        try Data([0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10] + Array("JFIF".utf8) + [0, 1]).write(to: jpeg)
        let tiny = dir.appendingPathComponent("c_t.jpg")
        try Data([0xFF, 0xD8]).write(to: tiny)
        #expect(VideoPoster.isVideo(mp4))
        #expect(!VideoPoster.isVideo(jpeg))
        #expect(!VideoPoster.isVideo(tiny))
        #expect(!VideoPoster.isVideo(dir.appendingPathComponent("missing")))
    }
}
