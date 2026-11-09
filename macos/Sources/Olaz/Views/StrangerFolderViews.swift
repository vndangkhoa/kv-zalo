import SwiftUI

// "Tin nhắn từ người lạ": the official client folds strangers I have not answered into one list entry
// (`stranger_box`); opening it lists them newest first (the "Người lạ" mailbox, which explains itself with a banner).
// The folder has no accept / ignore / block actions of its own in the official client — those live on the chat
// banner of each stranger.

/// The `stranger_box` entry shown in the conversation list.
struct StrangerBoxRow: View {
    let box: ConversationExtras.StrangerBox
    let newest: ZaloEngine.ConversationSummary?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "person.crop.circle.badge.questionmark.fill")
                    .font(.system(size: 19, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 44, height: 44)
                    .background(Circle().fill(.tint))
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 5) {
                        Text("Tin nhắn từ người lạ")
                            .font(.system(size: 13.5, weight: box.unreadChats > 0 ? .semibold : .medium))
                            .lineLimit(1)
                        Spacer(minLength: 4)
                        if let date = newest?.preview?.date {
                            Text(ConversationListTime.string(for: date))
                                .font(.system(size: 11.5)).monospacedDigit().foregroundStyle(.secondary).lineLimit(1)
                        }
                    }
                    HStack(alignment: .top, spacing: 6) {
                        Text(subtitle)
                            .font(.system(size: 12.5))
                            .foregroundStyle(box.unreadChats > 0 ? .primary : .secondary)
                            .lineLimit(2, reservesSpace: true)
                            .multilineTextAlignment(.leading)
                        Spacer(minLength: 4)
                        if box.unreadChats > 0 { ConversationUnreadBadge(count: box.unreadChats) }
                    }
                }
            }
            .padding(.vertical, 2)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
    }

    private var subtitle: String {
        if let newest {
            if let p = newest.preview, !p.text.isEmpty { return "\(newest.displayName): \(p.text)" }
            return newest.displayName
        }
        return "\(box.chats) hội thoại"
    }
}

/// Small "Người lạ" capsule shown beside a stranger's name.
struct StrangerBadge: View {
    var body: some View {
        Text("Người lạ")
            .font(.system(size: 9, weight: .semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 5).padding(.vertical, 1)
            .background(Capsule().fill(.secondary.opacity(0.15)))
    }
}
