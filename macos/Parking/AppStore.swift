import SwiftUI
import Observation

enum NavTab: String, CaseIterable, Identifiable {
    case messages, contacts, calls, todo, cloud, settings
    var id: String { rawValue }

    var title: String {
        switch self {
        case .messages: "Tin nhắn"
        case .contacts: "Danh bạ"
        case .calls: "Cuộc gọi"
        case .todo: "Công việc"
        case .cloud: "ZCloud"
        case .settings: "Cài đặt"
        }
    }

    var icon: String {
        switch self {
        case .messages: "bubble.left.and.bubble.right.fill"
        case .contacts: "person.2.fill"
        case .calls: "phone.fill"
        case .todo: "checklist.fill"
        case .cloud: "externaldrive.fill.badge.icloud"
        case .settings: "gearshape.fill"
        }
    }
}

/// Single observable store for the whole app (mirrors the renderer's Redux slices).
@MainActor
@Observable
final class AppStore {
    let service: any ZaloService
    private(set) var serviceState: ServiceState = .loggedOut
    var tab: NavTab = .messages
    var selectedConversationId: UUID?
    var searchText: String = ""
    var prefersDark: Bool = false
    var toast: String?

    // Populated after login
    private(set) var conversations: [Conversation] = []
    private(set) var messages: [UUID: [Message]] = [:]
    private(set) var users: [UUID: User] = [:]
    private(set) var labels: [ConversationLabel] = []
    private(set) var typingIn: [UUID: String] = [:]

    var loggedInUser: User? {
        if case .loggedIn(let user) = serviceState { user } else { nil }
    }

    var isLoggedIn: Bool {
        loggedInUser != nil
    }

    var isLoggedOut: Bool {
        if case .loggedOut = serviceState { true } else { false }
    }

    init(service: any ZaloService = DemoZaloService()) {
        self.service = service
        serviceState = service.state
        observeEvents()
    }

    private func observeEvents() {
        Task { [weak self] in
            guard let self else { return }
            for await event in self.service.events {
                switch event {
                case .stateChanged:
                    // Backup path: wrappers above mirror synchronously; this
                    // covers service-initiated transitions.
                    self.serviceState = self.service.state
                    await self.reload()
                case .messageReceived(let m):
                    guard self.isLoggedIn else { continue }
                    self.messages[m.conversationId, default: []].append(m)
                    if m.conversationId == self.selectedConversationId, !m.isMine {
                        // Already viewing it — keep unread in list only if not selected.
                        self.markRead(m.conversationId)
                    }
                    if let idx = self.conversations.firstIndex(where: { $0.id == m.conversationId }) {
                        self.conversations[idx].lastMessageAt = m.sentAt
                    }
                case .typing(let cid, let name):
                    guard self.isLoggedIn else { continue }
                    self.typingIn[cid] = name
                    Task { [weak self] in
                        try? await Task.sleep(for: .seconds(2.2))
                        self?.typingIn[cid] = nil
                    }
                }
            }
        }
    }

    /// Pinned first, then most recent.
    static func conversationOrder(_ a: Conversation, _ b: Conversation) -> Bool {
        if a.isPinned != b.isPinned { return a.isPinned }
        return a.lastMessageAt > b.lastMessageAt
    }

    func reload() async {
        guard isLoggedIn else {
            clearSession()
            return
        }
        conversations = service.loadConversations().sorted(by: Self.conversationOrder)
        if let demo = service as? DemoZaloService {
            users = Dictionary(uniqueKeysWithValues: DemoData.contacts.map { ($0.id, $0) })
            users[User.me.id] = .me
            labels = demo.labels
        }
        for conv in conversations {
            messages[conv.id] = service.loadMessages(conversationId: conv.id)
        }
        if selectedConversationId == nil {
            selectedConversationId = conversations.first?.id
            if let id = selectedConversationId { markRead(id) }
        }
    }

    var filteredConversations: [Conversation] {
        let q = searchText.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return conversations }
        return conversations.filter { $0.title.localizedCaseInsensitiveContains(q) }
    }

    var selectedConversation: Conversation? {
        conversations.first { $0.id == selectedConversationId }
    }

    func messages(in conversationId: UUID) -> [Message] {
        messages[conversationId] ?? []
    }

    func user(_ id: UUID) -> User? { users[id] }

    func markRead(_ conversationId: UUID) {
        if let idx = conversations.firstIndex(where: { $0.id == conversationId }) {
            conversations[idx].unreadCount = 0
        }
    }

    func select(_ conversationId: UUID) {
        selectedConversationId = conversationId
        markRead(conversationId)
    }

    // MARK: Login (store-wrapped so state mirrors synchronously into observable storage)

    func beginQrLogin() async {
        await service.beginQrLogin()
        serviceState = service.state
    }

    func simulateScan(deviceName: String) {
        service.simulateScan(deviceName: deviceName)
        serviceState = service.state
    }

    func simulateConfirm() async {
        service.simulateConfirm()
        serviceState = service.state
        await reload()
    }

    func logOut() {
        service.logOut()
        serviceState = service.state
        clearSession()
    }

    private func clearSession() {
        conversations = []
        messages = [:]
        users = [:]
        labels = []
        typingIn = [:]
        selectedConversationId = nil
        searchText = ""
        toast = nil
        tab = .messages
    }

    func send(_ kind: MessageKind, to conversationId: UUID) {
        guard let message = service.sendMessage(kind, to: conversationId) else { return }
        messages[conversationId, default: []].append(message)
        if let idx = conversations.firstIndex(where: { $0.id == conversationId }) {
            conversations[idx].lastMessageAt = message.sentAt
        }
    }

    func togglePin(_ conversationId: UUID) {
        guard let idx = conversations.firstIndex(where: { $0.id == conversationId }) else { return }
        let pinned = !conversations[idx].isPinned
        conversations[idx].isPinned = pinned
        conversations.sort(by: Self.conversationOrder)
        showToast(pinned ? "Đã ghim hội thoại" : "Đã bỏ ghim")
    }

    func showToast(_ text: String) {
        withAnimation(.smooth) { toast = text }
        Task {
            try? await Task.sleep(for: .seconds(2.2))
            withAnimation(.smooth) { self.toast = nil }
        }
    }
}
