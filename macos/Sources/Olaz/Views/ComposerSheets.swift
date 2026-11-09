import SwiftUI

// Sheets of the composer's "+" menu: my contact card, a new reminder, and the conversation picker of the screenshot tool's
// "Chia sẻ". They gather what the official modals gather; sending goes through `ZaloEngine+Composer.swift`.

// MARK: - Danh thiếp

/// "Gửi danh thiếp" for my own card: the payload of the official SHARE_CONTACT modal, with its "Gửi kèm số điện thoại" option.
struct MyCardSheet: View {
    let conversationId: String
    let conversationName: String

    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var card: MyCardInfo?
    @State private var loaded = false
    @State private var withPhone = true
    @State private var busy = false
    @State private var message: String?

    var body: some View {
        VStack(spacing: 0) {
            SheetHeader(title: "Gửi danh thiếp") { dismiss() }
            Divider()
            VStack(alignment: .leading, spacing: 14) {
                if let card {
                    preview(card)
                    Toggle("Gửi kèm số điện thoại", isOn: $withPhone)
                        .toggleStyle(.switch).controlSize(.small).font(.system(size: 13))
                        .disabled(card.phone.isEmpty)
                    Text("Danh thiếp của bạn sẽ được gửi tới \(conversationName).")
                        .font(.system(size: 12)).foregroundStyle(.secondary)
                } else if loaded {
                    Label("Chưa đọc được hồ sơ của bạn từ Zalo. Hãy thử lại sau giây lát.", systemImage: "info.circle.fill")
                        .font(.system(size: 12.5)).foregroundStyle(.secondary)
                } else {
                    ProgressView().frame(maxWidth: .infinity, minHeight: 90)
                }
            }
            .padding(16)
            Divider()
            SheetFooter(message: message, action: "Gửi", busy: busy, disabled: card == nil, cancel: { dismiss() }) {
                Task { await send() }
            }
        }
        .frame(width: 380)
        .task {
            card = await engine.myCard()
            withPhone = (card?.withPhone ?? true) && !(card?.phone.isEmpty ?? true)
            loaded = true
        }
    }

    private func preview(_ card: MyCardInfo) -> some View {
        HStack(spacing: 12) {
            RemoteAvatar(url: card.avatar, name: card.name, size: 48)
            VStack(alignment: .leading, spacing: 2) {
                Text(card.name.isEmpty ? "Tài khoản của bạn" : card.name).font(.system(size: 14, weight: .semibold)).lineLimit(1)
                Text(withPhone && !card.phone.isEmpty ? card.phone : "Danh thiếp Zalo")
                    .font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 0)
            Image(systemName: "person.crop.rectangle").font(.system(size: 18)).foregroundStyle(.tint)
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.primary.opacity(0.05)))
        .accessibilityElement(children: .combine)
    }

    private func send() async {
        guard card != nil, !busy else { return }
        busy = true; message = nil
        let outcome = await engine.sendMyCard(to: conversationId, withPhone: withPhone)
        busy = false
        switch outcome {
        case .done, .cancelled: dismiss()
        case .pending(let text): CardActionCenter.shared.show(text, in: conversationId); dismiss()
        case .failed(let text): message = text
        }
    }
}

// MARK: - Nhắc hẹn

/// "Tạo nhắc hẹn" (the official legacy compose modal): content, a time chip or a picked time, repeat, and the group pin.
struct ReminderComposeSheet: View {
    let conversationId: String
    let info: ReminderComposeInfo

    enum TimeChoice: Hashable { case in15, in30, tomorrow9, custom }

    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var choice = TimeChoice.in30
    @State private var start = ReminderComposeSheet.time(for: .in30)
    @State private var rule = ReminderRepeat.none
    @State private var pin = true
    @State private var busy = false
    @State private var message: String?
    @State private var confirmDiscard = false
    /// Re-evaluates "is the time still in the future" while the sheet stays open.
    @State private var now = Date()

    private var trimmed: String { title.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var issue: String? {
        if trimmed.isEmpty { return "Vui lòng nhập nội dung nhắc hẹn" }
        if info.titleTooLong(title) { return "Bạn đã nhập quá số kí tự cho phép" }
        if start < now.addingTimeInterval(-59) { return "Vui lòng chọn thời gian tương lai" }
        return nil
    }

    var body: some View {
        VStack(spacing: 0) {
            SheetHeader(title: "Tạo nhắc hẹn") { requestClose() }
            Divider()
            form.padding(16)
            Divider()
            SheetFooter(message: message, hint: issue, action: "Tạo nhắc hẹn", busy: busy, disabled: issue != nil, cancel: { requestClose() }) {
                Task { await create() }
            }
        }
        .frame(width: 440)
        .onReceive(Timer.publish(every: 20, on: .main, in: .common).autoconnect()) { now = $0 }
        .confirmationDialog("Bạn muốn huỷ nhắc hẹn đang được tạo?", isPresented: $confirmDiscard, titleVisibility: .visible) {
            Button("Hủy thay đổi", role: .destructive) { dismiss() }
            Button("Không", role: .cancel) {}
        }
    }

    private var form: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Nội dung").font(.system(size: 12, weight: .semibold)).foregroundStyle(.secondary)
                TextField("Nhập nội dung mới hoặc dán link", text: $title, axis: .vertical)
                    .lineLimit(2...6).textFieldStyle(.roundedBorder)
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("Chọn thời gian").font(.system(size: 12, weight: .semibold)).foregroundStyle(.secondary)
                Picker("Chọn thời gian", selection: Binding(get: { choice }, set: pick)) {
                    Text("15 phút nữa").tag(TimeChoice.in15)
                    Text("30 phút nữa").tag(TimeChoice.in30)
                    Text("9:00 ngày mai").tag(TimeChoice.tomorrow9)
                    Text("Khác").tag(TimeChoice.custom)
                }
                .pickerStyle(.segmented).labelsHidden()
                if choice == .custom {
                    DatePicker("Chọn ngày nhắc hẹn", selection: $start, in: Calendar.current.startOfDay(for: now)..., displayedComponents: [.date, .hourAndMinute])
                        .environment(\.locale, Locale(identifier: "vi_VN"))
                        .font(.system(size: 13))
                } else {
                    Label(start.formatted(.dateTime.locale(Locale(identifier: "vi_VN")).weekday(.wide).day().month(.twoDigits).hour().minute()),
                          systemImage: "alarm")
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                }
            }
            Picker("Lặp lại", selection: $rule) {
                ForEach(ReminderRepeat.allCases) { Text($0.title).tag($0) }
            }
            .font(.system(size: 13))
            if info.pinToggle {
                Toggle("Ghim nhắc hẹn", isOn: $pin).toggleStyle(.switch).controlSize(.small).font(.system(size: 13))
            }
        }
    }

    private func pick(_ next: TimeChoice) {
        choice = next
        if next != .custom { start = Self.time(for: next) }
    }

    /// The modal's chips: seconds dropped, "9:00 ngày mai" in local time.
    static func time(for choice: TimeChoice, from now: Date = Date()) -> Date {
        let calendar = Calendar.current
        let base = calendar.dateInterval(of: .minute, for: now)?.start ?? now
        switch choice {
        case .in15: return base.addingTimeInterval(15 * 60)
        case .in30, .custom: return base.addingTimeInterval(30 * 60)
        case .tomorrow9:
            let tomorrow = calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: now)) ?? now
            return calendar.date(bySettingHour: 9, minute: 0, second: 0, of: tomorrow) ?? tomorrow
        }
    }

    private func requestClose() { if trimmed.isEmpty { dismiss() } else { confirmDiscard = true } }

    /// WRITE: runs only from the "Tạo nhắc hẹn" button.
    private func create() async {
        now = Date()
        guard issue == nil, !busy else { return }
        busy = true; message = nil
        let outcome = await engine.createReminder(in: conversationId, title: title, start: start, repeat: rule, pin: info.canPin && pin)
        busy = false
        switch outcome {
        case .done, .cancelled: dismiss()
        case .pending(let text): CardActionCenter.shared.show(text, in: conversationId); dismiss()
        case .failed(let text): message = text
        }
    }
}

// MARK: - Chia sẻ (screenshot tool)

/// The conversations a screenshot (or its recognised text) is shared to — the official SHARE_MSG_COMPOSE picker, native.
struct ScreenshotShareSheet: View {
    let share: ScreenshotShare

    static let maxTargets = 10

    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var search = ""
    @State private var selected: [String] = []
    @State private var busy = false
    @State private var message: String?

    private var rows: [ZaloEngine.ConversationSummary] {
        let all = engine.visibleConversations.sorted { ($0.preview?.date ?? .distantPast) > ($1.preview?.date ?? .distantPast) }
        let needle = search.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return all }
        return all.filter { title(of: $0).range(of: needle, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
    }

    var body: some View {
        VStack(spacing: 0) {
            SheetHeader(title: share.text == nil ? "Chia sẻ ảnh chụp màn hình" : "Chia sẻ văn bản") { dismiss() }
            Divider()
            VStack(alignment: .leading, spacing: 10) {
                preview
                TextField("Tìm cuộc trò chuyện", text: $search).textFieldStyle(.roundedBorder)
            }
            .padding(16)
            List(rows) { conversation in
                row(conversation)
            }
            .listStyle(.plain)
            Divider()
            SheetFooter(message: message, hint: "Đã chọn \(selected.count)/\(Self.maxTargets)", action: "Chia sẻ", busy: busy,
                        disabled: selected.isEmpty, cancel: { dismiss() }) {
                Task { await send() }
            }
        }
        .frame(width: 440, height: 560)
    }

    @ViewBuilder private var preview: some View {
        if let image = share.image, let picture = NSImage(contentsOf: image) {
            Image(nsImage: picture).resizable().scaledToFit()
                .frame(maxWidth: .infinity, maxHeight: 120)
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        } else if let text = share.text {
            Text(text).font(.system(size: 12.5)).lineLimit(4)
                .padding(10).frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color.primary.opacity(0.05)))
        }
    }

    private func row(_ conversation: ZaloEngine.ConversationSummary) -> some View {
        let isOn = selected.contains(conversation.id)
        return Button {
            if isOn { selected.removeAll { $0 == conversation.id } }
            else if selected.count < Self.maxTargets { selected.append(conversation.id) }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: isOn ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 16)).foregroundStyle(isOn ? AnyShapeStyle(.tint) : AnyShapeStyle(.tertiary))
                RemoteAvatar(url: conversation.avatar, name: conversation.displayName, size: 30)
                Text(title(of: conversation)).font(.system(size: 13)).lineLimit(1)
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!isOn && selected.count >= Self.maxTargets)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }

    private func title(of conversation: ZaloEngine.ConversationSummary) -> String {
        conversation.id == engine.selfConversationId ? "Cloud của tôi" : conversation.displayName
    }

    private func send() async {
        guard !selected.isEmpty, !busy else { return }
        busy = true; message = nil
        let outcome = await engine.shareScreenshot(share, to: selected)
        busy = false
        switch outcome {
        case .done, .cancelled: dismiss()
        case .pending(let text): CardActionCenter.shared.show(text, in: share.conversationId); dismiss()
        case .failed(let text): message = text
        }
    }
}

// MARK: - Shared chrome

struct SheetHeader: View {
    let title: String
    let close: () -> Void

    var body: some View {
        HStack {
            Text(title).font(.system(size: 15, weight: .semibold))
            Spacer()
            Button("Đóng", systemImage: "xmark", action: close).labelStyle(.iconOnly).buttonStyle(.plain).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
    }
}

struct SheetFooter: View {
    var message: String?
    var hint: String? = nil
    let action: String
    let busy: Bool
    let disabled: Bool
    let cancel: () -> Void
    let run: () -> Void

    var body: some View {
        VStack(spacing: 6) {
            if let message {
                Label(message, systemImage: "exclamationmark.triangle.fill").font(.system(size: 12)).foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else if let hint {
                Text(hint).font(.system(size: 11.5)).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack {
                Spacer()
                Button("Hủy", action: cancel).keyboardShortcut(.cancelAction)
                Button(action: run) {
                    if busy { ProgressView().controlSize(.small) } else { Text(action) }
                }
                .buttonStyle(.glassProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(disabled || busy)
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
    }
}

// MARK: - Engine-wide prompts of composer actions

/// What a composer action may ask while it runs, whichever chat is on screen: the official confirms (e.g. "Gửi dưới dạng file"),
/// the screenshot tool's "Chia sẻ" picker and the screen-recording permission notice.
private struct ComposerPrompts: ViewModifier {
    @Environment(ZaloEngine.self) private var engine

    func body(content: Content) -> some View {
        @Bindable var engine = engine
        content
            // Answered only by its buttons (the dismissal binding must not answer first and turn an OK into a cancel).
            .alert(engine.composerConfirm?.title ?? "", isPresented: Binding(get: { engine.composerConfirm != nil }, set: { _ in }),
                   presenting: engine.composerConfirm) { confirm in
                Button(confirm.ok) { engine.answerComposerConfirm(confirm, ok: true) }
                Button(confirm.cancel, role: .cancel) { engine.answerComposerConfirm(confirm, ok: false) }
            } message: { confirm in
                Text(confirm.message)
            }
            .sheet(item: $engine.screenshotShare) { share in
                ScreenshotShareSheet(share: share)
            }
            .alert("Cho phép ghi màn hình", isPresented: $engine.screenCaptureNeedsPermission) {
                Button("Mở Cài đặt") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") { NSWorkspace.shared.open(url) }
                }
                Button("Để sau", role: .cancel) {}
            } message: {
                // STR_DES_NO_PERMISSION_MAC_1, then where to find it.
                Text("Để sử dụng tính năng chụp màn hình, cần được kích hoạt trong cài đặt của hệ điều hành. Hãy bật \(AppBrand.name) trong Quyền riêng tư & Bảo mật ▸ Ghi màn hình, rồi thử lại.")
            }
    }
}

extension View {
    func composerPrompts() -> some View { modifier(ComposerPrompts()) }
}
