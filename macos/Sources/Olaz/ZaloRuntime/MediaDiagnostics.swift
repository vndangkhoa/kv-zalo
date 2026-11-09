import Foundation

/// Minimal, rate-limited media-load diagnostics for `engine.log`.
///
/// A line carries the CDN host and a failure class only — never a full URL (paths and query strings
/// carry per-message tokens) and never content: `media fail kind=image host=f28-zpc.zdn.vn class=dns`.
/// The same (host, class) is reported at most once per window, with a count of what was suppressed,
/// and the total is capped per minute, so a screen full of retired CDN links cannot flood the log.
enum MediaDiagnostics {
    /// Admits a key at most once per `window`, and at most `maxPerMinute` lines overall.
    struct Limiter {
        var window: TimeInterval = 60
        var maxPerMinute = 30
        private var last: [String: Date] = [:]
        private var suppressed: [String: Int] = [:]
        private var minuteStart = Date.distantPast
        private var minuteCount = 0

        init(window: TimeInterval = 60, maxPerMinute: Int = 30) {
            self.window = window
            self.maxPerMinute = maxPerMinute
        }

        /// Nil when the line should be dropped, else how many identical lines were dropped since the last one.
        mutating func admit(_ key: String, now: Date = Date()) -> Int? {
            if now.timeIntervalSince(minuteStart) >= 60 { minuteStart = now; minuteCount = 0 }
            if let previous = last[key], now.timeIntervalSince(previous) < window {
                suppressed[key, default: 0] += 1
                return nil
            }
            guard minuteCount < maxPerMinute else { suppressed[key, default: 0] += 1; return nil }
            minuteCount += 1
            if last.count > 500 { last.removeAll(); suppressed.removeAll() }
            last[key] = now
            return suppressed.removeValue(forKey: key) ?? 0
        }
    }

    private final class State: @unchecked Sendable {
        let lock = NSLock()
        var limiter = Limiter()
        var sink: (@Sendable (String) -> Void)?
    }
    private static let state = State()

    /// Where lines go (the app points this at `EngineLog`; unset in tests, which then log nothing).
    static func setSink(_ sink: (@Sendable (String) -> Void)?) {
        state.lock.lock(); state.sink = sink; state.lock.unlock()
    }

    /// Failure class of a media error: `http-404`, `dns`, `timeout`, `offline`, `tls`, `undecodable`, `size`, `other`.
    /// Nil for cancellation (not a failure).
    static func classify(_ error: Error) -> String? {
        if error is CancellationError { return nil }
        if let load = error as? MediaLoader.LoadError {
            switch load {
            case .http(let code): return "http-\(code)"
            case .undecodable: return "undecodable"
            case .timedOut: return "timeout"
            case .tooLarge: return "size"
            case .hostNotFound: return "dns"
            case .badURL: return "other"
            }
        }
        if let resolve = error as? DoHResolver.ResolveError {
            if case .notFound = resolve { return "dns" }
            return "dns-unreachable"
        }
        if let urlError = error as? URLError {
            switch urlError.code {
            case .cancelled: return nil
            case .cannotFindHost, .dnsLookupFailed: return "dns"
            case .timedOut: return "timeout"
            case .notConnectedToInternet, .networkConnectionLost, .internationalRoamingOff: return "offline"
            case .secureConnectionFailed, .serverCertificateUntrusted, .serverCertificateHasBadDate,
                 .serverCertificateHasUnknownRoot, .serverCertificateNotYetValid, .clientCertificateRejected:
                return "tls"
            default: return "other"
            }
        }
        return "other"
    }

    static func line(kind: String, host: String, failureClass: String, suppressed: Int) -> String {
        "media fail kind=\(kind) host=\(host) class=\(failureClass)" + (suppressed > 0 ? " suppressed=\(suppressed)" : "")
    }

    /// Reports a failed media load (rate-limited). Safe from any thread.
    static func report(url: URL, error: Error, kind: String) {
        guard let failureClass = classify(error), let host = url.host?.lowercased(), !host.isEmpty else { return }
        state.lock.lock()
        let sink = state.sink
        let suppressed = sink == nil ? nil : state.limiter.admit("\(kind)|\(host)|\(failureClass)")
        state.lock.unlock()
        guard let sink, let suppressed else { return }
        sink(line(kind: kind, host: host, failureClass: failureClass, suppressed: suppressed))
    }
}
