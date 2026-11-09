import SwiftUI

/// Middle pane: conversation list with search ("Tìm kiếm"), pinned section,
/// labels ("Phân loại") — mirrors `conv-item` markup in the renderer.
struct ConversationListView: View {
    @Environment(AppStore.self) private var store
    @State private var filter: LabelFilter = .all

    enum LabelFilter: Hashable {
        case all, pinned
        case label(UUID)
    }

    var body: some View {
        @Bindable var store = store
        return VStack(spacing: 0) {
            header
            labelFilters
            list
        }
        .frame(minWidth: 300, idealWidth: 340, maxWidth: 420)
        .background(Color(NSColor.controlBackgroundColor).opacity(0.3))
    }

    private var header: some View {
        @Bindable var store = store
        return HStack(spacing: 10) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
            TextField("Tìm kiếm", text: $store.searchText)
                .textFieldStyle(.plain)
            if !store.searchText.isEmpty {
                Button {
                    store.searchText = ""
                } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .glassEffect(in: Capsule())
        .padding(.horizontal, 12)
        .padding(.top, 12)
    }

    private var labelFilters: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                filterChip("Tất cả", kind: .all)
                filterChip("Ưu tiên", kind: .pinned)
                ForEach(store.labels) { label in
                    filterChip(label.name, kind: .label(label.id), color: Color(hex: label.colorHex))
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
        }
    }

    private func filterChip(_ title: String, kind: LabelFilter, color: Color = ZaloTheme.blue) -> some View {
        let selected = filter == kind
        return Button {
            withAnimation(.snappy) { filter = kind }
        } label: {
            Text(title)
                .font(.system(size: 12, weight: selected ? .semibold : .regular))
                .foregroundStyle(selected ? .white : color)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
        }
        .buttonStyle(.plain)
        .background {
            if selected {
                Capsule().fill(color)
            } else {
                Capsule().fill(color.opacity(0.12))
            }
        }
    }

    private var visibleConversations: [Conversation] {
        let base = store.filteredConversations
        switch filter {
        case .all: return base
        case .pinned: return base.filter(\.isPinned)
        case .label(let id): return base.filter { $0.labelIds.contains(id) }
        }
    }

    private var list: some View {
        ScrollView {
            LazyVStack(spacing: 2) {
                if visibleConversations.isEmpty {
                    VStack(spacing: 8) {
                        Image(systemName: "bubble.left.and.text.bubble.right")
                            .font(.system(size: 34))
                            .foregroundStyle(.tertiary)
                        Text("Không có hội thoại nào")
                            .font(.system(size: 13))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.top, 80)
                }
                ForEach(visibleConversations) { conversation in
                    ConversationRow(conversation: conversation)
                        .id(conversation.id)
                }
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 12)
        }
    }
}

// MARK: - Row

struct ConversationRow: View {
    @Environment(AppStore.self) private var store
    let conversation: Conversation

    var body: some View {
        let selected = store.selectedConversationId == conversation.id
        Button {
            store.select(conversation.id)
        } label: {
            HStack(spacing: 10) {
                AvatarView(
                    name: conversation.title,
                    size: 46,
                    isOnline: firstPeer?.isOnline ?? false,
                    kind: conversation.kind
                )

                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 5) {
                        Text(conversation.title)
                            .font(.system(size: 14, weight: .semibold))
                            .lineLimit(1)
                        if conversation.isEndToEndEncrypted {
                            Image(systemName: "lock.fill")
                                .font(.system(size: 9))
                                .foregroundStyle(ZaloTheme.blue)
                        }
                        Spacer(minLength: 4)
                        Text(conversation.lastMessageAt, format: .relative(presentation: .named))
                            .font(.system(size: 11))
                            .foregroundStyle(.tertiary)
                    }

                    HStack(spacing: 6) {
                        // Last message preview
                        Text(previewText)
                            .font(.system(size: 12.5))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                        Spacer(minLength: 4)
                        if conversation.isMuted {
                            Image(systemName: "bell.slash.fill")
                                .font(.system(size: 9))
                                .foregroundStyle(.tertiary)
                        }
                        if conversation.unreadCount > 0 {
                            UnreadBadge(count: conversation.unreadCount)
                        }
                    }

                    if !conversation.labelIds.isEmpty {
                        HStack(spacing: 4) {
                            ForEach(store.labels.filter { conversation.labelIds.contains($0.id) }) { label in
                                LabelChip(label: label)
                            }
                        }
                    }
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(selected ? AnyShapeStyle(ZaloTheme.blue.opacity(0.12)) : AnyShapeStyle(.clear))
        }
        .overlay(alignment: .leading) {
            if selected {
                Capsule()
                    .fill(ZaloTheme.blue)
                    .frame(width: 3, height: 22)
                    .offset(x: -2)
            }
        }
        .contextMenu {
            Button(conversation.isPinned ? "Bỏ ghim hội thoại" : "Ghim hội thoại") {
                pin(conversation)
            }
            Button("Đánh dấu đã đọc") { store.markRead(conversation.id) }
        }
    }

    private var firstPeer: User? {
        conversation.participantIds.first.flatMap { store.user($0) }
    }

    private var previewText: String {
        if let typing = store.typingIn[conversation.id] {
            return "\(typing) đang nhập…"
        }
        let msgs = store.messages(in: conversation.id)
        guard let last = msgs.last else { return "Chưa có tin nhắn" }
        if last.isMine { return "Bạn: \(preview(of: last))" }
        return preview(of: last)
    }

    private func preview(of message: Message) -> String {
        switch message.kind {
        case .text(let s), .reply(_, let s): s
        case .image: "[Hình ảnh]"
        case .sticker: "[Sticker]"
        case .voice: "[Tin nhắn thoại]"
        case .file(let name, _): "[Tệp] \(name)"
        case .call(let video, let missed): missed ? "[Cuộc gọi \(video ? "video" : "thoại") nhỡ]" : "[Cuộc gọi]"
        case .todo(let title, _): "[Công việc] \(title)"
        case .system(let s): s
        }
    }

    private func pin(_ conversation: Conversation) {
        store.togglePin(conversation.id)
    }
}
