import SwiftUI

// Conversation actions UI: the official context menu (sidebar row = `Thread` preset, chat header = `ChatBox` preset), the delete
// confirmation, the full pin board ("Ghim hội thoại") and the toast. The data and every write go through
// `ZaloEngine+ConversationActions.swift` (official services, see the adapter header). Hidden-chat PIN flows: HiddenChatViews.swift.

/// Menu items in the official order: pin · (Khác/Ưu tiên · Phân loại · đã đọc/chưa đọc) · (thông báo · ẩn) · xóa.
struct ConversationActionsMenu: View {
    @Environment(ZaloEngine.self) private var engine
    let conversationId: String
    var preset: ConversationMenuPreset = .thread
    @Binding var showLabelManager: Bool

    var body: some View {
        let state = engine.conversationActions
        if state.loaded {
            let plan = ConversationMenuPlan.make(id: conversationId, preset: preset, state: state, unread: unread,
                                                 muted: engine.isConversationMuted(conversationId))
            pinItem(plan.pin)
            if plan.pin != nil, plan.archive != nil || plan.showsLabel || plan.read != nil { Divider() }
            archiveItem(plan.archive)
            if plan.showsLabel { ConversationLabelMenu(conversationId: conversationId, showLabelManager: $showLabelManager) }
            readItem(plan.read)
            if plan.mute != nil || plan.hide != nil, plan.pin != nil || plan.archive != nil || plan.showsLabel || plan.read != nil { Divider() }
            muteItems(plan.mute)
            hideItem(plan.hide)
            if plan.canDelete {
                Divider()
                Button("Xóa hội thoại", systemImage: "trash", role: .destructive) { engine.requestDeleteConversation(conversationId) }
            }
        } else {
            // The adapter has not published yet: only what does not depend on it.
            ConversationLabelMenu(conversationId: conversationId, showLabelManager: $showLabelManager)
            ConversationMuteMenu(conversationId: conversationId)
        }
    }

    private var unread: Int { engine.conversations.first { $0.id == conversationId }?.unread ?? 0 }

    @ViewBuilder private func pinItem(_ pin: ConversationMenuPlan.Pin?) -> some View {
        switch pin {
        case .pin: Button("Ghim hội thoại", systemImage: "pin") { engine.pinConversation(conversationId) }
        case .unpin: Button("Bỏ ghim hội thoại", systemImage: "pin.slash") { engine.unpinConversation(conversationId) }
        case nil: EmptyView()
        }
    }

    @ViewBuilder private func archiveItem(_ archive: ConversationMenuPlan.Archive?) -> some View {
        switch archive {
        case .moveToOther: Button("Chuyển sang mục Khác", systemImage: "tray.and.arrow.down") { engine.moveConversation(conversationId, toOther: true) }
        case .moveToFocus: Button("Chuyển sang Tin nhắn", systemImage: "tray.and.arrow.up") { engine.moveConversation(conversationId, toOther: false) }
        case nil: EmptyView()
        }
    }

    @ViewBuilder private func readItem(_ read: ConversationMenuPlan.Read?) -> some View {
        switch read {
        case .markRead: Button("Đánh dấu đã đọc", systemImage: "checkmark.message") { engine.markConversationRead(conversationId) }
        case .markUnread: Button("Đánh dấu chưa đọc", systemImage: "message.badge") { engine.markConversationUnread(conversationId) }
        case nil: EmptyView()
        }
    }

    @ViewBuilder private func muteItems(_ mute: ConversationMenuPlan.Mute?) -> some View {
        switch mute {
        case .unmute:
            if let info = engine.notificationSettings.mute(for: conversationId) {
                Button(info.statusText()) {}.disabled(true)
            }
            Button("Bật thông báo", systemImage: "bell") { engine.unmuteConversation(conversationId) }
        case .mute:
            Menu("Tắt thông báo", systemImage: "bell.slash") {
                ForEach(MuteDuration.allCases) { duration in
                    Button(duration.title) { engine.muteConversation(conversationId, duration) }
                }
            }
        case nil: EmptyView()
        }
    }

    @ViewBuilder private func hideItem(_ hide: ConversationMenuPlan.Hide?) -> some View {
        switch hide {
        case .hide: Button("Ẩn trò chuyện", systemImage: "eye.slash") { engine.conversationPrompt = .hide(id: conversationId) }
        case .unhide: Button("Bỏ ẩn trò chuyện", systemImage: "eye") { engine.conversationPrompt = .unhide(id: conversationId) }
        case nil: EmptyView()
        }
    }
}

/// "⋯" menu of the chat header (official `ChatBox` preset).
struct ConversationHeaderMenu: View {
    @Environment(ZaloEngine.self) private var engine
    let conversationId: String
    @State private var showLabelManager = false

    var body: some View {
        let state = engine.conversationActions
        let plan = ConversationMenuPlan.make(id: conversationId, preset: .chatBox, state: state, unread: 0,
                                             muted: engine.isConversationMuted(conversationId))
        // Without the adapter's data the menu would only offer the label / mute items, which stay in the sidebar row menu.
        if state.loaded, !plan.isEmpty {
            Menu {
                ConversationActionsMenu(conversationId: conversationId, preset: .chatBox, showLabelManager: $showLabelManager)
            } label: {
                Label("Tùy chọn hội thoại", systemImage: "ellipsis")
            }
            .help("Tùy chọn hội thoại")
            .sheet(isPresented: $showLabelManager) { LabelManagerView() }
        }
    }
}

/// Bell shown next to a muted conversation's name.
struct MutedBell: View {
    @Environment(ZaloEngine.self) private var engine
    let conversationId: String
    var size: CGFloat = 10

    var body: some View {
        if let info = engine.notificationSettings.mute(for: conversationId) {
            Image(systemName: "bell.slash.fill")
                .font(.system(size: size))
                .foregroundStyle(.secondary)
                .help(info.statusText())
        }
    }
}

// MARK: - Prompts (presented by the shell)

extension View {
    /// Delete confirmation, pin board and hidden-chat sheets requested by conversation actions.
    func conversationPrompts(engine: ZaloEngine, onUnlocked: @escaping (String) -> Void) -> some View {
        modifier(ConversationPromptModifier(engine: engine, onUnlocked: onUnlocked))
    }

    /// Toast of the latest conversation action (same look as the contacts toast).
    func conversationToast(engine: ZaloEngine) -> some View {
        modifier(ConversationToastModifier(engine: engine))
    }
}

private struct ConversationPromptModifier: ViewModifier {
    let engine: ZaloEngine
    let onUnlocked: (String) -> Void

    private var deleting: String? {
        if case .delete(let id) = engine.conversationPrompt { return id }
        return nil
    }

    func body(content: Content) -> some View {
        content
            // Official confirm: line 1 "Toàn bộ nội dung trò chuyện sẽ bị xóa vĩnh viễn." / line 2 "Bạn có chắc chắn muốn xóa?"; Xóa (danger) / Không.
            // The prompt is cleared inside the actions only, so the id the button acts on is the one that was asked about.
            .alert(ConversationActionText.deleteLine1,
                   isPresented: Binding(get: { deleting != nil }, set: { _ in }),
                   presenting: deleting) { id in
                Button(ConversationActionText.deleteConfirm, role: .destructive) {
                    engine.conversationPrompt = nil
                    engine.confirmDeleteConversation(id)
                }
                Button(ConversationActionText.deleteCancel, role: .cancel) { engine.conversationPrompt = nil }
            } message: { _ in
                Text(ConversationActionText.deleteLine2)
            }
            .sheet(item: Binding(
                get: { deleting == nil ? engine.conversationPrompt : nil },
                set: { if $0 == nil, deleting == nil { engine.conversationPrompt = nil } })) { prompt in
                    switch prompt {
                    case .pinBoard(let id): PinBoardSheet(conversationId: id)
                    case .hide(let id): HiddenChatSheet(flow: HiddenChatFlow(kind: .hide(id), hasPIN: engine.conversationActions.hasHiddenPIN), onUnlocked: onUnlocked)
                    case .unhide(let id):
                        HiddenChatSheet(flow: HiddenChatFlow(kind: .unhide(id, requiresPIN: !engine.conversationUI.isUnlocked(id)),
                                                             hasPIN: engine.conversationActions.hasHiddenPIN), onUnlocked: onUnlocked)
                    case .unlock(let id): HiddenChatSheet(flow: HiddenChatFlow(kind: .open(id), hasPIN: true), onUnlocked: onUnlocked)
                    case .delete: EmptyView()
                    }
                }
    }
}

private struct ConversationToastModifier: ViewModifier {
    let engine: ZaloEngine
    @State private var shown: ConversationActionUI.Toast?

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .bottom) {
                if let shown {
                    HStack(spacing: 8) {
                        switch shown.kind {
                        case .success: Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.green)
                        case .neutral: Image(systemName: "info.circle.fill").foregroundStyle(Color.secondary)
                        case .failure: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Color.orange)
                        }
                        Text(shown.text).font(.system(size: 12.5, weight: .medium)).lineLimit(2)
                    }
                    .padding(.horizontal, 14).padding(.vertical, 9)
                    .glassEffect(.regular, in: Capsule())
                    .padding(.bottom, 22)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .allowsHitTesting(false)
                }
            }
            .onChange(of: engine.conversationUI.toast) { _, toast in
                guard let toast else { return }
                withAnimation(.snappy) { shown = toast }
                Task {
                    try? await Task.sleep(for: .seconds(3.2))
                    if shown == toast { withAnimation(.snappy) { shown = nil } }
                }
            }
    }
}

// MARK: - Pin board (official POP_PINNED_CONVERSATION)

/// Shown when the tab is full: choose which pins to remove, then the new conversation is pinned.
struct PinBoardSheet: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    let conversationId: String
    @State private var board: PinBoard?
    @State private var loadFailed = false
    @State private var removed: Set<String> = []
    @State private var busy = false

    var body: some View {
        VStack(spacing: 0) {
            Text("Ghim hội thoại").font(.system(size: 15, weight: .semibold)).padding(.top, 18).padding(.bottom, 10)
            if let board {
                (Text("Bạn chỉ được ghim tối đa \(board.limit) trò chuyện.\nĐể ghim trò chuyện ") +
                 Text(name(of: conversationId)).fontWeight(.semibold) +
                 Text(", vui lòng bỏ ghim ít nhất \(board.need) trò chuyện bên dưới."))
                    .font(.system(size: 13))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 20).padding(.bottom, 10)
                List(board.items.filter { !removed.contains($0) }, id: \.self) { id in
                    HStack(spacing: 10) {
                        // A hidden chat keeps its identity private here too.
                        if engine.conversationActions.isHidden(id) {
                            Image(systemName: "lock.fill").foregroundStyle(.secondary).frame(width: 34, height: 34)
                                .background(Circle().fill(.quaternary))
                        } else {
                            RemoteAvatar(url: engine.conversations.first { $0.id == id }?.avatar, name: name(of: id), size: 34)
                        }
                        Text(name(of: id)).font(.system(size: 13.5)).lineLimit(1)
                        Spacer()
                        Button("Bỏ ghim") { removed.insert(id) }.buttonStyle(.glass).controlSize(.small)
                    }
                }
                .frame(minHeight: 200, maxHeight: 340)
            } else if loadFailed {
                ContentUnavailableView("Không tải được danh sách ghim", systemImage: "pin.slash").frame(height: 200)
            } else {
                ProgressView().frame(height: 200)
            }
            HStack {
                Spacer()
                Button("Hủy") { dismiss() }.buttonStyle(.glass).keyboardShortcut(.cancelAction)
                Button("Ghim hội thoại") { confirm() }
                    .buttonStyle(.glassProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(board.map { !$0.freesSlot(removing: removed.count) } ?? true || busy)
            }
            .padding(16)
        }
        .frame(width: 440)
        .task {
            board = await engine.pinBoard(for: conversationId)
            loadFailed = board == nil
        }
    }

    private func name(of id: String) -> String {
        if engine.conversationActions.isHidden(id) { return "Trò chuyện ẩn" }
        return engine.conversations.first { $0.id == id }?.displayName ?? (id.hasPrefix("g") ? "Nhóm" : "Người dùng Zalo")
    }

    private func confirm() {
        busy = true
        Task {
            let result = await engine.pinReplacing(Array(removed), pinning: conversationId)
            busy = false
            if result.ok { dismiss(); return }
            // The unpin may have gone through while the pin failed: show the board as it is now so a retry starts from it.
            removed = []
            board = await engine.pinBoard(for: conversationId)
            if board == nil { dismiss() }
        }
    }
}
