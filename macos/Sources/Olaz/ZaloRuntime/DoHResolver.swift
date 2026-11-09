import Foundation
import Network

/// DNS-over-HTTPS resolver used when the system resolver cannot resolve Zalo CDN hosts
/// (some ISP / VPN resolvers SERVFAIL on `*.zadn.vn`). Zalo PC is unaffected because
/// Chromium ships its own resolver; this mirrors that behaviour.
///
/// Endpoints are tried in order. The two hostname endpoints normally resolve through the
/// system resolver; the IP-literal endpoints (valid IP SANs on both certificates) are the
/// last resort for when even those do not.
actor DoHResolver {
    enum ResolveError: Error, LocalizedError {
        case notFound(String)
        case allEndpointsFailed(String)
        var errorDescription: String? {
            switch self {
            case .notFound(let h): "DoH: no A record for \(h)"
            case .allEndpointsFailed(let h): "DoH: all resolvers failed for \(h)"
            }
        }
    }

    private struct Entry { let addresses: [IPv4Address]; let expires: Date }
    private struct Answer: Decodable { let type: Int; let TTL: Int?; let data: String }
    private struct Reply: Decodable { let Status: Int; let Answer: [Answer]? }

    static let endpoints: [String] = [
        "https://dns.google/resolve",
        "https://cloudflare-dns.com/dns-query",
        "https://8.8.8.8/resolve",
        "https://1.1.1.1/dns-query",
    ]

    private let session: URLSession
    private let endpoints: [String]
    private var cache: [String: Entry] = [:]
    /// Hosts the resolver answered NXDOMAIN for (authoritative), until when. Re-asking every request
    /// would cost a DoH round trip per image for CDN hosts that no longer exist.
    private var negative: [String: Date] = [:]
    static let negativeTTL: TimeInterval = 300
    private var inFlight: [String: Task<[IPv4Address], Error>] = [:]

    init(endpoints: [String] = DoHResolver.endpoints) {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 5
        config.timeoutIntervalForResource = 8
        config.urlCache = nil
        config.waitsForConnectivity = false
        self.session = URLSession(configuration: config)
        self.endpoints = endpoints
    }

    /// Resolved IPv4 addresses for `host` (cached for the record TTL, clamped to 60...600 s).
    func resolve(_ host: String) async throws -> [IPv4Address] {
        let key = host.lowercased()
        if let entry = cache[key], entry.expires > Date() { return entry.addresses }
        if let until = negative[key] {
            if until > Date() { throw ResolveError.notFound(host) }
            negative[key] = nil
        }
        if let task = inFlight[key] { return try await task.value }
        let task = Task { try await self.query(key) }
        inFlight[key] = task
        defer { inFlight[key] = nil }
        do {
            return try await task.value
        } catch let error as ResolveError {
            if case .notFound = error {
                if negative.count > 2000 { negative.removeAll() }
                negative[key] = Date().addingTimeInterval(Self.negativeTTL)
            }
            throw error
        }
    }

    /// Drops a cached answer (e.g. after every address failed to connect).
    func invalidate(_ host: String) { cache[host.lowercased()] = nil }

    private func query(_ host: String) async throws -> [IPv4Address] {
        guard let encoded = host.addingPercentEncoding(withAllowedCharacters: .urlHostAllowed) else {
            throw ResolveError.notFound(host)
        }
        for endpoint in endpoints {
            guard let url = URL(string: "\(endpoint)?name=\(encoded)&type=A") else { continue }
            var request = URLRequest(url: url)
            request.setValue("application/dns-json", forHTTPHeaderField: "Accept")
            do {
                let (data, response) = try await session.data(for: request)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else { continue }
                let reply = try JSONDecoder().decode(Reply.self, from: data)
                if reply.Status == 3 { throw ResolveError.notFound(host) }   // NXDOMAIN is authoritative
                guard reply.Status == 0 else { continue }
                let records = (reply.Answer ?? []).filter { $0.type == 1 }
                let addresses = records.compactMap { IPv4Address($0.data) }
                guard !addresses.isEmpty else { continue }
                let ttl = records.compactMap(\.TTL).min() ?? 60
                cache[host] = Entry(addresses: addresses,
                                    expires: Date().addingTimeInterval(TimeInterval(min(max(ttl, 60), 600))))
                return addresses
            } catch let error as ResolveError {
                throw error
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                continue
            }
        }
        throw ResolveError.allEndpointsFailed(host)
    }
}
