import SwiftUI

/// What the sidebar picked; decides the content column (list) and the detail column.
enum SidebarDestination: Hashable {
    /// The official default list tab ("Ưu tiên" with the Khác feature on, "Tất cả" without it); Olaz calls it
    /// "Tin nhắn" (`ConversationExtras.inboxTitle`).
    case inbox
    /// Official "Khác" tab (only offered while `ConversationExtras.archivedEnabled`).
    case other
    /// Official "Chưa đọc" tab.
    case unread
    /// The stranger folder (`ConversationExtras.strangerFolderIds`).
    case strangers
    /// Hidden chats: listed only after the PIN was entered.
    case hidden
    /// A "Phân loại" label (official label filter on the default tab). Picked from the label button of the list column;
    /// the sidebar has no row for it and keeps highlighting "Tin nhắn".
    case label(String)
    /// "Cloud của tôi": the self conversation, opened in the inbox list.
    case myCloud
    case zcloud
    case friends, groups, requests, addFriend

    /// Destinations whose content column is a conversation list and whose detail is a chat.
    var isConversationList: Bool {
        switch self {
        case .inbox, .other, .unread, .strangers, .hidden, .label, .myCloud: true
        case .zcloud, .friends, .groups, .requests, .addFriend: false
        }
    }

    /// The sidebar row this destination belongs to: a label filter is a view of the inbox.
    var sidebarRow: SidebarDestination {
        if case .label = self { .inbox } else { self }
    }

    /// For the engine log: never a label id or conversation id in full.
    var logName: String {
        switch self {
        case .label: "label"
        default: "\(self)"
        }
    }

    var isContacts: Bool {
        switch self {
        case .friends, .groups, .requests, .addFriend: true
        default: false
        }
    }

    /// The official ConvListController filter this destination shows, nil when it is not a controller list
    /// (stranger folder and hidden chats are computed in Swift; contacts / zCloud have no list filter).
    func officialFilter(defaultTab: ConversationTypeFilter) -> (tab: ConversationTypeFilter, labelIds: [String])? {
        switch self {
        case .inbox, .myCloud: (defaultTab, [])
        case .other: (.other, [])
        case .unread: (.unread, [])
        case .label(let id): (defaultTab, [id])
        case .strangers, .hidden, .zcloud, .friends, .groups, .requests, .addFriend: nil
        }
    }
}

/// The coach-mark tour shown after setup (and from Trợ giúp ▸ Hướng dẫn sử dụng).
enum TourStep: Int, CaseIterable, Identifiable {
    case mailboxes, search, message, composer, toolbar

    var id: Int { rawValue }
    var next: TourStep? { TourStep(rawValue: rawValue + 1) }
    var previous: TourStep? { TourStep(rawValue: rawValue - 1) }
}

/// Window-level UI state shared by the shell views and the menu-bar commands (one main window).
/// Conversation data stays in `ZaloEngine`; this only holds what the user is looking at.
@MainActor
@Observable
final class ShellModel {
    var destination: SidebarDestination = .inbox
    /// The conversation shown in the detail column (conversation-list destinations).
    var selection: String?
    var columns: NavigationSplitViewVisibility = .all
    /// Conversation info inspector (⌥⌘I).
    var showInspector = false
    /// The window holds the inspector's room: its minimum width includes the inspector, from the end of the widening until the
    /// inspector is hidden. Raising it while the window is still widening would snap the window wide at once.
    var holdsInspectorRoom = false
    /// Trailing room the detail column keeps free during the inspector's reveal: the width the column gained over `revealChatWidth`,
    /// so the chat keeps its width while the window widens and the inspector then slides into that room (see `detailWidthChanged`).
    var inspectorRoomReserve: CGFloat = 0
    /// Width of the detail column at the moment a reveal that widens the window started; nil once the inspector has slid in.
    @ObservationIgnored private var revealChatWidth: CGFloat?
    /// Current width of the detail column (reported by the view).
    @ObservationIgnored private var detailWidth: CGFloat = 0
    /// Counts reveals, so the safety timeout of an earlier one cannot cut a later one short.
    @ObservationIgnored private var revealGeneration = 0
    /// The signed-in window (set by `MainShellView`), widened for the inspector.
    @ObservationIgnored weak var window: NSWindow?
    /// ⌘K palette.
    var showQuickSwitcher = false
    /// Search text of the conversation list column.
    var search = ""
    /// Hidden chats listed by the "Trò chuyện ẩn" mailbox after the PIN was entered; nil while locked.
    var unlockedHiddenIds: [String]?
    /// Selected row of the contacts lists (friend / group / request id).
    var contactSelection: String?
    /// Selected zCloud screen.
    var cloudRoute: CloudRoute? = .media
    /// Running coach-mark step, nil when no tour is shown.
    var tour: TourStep?
    /// The "Tạo nhóm" sheet, nil when it is not shown.
    var groupCompose: GroupComposeRequest?

    // Ticks: a view observing one of these performs the action when it changes.
    /// Focus the list search field (⌘F in the list, ⌥⌘F).
    private(set) var searchFocusTick = 0
    /// Reply to the last incoming message of the open chat (⌘R).
    private(set) var replyTick = 0

    func focusSearch() { searchFocusTick += 1 }

    /// Shows the "Tạo nhóm" sheet with `members` already picked.
    func composeGroup(with members: [String] = []) { groupCompose = GroupComposeRequest(members: members) }
    func replyToLatest() { replyTick += 1 }

    /// Shows a conversation: keeps the current list when it is a conversation list, otherwise goes to the inbox.
    func open(conversation id: String) {
        if !destination.isConversationList || destination == .hidden { destination = .inbox }
        search = ""
        selection = id
    }

    /// Back to a fresh window: the model outlives the sign-in, so a logout (or the next account's QR sign-in) must not
    /// inherit the previous account's open chat, search, unlocked hidden mailbox or running tour.
    func reset() {
        selection = nil
        destination = .inbox
        search = ""
        unlockedHiddenIds = nil
        contactSelection = nil
        cloudRoute = .media
        tour = nil
        showQuickSwitcher = false
        showInspector = false
        holdsInspectorRoom = false
        endReveal()
        groupCompose = nil
    }

    func toggleSidebar() {
        withAnimation(.smooth(duration: 0.3)) { columns = columns == .all ? .doubleColumn : .all }
    }

    func toggleInspector() {
        guard destination.isConversationList, selection != nil, !showInspector else { hideInspector(); return }
        // The window widens first, then the inspector slides in; throughout, `detailWidthChanged` keeps the chat at the width it had,
        // handing the gained room to a trailing spacer and then to the inspector. The two AppKit animations (window frame, split
        // view) start at their own pace, so nothing here is timed: the room is derived from the column's actual width each frame.
        // A window that is wide enough already goes straight to the slide, with nothing to reserve.
        let duration = InspectorRoom.revealDuration
        revealChatWidth = detailWidth
        InspectorRoom.make(in: window) { [weak self] in
            guard let self else { return }
            holdsInspectorRoom = true
            withAnimation(.easeInOut(duration: duration)) { showInspector = true }
        }
        // Should the slide never report back (no chat to show), the reserve must not stick.
        revealGeneration += 1
        let generation = revealGeneration
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(2))
            if let self, revealGeneration == generation { endReveal() }
        }
    }

    func hideInspector() {
        holdsInspectorRoom = false
        endReveal()
        withAnimation(.easeInOut(duration: InspectorRoom.revealDuration)) { showInspector = false }
    }

    /// The detail column's width, reported by `DetailColumn` whenever it changes. During a reveal the room the column gained
    /// beyond the chat's width is reserved; once the inspector has taken it all back, the reveal is over.
    func detailWidthChanged(_ width: CGFloat) {
        detailWidth = width
        guard let chatWidth = revealChatWidth else { return }
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) { inspectorRoomReserve = max(0, width - chatWidth) }
        if showInspector, width <= chatWidth { endReveal() }
    }

    private func endReveal() {
        revealChatWidth = nil
        if inspectorRoomReserve != 0 { inspectorRoomReserve = 0 }
    }

    // MARK: Tour

    func startTour() {
        destination = .inbox
        search = ""
        showQuickSwitcher = false
        columns = .all
        tour = .mailboxes
    }

    func advanceTour() { tour = tour?.next }
    func retreatTour() { if let previous = tour?.previous { tour = previous } }
    func endTour() { tour = nil }
}

/// The inspector takes its width from the window, never from the chat: a three-column window at its 960 pt minimum would otherwise
/// leave the chat a sliver (sidebar + list + inspector ≈ 876 pt), where its layout never settles and AppKit crashes on the endless
/// layout passes. So the window is widened before the inspector opens, and cannot be made narrower than this while it shows.
@MainActor
enum InspectorRoom {
    static let windowMinWidth: CGFloat = 960
    /// Room the inspector needs on top of `windowMinWidth` (its ideal width).
    static let inspectorWidth: CGFloat = 300
    static var windowMinWidthWithInspector: CGFloat { windowMinWidth + inspectorWidth }

    /// The window frame that gives the inspector its room: wider by what is missing, growing to the right and moving left only as far as
    /// the screen needs. A frame already wide enough is returned unchanged.
    static func frame(for window: CGRect, in screen: CGRect) -> CGRect {
        let width = min(max(window.width, windowMinWidthWithInspector), screen.width)
        guard width > window.width else { return window }
        var x = window.minX
        if x + width > screen.maxX { x = screen.maxX - width }
        return CGRect(x: max(screen.minX, x), y: window.minY, width: width, height: window.height)
    }

    /// How long the split view takes to slide the inspector in or out. It ignores the SwiftUI transaction (measured on macOS 27: about
    /// a quarter second whatever `withAnimation` asks for); the window widening uses the same duration and curve.
    static let revealDuration: TimeInterval = 0.25

    /// Widens the shell window for the inspector; `completion` runs when the widening animation has ended, or at once when there is
    /// nothing to widen (a window already wide enough, or a full-screen one, which is as wide as it gets).
    static func make(in window: NSWindow?, completion: @escaping @MainActor () -> Void) {
        guard let window, !window.styleMask.contains(.fullScreen), let screen = window.screen ?? NSScreen.main else { completion(); return }
        let target = frame(for: window.frame, in: screen.visibleFrame)
        guard target != window.frame else { completion(); return }
        // `setFrame(_:display:animate:)` is of no use here: on macOS 27 it moves a 300 pt change in one step.
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = revealDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            window.animator().setFrame(target, display: true)
        }, completionHandler: { MainActor.assumeIsolated { completion() } })
    }
}
