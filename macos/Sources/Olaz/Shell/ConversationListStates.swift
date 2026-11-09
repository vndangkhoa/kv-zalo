import SwiftUI

// Non-list states of the conversation list column: explanatory banners, the locked "Trò chuyện ẩn" PIN screen,
// loading and the per-mailbox empty states.

/// Explanatory strip at the top of the stranger / "Khác" / unlocked-hidden mailboxes: a bold lead-in and the explanation.
struct ConversationListBanner: View {
    let symbol: String
    let lead: String
    let message: String
    /// Between the lead-in and the message (a line break or a space).
    var separator = " "
    var actionTitle: String?
    var action: (() -> Void)?

    private var text: AttributedString {
        var lead = AttributedString(lead)
        lead.font = .system(size: 12, weight: .semibold)
        lead.foregroundColor = .primary
        return lead + AttributedString(separator + message)
    }

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(.tint)
                .frame(width: 20)
            Text(text)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let actionTitle, let action {
                Button(actionTitle, action: action).buttonStyle(.glass).controlSize(.small)
            }
        }
        .padding(.horizontal, 11).padding(.vertical, 9)
        .background(.quaternary.opacity(0.7), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityElement(children: .combine)
    }
}

/// "Đang đồng bộ hội thoại…" while the list (or the hidden-chat state it depends on) has not arrived.
struct ConversationListLoading: View {
    let stalled: Bool

    var body: some View {
        VStack(spacing: 10) {
            ProgressView()
            Text(stalled ? HiddenStateGuard.stalledMessage : "Đang đồng bộ hội thoại…")
                .font(.system(size: 12.5)).foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 20)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// "Trò chuyện ẩn" while locked: the 4-digit PIN lists the hidden chats (the same search the sidebar field runs for a PIN).
struct ConversationListLocked: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell
    @State private var wrong = false
    @State private var shakes: CGFloat = 0
    @State private var checking = false

    var body: some View {
        VStack(spacing: 12) {
            if let image = OlazAssets.image("illo-lock.png") {
                Image(nsImage: image).resizable().scaledToFit().frame(width: 132, height: 132)
                    .accessibilityHidden(true)
            } else {
                Image(systemName: "lock.fill").font(.system(size: 44)).foregroundStyle(.tertiary)
            }
            Text("Trò chuyện ẩn đang khoá").font(.headline)
            Text("Nhập mã PIN gồm 4 chữ số để xem các trò chuyện ẩn.")
                .font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
            PINEntryField(onDone: submit)
                .modifier(ConversationListShake(shakes: shakes))
                .padding(.top, 6)
                .disabled(checking)
            Text("Mã PIN không đúng")
                .font(.system(size: 12)).foregroundStyle(.red)
                .opacity(wrong ? 1 : 0)
                .accessibilityHidden(!wrong)
        }
        .padding(.horizontal, 24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func submit(_ pin: String) {
        checking = true
        Task {
            // Nothing is unlocked by this: the call lists the hidden chats, and opening one unlocks just that chat.
            let ids = await engine.hiddenConversations(matching: pin)
            checking = false
            if let ids {
                wrong = false
                shell.unlockedHiddenIds = ids
                return
            }
            wrong = true
            withAnimation(.linear(duration: 0.4)) { shakes += 3 }
            try? await Task.sleep(for: .seconds(2.5))
            wrong = false
        }
    }
}

/// Empty list of one mailbox (copy of the Olaz prototype), or no search result.
struct ConversationListEmpty: View {
    @Environment(ZaloEngine.self) private var engine
    let destination: SidebarDestination
    let searchText: String?

    var body: some View {
        if let searchText {
            ContentUnavailableView {
                Label("Không có kết quả cho “\(searchText)”", systemImage: "magnifyingglass")
            } description: {
                Text(engine.conversationActions.hiddenEnabled
                     ? "Thử tìm theo tên, hoặc nhập mã PIN để xem trò chuyện ẩn."
                     : "Thử tìm theo tên người hoặc nhóm.")
            }
        } else {
            let copy = self.copy
            ContentUnavailableView {
                Label(copy.title, systemImage: copy.symbol)
            } description: {
                Text(copy.message)
            }
        }
    }

    private var copy: (symbol: String, title: String, message: String) {
        switch destination {
        case .unread: ("checkmark.circle", "Không có tin nhắn chưa đọc", "Bạn đã đọc hết mọi cuộc trò chuyện.")
        case .strangers: ("person.crop.circle.badge.questionmark", "Không có tin nhắn từ người lạ", "Tin nhắn từ người chưa kết bạn sẽ hiện ở đây.")
        case .hidden: ("eye.slash", "Chưa có trò chuyện ẩn", "Chuột phải vào một cuộc trò chuyện và chọn Ẩn trò chuyện.")
        case .other: ("archivebox", "Mục Khác đang trống", "Chuột phải vào một cuộc trò chuyện để chuyển nó sang Khác.")
        case .label: ("tag", "Chưa có cuộc trò chuyện", "Chuột phải vào một cuộc trò chuyện và chọn Phân loại để gắn nhãn này.")
        default: ("bubble.left.and.bubble.right", "Chưa có cuộc trò chuyện", "Cuộc trò chuyện mới sẽ hiện ở đây.")
        }
    }
}
