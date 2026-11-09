import AppKit
import SwiftUI

extension TourStep {
    var title: String {
        switch self {
        case .mailboxes: "Hộp thư ở thanh bên"
        case .search: "Tìm mọi thứ với ⌘K"
        case .message: "Rê chuột lên tin nhắn"
        case .composer: "Nhắn tin, sticker và bình chọn"
        case .toolbar: "Gọi điện và xem thông tin"
        }
    }

    var message: String {
        switch self {
        case .mailboxes: "Tin nhắn, Khác, Chưa đọc và Người lạ là các hộp thư. Cloud của tôi và Danh bạ cũng nằm ở đây. Lọc theo thẻ phân loại bằng nút thẻ cạnh ô tìm kiếm."
        case .search: "Gõ tên một người hoặc nhóm để mở ngay. Nhấn ⌘K ở bất kỳ đâu để mở bảng tìm nhanh."
        case .message: "Rê chuột lên tin nhắn để thả cảm xúc hoặc trả lời. Chuột phải để chuyển tiếp, ghim, thu hồi và hơn thế nữa."
        case .composer: "Nhấn + để chọn sticker, emoji hoặc tạo bình chọn trong nhóm. Return để gửi, ⇧Return để xuống dòng, gõ @ để nhắc tên thành viên."
        case .toolbar: "Gọi thoại, gọi video và mở bảng thông tin (⌥⌘I) với thành viên, ảnh, tệp và liên kết trong cuộc trò chuyện."
        }
    }

    /// SF Symbol of the step's topic (setup welcome list).
    var symbol: String {
        switch self {
        case .mailboxes: "tray.2"
        case .search: "magnifyingglass"
        case .message: "face.smiling"
        case .composer: "plus.bubble"
        case .toolbar: "phone"
        }
    }

    /// Steps that point into a chat, so a conversation has to be open.
    var needsOpenChat: Bool {
        switch self {
        case .message, .composer, .toolbar: true
        case .mailboxes, .search: false
        }
    }
}

extension View {
    /// Marks the view a tour step points at. The coach mark for `step` is attached here while that step runs.
    func tourAnchor(_ step: TourStep, arrowEdge: Edge = .trailing) -> some View {
        modifier(TourAnchorModifier(step: step, arrowEdge: arrowEdge))
    }

    /// Prepares the window for the running tour step (opens a chat, skips steps whose anchor is missing) and
    /// handles the tour keys. Applied once around the signed-in window.
    func tourCoordinator() -> some View {
        modifier(TourCoordinator())
    }
}

// MARK: - Anchors

/// Which tour anchors are on screen, in order of appearance. The first instance of a step owns its coach mark, so an
/// anchor applied to something that fans out to several rows still shows a single popover.
@MainActor
@Observable
final class TourAnchorRegistry {
    private var live: [TourStep: [UUID]] = [:]

    func register(_ step: TourStep, id: UUID) {
        guard live[step]?.contains(id) != true else { return }
        live[step, default: []].append(id)
    }

    func unregister(_ step: TourStep, id: UUID) {
        live[step]?.removeAll { $0 == id }
    }

    func owner(of step: TourStep) -> UUID? { live[step]?.first }
    func isOnScreen(_ step: TourStep) -> Bool { live[step]?.isEmpty == false }
}

extension EnvironmentValues {
    /// Set by `tourCoordinator()`; nil outside the signed-in window, where anchors present their coach mark directly.
    @Entry var tourAnchors: TourAnchorRegistry? = nil
}

private struct TourAnchorModifier: ViewModifier {
    @Environment(ShellModel.self) private var shell
    @Environment(\.tourAnchors) private var anchors
    @State private var anchorID = UUID()
    let step: TourStep
    let arrowEdge: Edge

    private var isPresented: Bool {
        shell.tour == step && (anchors == nil || anchors?.owner(of: step) == anchorID)
    }

    func body(content: Content) -> some View {
        content
            .onAppear { anchors?.register(step, id: anchorID) }
            .onDisappear { anchors?.unregister(step, id: anchorID) }
            .popover(isPresented: Binding(get: { isPresented }, set: { if !$0 { popoverClosed() } }),
                     arrowEdge: arrowEdge) {
                CoachMarkCard(step: step)
                    .environment(shell)
            }
    }

    /// The popover closed. Ends the tour only when the user dismissed it. The anchor view going away closes it too (the
    /// `.message` anchor moves to the newest message whenever one arrives): then another instance takes over, or none is
    /// left, and the tour goes on. Told apart by waiting for the unregister: a dismissed anchor is still the owner afterwards.
    private func popoverClosed() {
        guard shell.tour == step else { return }
        let (shell, anchors, id, step) = (shell, anchors, anchorID, step)
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(300))
            guard shell.tour == step, anchors == nil || anchors?.owner(of: step) == id else { return }
            shell.endTour()
        }
    }
}

// MARK: - Coach mark

/// Popover content: step counter, title, message and the navigation row.
private struct CoachMarkCard: View {
    @Environment(ShellModel.self) private var shell
    let step: TourStep

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Bước \(step.rawValue + 1)/\(TourStep.allCases.count)")
                    .font(.caption.weight(.semibold).smallCaps())
                    .foregroundStyle(.tint)
                Spacer()
                HStack(spacing: 4) {
                    ForEach(TourStep.allCases) { other in
                        Capsule()
                            .fill(other == step ? AnyShapeStyle(.tint) : AnyShapeStyle(.quaternary))
                            .frame(width: other == step ? 14 : 5, height: 5)
                    }
                }
                .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(step.title).font(.headline)
                Text(step.message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                Button("Bỏ qua") { shell.endTour() }
                    .buttonStyle(.plain)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Spacer()
                if step.previous != nil {
                    Button("Quay lại") { shell.retreatTour() }
                        .buttonStyle(.glass)
                }
                Button(step.next == nil ? "Xong" : "Tiếp") { shell.advanceTour() }
                    .buttonStyle(.glassProminent)
                    .keyboardShortcut(.defaultAction)
            }
            .controlSize(.regular)
            .padding(.top, 2)
        }
        .padding(16)
        .frame(width: 300)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(step.title)
    }
}

// MARK: - Coordinator

/// Keeps the tour moving on the real UI: chat steps get a chat to point at ("Cloud của tôi" when nothing is open, since
/// opening it never marks anybody else's messages read) and a step whose anchor does not show up is skipped instead of
/// stalling. Also routes → / Return (next), ← (back) and Esc (end) while a step is showing, even from the composer.
private struct TourCoordinator: ViewModifier {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell
    @State private var anchors = TourAnchorRegistry()
    @State private var keys = TourKeyMonitor()
    @State private var lastStep: TourStep?

    /// How long an anchor may take to appear (polled every 100 ms).
    private static let anchorPatience = 15

    func body(content: Content) -> some View {
        content
            .environment(\.tourAnchors, anchors)
            .task(id: shell.tour) { await prepare(shell.tour) }
            .background(HostWindowReader { keys.window = $0 })
            .onChange(of: shell.tour != nil, initial: true) { _, running in
                if running { keys.start(shell) } else { keys.stop() }
            }
            .onDisappear {
                keys.stop()
                shell.endTour()
            }
    }

    private func prepare(_ step: TourStep?) async {
        guard let step else { lastStep = nil; return }
        let goingBack = lastStep.map { step.rawValue < $0.rawValue } ?? false
        lastStep = step
        for _ in 0..<Self.anchorPatience {
            if step.needsOpenChat { openChatIfNeeded() }
            if anchors.isOnScreen(step) { return }
            try? await Task.sleep(for: .milliseconds(100))
            if Task.isCancelled { return }
        }
        guard shell.tour == step else { return }
        if goingBack {
            if step.previous == nil { shell.endTour() } else { shell.retreatTour() }
        } else {
            shell.advanceTour()
        }
    }

    private func openChatIfNeeded() {
        if shell.selection == nil {
            guard let id = engine.selfConversationId else { return }
            shell.destination = .inbox
            shell.search = ""
            shell.selection = id
        } else if !shell.destination.isConversationList || shell.destination == .hidden {
            shell.destination = .inbox
        }
    }
}

/// Reports the window its (invisible) view ends up in.
struct HostWindowReader: NSViewRepresentable {
    let onWindow: @MainActor (NSWindow?) -> Void

    func makeNSView(context: Context) -> NSView { Probe(onWindow: onWindow) }
    func updateNSView(_ view: NSView, context: Context) {}

    private final class Probe: NSView {
        let onWindow: @MainActor (NSWindow?) -> Void

        init(onWindow: @escaping @MainActor (NSWindow?) -> Void) {
            self.onWindow = onWindow
            super.init(frame: .zero)
        }

        required init?(coder: NSCoder) { nil }

        override func viewDidMoveToWindow() { onWindow(window) }
    }
}

/// Window-level key handling for the tour. A local monitor, because the key view may be the composer or a popover.
@MainActor
private final class TourKeyMonitor {
    private var token: Any?
    /// The main shell window the tour runs in; keys typed anywhere else (Settings) are none of the tour's business.
    weak var window: NSWindow?

    func start(_ shell: ShellModel) {
        guard token == nil else { return }
        token = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            let consumed = MainActor.assumeIsolated { self?.handle(event, shell: shell) ?? false }
            return consumed ? nil : event
        }
    }

    func stop() {
        if let token { NSEvent.removeMonitor(token) }
        token = nil
    }

    /// True when the tour consumed the key.
    private func handle(_ event: NSEvent, shell: ShellModel) -> Bool {
        // The shell window or its coach-mark popover (borderless), not Settings, a sheet or the debug renderer.
        guard shell.tour != nil, !shell.showQuickSwitcher, let home = window, let key = NSApp.keyWindow, key.sheetParent == nil,
              key === home || !key.styleMask.contains(.titled)
        else { return false }
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard flags.isDisjoint(with: [.command, .option, .control, .shift]) else { return false }
        if event.keyCode == 53 {   // Esc
            shell.endTour()
            return true
        }
        // Not even a draft in the composer keeps these keys: Return must never reach it and send the message mid-tour.
        // (Marked text is still being composed by the input method; its Return only commits and sends nothing.)
        if let text = key.firstResponder as? NSTextView, text.hasMarkedText() { return false }
        switch event.specialKey {
        case .rightArrow, .carriageReturn, .enter:
            shell.advanceTour()
            return true
        case .leftArrow:
            shell.retreatTour()
            return true
        default:
            return false
        }
    }
}
