import Foundation

/// Incremental HTTP/1.1 response parser: headers are parsed once, then the body is accumulated
/// (fixed length or chunked) without re-scanning earlier bytes. All sizes are parsed strictly
/// (digits only, bounded) and the total body is capped.
///
/// With a `sink`, the body of 2xx responses is written incrementally to the file handle instead of being
/// accumulated (`Response.body` stays empty; use `bodyLength`). Bodies of other statuses are counted but
/// discarded in that mode. Framing is validated identically either way; the caller owns the sink and must
/// discard the file when `feed`/`finish` throw or the response is not `framed`.
struct HTTPResponseParser {
    struct Response: Sendable {
        let status: Int
        let headers: [String: String]   // lower-cased names
        let body: Data
        /// Number of body bytes received (equals `body.count` unless a sink was used).
        let bodyLength: Int
        /// True when the body length was established by the protocol (Content-Length reached or chunked
        /// terminator seen); false for read-until-close bodies, which may be truncated.
        let framed: Bool
    }

    enum ParseError: Error, Equatable, LocalizedError {
        case malformed, tooLarge, closedEarly
        var errorDescription: String? {
            switch self {
            case .malformed: "HTTP: malformed response"
            case .tooLarge: "HTTP: response too large"
            case .closedEarly: "HTTP: connection closed before the response was complete"
            }
        }
    }

    static let defaultMaxBody = 50 * 1024 * 1024
    private static let maxHeaderBytes = 64 * 1024

    private enum Phase {
        case head
        case fixed(remaining: Int)
        case chunkSize
        case chunkData(remaining: Int)
        case chunkDataEnd           // CRLF after chunk data
        case trailers
        case untilClose
        case done(framed: Bool)
    }

    let maxBody: Int
    private var phase: Phase = .head
    private var pending: [UInt8] = []
    private var cursor = 0
    private(set) var status = 0
    private var headers: [String: String] = [:]
    private var body = Data()
    private var bodyCount = 0
    private let sink: FileHandle?

    init(maxBody: Int = HTTPResponseParser.defaultMaxBody, sink: FileHandle? = nil) {
        self.maxBody = maxBody
        self.sink = sink
    }

    /// Body bytes received so far.
    var bodyBytes: Int { bodyCount }

    /// Declared body length once known (Content-Length), nil for chunked / read-until-close.
    private(set) var expectedLength: Int?

    private mutating func emit(_ range: Range<Int>) throws {
        bodyCount += range.count
        if let sink {
            if (200..<300).contains(status) { try sink.write(contentsOf: Data(pending[range])) }
        } else {
            body.append(contentsOf: pending[range])
        }
    }

    /// Feeds received bytes; returns true once the response is complete.
    mutating func feed(_ data: Data) throws -> Bool {
        if case .done = phase { return true }
        pending.append(contentsOf: data)
        try advance()
        if case .done = phase { return true }
        if cursor > 64 * 1024 { pending.removeSubrange(0..<cursor); cursor = 0 }
        return false
    }

    /// Call when the peer closed the connection cleanly.
    mutating func finish() throws -> Response {
        switch phase {
        case .done(let framed): return makeResponse(framed: framed)
        case .untilClose: return makeResponse(framed: false)
        default: throw ParseError.closedEarly
        }
    }

    var response: Response? {
        if case .done(let framed) = phase { return makeResponse(framed: framed) }
        return nil
    }

    private func makeResponse(framed: Bool) -> Response {
        Response(status: status, headers: headers, body: body, bodyLength: bodyCount, framed: framed)
    }

    // MARK: State machine

    private mutating func advance() throws {
        while true {
            switch phase {
            case .done: return
            case .head:
                guard let end = find([13, 10, 13, 10], from: cursor) else {
                    if pending.count - cursor > Self.maxHeaderBytes { throw ParseError.malformed }
                    return
                }
                try parseHead(Array(pending[cursor..<end]))
                cursor = end + 4
            case .fixed(let remaining):
                let take = min(remaining, pending.count - cursor)
                try emit(cursor..<(cursor + take))
                cursor += take
                if take == remaining { phase = .done(framed: true) } else { phase = .fixed(remaining: remaining - take); return }
            case .untilClose:
                let available = pending.count - cursor
                if bodyCount + available > maxBody { throw ParseError.tooLarge }
                try emit(cursor..<pending.count)
                cursor = pending.count
                return
            case .chunkSize:
                guard let end = find([13, 10], from: cursor) else {
                    if pending.count - cursor > 1024 { throw ParseError.malformed }
                    return
                }
                let line = String(decoding: pending[cursor..<end], as: UTF8.self)
                let sizeText = line.split(separator: ";", maxSplits: 1, omittingEmptySubsequences: false).first.map {
                    $0.trimmingCharacters(in: .whitespaces)
                } ?? ""
                guard let size = Self.parseSize(sizeText, radix: 16) else { throw ParseError.malformed }
                cursor = end + 2
                if size == 0 { phase = .trailers; continue }
                guard size <= maxBody - bodyCount else { throw ParseError.tooLarge }
                phase = .chunkData(remaining: size)
            case .chunkData(let remaining):
                let take = min(remaining, pending.count - cursor)
                try emit(cursor..<(cursor + take))
                cursor += take
                if take == remaining { phase = .chunkDataEnd } else { phase = .chunkData(remaining: remaining - take); return }
            case .chunkDataEnd:
                guard pending.count - cursor >= 2 else { return }
                guard pending[cursor] == 13, pending[cursor + 1] == 10 else { throw ParseError.malformed }
                cursor += 2
                phase = .chunkSize
            case .trailers:
                guard let end = find([13, 10], from: cursor) else {
                    if pending.count - cursor > Self.maxHeaderBytes { throw ParseError.malformed }
                    return
                }
                let empty = end == cursor
                cursor = end + 2
                if empty { phase = .done(framed: true) }
            }
        }
    }

    private mutating func parseHead(_ bytes: [UInt8]) throws {
        let text = String(decoding: bytes, as: UTF8.self)
        var lines = text.components(separatedBy: "\r\n")
        let statusLine = lines.removeFirst().split(separator: " ", maxSplits: 2)
        guard statusLine.count >= 2, statusLine[0].hasPrefix("HTTP/"),
              let code = Self.parseSize(String(statusLine[1]), radix: 10), (100..<600).contains(code) else {
            throw ParseError.malformed
        }
        status = code
        for line in lines {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces).lowercased()
            let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            headers[name] = headers[name].map { $0 + ", " + value } ?? value
        }
        if (100..<200).contains(status) || status == 204 || status == 304 {
            phase = .done(framed: true)
        } else if headers["transfer-encoding"]?.lowercased().contains("chunked") == true {
            phase = .chunkSize
        } else if let lengthText = headers["content-length"] {
            guard let length = Self.parseSize(lengthText, radix: 10) else { throw ParseError.malformed }
            guard length <= maxBody else { throw ParseError.tooLarge }
            expectedLength = length
            phase = length == 0 ? .done(framed: true) : .fixed(remaining: length)
        } else {
            phase = .untilClose
        }
    }

    /// Digits only (no sign, no whitespace), at most 15 hex / 18 decimal digits so it cannot overflow.
    static func parseSize(_ text: String, radix: Int) -> Int? {
        guard !text.isEmpty, text.utf8.count <= (radix == 16 ? 15 : 18) else { return nil }
        for byte in text.utf8 {
            switch byte {
            case UInt8(ascii: "0")...UInt8(ascii: "9"): continue
            case UInt8(ascii: "a")...UInt8(ascii: "f"), UInt8(ascii: "A")...UInt8(ascii: "F"):
                if radix == 16 { continue } else { return nil }
            default: return nil
            }
        }
        return Int(text, radix: radix)
    }

    private func find(_ pattern: [UInt8], from start: Int) -> Int? {
        let count = pending.count
        guard count >= pattern.count, start <= count - pattern.count else { return nil }
        var i = start
        let last = count - pattern.count
        while i <= last {
            if pending[i] == pattern[0] {
                var match = true
                var j = 1
                while j < pattern.count { if pending[i + j] != pattern[j] { match = false; break }; j += 1 }
                if match { return i }
            }
            i += 1
        }
        return nil
    }
}
