import SwiftUI

// "Ẩn trò chuyện" (hidden chats): the PIN flows of the official modal HIDDEN_CHAT (lazy bundle module ~12600465; screens
// SCREEN_POPUP_HIDDEN_CHAT) and the settings entry ("Thiết lập ẩn trò chuyện"). The PIN is 4 digits; it is only handed to the
// official hidden-chat manager (`isKeyPIN`, `changePIN`, `resetPIN`, `updateStateItemHiddenChat`) and never stored or logged here.

/// The screens of one flow and how they lead into each other (pure, the views only render it).
struct HiddenChatFlow: Equatable {
    enum Kind: Equatable {
        case hide(String)
        /// `requiresPIN` false = opened with the PIN already (official `byPassPIN` → UNHIDE_CONV, a plain confirmation).
        case unhide(String, requiresPIN: Bool)
        case open(String)
        case setPIN
        case changePIN
        case resetPIN
    }

    enum Screen: Equatable {
        case introduce, createPIN, confirmPIN, enterPIN, unhideConfirm
        case oldPIN, newPIN, repeatPIN
        case resetConfirm
        case hideSuccess, setPINSuccess, changeSuccess
    }

    /// What the view has to run after a PIN was typed.
    enum Step: Equatable {
        case none
        case hide(pin: String)
        case hideWithNewPIN(pin: String)
        case unhide(pin: String?)
        case verifyToOpen(pin: String)
        case verifyOld(pin: String)
        case setPIN(pin: String, changed: Bool)
    }

    static let wrongPIN = "Mã PIN không đúng. Vui lòng thử lại."           // STR_ERROR_POPUP_HIDDEN_ENTER_PIN_INCORRECT / _CONFIRM_PIN_INCORRECT

    let kind: Kind
    private(set) var screen: Screen
    private(set) var error: String?
    /// First entry of a new PIN while the confirmation is pending (memory only).
    private var pending: String?

    init(kind: Kind, hasPIN: Bool) {
        self.kind = kind
        switch kind {
        case .hide: screen = hasPIN ? .enterPIN : .introduce
        case .unhide(_, let requiresPIN): screen = requiresPIN ? .enterPIN : .unhideConfirm
        case .open: screen = .enterPIN
        case .setPIN: screen = .createPIN
        case .changePIN: screen = .oldPIN
        case .resetPIN: screen = .resetConfirm
        }
    }

    var conversationId: String? {
        switch kind {
        case .hide(let id), .unhide(let id, _), .open(let id): id
        default: nil
        }
    }

    /// Official `_getTitle`.
    var title: String {
        switch screen {
        case .introduce: "Ẩn trò chuyện"
        case .createPIN: "Đặt mã PIN"
        case .confirmPIN: "Xác nhận lại mã PIN"
        case .enterPIN:
            switch kind {
            case .hide: "Nhập mã PIN để ẩn trò chuyện"
            case .open: "Nhập mã PIN để mở trò chuyện"
            default: "Bỏ ẩn trò chuyện"
            }
        case .unhideConfirm: "Bỏ ẩn trò chuyện"
        case .oldPIN, .newPIN, .repeatPIN: "Đổi mã PIN"
        case .resetConfirm: "Đặt lại mã PIN"
        case .hideSuccess: "Ẩn trò chuyện thành công"
        case .setPINSuccess: "Cài đặt mã PIN thành công"
        case .changeSuccess: "Đổi mã PIN thành công"
        }
    }

    /// Screens that take 4 digits.
    var takesPIN: Bool {
        switch screen {
        case .createPIN, .confirmPIN, .enterPIN, .oldPIN, .newPIN, .repeatPIN: true
        default: false
        }
    }

    /// Introduction → "Đặt mã PIN".
    mutating func startSettingPIN() { error = nil; screen = .createPIN }

    mutating func submit(pin: String) -> Step {
        switch screen {
        case .createPIN:
            pending = pin; error = nil; screen = .confirmPIN
            return .none
        case .confirmPIN:
            guard pin == pending else { error = Self.wrongPIN; return .none }
            error = nil
            if case .setPIN = kind { return .setPIN(pin: pin, changed: false) }
            return .hideWithNewPIN(pin: pin)
        case .enterPIN:
            switch kind {
            case .hide: return .hide(pin: pin)
            case .unhide: return .unhide(pin: pin)
            case .open: return .verifyToOpen(pin: pin)
            default: return .none
            }
        case .oldPIN:
            return .verifyOld(pin: pin)
        case .newPIN:
            pending = pin; error = nil; screen = .repeatPIN
            return .none
        case .repeatPIN:
            guard pin == pending else { error = Self.wrongPIN; return .none }
            error = nil
            return .setPIN(pin: pin, changed: true)
        default:
            return .none
        }
    }

    /// The old PIN of "Đổi mã PIN" checked out.
    mutating func oldPINVerified() { error = nil; screen = .newPIN }

    /// "Quay lại" on the repeat screens.
    mutating func back() {
        pending = nil; error = nil
        switch screen {
        case .repeatPIN: screen = .newPIN
        case .confirmPIN: screen = .createPIN
        default: break
        }
    }

    mutating func fail(_ text: String) { error = text }

    static let pinAlreadySet = "Đã có mã PIN cho trò chuyện ẩn. Hãy dùng “Đổi mã PIN” nếu muốn đổi."

    /// The page refused to set a PIN because one exists (this view was stale): ask for it instead, never overwrite it.
    mutating func pinAlreadyExists() {
        pending = nil
        if case .hide = kind { error = nil; screen = .enterPIN } else { error = Self.pinAlreadySet }
    }

    /// The work finished: the success screen of the kind (nil = nothing more to show, close).
    mutating func succeeded() -> Bool {
        pending = nil; error = nil
        switch (kind, screen) {
        case (.hide, .confirmPIN): screen = .hideSuccess; return true
        case (.setPIN, _): screen = .setPINSuccess; return true
        case (.changePIN, _): screen = .changeSuccess; return true
        default: return false
        }
    }
}

struct HiddenChatSheet: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State var flow: HiddenChatFlow
    /// Called when a hidden conversation was unlocked with the PIN (the shell then opens it).
    var onUnlocked: (String) -> Void = { _ in }
    /// Settings window: results are shown inline (the shell toast is not visible there).
    var inlineResult: Binding<String?>?

    @State private var busy = false
    @State private var resetToken = 0

    var body: some View {
        VStack(spacing: 16) {
            Text(flow.title).font(.system(size: 15, weight: .semibold)).multilineTextAlignment(.center)
            content
            if let error = flow.error {
                Text(error).font(.system(size: 12.5)).foregroundStyle(.red).multilineTextAlignment(.center)
            }
            buttons
        }
        .padding(22)
        .frame(width: 380)
        .disabled(busy)
    }

    // MARK: Content

    @ViewBuilder private var content: some View {
        switch flow.screen {
        case .introduce:
            Text("Trò chuyện sẽ được ẩn khỏi danh sách tin nhắn. Chỉ có bạn mới có thể tìm thấy và mở lại chúng bằng cách sử dụng mã PIN.")
                .font(.system(size: 13.5)).multilineTextAlignment(.center)
        case .createPIN:
            pinEntry(prompt: "Nhập mã PIN", notice: true)
        case .confirmPIN:
            pinEntry(prompt: "Nhập lại mã PIN của bạn", notice: true)
        case .enterPIN:
            VStack(spacing: 10) {
                pinEntry(prompt: nil, notice: false)
                // STR_DES_POPUP_HIDDEN_OPEN_CONV_1 + _2 (the official client opens its Settings from the second part).
                (Text("Quên mã PIN? Bạn phải ") + Text("cài đặt lại mã.").fontWeight(.semibold).underline())
                    .font(.system(size: 12)).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
        case .unhideConfirm:
            (Text("Bạn có muốn hiện lại trò chuyện ") + Text(conversationName).fontWeight(.semibold) + Text(" ?"))
                .font(.system(size: 13.5)).multilineTextAlignment(.center)
        case .oldPIN: pinEntry(prompt: "Nhập mã PIN cũ", notice: false)
        case .newPIN: pinEntry(prompt: "Nhập mã PIN mới", notice: false)
        case .repeatPIN: pinEntry(prompt: "Xác nhận lại mã PIN mới", notice: false)
        case .resetConfirm:
            Text("Khi đặt lại mã PIN, tất cả trò chuyện đang được ẩn sẽ bị xoá (bao gồm toàn bộ nội dung cuộc trò chuyện). Bạn có muốn tiếp tục?")
                .font(.system(size: 13.5)).multilineTextAlignment(.center)
        case .hideSuccess:
            VStack(spacing: 8) {
                (Text("Bạn đã ẩn trò chuyện ") + Text(conversationName).fontWeight(.semibold) + Text(" thành công."))
                (Text("Để mở các trò chuyện đã ẩn, bạn có thể nhập ") + Text("mã PIN đã cài đặt hoặc tên trò chuyện ẩn").fontWeight(.semibold) + Text(" tại khung tìm kiếm."))
                Text("Hướng dẫn:").foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                (Text("Ngoài ra bạn có thể vào ") + Text("Cài đặt > Tin nhắn > Thiết lập ẩn trò chuyện").fontWeight(.semibold) + Text(" để thay đổi hoặc đặt lại mã PIN."))
                    .font(.system(size: 12.5)).foregroundStyle(.secondary)
            }
            .font(.system(size: 13.5)).multilineTextAlignment(.center)
        case .setPINSuccess:
            successLabel("Chúc mừng bạn đã cài mã PIN thành công, bạn có thể dùng mã PIN này để ẩn các trò chuyện và mở lại các trò chuyện đã ẩn bằng cách tìm kiếm theo tên hoặc mã PIN vừa cài đặt.")
        case .changeSuccess:
            successLabel("Mã PIN của bạn đã được đổi thành công")
        }
    }

    private func successLabel(_ text: String) -> some View {
        VStack(spacing: 10) {
            Image(systemName: "checkmark.circle.fill").font(.system(size: 40)).foregroundStyle(.green)
            Text(text).font(.system(size: 13.5)).multilineTextAlignment(.center)
        }
    }

    private func pinEntry(prompt: String?, notice: Bool) -> some View {
        VStack(spacing: 12) {
            if let prompt { Text(prompt).font(.system(size: 13.5)) }
            PINEntryField(onDone: submit).id(resetToken)
            if notice {
                (Text("Chú ý: ").fontWeight(.semibold) + Text("Ghi nhớ mã PIN để xem lại trò chuyện. Trò chuyện sẽ mất nếu bạn cài đặt lại mã PIN."))
                    .font(.system(size: 12)).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
        }
    }

    // MARK: Buttons

    @ViewBuilder private var buttons: some View {
        switch flow.screen {
        case .introduce:
            HStack {
                Button("Hủy") { dismiss() }.buttonStyle(.glass).keyboardShortcut(.cancelAction)
                Button("Đặt mã PIN") { flow.startSettingPIN() }.buttonStyle(.glassProminent).keyboardShortcut(.defaultAction)
            }
        case .unhideConfirm:
            HStack {
                Button("Hủy") { dismiss() }.buttonStyle(.glass).keyboardShortcut(.cancelAction)
                Button("Bỏ ẩn trò chuyện") { run(.unhide(pin: nil)) }.buttonStyle(.glassProminent).keyboardShortcut(.defaultAction)
            }
        case .resetConfirm:
            HStack {
                Button("Đóng") { dismiss() }.buttonStyle(.glass).keyboardShortcut(.cancelAction)
                Button("Đặt lại mã PIN", role: .destructive) { resetPIN() }.buttonStyle(.glassProminent).tint(.red)
            }
        case .hideSuccess, .setPINSuccess, .changeSuccess:
            Button("Đóng") { dismiss() }.buttonStyle(.glassProminent).keyboardShortcut(.defaultAction)
        case .repeatPIN, .confirmPIN:
            HStack {
                Button("Quay lại") { flow.back(); resetToken += 1 }.buttonStyle(.glass)
                Button("Hủy") { dismiss() }.buttonStyle(.glass).keyboardShortcut(.cancelAction)
            }
        default:
            Button("Hủy") { dismiss() }.buttonStyle(.glass).keyboardShortcut(.cancelAction)
        }
    }

    // MARK: Work

    private var conversationName: String {
        guard let id = flow.conversationId else { return "" }
        return engine.conversations.first { $0.id == id }?.displayName ?? (id.hasPrefix("g") ? "Nhóm" : "Người dùng Zalo")
    }

    private func submit(_ pin: String) {
        let step = flow.submit(pin: pin)
        resetToken += 1
        run(step)
    }

    private func resetPIN() {
        busy = true
        Task {
            let result = await engine.resetHiddenChatPIN()
            busy = false
            finish(result)
        }
    }

    private func finish(_ result: ConversationActionResult) {
        if let inlineResult { inlineResult.wrappedValue = result.text } else { engine.report(result) }
        if result.ok { dismiss() }
    }

    private func run(_ step: HiddenChatFlow.Step) {
        guard step != .none else { return }
        busy = true
        Task {
            defer { busy = false }
            switch step {
            case .none:
                break
            case .hide(let pin):
                guard let id = flow.conversationId else { return }
                let result = await engine.hideConversation(id, pin: pin)
                if result.badPIN { flow.fail(result.text); resetToken += 1 } else { engine.report(result); if result.ok { dismiss() } }
            case .hideWithNewPIN(let pin):
                guard let id = flow.conversationId else { return }
                let result = await engine.hideConversation(id, settingNewPIN: pin)
                if result.ok { _ = flow.succeeded() }
                else if result.pinExists { flow.pinAlreadyExists(); resetToken += 1 }
                else { flow.fail(result.text); resetToken += 1 }
            case .unhide(let pin):
                guard let id = flow.conversationId else { return }
                let result = await engine.unhideConversation(id, pin: pin)
                if result.badPIN { flow.fail(result.text); resetToken += 1 } else { engine.report(result); if result.ok { dismiss() } }
            case .verifyToOpen(let pin):
                guard let id = flow.conversationId else { return }
                if await engine.checkHiddenChatPIN(pin) {
                    engine.unlockHiddenConversation(id)
                    onUnlocked(id)
                    dismiss()
                } else {
                    flow.fail(HiddenChatFlow.wrongPIN)
                    resetToken += 1
                }
            case .verifyOld(let pin):
                if await engine.checkHiddenChatPIN(pin) { flow.oldPINVerified() } else { flow.fail(HiddenChatFlow.wrongPIN) }
                resetToken += 1
            case .setPIN(let pin, let changed):
                let result = await engine.setHiddenChatPIN(pin, changed: changed)
                if result.ok { _ = flow.succeeded() }
                else if result.pinExists { flow.pinAlreadyExists(); resetToken += 1 }
                else { flow.fail(result.text); resetToken += 1 }
            }
        }
    }
}

/// Four-digit PIN field: dots for the typed digits, calls `onDone` with the 4th digit. The field is a `SecureField`
/// (secure keyboard input); digits only.
struct PINEntryField: View {
    let onDone: (String) -> Void
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        ZStack {
            HStack(spacing: 10) {
                ForEach(0..<4, id: \.self) { index in
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(.quaternary.opacity(index < text.count ? 0.9 : 0.4))
                        .frame(width: 48, height: 58)
                        .overlay {
                            if index < text.count { Circle().fill(.primary).frame(width: 10, height: 10) }
                        }
                        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .stroke(index == text.count && focused ? AnyShapeStyle(.tint) : AnyShapeStyle(.clear), lineWidth: 1.5))
                }
            }
            SecureField("", text: $text)
                .textFieldStyle(.plain)
                .focused($focused)
                .opacity(0.02)
                .frame(width: 200, height: 58)
                .onChange(of: text) { _, value in
                    let digits = String(value.filter(\.isASCII).filter(\.isNumber).prefix(4))
                    if digits != value { text = digits; return }
                    if digits.count == 4 { onDone(digits); text = "" }
                }
        }
        .contentShape(Rectangle())
        .onTapGesture { focused = true }
        .onAppear { focused = true }
    }
}

// MARK: - Settings

/// "Thiết lập ẩn trò chuyện": set, change or reset the PIN (official Settings ▸ Tin nhắn).
struct HiddenChatSettingsSection: View {
    @Environment(ZaloEngine.self) private var engine
    @State private var flow: HiddenChatFlow?
    @State private var result: String?

    var body: some View {
        let state = engine.conversationActions
        if state.loaded, state.hiddenEnabled {
            Section {
                if state.hasHiddenPIN {
                    LabeledContent {
                        Button("Đổi mã PIN") { flow = HiddenChatFlow(kind: .changePIN, hasPIN: true) }
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Đổi mã PIN")
                            Text("Đổi mã PIN cho trò chuyện ẩn").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    LabeledContent {
                        Button("Đặt lại mã PIN", role: .destructive) { flow = HiddenChatFlow(kind: .resetPIN, hasPIN: true) }
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Đặt lại mã PIN")
                            Text("Tất cả trò chuyện đang ẩn sẽ bị xóa và không thể khôi phục").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                } else {
                    LabeledContent {
                        Button("Cài đặt mã PIN") { flow = HiddenChatFlow(kind: .setPIN, hasPIN: false) }
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Ẩn trò chuyện")
                            Text("Đặt mã PIN cho các cuộc trò chuyện riêng tư để tránh người khác nhìn thấy trên máy của bạn").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                if let result { Text(result).font(.caption).foregroundStyle(.secondary) }
            } header: {
                Text("Thiết lập ẩn trò chuyện")
            }
            .sheet(item: Binding(get: { flow.map(FlowBox.init) }, set: { if $0 == nil { flow = nil } })) { box in
                HiddenChatSheet(flow: box.flow, inlineResult: $result)
                    .environment(engine)
            }
        }
    }

    private struct FlowBox: Identifiable {
        let flow: HiddenChatFlow
        var id: String { String(describing: flow.kind) }
    }
}
