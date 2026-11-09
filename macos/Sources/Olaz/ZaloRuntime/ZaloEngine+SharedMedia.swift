import Foundation

// Shared media of a conversation ("Kho lưu trữ"), bridged to `HostScripts/media-store-adapter.js` (`window.__zaloMediaStore`).
// Everything ever shared comes from the official media service (its local DB plus the sources the account allows), not from the
// messages loaded in the chat box.
// Swift → JS : page(convId, kind, cursor, limit) → Promise<{items:[ChatMessageJSON], cursor: string|null, more: Bool} | null>
//   Items are ordinary chat messages (photo / video / file / link kinds, same ids as the loaded copy of the message), newest first.
//   `cursor` is what the next page is asked after (nil on the first); `more` false at the end. null = service unavailable / failed.

/// What the three lists of the storage panel hold.
enum SharedMediaKind: String, CaseIterable, Identifiable, Sendable {
    case images, files, links

    var id: String { rawValue }

    /// Whether a chat message belongs in this list (the same split the official panel makes by message type).
    func accepts(_ kind: ChatMessage.Kind) -> Bool {
        switch (self, kind) {
        case (.images, .photo), (.images, .video), (.files, .file): true
        case (.links, .link(_, let url, _, _, _)): url != nil
        default: false
        }
    }
}

/// One page of a shared-media list.
struct SharedMediaPage: Equatable, Sendable {
    var items: [ChatMessage]
    /// Pass back as `after` to read the next (older) page.
    var cursor: String?
    var more: Bool

    init(items: [ChatMessage], cursor: String?, more: Bool) {
        self.items = items
        self.cursor = cursor
        self.more = more
    }

    /// Parses what `page()` resolves to; nil for anything that is not a page (the adapter answers null when the service is unavailable).
    init?(json: Any?, conversationId: String) {
        guard let root = json as? [String: Any], let rows = root["items"] as? [[String: Any]] else { return nil }
        items = rows.compactMap { ChatMessage(json: $0, fallbackConversation: conversationId) }
        cursor = (root["cursor"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        more = (root["more"] as? NSNumber)?.boolValue ?? false
    }
}

/// Where the pages come from: the engine, or a stand-in in tests.
@MainActor
protocol SharedMediaSource: AnyObject {
    func sharedMedia(in conversationId: String, kind: SharedMediaKind, after cursor: String?, limit: Int) async -> SharedMediaPage?
}

extension ZaloEngine: SharedMediaSource {
    /// A page of the photos / videos, files or links ever shared in `conversationId`, older than `cursor` (nil = the newest).
    /// nil when the official media service could not answer (not ready yet, error, timeout) — not the same as "nothing shared".
    func sharedMedia(in conversationId: String, kind: SharedMediaKind, after cursor: String?, limit: Int) async -> SharedMediaPage? {
        let args = [conversationId, kind.rawValue, cursor ?? NSNull(), limit] as [Any]
        guard let data = try? JSONSerialization.data(withJSONObject: args, options: [.fragmentsAllowed]),
              let json = String(data: data, encoding: .utf8) else { return nil }
        let script = "const s = window.__zaloMediaStore; if (!s || !s.page) return null; return await s.page(...\(json));"
        return SharedMediaPage(json: await host?.evaluate(script), conversationId: conversationId)
    }
}
