import SwiftUI

/// Menu-bar commands of the signed-in window (go to, View, Hội thoại, Help). They only drive `ShellModel` and call the same
/// engine actions as the conversation context menu; the window state itself stays in the shell views.
struct OlazCommands: Commands {
    let engine: ZaloEngine
    let shell: ShellModel
    @AppStorage(OlazPrefs.setupPending) private var setupPending = false

    /// The main window is on screen and usable: signed in, past the first-run setup, and not behind the transfer overlay.
    private var isReady: Bool { engine.phase == .ready && !setupPending && engine.transfer == nil }

    /// A conversation is open in a conversation-list destination (inspector, Hội thoại menu).
    private var hasConversation: Bool { isReady && shell.destination.isConversationList && shell.selection != nil }

    var body: some Commands {
        // No "New Window"; the File menu carries the quick switcher instead.
        CommandGroup(replacing: .newItem) {
            Button("Đi tới cuộc trò chuyện…") { shell.showQuickSwitcher.toggle() }
                .keyboardShortcut("k", modifiers: .command)
                .disabled(!isReady)
            Button("Tạo nhóm…") { shell.composeGroup() }
                .keyboardShortcut("n", modifiers: [.shift, .command])
                .disabled(!isReady || shell.groupCompose != nil)
        }
        // The app prints nothing; this frees ⇧⌘P (Page Setup) for "Ghim hội thoại".
        CommandGroup(replacing: .printItem) {}
        CommandGroup(after: .pasteboard) {
            Divider()
            Button("Tìm kiếm") { shell.focusSearch() }
                .keyboardShortcut("f", modifiers: [.option, .command])
                .disabled(!isReady)
        }

        // The shell's NavigationSplitView is bound to `shell.columns`, so the toggle goes through the model.
        CommandGroup(replacing: .sidebar) {
            Button(shell.columns == .all ? "Ẩn thanh bên" : "Hiện thanh bên") { shell.toggleSidebar() }
                .keyboardShortcut("s", modifiers: [.control, .command])
                .disabled(!isReady)
            Button(shell.showInspector ? "Ẩn thông tin" : "Hiện thông tin") { shell.toggleInspector() }
                .keyboardShortcut("i", modifiers: [.option, .command])
                .disabled(!hasConversation)
            Divider()
            destination(ConversationExtras.inboxTitle, .inbox, key: "1")
            if engine.conversationExtras.archivedEnabled {
                destination(ConversationTypeFilter.other.title, .other, key: "2")
            }
            destination(ConversationTypeFilter.unread.title, .unread, key: "3")
            destination("Người lạ", .strangers, key: "4")
            Divider()
            destination("Danh bạ", .friends, key: "5")
            destination("zCloud", .zcloud, key: "6")
        }

        CommandMenu("Hội thoại") {
            ConversationCommandItems(engine: engine, shell: shell, isReady: isReady)
        }

        CommandGroup(replacing: .help) {
            Button("Hướng dẫn sử dụng \(AppBrand.name)") { shell.startTour() }
                .disabled(!isReady)
        }
    }

    /// Mailbox / Danh bạ item with a check mark for the current sidebar destination. `friends` stands for the whole Danh bạ group.
    private func destination(_ title: String, _ target: SidebarDestination, key: KeyEquivalent) -> some View {
        // "Tin nhắn" is checked under a label filter too (the sidebar keeps it highlighted); choosing it there clears the filter.
        let isCurrent = target == .friends ? shell.destination.isContacts : shell.destination.sidebarRow == target
        let isOn = Binding(get: { isCurrent }, set: { picked in
            guard picked || (target == .inbox && shell.destination != .inbox) else { return }
            // Like a sidebar click: picking a mailbox ends the search of the previous one.
            if !shell.search.isEmpty { shell.search = "" }
            shell.destination = target
        })
        return Toggle(title, isOn: isOn)
            .keyboardShortcut(key, modifiers: [.option, .command])
            .disabled(!isReady)
    }
}

/// Items of the "Hội thoại" menu for the open conversation. The pin / read / archive entries follow
/// `ConversationMenuPlan`, the plan the row context menu (`ConversationActionsMenu`) uses, so both always agree on what
/// is allowed (My Cloud, hidden chats, full pin tabs, left groups…) and call the same engine actions.
private struct ConversationCommandItems: View {
    let engine: ZaloEngine
    let shell: ShellModel
    /// `OlazCommands.isReady`: the window is usable (signed in, setup done, no transfer).
    let isReady: Bool

    var body: some View {
        // A hidden chat that is still locked is not open, so nothing in this menu may act on it.
        let id = isReady && shell.destination.isConversationList ? shell.selection.flatMap { engine.isConversationLocked($0) ? nil : $0 } : nil
        let plan = id.flatMap(plan(for:))

        Button("Trả lời tin nhắn cuối") { shell.replyToLatest() }
            .keyboardShortcut("r", modifiers: .command)
            .disabled(id == nil)
        // Zalo PC's screenshot shortcut (⌘⌥S), here while the app is active.
        Button("Chụp màn hình") { if let id { engine.startScreenCapture(in: id, withOlaz: true) } }
            .keyboardShortcut("s", modifiers: [.option, .command])
            .disabled(id == nil)
        Divider()
        pinItem(plan?.pin, id: id)
        readItem(plan?.read, id: id)
        if engine.conversationExtras.archivedEnabled {
            archiveItem(plan?.archive, id: id)
        }
    }

    private func plan(for id: String) -> ConversationMenuPlan? {
        let state = engine.conversationActions
        guard state.loaded else { return nil }
        let unread = engine.conversations.first { $0.id == id }?.unread ?? 0
        return ConversationMenuPlan.make(id: id, preset: .thread, state: state, unread: unread, muted: engine.isConversationMuted(id))
    }

    @ViewBuilder private func pinItem(_ pin: ConversationMenuPlan.Pin?, id: String?) -> some View {
        let action = { if let id { if pin == .unpin { engine.unpinConversation(id) } else { engine.pinConversation(id) } } }
        Button(pin == .unpin ? "Bỏ ghim hội thoại" : "Ghim hội thoại", action: action)
            .keyboardShortcut("p", modifiers: [.shift, .command])
            .disabled(pin == nil)
    }

    @ViewBuilder private func readItem(_ read: ConversationMenuPlan.Read?, id: String?) -> some View {
        let action = { if let id { if read == .markRead { engine.markConversationRead(id) } else { engine.markConversationUnread(id) } } }
        Button(read == .markRead ? "Đánh dấu là đã đọc" : "Đánh dấu là chưa đọc", action: action)
            .keyboardShortcut("u", modifiers: [.shift, .command])
            .disabled(read == nil)
    }

    @ViewBuilder private func archiveItem(_ archive: ConversationMenuPlan.Archive?, id: String?) -> some View {
        let action = { if let id { engine.moveConversation(id, toOther: archive != .moveToFocus) } }
        Button(archive == .moveToFocus ? "Chuyển sang Tin nhắn" : "Chuyển sang mục Khác", action: action)
            .disabled(archive == nil)
    }
}
