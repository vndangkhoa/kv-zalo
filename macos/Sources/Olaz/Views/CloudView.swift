import SwiftUI

// "zCloud": the official client's "Zalo Cloud" tool — storage quota & plan, media / files kept in the cloud
// (by type, by conversation, My Cloud), settings. Everything is fed by the official cloud services
// through `engine.cloud` (ZaloEngine+Cloud.swift). The shell lists the routes (CloudColumns.swift) and shows
// the picked screen in the detail column.

/// The zCloud screens, picked in the content column (`ShellModel.cloudRoute`).
enum CloudRoute: Hashable {
    case media, conversations, myCloud, plan, settings
}

extension CloudState {
    /// Free accounts only have "Cloud của tôi" (official: the media entries are hidden for the free plan).
    var hasMediaScreens: Bool { plan.map { $0.kind != .free && $0.kind != .afterGrace } ?? true }
}

/// "Cloud của tôi" documents (needs the official My Cloud conversation id).
struct CloudMyCloudScreen: View {
    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        if let id = engine.cloud.selfId ?? engine.selfConversationId {
            CloudMediaView(scope: .myCloud(id: id))
        } else {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

// MARK: - Storage card

private let cloudMediaColor = Color.orange
private let cloudBackupColor = Color.green
private let cloudMyCloudColor = Color.yellow
private let cloudFamilyColor = Color(red: 0.36, green: 0.52, blue: 0.78)

/// Quota bar with the official legend (Tin nhắn media / Sao lưu tin nhắn / Cloud của tôi / Gia đình).
struct CloudUsageBar: View {
    let plan: CloudState.Plan
    var height: CGFloat = 8

    private var segments: [(Color, Int64)] {
        [(cloudMediaColor, plan.cloudMedia), (cloudBackupColor, plan.messageBackup), (cloudMyCloudColor, plan.myCloud), (cloudFamilyColor, plan.family)]
    }

    var body: some View {
        GeometryReader { proxy in
            let quota = Double(max(plan.quota, 1))
            HStack(spacing: 1.5) {
                ForEach(Array(segments.enumerated()), id: \.offset) { _, segment in
                    let fraction = min(1, Double(max(0, segment.1)) / quota)
                    if segment.1 > 0 {
                        Rectangle().fill(segment.0.gradient).frame(width: max(2, proxy.size.width * fraction))
                    }
                }
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Capsule().fill(.quaternary))
            .clipShape(Capsule())
        }
        .frame(height: height)
    }
}

struct CloudLegendRow: View {
    let color: Color
    let title: String
    let bytes: Int64
    var body: some View {
        HStack(spacing: 8) {
            Circle().fill(color.gradient).frame(width: 9, height: 9)
            Text(title).font(.system(size: 12.5))
            Spacer(minLength: 4)
            Text(CloudFormat.size(bytes)).font(.system(size: 12.5, weight: .medium)).foregroundStyle(.secondary)
        }
    }
}

struct CloudStorageCard: View {
    let plan: CloudState.Plan?
    let loaded: Bool
    let media: CloudBreakdown
    let myCloud: CloudBreakdown

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                Text("Dung lượng lưu trữ").font(.system(size: 12.5, weight: .semibold))
                Spacer()
                if let plan {
                    Text("\(CloudFormat.size(plan.usage + plan.family)) / \(CloudFormat.size(plan.quota))")
                        .font(.system(size: 12, weight: .medium)).foregroundStyle(.secondary)
                }
            }
            if let plan {
                CloudUsageBar(plan: plan)
                // Wraps in the narrow content column.
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 96), spacing: 8, alignment: .leading)], alignment: .leading, spacing: 4) {
                    legend(cloudMediaColor, "Media")
                    legend(cloudBackupColor, "Sao lưu")
                    legend(cloudMyCloudColor, "Cloud của tôi")
                    if plan.family > 0 { legend(cloudFamilyColor, "Gia đình") }
                }
                Text("Còn trống \(CloudFormat.size(plan.remaining))").font(.system(size: 11.5)).foregroundStyle(.secondary)
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Đang tính toán…").font(.system(size: 12)).foregroundStyle(.secondary)
                }
            }
        }
        .padding(12)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
    }

    private func legend(_ color: Color, _ title: String) -> some View {
        HStack(spacing: 4) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(title).font(.system(size: 10.5)).foregroundStyle(.secondary)
        }
    }
}

// MARK: - Conversations

struct CloudConversationsView: View {
    @Environment(ZaloEngine.self) private var engine
    @State private var search = ""
    @State private var bySize = true
    @State private var path: [CloudState.Conversation] = []

    /// Conversations with a resolved display name (conversation list first, then the official cloud name).
    private var rows: [CloudState.Conversation] {
        let known = Dictionary(engine.conversations.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        var list = engine.cloud.conversations.map { conv -> CloudState.Conversation in
            let match = known[conv.id]
            let name = !conv.name.isEmpty ? conv.name : (match?.displayName ?? (conv.isGroup ? "Nhóm" : "Trò chuyện"))
            return CloudState.Conversation(id: conv.id, name: name, avatar: conv.avatar ?? match?.avatar, isGroup: conv.isGroup,
                                           total: conv.total, lastActivity: conv.lastActivity)
        }
        let q = ZaloEngine.contactFold(search.trimmingCharacters(in: .whitespaces))
        if !q.isEmpty { list = list.filter { ZaloEngine.contactFold($0.name).contains(q) } }
        list.sort { bySize ? $0.total > $1.total : ($0.lastActivity ?? .distantPast) > ($1.lastActivity ?? .distantPast) }
        return list
    }

    private func displayName(_ conv: CloudState.Conversation) -> String { conv.name }

    var body: some View {
        NavigationStack(path: $path) {
            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    ContactsSearchField(text: $search, prompt: "Nhập tên trò chuyện")
                    Picker("Sắp xếp", selection: $bySize) {
                        Text("Dung lượng").tag(true)
                        Text("Hoạt động gần nhất").tag(false)
                    }
                    .pickerStyle(.segmented).labelsHidden().frame(width: 230)
                }
                .padding(.horizontal, 20).padding(.top, 14).padding(.bottom, 8)
                if !engine.cloud.loaded {
                    ProgressView("Đang tính toán…").frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if !engine.cloud.hiddenLookup {
                    ContentUnavailableView("Chưa thể hiển thị", systemImage: "lock.shield",
                                           description: Text("Chưa xác định được các trò chuyện ẩn nên dữ liệu theo trò chuyện tạm thời bị ẩn."))
                } else if rows.isEmpty {
                    ContentUnavailableView(search.isEmpty ? "Chưa có dữ liệu" : "Không tìm thấy", systemImage: "bubble.left.and.bubble.right")
                } else {
                    List(rows) { conv in
                        Button { path = [conv] } label: {
                            HStack(spacing: 12) {
                                RemoteAvatar(url: conv.avatar, name: displayName(conv), size: 40)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(displayName(conv)).font(.system(size: 13.5, weight: .medium)).lineLimit(1)
                                    Text(conv.isGroup ? "Nhóm" : "Cá nhân").font(.system(size: 11.5)).foregroundStyle(.secondary)
                                }
                                Spacer()
                                Text(CloudFormat.size(conv.total)).font(.system(size: 13, weight: .semibold))
                                Text(conv.lastActivity.map { CloudFormat.date($0) } ?? "--")
                                    .font(.system(size: 12)).foregroundStyle(.secondary).frame(width: 82, alignment: .trailing)
                                Image(systemName: "chevron.right").font(.system(size: 11, weight: .semibold)).foregroundStyle(.tertiary)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .listRowInsets(EdgeInsets(top: 4, leading: 14, bottom: 4, trailing: 14))
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle("Theo trò chuyện")
            .navigationDestination(for: CloudState.Conversation.self) { conv in
                CloudMediaView(scope: .conversation(id: conv.id, name: displayName(conv)))
                    .id(conv.id)
            }
        }
    }
}

// MARK: - Plan

struct CloudPlanView: View {
    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        ScrollView {
            if let plan = engine.cloud.plan {
                VStack(alignment: .leading, spacing: 16) {
                    header(plan)
                    usageCard(plan)
                    typeCard
                    detailsCard(plan)
                    backupCard(plan)
                    Label("Bạn có thể quản lý gói này trên điện thoại.", systemImage: "info.circle")
                        .font(.system(size: 12.5)).foregroundStyle(.secondary)
                        .padding(.horizontal, 4)
                }
                .padding(20)
                .frame(maxWidth: 760)
                .frame(maxWidth: .infinity)
            } else {
                ProgressView("Đang tính toán…").frame(maxWidth: .infinity, minHeight: 300)
            }
        }
        .navigationTitle("Thông tin gói")
    }

    private func header(_ plan: CloudState.Plan) -> some View {
        HStack(spacing: 14) {
            Image(systemName: "externaldrive.fill")
                .font(.system(size: 26)).foregroundStyle(.white)
                .frame(width: 56, height: 56)
                .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(.tint))
            VStack(alignment: .leading, spacing: 3) {
                Text(planTitle(plan)).font(.system(size: 20, weight: .bold))
                Text(status(plan)).font(.system(size: 13)).foregroundStyle(.secondary)
            }
            Spacer()
            Text(CloudFormat.size(plan.quota))
                .font(.system(size: 13, weight: .semibold))
                .padding(.horizontal, 12).padding(.vertical, 6)
                .glassEffect(.regular, in: Capsule())
        }
        .padding(16)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    private func planTitle(_ plan: CloudState.Plan) -> String {
        switch plan.kind {
        case .paid, .grace: plan.name
        case .free, .afterGrace: "Gói miễn phí"
        }
    }

    private func status(_ plan: CloudState.Plan) -> String {
        switch plan.kind {
        case .paid:
            if let date = plan.expireAt { return "Đang hoạt động • hết hạn \(CloudFormat.date(date))" }
            return "Đang hoạt động"
        case .grace: return "Đã hết hạn — dữ liệu còn được giữ trong thời gian gia hạn"
        case .free: return "Chỉ lưu trữ Cloud của tôi"
        case .afterGrace: return "Đã hết thời gian gia hạn zCloud"
        }
    }

    private func usageCard(_ plan: CloudState.Plan) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                Text("Dung lượng lưu trữ").font(.system(size: 15, weight: .semibold))
                Spacer()
                Text("Đã dùng \(CloudFormat.size(plan.usage + plan.family))").font(.system(size: 13, weight: .medium))
                Text("•").foregroundStyle(.tertiary)
                Text("Còn trống \(CloudFormat.size(plan.remaining))").font(.system(size: 13)).foregroundStyle(.secondary)
            }
            CloudUsageBar(plan: plan, height: 12)
            VStack(spacing: 7) {
                CloudLegendRow(color: cloudMediaColor, title: "Tin nhắn media", bytes: plan.cloudMedia)
                CloudLegendRow(color: cloudBackupColor, title: "Sao lưu tin nhắn", bytes: plan.messageBackup)
                CloudLegendRow(color: cloudMyCloudColor, title: "Cloud của tôi", bytes: plan.myCloud)
                if plan.family > 0 { CloudLegendRow(color: cloudFamilyColor, title: "Gia đình", bytes: plan.family) }
            }
        }
        .padding(16)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    private var typeCard: some View {
        let media = engine.cloud.media, mine = engine.cloud.myCloudTypes
        let maxBytes = Double(max(1, [CloudItemKind.photo, .video, .file, .voice].map { media.bytes($0) + mine.bytes($0) }.max() ?? 1))
        return VStack(alignment: .leading, spacing: 12) {
            Text("Dung lượng của tôi").font(.system(size: 15, weight: .semibold))
            ForEach([CloudItemKind.photo, .video, .file, .voice], id: \.self) { kind in
                let bytes = media.bytes(kind) + mine.bytes(kind)
                VStack(alignment: .leading, spacing: 5) {
                    HStack {
                        Label(kind.title, systemImage: kind.symbol).font(.system(size: 13)).labelStyle(.titleAndIcon)
                        Spacer()
                        Text(CloudFormat.size(bytes)).font(.system(size: 13, weight: .medium)).foregroundStyle(.secondary)
                    }
                    GeometryReader { proxy in
                        Capsule().fill(.quaternary).overlay(alignment: .leading) {
                            Capsule().fill(color(kind).gradient).frame(width: max(bytes > 0 ? 4 : 0, proxy.size.width * Double(bytes) / maxBytes))
                        }
                    }
                    .frame(height: 6)
                }
            }
        }
        .padding(16)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    private func color(_ kind: CloudItemKind) -> Color {
        switch kind {
        case .photo: .orange
        case .video: .green
        case .file: .yellow
        default: cloudFamilyColor
        }
    }

    private func detailsCard(_ plan: CloudState.Plan) -> some View {
        VStack(spacing: 0) {
            infoRow("Gói", planTitle(plan))
            if plan.isPaid {
                Divider().opacity(0.4)
                infoRow("Hết hạn vào", CloudFormat.date(plan.expireAt))
                if let auto = plan.autoRenew {
                    Divider().opacity(0.4)
                    infoRow("Tự động gia hạn", auto ? "Bật" : "Tắt")
                }
                if plan.months > 0 {
                    Divider().opacity(0.4)
                    infoRow("Chu kỳ", "\(plan.months) tháng")
                }
            }
            if plan.fileLimit > 0 {
                Divider().opacity(0.4)
                infoRow("Dung lượng tối đa mỗi file", CloudFormat.size(plan.fileLimit))
            }
            Divider().opacity(0.4)
            infoRow("Khoá cloud", plan.hasKey ? "Đã có" : "Chưa có — mở Zalo trên điện thoại để đồng bộ")
        }
        .padding(.horizontal, 16).padding(.vertical, 6)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    private func backupCard(_ plan: CloudState.Plan) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Tự động lưu trữ lên zCloud").font(.system(size: 15, weight: .semibold))
            toggleRow("Tin nhắn media", plan.backupMedia)
            toggleRow("Cloud của tôi", plan.backupMyCloud)
            toggleRow("Sao lưu tin nhắn", plan.backupMessages)
            Text("Cài đặt lưu trữ được điều khiển từ Zalo trên điện thoại.")
                .font(.system(size: 11.5)).foregroundStyle(.secondary)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    private func toggleRow(_ title: String, _ on: Bool) -> some View {
        HStack {
            Text(title).font(.system(size: 13))
            Spacer()
            Label(on ? "Bật" : "Tắt", systemImage: on ? "checkmark.circle.fill" : "minus.circle")
                .font(.system(size: 12.5, weight: .medium))
                .foregroundStyle(on ? Color.green : Color.secondary)
        }
    }

    private func infoRow(_ title: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title).font(.system(size: 13)).foregroundStyle(.secondary)
            Spacer(minLength: 12)
            Text(value).font(.system(size: 13, weight: .medium)).multilineTextAlignment(.trailing)
        }
        .padding(.vertical, 9)
    }
}

// MARK: - Settings

struct CloudSettingsView: View {
    @Environment(ZaloEngine.self) private var engine
    @State private var confirming: Bool?
    @State private var busy = false
    @State private var message: String?

    private var settings: CloudState.Settings { engine.cloud.settings }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .top, spacing: 14) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Tự động giải phóng dung lượng").font(.system(size: 15, weight: .semibold))
                            Text(description).font(.system(size: 12.5)).foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Spacer(minLength: 12)
                        Toggle("", isOn: Binding(get: { settings.offloadEnabled }, set: { confirming = $0 }))
                            .labelsHidden().toggleStyle(.switch)
                            .disabled(!settings.offloadAvailable || busy)
                    }
                    if let hint = unavailableHint {
                        Label(hint, systemImage: "info.circle").font(.system(size: 12)).foregroundStyle(.secondary)
                    } else if settings.offloadEnabled {
                        Text(lastInfo).font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                    if let message { Text(message).font(.system(size: 12)).foregroundStyle(.orange) }
                }
                .padding(16)
                .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))

                VStack(alignment: .leading, spacing: 6) {
                    Text("Hỗ trợ").font(.system(size: 15, weight: .semibold))
                    Text("Quản lý gói, gia hạn và cài đặt lưu trữ (Tin nhắn media, Cloud của tôi, Sao lưu tin nhắn) được thực hiện trên Zalo điện thoại.")
                        .font(.system(size: 12.5)).foregroundStyle(.secondary)
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
            }
            .padding(20)
            .frame(maxWidth: 680)
            .frame(maxWidth: .infinity)
        }
        .navigationTitle("Cài đặt zCloud")
        .alert(confirming == true ? "Bật tự động giải phóng dung lượng?" : "Tắt tự động giải phóng dung lượng?",
               isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } })) {
            Button(confirming == true ? "Bật" : "Tắt") {
                guard let enable = confirming else { return }
                confirming = nil
                apply(enable)
            }
            Button("Huỷ", role: .cancel) { confirming = nil }
        } message: {
            Text(confirming == true
                 ? "Nội dung đã lưu trên zCloud sẽ được gỡ khỏi máy sau \(max(settings.ttlDays, 1)) ngày. Bạn có thể xem lại những nội dung này bất kỳ lúc nào."
                 : "Zalo sẽ không tự động gỡ nội dung đã lưu trên zCloud khỏi máy nữa.")
        }
    }

    private var description: String {
        "Nội dung đã lưu trên zCloud sẽ được gỡ khỏi máy sau \(max(settings.ttlDays, 1)) ngày. Bạn có thể xem lại những nội dung này bất kỳ lúc nào."
    }

    private var unavailableHint: String? {
        guard !settings.offloadAvailable else { return nil }
        guard let plan = engine.cloud.plan else { return "Đang tải thông tin gói…" }
        if !plan.isPaid { return "Tính năng này chỉ hoạt động khi bạn nâng cấp zCloud." }
        if !plan.hasKey { return "Vui lòng mở Zalo trên điện thoại để hoàn tất đồng bộ." }
        return "Tính năng tạm thời chưa khả dụng, vui lòng thử lại sau."
    }

    private var lastInfo: String {
        if let at = settings.lastOffloadAt, settings.lastOffloadSize > 0 {
            return "Tối ưu dung lượng gần nhất lúc \(CloudFormat.dateTime(at)), giải phóng \(CloudFormat.size(settings.lastOffloadSize)) trên máy tính."
        }
        return "Quá trình dọn dẹp diễn ra tự động, bạn sẽ nhận được thông báo khi hoàn tất."
    }

    private func apply(_ enable: Bool) {
        busy = true
        message = nil
        Task {
            let result = await engine.cloudSetOffload(enable)
            busy = false
            if !result.ok { message = result.error ?? "Không thay đổi được cài đặt." }
        }
    }
}
