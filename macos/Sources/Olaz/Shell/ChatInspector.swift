import SwiftUI

/// Conversation info shown in the chat's inspector (⌥⌘I): who it is, quick actions, the members of a group, the
/// polls found in the loaded history, and a preview of the photos / files / links ever shared (the whole list is one
/// "Xem tất cả" away, in `SharedMediaBrowser`). Everything comes from `ZaloEngine`; nothing is invented, and a section
/// with nothing to show says so.
struct ChatInspector: View {
    let conversation: ZaloEngine.ConversationSummary

    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell
    /// Group members from the official @mention suggestions; nil while loading.
    @State private var members: [MentionCandidate]?
    @State private var showLabelManager = false
    @State private var pollSheet: PollEntry?
    @State private var showPollCreate = false
    /// Width of the overview's scroll view itself (not of its content, see `SharedMediaGrid`).
    @State private var outerWidth: CGFloat = 300
    /// Photos / files / links ever shared, read from the official media service (the overview shows the newest of each).
    @State private var media: SharedMediaModel
    /// The list shown on the full "Kho lưu trữ" page; nil = the overview.
    @State private var browsing: SharedMediaKind?

    private var id: String { conversation.id }
    private let memberLimit = 8
    private let listLimit = 5

    init(conversation: ZaloEngine.ConversationSummary) {
        self.conversation = conversation
        _media = State(initialValue: SharedMediaModel(conversationId: conversation.id))
    }

    var body: some View {
        Group {
            if browsing != nil {
                SharedMediaBrowser(conversationId: id, model: media,
                                   kind: Binding(get: { browsing ?? .images }, set: { browsing = $0 }),
                                   close: { browsing = nil })
            } else {
                overview
            }
        }
        // On the container, not the overview: leaving for the storage page must not restart these.
        .task(id: id) { await loadMembers() }
        .task(id: engine.messages[id]?.last?.id) { await refreshPreview() }
        .sheet(item: $pollSheet) { poll in
            PollDetailSheet(pollId: poll.pollId, conversationId: id, question: poll.question)
        }
        .sheet(isPresented: $showPollCreate) { PollCreateSheet(conversationId: id) }
        .sheet(isPresented: $showLabelManager) { LabelManagerView() }
    }

    private var overview: some View {
        let loaded = engine.messages[id] ?? []
        return ScrollView {
            VStack(spacing: 20) {
                header
                actions
                if conversation.isGroup {
                    groupBoard(LoadedPolls(messages: loaded).polls)
                    membersSection
                }
                mediaSection(preview(.images, loaded))
                filesSection(preview(.files, loaded))
                linksSection(preview(.links, loaded))
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)
            .padding(.bottom, 20)
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { outerWidth = $0 }
    }

    // MARK: Header

    private var header: some View {
        VStack(spacing: 6) {
            RemoteAvatar(url: conversation.avatar, name: conversation.displayName, size: 76)
                .padding(.top, 14)
            HStack(spacing: 5) {
                Text(conversation.displayName)
                    .font(.system(size: 17, weight: .bold))
                    .lineLimit(2)
                    .multilineTextAlignment(.center)
                MutedBell(conversationId: id, size: 12)
            }
            if let subtitle {
                Text(subtitle).font(.system(size: 12)).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }

    private var subtitle: String? {
        if id == engine.selfConversationId { return "Chỉ mình bạn thấy" }
        if conversation.isGroup { return conversation.members > 0 ? "\(conversation.members) thành viên" : "Nhóm" }
        if engine.contacts.friends.contains(where: { $0.id == id }) { return "Bạn bè" }
        return engine.contacts.friendsLoaded ? "Chưa là bạn" : nil
    }

    // MARK: Actions

    private var actions: some View {
        let state = engine.conversationActions
        let muted = engine.isConversationMuted(id)
        let plan = ConversationMenuPlan.make(id: id, preset: .chatBox, state: state, unread: 0, muted: muted)
        return GlassEffectContainer(spacing: 14) {
            HStack(alignment: .top, spacing: 18) {
                if !state.loaded || plan.mute != nil {
                    if muted {
                        ActionSlot("Bật thông báo") {
                            Button { engine.unmuteConversation(id) } label: { actionGlyph("bell") }
                                .glassAction()
                        }
                    } else {
                        ActionSlot("Tắt thông báo") {
                            Menu {
                                ForEach(MuteDuration.allCases) { duration in
                                    Button(duration.title) { engine.muteConversation(id, duration) }
                                }
                            } label: { actionGlyph("bell.slash") }
                                .glassAction()
                        }
                    }
                }
                switch plan.pin {
                case .pin:
                    ActionSlot("Ghim") {
                        Button { engine.pinConversation(id) } label: { actionGlyph("pin") }.glassAction()
                    }
                case .unpin:
                    ActionSlot("Bỏ ghim") {
                        Button { engine.unpinConversation(id) } label: { actionGlyph("pin.slash") }.glassAction()
                    }
                case nil:
                    EmptyView()
                }
                // Official 1:1 info panel button STR_BTN_CREATE_CONVERSATION_GROUP ("Tạo nhóm trò chuyện", shortened for the slot),
                // with this friend already picked.
                if !conversation.isGroup, id != engine.selfConversationId, engine.contacts.friends.contains(where: { $0.id == id }) {
                    ActionSlot("Tạo nhóm") {
                        Button { shell.composeGroup(with: [id]) } label: { actionGlyph("person.2.badge.plus") }
                            .glassAction()
                            .help("Tạo nhóm với \(conversation.displayName)")
                    }
                }
                ActionSlot("Thêm") {
                    Menu {
                        ConversationActionsMenu(conversationId: id, preset: .thread, showLabelManager: $showLabelManager)
                    } label: { actionGlyph("ellipsis") }
                        .glassAction()
                }
            }
            .frame(maxWidth: .infinity)
        }
    }

    private func actionGlyph(_ symbol: String) -> some View {
        Image(systemName: symbol).font(.system(size: 16)).frame(width: 22, height: 22)
    }

    // MARK: Group

    private func groupBoard(_ polls: [LoadedPolls.Poll]) -> some View {
        section("Bảng tin nhóm") {
            list {
                row(symbol: "plus.circle", title: "Tạo bình chọn") { showPollCreate = true }
                ForEach(polls.prefix(listLimit)) { poll in
                    Divider().padding(.leading, 40)
                    row(symbol: "chart.bar.fill", title: poll.question.isEmpty ? "Bình chọn" : poll.question, lines: 2) {
                        pollSheet = PollEntry(pollId: poll.id, question: poll.question)
                    }
                }
                if polls.isEmpty {
                    Divider().padding(.leading, 40)
                    emptyLine("Chưa có bình chọn trong nội dung đã tải")
                }
            }
        }
    }

    @ViewBuilder
    private var membersSection: some View {
        // The official suggestion list is the only member source; without it the section is left out.
        if let members, !members.isEmpty {
            let shown = Array(members.prefix(memberLimit))
            section("Thành viên", trailing: conversation.members > 0 ? "\(conversation.members)" : nil) {
                list {
                    ForEach(Array(shown.enumerated()), id: \.element.id) { index, member in
                        if index > 0 { Divider().padding(.leading, 46) }
                        Button { engine.openContactConversation(member.id) } label: {
                            HStack(spacing: 10) {
                                RemoteAvatar(url: member.avatar, name: member.name, size: 26)
                                Text(member.name).font(.system(size: 13)).lineLimit(1)
                                Spacer(minLength: 0)
                            }
                            .padding(.horizontal, 12)
                            .frame(minHeight: 38)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .help("Nhắn tin với \(member.name)")
                    }
                    if conversation.members > shown.count {
                        Divider()
                        emptyLine("và \(conversation.members - shown.count) thành viên khác")
                    }
                }
            }
        } else if members == nil {
            section("Thành viên") {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, minHeight: 40)
            }
        }
    }

    private func loadMembers() async {
        members = nil
        guard conversation.isGroup else { members = []; return }
        let found = await engine.mentionCandidates(in: id, query: "", excluding: [])
        guard !Task.isCancelled else { return }
        members = found.filter { !$0.isAll }
    }

    // MARK: Shared content

    private func preview(_ kind: SharedMediaKind, _ loaded: [ChatMessage]) -> [ChatMessage] {
        media.preview(kind, loaded: loaded)
    }

    /// The first read starts at once; later ones (a new newest message) wait a second so a burst of messages reads the store once.
    private func refreshPreview() async {
        if media.hasLoaded {
            try? await Task.sleep(for: .seconds(1))
            guard !Task.isCancelled else { return }
        }
        await media.refresh(using: engine)
    }

    /// What stands in for an empty list: a spinner while the first read is out, a retry when it failed (not knowing is not "nothing shared"),
    /// else the "nothing here" line.
    @ViewBuilder
    private func emptyOrLoading(_ kind: SharedMediaKind, _ text: String) -> some View {
        let feed = media.feed(kind)
        if feed.isPending {
            ProgressView().controlSize(.small).frame(maxWidth: .infinity, minHeight: 38)
        } else if feed.didFail {
            failureLine(kind)
        } else {
            emptyLine(text)
        }
    }

    /// The first read of a list failed: say so, with a retry. (Shown under whatever the loaded messages already gave, too.)
    private func failureLine(_ kind: SharedMediaKind) -> some View {
        HStack(spacing: 8) {
            Text("Không tải được").font(.system(size: 12)).foregroundStyle(.secondary)
            Spacer(minLength: 0)
            Button("Thử lại") { Task { await media.retry(kind, using: engine) } }
                .buttonStyle(.plain)
                .font(.system(size: 12))
                .foregroundStyle(.tint)
        }
        .frame(maxWidth: .infinity, minHeight: 38)
        .padding(.horizontal, 12)
    }

    /// "Xem tất cả" is there when the list has something to show, and when it could not be read (the full page has the retry too).
    private func seeAll(_ kind: SharedMediaKind, hasItems: Bool) -> (() -> Void)? {
        hasItems || media.feed(kind).didFail ? { browsing = kind } : nil
    }

    private func mediaSection(_ items: [ChatMessage]) -> some View {
        section("Ảnh và video", seeAll: seeAll(.images, hasItems: !items.isEmpty)) {
            if items.isEmpty {
                list { emptyOrLoading(.images, "Chưa có ảnh hoặc video") }
            } else {
                let tile = max(48, ((SharedMediaGrid.contentWidth(outer: outerWidth, inset: 16) - 8) / 3).rounded(.down))
                VStack(spacing: 6) {
                    LazyVGrid(columns: Array(repeating: GridItem(.fixed(tile), spacing: 4), count: 3), spacing: 4) {
                        ForEach(items) { message in
                            SharedMediaTile(message: message, conversationId: id, side: tile)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if media.feed(.images).didFail { list { failureLine(.images) } }
                }
            }
        }
    }

    private func filesSection(_ files: [ChatMessage]) -> some View {
        section("Tệp", seeAll: seeAll(.files, hasItems: !files.isEmpty)) {
            list {
                if files.isEmpty {
                    emptyOrLoading(.files, "Chưa có tệp")
                } else {
                    ForEach(Array(files.enumerated()), id: \.element.id) { index, message in
                        if index > 0 { Divider().padding(.leading, 12) }
                        SharedFileRow(message: message)
                    }
                    if media.feed(.files).didFail {
                        Divider()
                        failureLine(.files)
                    }
                }
            }
        }
    }

    private func linksSection(_ links: [ChatMessage]) -> some View {
        section("Liên kết", seeAll: seeAll(.links, hasItems: !links.isEmpty)) {
            list {
                if links.isEmpty {
                    emptyOrLoading(.links, "Chưa có liên kết")
                } else {
                    ForEach(Array(links.enumerated()), id: \.element.id) { index, message in
                        if index > 0 { Divider().padding(.leading, 40) }
                        SharedLinkRow(message: message)
                    }
                    if media.feed(.links).didFail {
                        Divider()
                        failureLine(.links)
                    }
                }
            }
        }
    }

    // MARK: Building blocks

    /// `trailing`: a quiet value (the member count); `seeAll`: the "Xem tất cả" button of a list that continues on the storage page.
    private func section<Content: View>(_ title: String, trailing: String? = nil, seeAll: (() -> Void)? = nil, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(title).font(.system(size: 12, weight: .semibold)).foregroundStyle(.secondary)
                    .accessibilityAddTraits(.isHeader)
                Spacer()
                if let trailing { Text(trailing).font(.system(size: 12)).foregroundStyle(.tertiary) }
                if let seeAll {
                    Button("Xem tất cả", action: seeAll)
                        .buttonStyle(.plain)
                        .font(.system(size: 12))
                        .foregroundStyle(.tint)
                }
            }
            .padding(.horizontal, 4)
            content()
        }
    }

    private func list<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        InspectorList(content: content)
    }

    private func row(symbol: String, title: String, subtitle: String = "", lines: Int = 1, action: @escaping () -> Void) -> some View {
        InspectorRow(symbol: symbol, title: title, subtitle: subtitle, lines: lines, action: action)
    }

    private func emptyLine(_ text: String) -> some View {
        InspectorEmptyLine(text)
    }
}

// MARK: - Pieces

/// A round glass action with its caption underneath.
private struct ActionSlot<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    init(_ title: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.content = content()
    }

    var body: some View {
        VStack(spacing: 5) {
            content
            Text(title).font(.system(size: 10.5, weight: .medium)).foregroundStyle(.secondary).lineLimit(1)
        }
        .frame(minWidth: 58)
    }
}

private extension View {
    /// Round glass button (also for a `Menu`), the style of the composer's controls.
    func glassAction() -> some View {
        menuStyle(.button)
            .menuIndicator(.hidden)
            .buttonStyle(.glass)
            .buttonBorderShape(.circle)
            .controlSize(.large)
    }
}

private struct PollEntry: Identifiable {
    let pollId: String
    let question: String
    var id: String { pollId }
}

/// Polls found in the loaded part of the history, newest first (the poll board has no store of its own to page through).
private struct LoadedPolls {
    struct Poll: Identifiable {
        let id: String
        let question: String
    }

    var polls: [Poll] = []

    init(messages: [ChatMessage]) {
        var seenPolls: Set<String> = []
        for message in messages.reversed() {
            if case .poll(let info) = message.kind, !info.pollId.isEmpty, seenPolls.insert(info.pollId).inserted {
                polls.append(Poll(id: info.pollId, question: info.question))
            }
        }
    }
}
