import SwiftUI

// MARK: - Contacts ("Danh bạ")

struct ContactsView: View {
    @Environment(AppStore.self) private var store
    @State private var search = ""

    private var grouped: [(String, [User])] {
        let contacts = DemoData.contacts.filter {
            search.isEmpty || $0.displayName.localizedCaseInsensitiveContains(search)
        }
        let dict = Dictionary(grouping: contacts) { String($0.displayName.prefix(1)).uppercased() }
        return dict.sorted { $0.key < $1.key }
    }

    var body: some View {
        VStack(spacing: 0) {
            PaneHeader("Danh bạ", trailing: AnyView(addFriendButton))
            searchBar
            List {
                ForEach(grouped, id: \.0) { letter, people in
                    Section(letter) {
                        ForEach(people) { person in
                            HStack(spacing: 12) {
                                AvatarView(name: person.displayName, size: 40, isOnline: person.isOnline)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(person.displayName).font(.system(size: 13.5, weight: .medium))
                                    Text(person.phone ?? "")
                                        .font(.system(size: 11.5))
                                        .foregroundStyle(.tertiary)
                                }
                                Spacer()
                                Button { store.showToast("Nhắn tin cho \(person.displayName)") } label: {
                                    Image(systemName: "bubble.left.and.bubble.right.fill")
                                        .font(.system(size: 13))
                                        .foregroundStyle(ZaloTheme.blue)
                                }
                                .buttonStyle(.glass)
                            }
                            .padding(.vertical, 3)
                        }
                    }
                }
            }
            .listStyle(.inset)
            .scrollContentBackground(.hidden)
        }
    }

    private var addFriendButton: some View {
        Button {
            store.showToast("Thêm bạn bè — bản demo")
        } label: {
            Image(systemName: "person.badge.plus")
        }
        .buttonStyle(.glass)
        .help("Thêm bạn bè")
    }

    private var searchBar: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Tìm kiếm danh bạ", text: $search).textFieldStyle(.plain)
        }
        .padding(8).padding(.horizontal, 4)
        .glassEffect(in: Capsule())
        .padding(.horizontal, 16).padding(.bottom, 8)
    }
}

// MARK: - Calls ("Cuộc gọi")

struct CallsView: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        VStack(spacing: 0) {
            PaneHeader("Cuộc gọi", trailing: AnyView(callButtons))
            List(DemoData.calls) { call in
                HStack(spacing: 12) {
                    AvatarView(name: call.peer, size: 40)
                    HStack(spacing: 6) {
                        Image(systemName: call.incoming ? "arrow.down.left" : "arrow.up.right")
                            .font(.system(size: 10, weight: .bold))
                            .foregroundStyle(call.missed ? .red : .green)
                    }
                    .frame(width: 14)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(call.peer).font(.system(size: 13.5, weight: call.missed ? .semibold : .regular))
                        Text(callSubtitle(call))
                            .font(.system(size: 11.5))
                            .foregroundStyle(call.missed ? .red : .secondary)
                    }
                    Spacer()
                    Text(call.at, format: .relative(presentation: .named))
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                    Button {
                        store.showToast("Gọi lại — cần `zcall` (xem README)")
                    } label: {
                        Image(systemName: call.isVideo ? "video.fill" : "phone.fill")
                            .font(.system(size: 13))
                            .foregroundStyle(ZaloTheme.blue)
                    }
                    .buttonStyle(.glass)
                }
                .padding(.vertical, 3)
            }
            .listStyle(.inset)
            .scrollContentBackground(.hidden)
        }
    }

    private func callSubtitle(_ call: CallRecord) -> String {
        if call.missed { return call.incoming ? "Cuộc gọi nhỡ" : "Đã huỷ" }
        return "\(call.durationMinutes) phút • \(call.incoming ? "Cuộc gọi đến" : "Cuộc gọi đi")"
    }

    private var callButtons: some View {
        Button { store.showToast("Cuộc gọi mới — cần `zcall` (xem README)") } label: {
            Image(systemName: "plus")
        }
        .buttonStyle(.glass)
        .help("Cuộc gọi mới")
    }
}

// MARK: - Todo ("Công việc")

struct TodoView: View {
    @Environment(AppStore.self) private var store
    @State private var tasks = DemoData.todos

    var body: some View {
        VStack(spacing: 0) {
            PaneHeader("Công việc", trailing: AnyView(addTask))
            List {
                Section("Được giao cho bạn") {
                    ForEach($tasks) { $task in
                        Toggle(isOn: $task.isDone) {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(task.title)
                                        .font(.system(size: 13.5, weight: task.isDone ? .regular : .medium))
                                        .strikethrough(task.isDone)
                                    Text("\(task.assignee) • Hạn: \(task.dueLabel)")
                                        .font(.system(size: 11))
                                        .foregroundStyle(.secondary)
                                }
                                Spacer()
                                if task.dueLabel == "Quá hạn" && !task.isDone {
                                    Text("Quá hạn")
                                        .font(.system(size: 10, weight: .semibold))
                                        .foregroundStyle(.red)
                                        .padding(.horizontal, 6).padding(.vertical, 2)
                                        .background(Capsule().fill(.red.opacity(0.12)))
                                }
                            }
                        }
                        .toggleStyle(.checkbox)
                    }
                }
            }
            .listStyle(.inset)
            .scrollContentBackground(.hidden)
        }
    }

    private var addTask: some View {
        Button {
            store.showToast("Tạo công việc — bản demo")
        } label: {
            Image(systemName: "plus")
        }
        .buttonStyle(.glass)
    }
}

// MARK: - ZCloud

struct CloudView: View {
    @Environment(AppStore.self) private var store

    private let folders = ["Hình ảnh", "Video", "Tệp tin", "Tin nhắn thoại"]

    var body: some View {
        VStack(spacing: 0) {
            PaneHeader("ZCloud", trailing: AnyView(uploadButton))
            VStack(spacing: 16) {
                GlassEffectContainer(spacing: 12) {
                    HStack(spacing: 24) {
                        storageMeter
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Bộ nhớ Zalo")
                                .font(.system(size: 14, weight: .bold))
                            Text("Đã dùng 6,2 GB / 15 GB")
                                .font(.system(size: 12))
                                .foregroundStyle(.secondary)
                            Text("Tin nhắn, tệp tin của bạn được lưu bảo mật trên đám mây.")
                                .font(.system(size: 11))
                                .foregroundStyle(.tertiary)
                        }
                        Spacer()
                    }
                    .padding(20)
                    .glassEffect(in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                }
                .padding(.horizontal, 20)

                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 12), count: 4), spacing: 12) {
                    ForEach(folders, id: \.self) { folder in
                        Button {
                            store.showToast("\(folder) — bản demo")
                        } label: {
                            VStack(spacing: 10) {
                                Image(systemName: icon(for: folder))
                                    .font(.system(size: 26))
                                    .foregroundStyle(ZaloTheme.blue)
                                Text(folder).font(.system(size: 12.5, weight: .medium))
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 22)
                        }
                        .buttonStyle(.plain)
                        .glassEffect(in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                    }
                }
                .padding(.horizontal, 20)
                Spacer()
            }
            .padding(.top, 16)
        }
    }

    private var storageMeter: some View {
        ZStack {
            Circle().stroke(.quinary, lineWidth: 9)
            Circle()
                .trim(from: 0, to: 0.41)
                .stroke(ZaloTheme.blue, style: StrokeStyle(lineWidth: 9, lineCap: .round))
                .rotationEffect(.degrees(-90))
            Text("41%")
                .font(.system(size: 15, weight: .bold, design: .rounded))
        }
        .frame(width: 84, height: 84)
    }

    private func icon(for folder: String) -> String {
        switch folder {
        case "Hình ảnh": "photo.on.rectangle.angled"
        case "Video": "video.stack"
        case "Tệp tin": "doc.stack"
        default: "waveform"
        }
    }

    private var uploadButton: some View {
        Button {
            store.showToast("Tải lên ZCloud — bản demo")
        } label: {
            Image(systemName: "arrow.up.to.line")
        }
        .buttonStyle(.glass)
    }
}

// MARK: - Settings ("Cài đặt")

struct SettingsView: View {
    @Environment(AppStore.self) private var store
    @AppStorage("swiftzalo.notifications") private var notifications = true
    @AppStorage("swiftzalo.sounds") private var sounds = true
    @AppStorage("swiftzalo.autoStart") private var autoStart = false

    var body: some View {
        @Bindable var store = store
        return VStack(spacing: 0) {
            PaneHeader("Cài đặt")
            Form {
                Section("Giao diện") {
                    Toggle("Chế độ tối", isOn: $store.prefersDark)
                    LabeledContent("Phiên bản") {
                        Text("SwiftZalo 1.0 (demo)")
                    }
                }
                Section("Thông báo") {
                    Toggle("Hiện thông báo", isOn: $notifications)
                    Toggle("Âm thanh thông báo", isOn: $sounds)
                }
                Section("Khác") {
                    Toggle("Khởi động cùng hệ thống", isOn: $autoStart)
                    LabeledContent("Engine") {
                        Text("DemoZaloService (cục bộ)")
                    }
                    LabeledContent("Ghi chú") {
                        Text("Bản dựng lại giao diện Zalo PC bằng SwiftUI + Liquid Glass. Không liên kết với VNG.")
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                }
                Section {
                    Button("Đăng xuất", role: .destructive) {
                        store.logOut()
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }
}

// MARK: - Shared pane header

struct PaneHeader: View {
    @Environment(AppStore.self) private var store
    let title: String
    var trailing: AnyView? = nil

    init(_ title: String, trailing: AnyView? = nil) {
        self.title = title
        self.trailing = trailing
    }

    var body: some View {
        HStack {
            Text(title)
                .font(.system(size: 20, weight: .bold, design: .rounded))
            Spacer()
            trailing
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .background(.bar)
    }
}
