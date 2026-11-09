import AppKit
import SwiftUI

/// Signed-in window: Mail / Messages-style three columns. The sidebar picks a destination (mailboxes, Cloud, zCloud,
/// Danh bạ), the content column lists it (the inbox list also filters by label) and the detail column shows the chat / card.
/// Everything is fed by the official client's state through `ZaloEngine`; window state lives in `ShellModel`.
struct MainShellView: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell
    @State private var resignedAt: Date?

    var body: some View {
        @Bindable var shell = shell
        NavigationSplitView(columnVisibility: $shell.columns) {
            SidebarView()
                .navigationSplitViewColumnWidth(min: 200, ideal: 236, max: 300)
        } content: {
            ContentColumn()
                .navigationSplitViewColumnWidth(min: 300, ideal: 340, max: 440)
        } detail: {
            // A fixed ideal size: the split view sizes columns by their content's ideal size, and the chat's changes with every
            // measuring pass (its lazy message list builds different rows for each height tried). With the inspector open that kept
            // the columns being re-divided at some window widths until AppKit crashed on the endless layout.
            DetailColumn()
                .frame(minWidth: 0, idealWidth: 400, maxWidth: .infinity, minHeight: 0, idealHeight: 600, maxHeight: .infinity)
        }
        .background(HostWindowReader { shell.window = $0 })
        // On the split view, not inside the chat: inside a column it takes over that column's toolbar section.
        .inspector(isPresented: $shell.showInspector) {
            InspectorColumn()
                .inspectorColumnWidth(min: 260, ideal: 300, max: 440)
        }
        .onChange(of: shell.selection) { _, new in
            EngineLog.shared.write("shell selection \(new.map { String($0.prefix(4)) + "…" } ?? "-")")
            openSelection(new)
        }
        .onChange(of: shell.destination) { _, destination in
            EngineLog.shared.write("shell destination \(destination.logName)")
            applyOfficialFilter(destination)
            // Leaving the conversation lists hides the inspector (the chat itself stays open in the official client).
            if !destination.isConversationList { shell.hideInspector() }
        }
        .onChange(of: engine.conversationExtras.loaded) { _, _ in applyOfficialFilter(shell.destination) }
        // A label deleted here or on another device takes its list with it.
        .onChange(of: engine.conversationExtras.labels.map(\.id)) { _, ids in
            if case .label(let id) = shell.destination, engine.conversationExtras.loaded, !ids.contains(id) { shell.destination = .inbox }
        }
        // A deleted / hidden conversation leaves the detail pane.
        .onChange(of: engine.conversationUI.removal) { _, removal in
            if let removal, let selected = shell.selection, removal.ids.contains(selected) { shell.selection = nil }
        }
        // Hidden-chat state arrived / changed (also from another device): a locked conversation must not stay open in the official chat box.
        .onChange(of: engine.conversationActions.hiddenKnown) { _, _ in openSelection(shell.selection) }
        .onChange(of: engine.conversationActions.hiddenIds) { _, _ in closeIfLocked() }
        .onChange(of: engine.conversationUI.unlockedId) { _, _ in closeIfLocked() }
        // The mailbox lists a snapshot, but what really lets a hidden chat open is the PIN search result: once that no longer holds
        // every listed chat (any other 4-character search replaces it), the mailbox is locked again.
        .onChange(of: engine.conversationUI.pinSearchIds) { _, ids in
            if let listed = shell.unlockedHiddenIds, !listed.allSatisfy(ids.contains) { shell.unlockedHiddenIds = nil }
        }
        // The PIN bypass ends after a real absence (60 s inactive, checked on return); a short focus loss, e.g. the ZaloCall window, keeps it.
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didResignActiveNotification)) { _ in resignedAt = .now }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            if let resignedAt, HiddenStateGuard.shouldRelock(inactiveFor: Date.now.timeIntervalSince(resignedAt)) {
                engine.conversationUI.relock()
                shell.unlockedHiddenIds = nil
            }
            resignedAt = nil
        }
        // `ShellModel` outlives the window (⌘W, reopen hours later), the minute counter above does not: lock everything on the way out.
        .onDisappear {
            // The media viewer is a window of its own: it must not outlive the shell (and keep the Dock icon from reopening it).
            MediaViewer.shared.close()
            engine.conversationUI.relock()
            shell.unlockedHiddenIds = nil
            if let selected = shell.selection, engine.conversationActions.isHidden(selected) {
                closeIfLocked()
                shell.selection = nil
            }
        }
        // A conversation that stayed open during a call is closed once the call is over.
        .onChange(of: engine.isInCall) { _, inCall in if !inCall { closeIfLocked() } }
        // Other views (contacts, notifications…) ask to show a conversation.
        .onChange(of: engine.navigationRequest) { _, id in consumeNavigationRequest(id) }
        // Side-bar actions of message cards (`action.open.tab.chat` / `.tab.contact` / `.grouplist`).
        .onReceive(NotificationCenter.default.publisher(for: SectionRouter.notification)) { note in
            switch (note.object as? String).flatMap(SidebarTarget.init(rawValue:)) {
            case .chats: shell.destination = .inbox
            case .groups: shell.destination = .groups
            case .contacts, nil: shell.destination = .friends
            }
            SectionRouter.wantsGroupList = false
        }
        .task {
            consumeNavigationRequest(engine.navigationRequest)
            applyOfficialFilter(shell.destination)
        }
        // The switcher goes below `disabled` so it is inert during a transfer too.
        .overlay { QuickSwitcher() }
        .disabled(engine.transfer != nil)
        .overlay { TransferOverlay() }
        .conversationPrompts(engine: engine) { id in if shell.selection == id { engine.openConversation(id) } }
        .conversationToast(engine: engine)
        .sheet(item: $shell.groupCompose) { request in CreateGroupSheet(request: request) }
    }

    /// Opens the conversation in the official client; a hidden one stays closed until its PIN was entered (official OPEN_CONV screen).
    private func openSelection(_ id: String?) {
        // The bypass belongs to the one conversation opened with the PIN.
        if engine.conversationUI.unlockedId != id { engine.conversationUI.dropUnlock() }
        guard let id else { engine.closeConversation(); return }
        // Until the hidden-chat state is known any conversation could be hidden: nothing is opened.
        guard engine.conversationActions.hiddenKnown else { engine.closeConversation(); return }
        if engine.isConversationLocked(id) {
            if engine.unlockFromPINSearch(id) { engine.openConversation(id); return }
            engine.closeConversation()
            engine.conversationPrompt = .unlock(id: id)
        } else {
            engine.openConversation(id)
        }
    }

    private func closeIfLocked() {
        // Never under a running call (closing the chat box would disturb it); retried when the call ends.
        if let selected = shell.selection, engine.conversationActions.hiddenKnown, engine.isConversationLocked(selected), !engine.isInCall { engine.closeConversation() }
    }

    private func consumeNavigationRequest(_ id: String?) {
        guard let id, !id.isEmpty else { return }
        // Back to the unfiltered default list so the requested conversation is visible.
        shell.destination = .inbox
        shell.search = ""
        shell.selection = id
        engine.navigationRequest = nil
    }

    /// Mailboxes and labels are lists computed by the official ConvListController: tell it which filter to use.
    private func applyOfficialFilter(_ destination: SidebarDestination) {
        let extras = engine.conversationExtras
        guard extras.loaded, let filter = destination.officialFilter(defaultTab: extras.defaultTab) else { return }
        guard extras.typeFilter != filter.tab.rawValue || extras.labelFilterIds != filter.labelIds else { return }
        engine.setConversationFilter(filter.tab, labelIds: filter.labelIds)
    }
}

// MARK: - Columns

/// Middle column: the list for the sidebar destination.
private struct ContentColumn: View {
    @Environment(ShellModel.self) private var shell

    var body: some View {
        let destination = shell.destination
        Group {
            if destination.isConversationList {
                ConversationListColumn()
            } else if destination == .zcloud {
                CloudListColumn()
            } else {
                ContactsListColumn(destination: destination)
            }
        }
    }
}

/// Right column: the chat, contact card or zCloud screen.
private struct DetailColumn: View {
    @Environment(ShellModel.self) private var shell

    var body: some View {
        let destination = shell.destination
        Group {
            if destination.isConversationList {
                ConversationDetail(conversationId: shell.selection)
            } else if destination == .zcloud {
                CloudDetailColumn()
            } else {
                ContactsDetailColumn(destination: destination)
            }
        }
        // Room kept free at the trailing edge during the inspector's reveal (see `ShellModel.toggleInspector`); measured outside it.
        .safeAreaInset(edge: .trailing, spacing: 0) { Color.clear.frame(width: shell.inspectorRoomReserve) }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { shell.detailWidthChanged($0) }
    }
}

/// Inspector of the open chat. Shows nothing for a hidden chat that is still locked (the chat itself is not shown either).
private struct InspectorColumn: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell

    var body: some View {
        if shell.destination.isConversationList, engine.conversationActions.hiddenKnown,
           let conversation = ConversationDetail.summary(for: shell.selection, engine: engine),
           !engine.isConversationLocked(conversation.id) {
            ChatInspector(conversation: conversation)
                .id(conversation.id)
        } else {
            ContentUnavailableView("Không có cuộc trò chuyện", systemImage: "info.circle")
        }
    }
}

// MARK: - Detail

struct ConversationDetail: View {
    @Environment(ZaloEngine.self) private var engine
    let conversationId: String?

    /// The summary the official list knows; for a contact or group without a conversation yet (opened from
    /// Danh bạ) a placeholder built from the contact data — the official chatbox creates the conversation.
    private var conversation: ZaloEngine.ConversationSummary? { Self.summary(for: conversationId, engine: engine) }

    static func summary(for conversationId: String?, engine: ZaloEngine) -> ZaloEngine.ConversationSummary? {
        guard let id = conversationId else { return nil }
        if var known = engine.conversations.first(where: { $0.id == id }) {
            // The official record names My Documents in English; the list, chat and inspector all say "Cloud của tôi".
            if id == engine.selfConversationId { known.name = "Cloud của tôi" }
            return known
        }
        if let friend = engine.contacts.friends.first(where: { $0.id == id }) {
            return .init(id: id, isGroup: false, name: friend.name, avatar: friend.avatar, pinned: false, members: 0, unread: 0, preview: nil)
        }
        if let group = engine.contacts.groups.first(where: { $0.id == id }) {
            return .init(id: id, isGroup: true, name: group.name, avatar: group.avatar, pinned: false, members: group.members, unread: 0, preview: nil)
        }
        return nil
    }

    var body: some View {
        if conversationId != nil, !engine.conversationActions.hiddenKnown {
            ProgressView()
        } else if let conversation, engine.isConversationLocked(conversation.id) {
            ContentUnavailableView {
                Label("Trò chuyện ẩn", systemImage: "lock.fill")
            } description: {
                Text("Nhập mã PIN để mở trò chuyện")
            } actions: {
                Button("Nhập mã PIN") { engine.conversationPrompt = .unlock(id: conversation.id) }
                    .buttonStyle(.glassProminent)
            }
        } else if let conversation {
            ChatView(conversation: conversation)
                .id(conversation.id)
        } else {
            ContentUnavailableView("Chọn một cuộc trò chuyện", systemImage: "bubble.left.and.bubble.right",
                                   description: Text("Chọn ở danh sách bên cạnh, hoặc nhấn ⌘K để đi tới một cuộc trò chuyện."))
        }
    }
}
