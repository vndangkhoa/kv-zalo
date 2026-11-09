import Testing
import Foundation
import AppKit
import Network
@testable import Olaz

@Suite struct HTTPParserTests {
    private func parse(_ text: String, eof: Bool = false, maxBody: Int = 1 << 20) throws -> HTTPResponseParser.Response? {
        var parser = HTTPResponseParser(maxBody: maxBody)
        if try parser.feed(Data(text.utf8)) { return parser.response }
        return eof ? try parser.finish() : nil
    }

    @Test func contentLength() throws {
        let r = try parse("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: image/png\r\n\r\nhello")
        #expect(r?.status == 200)
        #expect(r?.headers["content-type"] == "image/png")
        #expect(r?.body == Data("hello".utf8))
        #expect(r?.framed == true)
        #expect(try parse("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhel") == nil)
        #expect(throws: HTTPResponseParser.ParseError.closedEarly) {
            try parse("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhel", eof: true)
        }
    }

    @Test func chunked() throws {
        let text = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n"
        let r = try parse(text)
        #expect(r?.body == Data("hello world".utf8))
        #expect(r?.framed == true)
        #expect(try parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel") == nil)
    }

    @Test func incrementalByteByByteMatchesWhole() throws {
        let text = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\nA\r\n0123456789\r\n0\r\nX-T: 1\r\n\r\n"
        var parser = HTTPResponseParser()
        var done = false
        for byte in Data(text.utf8) { done = try parser.feed(Data([byte])) }
        #expect(done)
        #expect(parser.response?.body == Data("abc0123456789".utf8))
    }

    @Test func readUntilCloseIsNotFramed() throws {
        let text = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc"
        #expect(try parse(text) == nil)
        let r = try parse(text, eof: true)
        #expect(r?.body == Data("abc".utf8))
        #expect(r?.framed == false)
    }

    @Test func redirectHeaders() throws {
        let r = try parse("HTTP/1.1 302 Found\r\nLocation: https://x.example/a\r\nContent-Length: 0\r\n\r\n")
        #expect(r?.status == 302)
        #expect(r?.headers["location"] == "https://x.example/a")
    }

    @Test func garbageThrows() {
        #expect(throws: HTTPResponseParser.ParseError.self) { try parse("nope\r\n\r\n", eof: true) }
    }

    @Test func rejectsNegativeAndSignedSizes() {
        for size in ["-1", "-5", "+5", "0x5", "5 5", ""] {
            #expect(throws: HTTPResponseParser.ParseError.malformed, "chunk \(size)") {
                try parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\(size)\r\nhello\r\n0\r\n\r\n")
            }
        }
        for length in ["-1", "+5", "abc", "5, 6", "1e3"] {
            #expect(throws: HTTPResponseParser.ParseError.malformed, "length \(length)") {
                try parse("HTTP/1.1 200 OK\r\nContent-Length: \(length)\r\n\r\nhello")
            }
        }
    }

    @Test func rejectsHugeSizes() {
        #expect(throws: HTTPResponseParser.ParseError.malformed) {      // would overflow Int
            try parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFFFF\r\nx")
        }
        #expect(throws: HTTPResponseParser.ParseError.tooLarge) {
            try parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFF\r\nx")
        }
        #expect(throws: HTTPResponseParser.ParseError.malformed) {
            try parse("HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999\r\n\r\n")
        }
        #expect(throws: HTTPResponseParser.ParseError.tooLarge) {
            try parse("HTTP/1.1 200 OK\r\nContent-Length: 2000000\r\n\r\n")
        }
    }

    @Test func capsAccumulatedChunks() {
        #expect(throws: HTTPResponseParser.ParseError.tooLarge) {
            try parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nabcdef\r\n6\r\nabcdef\r\n0\r\n\r\n", maxBody: 10)
        }
        #expect(throws: HTTPResponseParser.ParseError.tooLarge) {
            try parse("HTTP/1.1 200 OK\r\n\r\nabcdefghijkl", maxBody: 10)
        }
    }
}

/// Streaming mode: the body goes to a file handle instead of memory, with the same framing validation.
@Suite struct HTTPStreamingParserTests {
    private let temp = FileManager.default.temporaryDirectory.appendingPathComponent("sz-stream-\(UUID().uuidString).part")

    /// Feeds `pieces` into a sink-backed parser. Returns the response (nil if incomplete) and the file bytes.
    private func stream(_ pieces: [Data], eof: Bool = false, maxBody: Int = 1 << 20) throws -> (HTTPResponseParser.Response?, Data) {
        #expect(FileManager.default.createFile(atPath: temp.path, contents: nil))
        let handle = try FileHandle(forWritingTo: temp)
        var parser = HTTPResponseParser(maxBody: maxBody, sink: handle)
        var response: HTTPResponseParser.Response?
        do {
            for piece in pieces where try parser.feed(piece) { response = parser.response; break }
            if response == nil, eof { response = try parser.finish() }
        } catch {
            try? handle.close()
            throw error
        }
        try handle.close()
        return (response, try Data(contentsOf: temp))
    }

    private func cleanup() { try? FileManager.default.removeItem(at: temp) }

    private func bytes(_ text: String) -> Data { Data(text.utf8) }

    @Test func contentLengthIntoFile() throws {
        defer { cleanup() }
        let payload = Data((0..<200_000).map { UInt8($0 % 251) })
        var pieces = [bytes("HTTP/1.1 200 OK\r\nContent-Length: \(payload.count)\r\n\r\n")]
        var offset = 0
        while offset < payload.count {            // uneven pieces
            let end = min(payload.count, offset + 7_919)
            pieces.append(payload[offset..<end]); offset = end
        }
        let (response, file) = try stream(pieces)
        #expect(response?.status == 200)
        #expect(response?.framed == true)
        #expect(response?.body.isEmpty == true)
        #expect(response?.bodyLength == payload.count)
        #expect(file == payload)
    }

    @Test func chunkedIntoFile() throws {
        defer { cleanup() }
        let text = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nX-T: 1\r\n\r\n"
        let (response, file) = try stream(Array(bytes(text)).map { Data([$0]) })    // byte by byte
        #expect(response?.framed == true)
        #expect(response?.bodyLength == 11)
        #expect(file == bytes("hello world"))
    }

    @Test func truncatedContentLengthThrowsAndIsNotFramed() throws {
        defer { cleanup() }
        #expect(throws: HTTPResponseParser.ParseError.closedEarly) {
            try stream([bytes("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhel")], eof: true)
        }
    }

    @Test func truncatedChunkedThrows() throws {
        defer { cleanup() }
        #expect(throws: HTTPResponseParser.ParseError.closedEarly) {
            try stream([bytes("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n5\r\nwor")], eof: true)
        }
        #expect(throws: HTTPResponseParser.ParseError.closedEarly) {       // missing terminating chunk
            try stream([bytes("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n")], eof: true)
        }
    }

    @Test func bodyOfNon2xxIsDiscarded() throws {
        defer { cleanup() }
        let (redirect, redirectFile) = try stream([bytes("HTTP/1.1 302 Found\r\nLocation: https://x.example/a\r\nContent-Length: 5\r\n\r\nmoved")])
        #expect(redirect?.status == 302)
        #expect(redirect?.headers["location"] == "https://x.example/a")
        #expect(redirect?.bodyLength == 5)
        #expect(redirectFile.isEmpty)
    }

    @Test func limitsApplyWhileStreaming() throws {
        defer { cleanup() }
        #expect(throws: HTTPResponseParser.ParseError.tooLarge) {
            try stream([bytes("HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n")], maxBody: 10)
        }
        #expect(throws: HTTPResponseParser.ParseError.tooLarge) {
            try stream([bytes("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nabcdef\r\n6\r\nabcdef\r\n0\r\n\r\n")], maxBody: 10)
        }
        #expect(throws: HTTPResponseParser.ParseError.tooLarge) {
            try stream([bytes("HTTP/1.1 200 OK\r\n\r\nabcdefghijkl")], maxBody: 10)
        }
    }

    @Test func memoryModeReportsBodyLength() throws {
        var parser = HTTPResponseParser()
        #expect(try parser.feed(bytes("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")))
        #expect(parser.response?.bodyLength == 5)
        #expect(parser.expectedLength == 5)
    }

    @Test func downloadFailureLeavesNoFile() async throws {
        // Unsupported URL scheme fails before any I/O; the destination must not exist afterwards.
        let target = FileManager.default.temporaryDirectory.appendingPathComponent("sz-\(UUID().uuidString).part")
        await #expect(throws: (any Error).self) {
            try await PinnedHTTPSClient.download(URL(string: "http://example.com/x")!, addresses: [], to: target)
        }
        #expect(!FileManager.default.fileExists(atPath: target.path))
        // No usable address: the (empty) destination is cleaned up too.
        await #expect(throws: (any Error).self) {
            try await PinnedHTTPSClient.download(URL(string: "https://example.com/x")!, addresses: [], to: target)
        }
        #expect(!FileManager.default.fileExists(atPath: target.path))
    }
}

@Suite struct FilesCacheTests {
    private func tempDirectory() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("sz-files-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Writes `size` bytes and sets both access and modification time to `age` seconds ago.
    @discardableResult
    private func make(_ name: String, in dir: URL, size: Int, age: TimeInterval) throws -> URL {
        let url = dir.appendingPathComponent(name)
        try Data(count: size).write(to: url)
        let t = time_t(Date().timeIntervalSince1970 - age)
        var times = [timeval(tv_sec: t, tv_usec: 0), timeval(tv_sec: t, tv_usec: 0)]
        #expect(utimes(url.path, &times) == 0)
        return url
    }

    @Test func removesStalePartsKeepsFreshOnes() throws {
        let dir = try tempDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let stale = try make(".a.part", in: dir, size: 10, age: 3600)
        let fresh = try make(".b.part", in: dir, size: 10, age: 5)
        let done = try make("c.pdf", in: dir, size: 10, age: 3600)
        MediaLoader.trimFiles(in: dir, limit: 1_000_000)
        #expect(!FileManager.default.fileExists(atPath: stale.path))
        #expect(FileManager.default.fileExists(atPath: fresh.path))
        #expect(FileManager.default.fileExists(atPath: done.path))
    }

    @Test func evictsLeastRecentlyUsedFirstAndSparesKept() throws {
        let dir = try tempDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let oldest = try make("old.bin", in: dir, size: 100, age: 3000)
        let middle = try make("mid.bin", in: dir, size: 100, age: 2000)
        let newest = try make("new.bin", in: dir, size: 100, age: 1000)
        let part = try make(".x.part", in: dir, size: 100, age: 1)       // not counted, never evicted by size
        MediaLoader.trimFiles(in: dir, limit: 250, keeping: oldest)       // 300 > 250: oldest is kept, so mid goes
        #expect(FileManager.default.fileExists(atPath: oldest.path))
        #expect(!FileManager.default.fileExists(atPath: middle.path))
        #expect(FileManager.default.fileExists(atPath: newest.path))
        #expect(FileManager.default.fileExists(atPath: part.path))
        MediaLoader.trimFiles(in: dir, limit: 150)                        // 200 > 150: now oldest is evicted
        #expect(!FileManager.default.fileExists(atPath: oldest.path))
        #expect(FileManager.default.fileExists(atPath: newest.path))
    }

    @Test func quarantineIsApplied() throws {
        let dir = try tempDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = try make("q.bin", in: dir, size: 4, age: 0)
        MediaLoader.applyQuarantine(to: file, origin: URL(string: "https://res-zalo.zadn.vn/x.bin")!)
        let values = try file.resourceValues(forKeys: [.quarantinePropertiesKey])
        #expect(values.quarantineProperties?["LSQuarantineAgentName"] as? String == "Olaz")
    }

    @Test func loaderUsesInjectedDirectoryForFiles() async throws {
        let dir = try tempDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let loader = MediaLoader(cacheDirectory: dir)
        #expect(loader.filesDirectory.path.hasPrefix(dir.path))
    }
}

@Suite struct MediaPolicyTests {
    @Test func dohOnlyForZaloCDNHosts() {
        for host in ["s160-26-ava-talk.zadn.vn", "res-zalo.zadn.vn", "stc-zaloprofile.zdn.vn", "chat.zalo.me",
                     "tt-convers-wpa.api.zaloapp.com", "ZADN.VN"] {
            #expect(MediaLoader.isZaloCDN(host), "\(host)")
        }
        for host in ["evil.com", "zadn.vn.evil.com", "notzadn.vn", "example.org", "localhost", "192.168.1.1"] {
            #expect(!MediaLoader.isZaloCDN(host), "\(host)")
        }
    }

    @Test func stableFileHash() {
        #expect(MediaLoader.stableHash("a") == MediaLoader.stableHash("a"))
        #expect(MediaLoader.stableHash("a") != MediaLoader.stableHash("b"))
    }

    @Test func nonCDNHostsAreNotFetchedViaDoH() async {
        let loader = MediaLoader(cacheDirectory: FileManager.default.temporaryDirectory.appendingPathComponent("sz-\(UUID().uuidString)"))
        await #expect(throws: MediaLoader.LoadError.self) {
            try await loader.fetchViaDoH(URL(string: "https://www.apple.com/")!)
        }
    }

    @Test func animatedDecodeDetectsFrames() throws {
        // Minimal 2-frame GIF built with ImageIO.
        let data = NSMutableData()
        let dest = try #require(CGImageDestinationCreateWithData(data, "com.compuserve.gif" as CFString, 2, nil))
        let ctx = try #require(CGContext(data: nil, width: 4, height: 4, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        for _ in 0..<2 { CGImageDestinationAddImage(dest, ctx.makeImage()!, nil) }
        #expect(CGImageDestinationFinalize(dest))
        #expect(MediaLoader.decodeAnimated(data as Data) != nil)
        let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 4, pixelsHigh: 4, bitsPerSample: 8, samplesPerPixel: 4,
                                   hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        #expect(MediaLoader.decodeAnimated(rep.representation(using: .png, properties: [:])!) == nil)
    }
}

@Suite struct ImageDecodeTests {
    @Test func downsamplesLargeImages() throws {
        let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 800, pixelsHigh: 400, bitsPerSample: 8,
                                   samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                   colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        let png = rep.representation(using: .png, properties: [:])!
        let small = try #require(MediaLoader.decode(png, maxPixelSize: 200))
        #expect(max(small.size.width, small.size.height) == 200)
        let full = try #require(MediaLoader.decode(png, maxPixelSize: 2000))
        #expect(full.size.width == 800)
        #expect(MediaLoader.decode(Data("not an image".utf8), maxPixelSize: 100) == nil)
    }

    @Test func dnsFailureClassification() {
        #expect(MediaLoader.isDNSFailure(URLError(.cannotFindHost)))
        #expect(MediaLoader.isDNSFailure(URLError(.dnsLookupFailed)))
        #expect(MediaLoader.isDNSFailure(URLError(.notConnectedToInternet)))   // macOS reports some DNS failures this way
        #expect(!MediaLoader.isDNSFailure(URLError(.cancelled)))
        #expect(!MediaLoader.isDNSFailure(URLError(.badServerResponse)))
    }
}

/// Live-network tests; they return early (skip) when no DoH endpoint is reachable.
@Suite(.serialized) struct DoHIntegrationTests {
    /// Large JSON on a *.zadn.vn host whose lookups fail on some system resolvers.
    static let zaloURL = URL(string: "https://res-zalo.zadn.vn/upload/media/2020/3/26/keyword_all_v43_26_03_todo_fix_bug_root_undefined_1585216420130_679327.json")!

    private func tempLoader() -> MediaLoader {
        MediaLoader(cacheDirectory: FileManager.default.temporaryDirectory
            .appendingPathComponent("olaz-media-test-\(UUID().uuidString)"))
    }

    @Test func resolvesViaDoH() async throws {
        let resolver = DoHResolver()
        let addresses: [IPv4Address]
        do { addresses = try await resolver.resolve("res-zalo.zadn.vn") }
        catch DoHResolver.ResolveError.allEndpointsFailed { print("offline: skipping"); return }
        #expect(!addresses.isEmpty)
        let again = try await resolver.resolve("res-zalo.zadn.vn")      // served from TTL cache
        #expect(again == addresses)
        await #expect(throws: DoHResolver.ResolveError.self) {
            try await resolver.resolve("definitely-not-a-real-host-olaz.invalid")
        }
    }

    /// TLS/SNI/cert validation against a host the system resolver handles fine (DoH IP, original hostname).
    @Test func pinnedConnectionValidatesCertificate() async throws {
        let resolver = DoHResolver()
        let host = "www.apple.com"
        let addresses: [IPv4Address]
        do { addresses = try await resolver.resolve(host) }
        catch DoHResolver.ResolveError.allEndpointsFailed { print("offline: skipping"); return }
        let response = try await PinnedHTTPSClient.get(URL(string: "https://\(host)/")!, addresses: addresses)
        #expect((200..<400).contains(response.status))
    }

    @Test func wrongHostnameIsRejected() async throws {
        let resolver = DoHResolver()
        let addresses: [IPv4Address]
        do { addresses = try await resolver.resolve("www.apple.com") }
        catch DoHResolver.ResolveError.allEndpointsFailed { print("offline: skipping"); return }
        // Same IPs but a hostname the certificate does not cover must fail TLS validation.
        await #expect(throws: (any Error).self) {
            try await PinnedHTTPSClient.get(URL(string: "https://example.invalid/")!, addresses: addresses, timeout: 8)
        }
    }

    @Test func zaloCDNLoadsThroughDoHPath() async throws {
        let loader = tempLoader()
        let data: Data
        do { data = try await loader.fetchViaDoH(Self.zaloURL) }
        catch DoHResolver.ResolveError.allEndpointsFailed { print("offline: skipping"); return }
        #expect(data.count > 100_000)
        #expect((try? JSONSerialization.jsonObject(with: data)) != nil)
    }

    @Test func dataForURLCachesAndDedupes() async throws {
        let loader = tempLoader()
        let urls = [Self.zaloURL, Self.zaloURL, Self.zaloURL]
        let results: [Data]
        do {
            results = try await withThrowingTaskGroup(of: Data.self) { group in
                for u in urls { group.addTask { try await loader.data(for: u) } }
                return try await group.reduce(into: []) { $0.append($1) }
            }
        } catch let e as URLError { print("offline: skipping", e.code); return }
        catch DoHResolver.ResolveError.allEndpointsFailed { print("offline: skipping"); return }
        #expect(results.count == 3 && Set(results.map(\.count)).count == 1)
        print("zalo host needed DoH on this machine:", await loader.usesDoH(host: "res-zalo.zadn.vn"))
        // Second call must come from URLCache.
        let cached = try await loader.data(for: Self.zaloURL)
        #expect(cached == results[0])
    }

    @Test func localFileDownloadsAndReuses() async throws {
        let loader = tempLoader()
        defer { try? FileManager.default.removeItem(at: loader.filesDirectory.deletingLastPathComponent()) }
        let file: URL
        do { file = try await loader.localFile(for: Self.zaloURL) }
        catch let e as URLError { print("offline: skipping", e.code); return }
        catch DoHResolver.ResolveError.allEndpointsFailed { print("offline: skipping"); return }
        #expect(file.pathExtension == "json")
        #expect(((try? FileManager.default.attributesOfItem(atPath: file.path)[.size]) as? Int ?? 0) > 100_000)
        #expect(try await loader.localFile(for: Self.zaloURL) == file)
        let seen = ProgressBox()
        #expect(try await loader.localFile(for: Self.zaloURL, progress: { seen.add($0) }) == file)
        #expect(seen.last == 1)
        let quarantine = try file.resourceValues(forKeys: [.quarantinePropertiesKey]).quarantineProperties
        #expect(quarantine?["LSQuarantineAgentName"] as? String == "Olaz")
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: loader.filesDirectory.path).filter { $0.hasSuffix(".part") }
        #expect(leftovers.isEmpty)
    }
}

private final class ProgressBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value = 0.0
    func add(_ fraction: Double) { lock.lock(); value = max(value, fraction); lock.unlock() }
    var last: Double { lock.lock(); defer { lock.unlock() }; return value }
}
