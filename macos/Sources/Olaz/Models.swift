import Foundation

// MARK: - Domain models (mirror the renderer's data model)

struct User: Identifiable, Hashable, Codable {
    let id: UUID
    var displayName: String
    var phone: String?
    var isOnline: Bool = false
    var statusText: String?
    var avatarInitials: String { nameInitials(displayName) }

    init(id: UUID = UUID(), displayName: String, phone: String? = nil, isOnline: Bool = false, statusText: String? = nil) {
        self.id = id
        self.displayName = displayName
        self.phone = phone
        self.isOnline = isOnline
        self.statusText = statusText
    }
}

enum ConversationKind: Hashable, Codable {
    case direct
    case group(memberCount: Int)
    case community(memberCount: Int)
    case official

    var isGroupLike: Bool {
        switch self {
        case .group, .community: true
        case .direct, .official: false
        }
    }
}

struct ConversationLabel: Identifiable, Hashable, Codable {
    let id: UUID
    var name: String
    var colorHex: String
}

struct Conversation: Identifiable, Hashable, Codable {
    let id: UUID
    var title: String
    var kind: ConversationKind
    var participantIds: [UUID] = []
    var labelIds: [UUID] = []
    var isPinned: Bool = false
    var isMuted: Bool = false
    var unreadCount: Int = 0
    var lastMessageAt: Date = .now
    var isEndToEndEncrypted: Bool = false
    /// "Ghim tin nhắn" — shown as a glass bar above the message list.
    var pinnedMessageId: UUID?
}

/// A local or demo image. `fileURL` wins when present (drag & drop / attach).
struct MediaRef: Hashable, Codable {
    var fileURL: URL?
    var demoSymbol: String?
}

struct FileRef: Hashable, Codable {
    var name: String
    var bytes: Int64
    var fileURL: URL?

    var sizeLabel: String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }
}

enum MessageKind: Hashable, Codable {
    case text(String)
    case image(MediaRef)
    case sticker(String)        // emoji-rendered sticker
    case voice(seconds: Int)
    case file(FileRef)
    case call(video: Bool, missed: Bool, minutes: Int)
    case todo(title: String, due: String)
    case system(String)

    /// Conversation-list / notification / quote preview (STR_* bracket forms).
    var previewText: String {
        switch self {
        case .text(let s): s
        case .image: "[Hình ảnh]"
        case .sticker: "[Sticker]"
        case .voice: "[Tin nhắn thoại]"
        case .file(let f): "[File] \(f.name)"
        case .call(let video, let missed, _):
            missed ? "[Cuộc gọi \(video ? "video" : "thoại") nhỡ]" : "[Cuộc gọi \(video ? "video" : "thoại")]"
        case .todo(let title, _): "[Công việc] \(title)"
        case .system(let s): s
        }
    }

    var isSystem: Bool {
        if case .system = self { true } else { false }
    }
}

/// "Trả lời" — a quote of another message carried by the reply.
struct QuotedMessage: Hashable, Codable {
    var messageId: UUID
    var senderName: String
    var preview: String
}

struct Reaction: Hashable, Codable {
    var emoji: String
    var count: Int
    var includesMe: Bool = false
}

/// Sender-side delivery state; Zalo shows "Đã gửi / Đã nhận / Đã xem" under
/// the last outgoing message.
enum DeliveryStatus: String, Hashable, Codable {
    case sending, sent, delivered, seen, failed

    var label: String {
        switch self {
        case .sending: "Đang gửi"
        case .sent: "Đã gửi"
        case .delivered: "Đã nhận"
        case .seen: "Đã xem"
        case .failed: "Gửi không thành công"
        }
    }
}

struct Message: Identifiable, Hashable, Codable {
    var id: UUID = UUID()
    var conversationId: UUID
    var senderId: UUID
    var kind: MessageKind
    var sentAt: Date
    var quote: QuotedMessage?
    var reactions: [Reaction] = []
    var isRecalled: Bool = false
    var isForwarded: Bool = false
    var status: DeliveryStatus = .seen
    /// Signal ciphertext this message travelled as (E2EE conversations only).
    var e2ee: E2EEEnvelope?

    var isMine: Bool { senderId == User.me.id }

    var previewText: String {
        isRecalled ? "Tin nhắn đã thu hồi" : kind.previewText
    }

    /// Reacts with `emoji`; reacting again with the same emoji is a no-op
    /// (Zalo stacks taps on the same reaction, it does not toggle).
    mutating func addReaction(_ emoji: String) {
        if let i = reactions.firstIndex(where: { $0.emoji == emoji }) {
            if !reactions[i].includesMe {
                reactions[i].count += 1
                reactions[i].includesMe = true
            }
        } else {
            reactions.append(Reaction(emoji: emoji, count: 1, includesMe: true))
        }
    }

    mutating func removeMyReactions() {
        reactions = reactions.compactMap { r in
            guard r.includesMe else { return r }
            var r = r
            r.count -= 1
            r.includesMe = false
            return r.count > 0 ? r : nil
        }
    }
}

/// What the composer hands to the service.
struct OutgoingMessage {
    var id = UUID()
    var kind: MessageKind
    var quote: QuotedMessage?
    var isForwarded = false
}

struct CallRecord: Identifiable, Hashable {
    let id = UUID()
    var peer: String
    var isVideo: Bool
    var incoming: Bool
    var missed: Bool
    var at: Date
    var durationMinutes: Int
}

struct TodoTask: Identifiable, Hashable {
    var id = UUID()
    var title: String
    var assignee: String
    var dueLabel: String
    var isDone: Bool = false
    var assignedByMe: Bool = false
}

struct FriendRequest: Identifiable, Hashable {
    let id = UUID()
    var user: User
    var note: String
    var at: Date
}

extension User {
    /// The logged-in account used by the demo engine (stable across launches
    /// so persisted demo state stays consistent).
    static let me = User(
        id: UUID(uuidString: "00000000-0000-4000-8000-000000000001")!,
        displayName: "Bạn", phone: "+84 90 xxx xxxx", isOnline: true
    )
}

func nameInitials(_ name: String) -> String {
    let words = name.split(separator: " ").filter { $0.first?.isLetter ?? false }
    guard !words.isEmpty else { return String(name.prefix(1)) }
    let base = words.count > 1
        ? String(words.first!.prefix(1)) + String(words.last!.prefix(1))
        : String(words[0].prefix(2))
    return base.uppercased()
}
