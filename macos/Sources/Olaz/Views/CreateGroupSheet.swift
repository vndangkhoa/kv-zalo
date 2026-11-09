import SwiftUI
import UniformTypeIdentifiers

/// "Tạo nhóm": the official group compose (CREATE_GROUP_COMPOSE) as a native sheet — group photo and name, friends to pick ("Trò chuyện
/// gần đây" then A–Z), the picks as removable chips. Rules and limits are the official ones (`GroupCreateLimits`,
/// `GroupComposeSelection`); "Tạo nhóm" runs the official create (ZaloEngine+GroupCreate.swift), then the new group opens.
struct CreateGroupSheet: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    let request: GroupComposeRequest

    @State private var limits = GroupCreateLimits()
    @State private var name = ""
    @State private var query = ""
    @State private var selection: GroupComposeSelection
    @State private var avatar: GroupAvatarImage?
    @State private var pickingAvatar = false
    @State private var creating = false
    @State private var notice: Notice?
    @State private var problem: Problem?
    @State private var confirmCancel = false
    @FocusState private var focus: Field?

    private enum Field { case name, search }

    /// A short note under the list (official toasts of the picker).
    private struct Notice: Equatable {
        let id = UUID()
        let text: String
    }

    /// Why the group was not created. `closes`: the official compose closes after this one (quota check).
    private struct Problem: Identifiable {
        let id = UUID()
        let title: String
        let message: String
        let closes: Bool
    }

    init(request: GroupComposeRequest) {
        self.request = request
        _selection = State(initialValue: GroupComposeSelection(request.members))
    }

    private var roster: GroupComposeRoster {
        GroupComposeRoster(friends: engine.visibleFriends, conversations: engine.visibleConversations, selfId: engine.selfConversationId,
                           officialAccounts: limits.officialAccounts)
    }
    private var nameIsValid: Bool { limits.isValidName(name) }
    private var canCreate: Bool { selection.canConfirm && nameIsValid && !creating }

    var body: some View {
        let roster = roster
        VStack(spacing: 0) {
            Text("Tạo nhóm")
                .font(.system(size: 16, weight: .semibold))
                .padding(.top, 18)
                .padding(.bottom, 14)
            identity
            searchField
            if !selection.ids.isEmpty { chips(roster) }
            Divider()
            list(roster)
            Divider()
            footer
        }
        .frame(width: 440, height: 640)
        .disabled(creating)
        .onAppear {
            focus = .name
            dropUnavailablePicks()
        }
        .task {
            guard let loaded = await engine.groupCreateLimitsWhenReady() else { return }
            limits = loaded
            selection.keep { !loaded.blocked.contains($0) }
        }
        .onExitCommand { cancel() }
        .fileImporter(isPresented: $pickingAvatar, allowedContentTypes: [.image]) { result in
            if case .success(let url) = result { loadAvatar(url) }
        }
        .alert("Bạn muốn hủy tạo nhóm này?", isPresented: $confirmCancel) {
            Button("Có", role: .destructive) { dismiss() }
            Button("Không", role: .cancel) {}
        }
        .alert(problem?.title ?? "", isPresented: Binding(get: { problem != nil }, set: { if !$0 { problem = nil } }), presenting: problem) { shown in
            Button(shown.closes ? "Đã hiểu" : "OK") { if shown.closes { dismiss() } }
        } message: { shown in
            Text(shown.message)
        }
    }

    // MARK: Photo and name

    private var identity: some View {
        HStack(alignment: .top, spacing: 12) {
            avatarButton
            VStack(alignment: .leading, spacing: 4) {
                TextField("Nhập tên nhóm...", text: $name)
                    .textFieldStyle(.plain)
                    .font(.system(size: 14))
                    .focused($focus, equals: .name)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .glassEffect(.regular, in: Capsule())
                    .overlay {
                        if !nameIsValid { Capsule().strokeBorder(Color.red.opacity(0.7), lineWidth: 1) }
                    }
                    .onChange(of: name) { _, typed in
                        let clamped = limits.clampedName(typed)
                        if clamped != typed { name = clamped }
                    }
                    .accessibilityLabel("Tên nhóm")
                Text(nameIsValid ? "Để trống để dùng tên các thành viên" : GroupCreateLimits.invalidNameText)
                    .font(.system(size: 11.5))
                    .foregroundStyle(nameIsValid ? AnyShapeStyle(.tertiary) : AnyShapeStyle(Color.red))
                    .padding(.leading, 12)
            }
            .padding(.top, 6)
        }
        .padding(.horizontal, 20)
        .padding(.bottom, 12)
    }

    @ViewBuilder private var avatarButton: some View {
        let face = Group {
            if let image = avatar?.image {
                Image(nsImage: image).resizable().scaledToFill()
            } else {
                Image(systemName: "camera.fill")
                    .font(.system(size: 18))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .glassEffect(.regular.interactive(), in: Circle())
            }
        }
        .frame(width: 52, height: 52)
        .clipShape(Circle())
        .contentShape(Circle())

        Group {
            if avatar != nil {
                Menu {
                    Button("Đổi ảnh đại diện") { pickingAvatar = true }
                    Button("Xoá ảnh đại diện", role: .destructive) { avatar = nil }
                } label: { face }
                    .menuStyle(.button)
                    .menuIndicator(.hidden)
                    .buttonStyle(.plain)
            } else {
                Button { pickingAvatar = true } label: { face }
                    .buttonStyle(.plain)
            }
        }
        .fixedSize()
        .help(avatar == nil ? "Chọn ảnh đại diện cho nhóm (hoặc kéo ảnh vào đây)" : "Ảnh đại diện của nhóm")
        .accessibilityLabel("Ảnh đại diện nhóm")
        .dropDestination(for: URL.self) { urls, _ in
            guard let url = urls.first else { return false }
            loadAvatar(url)
            return true
        }
    }

    private func loadAvatar(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        switch GroupAvatarImage.make(from: url) {
        case .success(let image): avatar = image
        case .failure(let error): problem = Problem(title: "Không thể dùng ảnh này", message: error.text, closes: false)
        }
    }

    // MARK: Search and picks

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Nhập tên bạn bè", text: $query)
                .textFieldStyle(.plain)
                .focused($focus, equals: .search)
            if !query.isEmpty {
                Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel("Xoá tìm kiếm")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .glassEffect(.regular, in: Capsule())
        .padding(.horizontal, 16)
        .padding(.bottom, 10)
    }

    private func chips(_ roster: GroupComposeRoster) -> some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(selection.ids, id: \.self) { id in
                        let person = roster.person(id) ?? GroupComposeRoster.person(id, in: engine.contacts.friends)
                        HStack(spacing: 5) {
                            RemoteAvatar(url: person?.avatar, name: person?.name ?? "", size: 20)
                            Text(person?.name ?? "Người dùng Zalo")
                                .font(.system(size: 12))
                                .lineLimit(1)
                            Button { toggle(id) } label: {
                                Image(systemName: "xmark.circle.fill").font(.system(size: 12))
                            }
                            .buttonStyle(.plain)
                            .foregroundStyle(.secondary)
                            .accessibilityLabel("Bỏ chọn \(person?.name ?? "")")
                        }
                        .padding(.leading, 3)
                        .padding(.trailing, 7)
                        .padding(.vertical, 3)
                        .background(.tint.opacity(0.14), in: Capsule())
                        .id(id)
                    }
                }
                .padding(.horizontal, 16)
            }
            .padding(.bottom, 10)
            .onChange(of: selection.ids.last) { _, last in
                if let last { withAnimation(.smooth(duration: 0.2)) { proxy.scrollTo(last, anchor: .trailing) } }
            }
        }
    }

    // MARK: List

    @ViewBuilder private func list(_ roster: GroupComposeRoster) -> some View {
        let searching = !query.trimmingCharacters(in: .whitespaces).isEmpty
        if !engine.contacts.friendsLoaded || !engine.conversationActions.hiddenKnown {
            ProgressView("Đang tải danh sách bạn bè…")
                .controlSize(.small)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if roster.friends.isEmpty {
            ContentUnavailableView("Chưa có bạn bè", systemImage: "person.2",
                                   description: Text("Nhóm được tạo từ danh sách bạn bè của bạn."))
                .frame(maxHeight: .infinity)
        } else if searching {
            let results = roster.matching(query)
            if results.isEmpty {
                ContentUnavailableView.search(text: query).frame(maxHeight: .infinity)
            } else {
                scroll { ForEach(results) { row($0) } }
            }
        } else {
            scroll {
                if !roster.recent.isEmpty {
                    header("Trò chuyện gần đây")
                    ForEach(roster.recent) { row($0) }
                }
                ForEach(roster.sections, id: \.title) { section in
                    header(section.title)
                    ForEach(section.people) { row($0) }
                }
            }
        }
    }

    private func scroll<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 2) { content() }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
        }
    }

    private func header(_ title: String) -> some View {
        Text(title)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.top, 10)
            .padding(.bottom, 2)
            .accessibilityAddTraits(.isHeader)
    }

    private func row(_ person: GroupComposeRoster.Person) -> some View {
        let blocked = limits.blocked.contains(person.id)
        let checked = selection.contains(person.id)
        return Button { toggle(person.id) } label: {
            HStack(spacing: 10) {
                Image(systemName: blocked ? "nosign" : checked ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 18))
                    .foregroundStyle(checked ? AnyShapeStyle(.tint) : AnyShapeStyle(Color.secondary.opacity(0.6)))
                RemoteAvatar(url: person.avatar, name: person.name, size: 34)
                VStack(alignment: .leading, spacing: 0) {
                    Text(person.name)
                        .font(.system(size: 14))
                        .lineLimit(1)
                    if blocked {
                        Text("Đã chặn").font(.system(size: 11.5)).foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .opacity(blocked ? 0.55 : 1)
        .accessibilityLabel(person.name)
        .accessibilityValue(blocked ? "Đã chặn" : "")
        .accessibilityAddTraits(checked ? .isSelected : [])
    }

    private func toggle(_ id: String) {
        switch selection.toggle(id, limits: limits) {
        case .added: if !query.isEmpty { query = ""; focus = .search }
        case .removed: break
        case .overLimit: flash(limits.overLimitText)
        case .blocked: flash(GroupCreateLimits.blockedText)
        }
    }

    // MARK: Footer

    private var footer: some View {
        VStack(spacing: 8) {
            if let notice {
                Text(notice.text)
                    .font(.system(size: 12))
                    .foregroundStyle(.orange)
                    .transition(.opacity)
            }
            HStack(spacing: 8) {
                Text(selection.ids.count < 2 ? "Chọn ít nhất 2 người" : "Đã chọn \(selection.ids.count)/\(limits.maxMembers)")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                Spacer()
                Button("Hủy") { cancel() }
                    .buttonStyle(.glass)
                    .keyboardShortcut(.cancelAction)
                Button { create() } label: {
                    if creating {
                        ProgressView().controlSize(.small).frame(width: 58)
                    } else {
                        Text("Tạo nhóm")
                    }
                }
                .buttonStyle(.glassProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canCreate)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .animation(.smooth(duration: 0.2), value: notice)
    }

    private func flash(_ text: String) {
        let shown = Notice(text: text)
        notice = shown
        Task {
            try? await Task.sleep(for: .seconds(2.5))
            if notice == shown { notice = nil }
        }
    }

    // MARK: Actions

    /// Official `onCancel`: with a group ready to create (2 picks) it asks first.
    private func cancel() {
        guard !creating else { return }
        if selection.canConfirm { confirmCancel = true } else { dismiss() }
    }

    private func create() {
        guard canCreate else { return }
        creating = true
        Task {
            let outcome = await engine.createGroup(name: name.trimmingCharacters(in: .whitespacesAndNewlines), members: selection.ids, avatar: avatar)
            creating = false
            switch outcome {
            case .created, .exists, .pending:
                dismiss()
            case .invalid(let text, let dropped):
                selection.keep { !dropped.contains($0) }
                problem = Problem(title: "Không thể tạo nhóm", message: text, closes: false)
            case .failed(let title, let message, let closes):
                problem = Problem(title: title ?? "Không thể tạo nhóm", message: message, closes: closes)
            }
        }
    }

    /// Picks handed in by the caller that are not friends are dropped (only friends can be added). A friend whose chat is hidden stays
    /// picked — the official compose adds the conversation's user whatever its state — even though the list does not show them.
    private func dropUnavailablePicks() {
        guard engine.contacts.friendsLoaded else { return }
        let friends = Set(engine.contacts.friends.map(\.id))
        selection.keep { friends.contains($0) }
    }
}
