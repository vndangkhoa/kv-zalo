import SwiftUI

// Small pieces shared by the conversation list column and the sidebar: timestamps, the unread capsule, the short tile
// name, unread counts per mailbox / label.

/// Unread numbers beside the sidebar rows and the label menu: chats (not messages) with something unread. Hidden chats never count.
struct UnreadCounts {
    var inbox = 0
    var other = 0
    var strangers = 0
    private var unreadIds: Set<String> = []

    @MainActor init(engine: ZaloEngine) {
        let actions = engine.conversationActions
        let folder = Set(engine.conversationExtras.strangerFolderIds)
        for conversation in engine.visibleConversations where conversation.unread > 0 {
            unreadIds.insert(conversation.id)
            // Counted from the visible list, not from the controller's stranger box: that one still counts hidden chats.
            if folder.contains(conversation.id) { strangers += 1 }
            if actions.flags(for: conversation.id).contains(.archived) {
                other += 1
            } else if !folder.contains(conversation.id) {
                inbox += 1
            }
        }
    }

    func unread(in label: ConversationExtras.Label) -> Int {
        label.convIds.reduce(0) { $0 + (unreadIds.contains($1) ? 1 : 0) }
    }
}

/// Compact timestamp of a list row: today "HH:mm", yesterday "Hôm qua", within the week "T2…CN", otherwise "dd/MM".
@MainActor
enum ConversationListTime {
    private static let clock = formatter("HH:mm")
    private static let day = formatter("dd/MM")
    /// Indexed by `Calendar.component(.weekday)` - 1 (Sunday first).
    private static let weekdays = ["CN", "T2", "T3", "T4", "T5", "T6", "T7"]

    private static func formatter(_ format: String) -> DateFormatter {
        let f = DateFormatter()
        f.locale = Locale(identifier: "vi_VN")
        f.dateFormat = format
        return f
    }

    static func string(for date: Date, now: Date = .now, calendar: Calendar = .current) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return clock.string(from: date) }
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: calendar.startOfDay(for: now)).day ?? 0
        if days == 1 { return "Hôm qua" }
        if (2..<7).contains(days) { return weekdays[calendar.component(.weekday, from: date) - 1] }
        return day.string(from: date)
    }
}

/// Unread count capsule: accent while the chat notifies, grey while it is muted. On a selected (accent) row it
/// inverts so it stays visible.
struct ConversationUnreadBadge: View {
    let count: Int
    var muted = false
    @Environment(\.backgroundProminence) private var prominence

    var body: some View {
        let onSelectedRow = prominence == .increased
        Text(count > 99 ? "99+" : "\(count)")
            .font(.system(size: 11, weight: .bold))
            .monospacedDigit()
            .foregroundStyle(onSelectedRow ? AnyShapeStyle(.tint) : AnyShapeStyle(.white))
            .padding(.horizontal, 5.5)
            .frame(minWidth: 18, minHeight: 18)
            .background(Capsule().fill(onSelectedRow ? AnyShapeStyle(.white.opacity(0.95)) : muted ? AnyShapeStyle(Color.gray) : AnyShapeStyle(.tint)))
            .accessibilityLabel("\(count) tin chưa đọc")
    }
}

/// Dot for a manual "chưa đọc" mark (official unread mark): unread without a count.
struct ConversationUnreadDot: View {
    @Environment(\.backgroundProminence) private var prominence

    var body: some View {
        Circle()
            .fill(prominence == .increased ? AnyShapeStyle(.white) : AnyShapeStyle(.tint))
            .frame(width: 9, height: 9)
            .help("Đánh dấu chưa đọc")
            .accessibilityLabel("Đánh dấu chưa đọc")
    }
}

extension ZaloEngine.ConversationSummary {
    /// Name under a pinned tile: the given name (last word) of a person, the full name of a group.
    func tileName(isSelf: Bool) -> String {
        if isSelf { return "Cloud" }
        if isGroup { return displayName }
        return displayName.split(separator: " ").last.map(String.init) ?? displayName
    }
}

/// Horizontal shake for a rejected PIN.
struct ConversationListShake: GeometryEffect {
    var travel: CGFloat = 8
    var shakes: CGFloat
    var animatableData: CGFloat {
        get { shakes }
        set { shakes = newValue }
    }

    func effectValue(size: CGSize) -> ProjectionTransform {
        ProjectionTransform(CGAffineTransform(translationX: travel * sin(shakes * .pi * 2), y: 0))
    }
}
