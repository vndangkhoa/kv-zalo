import SwiftUI

/// Native Liquid Glass UI for the phone → PC message transfer ("Đồng bộ tin nhắn").
/// The flow itself (offer, phone confirmation, progress, result) is driven by the official
/// Zalo client (`ZaloEngine.transfer`, see `transfer-adapter.js`); this view only renders it.
///
/// Drop onto the main window: `.overlay { TransferOverlay() }` — renders nothing while
/// `engine.transfer == nil`, so it never intercepts clicks in that case.
struct TransferOverlay: View {
    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        ZStack {
            if let state = engine.transfer {
                Color.black.opacity(0.32)
                    .ignoresSafeArea()
                    .contentShape(Rectangle())
                    .onTapGesture {}   // modal: swallow clicks on the dimmed window
                    .transition(.opacity)
                TransferCard(state: state)
                    .padding(24)
                    .transition(.scale(scale: 0.92).combined(with: .opacity))
            }
        }
        .animation(.smooth(duration: 0.4), value: engine.transfer != nil)
    }
}

// MARK: - Card

private struct TransferCard: View {
    @Environment(ZaloEngine.self) private var engine
    @FocusState private var cardFocused: Bool
    let state: MessageTransferState

    private enum Kind: Hashable { case offer, waiting, transferring, finished, failed }

    private var kind: Kind {
        switch state.phase {
        case .offer: .offer
        case .waitingForPhone: .waiting
        case .transferring: .transferring
        case .finished: .finished
        case .failed: .failed
        }
    }

    var body: some View {
        GlassEffectContainer {
            VStack(spacing: 18) {
                Group {
                    switch state.phase {
                    case .offer: offer
                    case .waitingForPhone: waiting
                    case .transferring: transferring
                    case .finished: finished
                    case .failed(let reason): failed(reason)
                    }
                }
                .id(kind)
                .transition(.blurReplace)
            }
            .padding(30)
            .frame(width: 420)
            .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 28, style: .continuous))
        }
        .shadow(color: .black.opacity(0.18), radius: 30, y: 12)
        .animation(.smooth(duration: 0.35), value: kind)
        // Modal keyboard focus: take it from whatever sits under the overlay (e.g. the chat composer),
        // so typing never reaches the content behind the dim layer. Esc = "Để sau" / close.
        .focusable()
        .focusEffectDisabled()
        .focused($cardFocused)
        .onAppear { cardFocused = true }
        .onChange(of: kind) { cardFocused = true }
        .onKeyPress(.escape) {
            if kind == .offer { engine.respondToTransfer(accept: false) } else { engine.dismissTransfer() }
            return .handled
        }
        .task(id: kind) {
            // "Hoàn tất" closes itself after a few seconds.
            guard kind == .finished else { return }
            try? await Task.sleep(for: .seconds(4))
            guard !Task.isCancelled else { return }
            engine.dismissTransfer()
        }
    }

    // MARK: Offer

    private var offer: some View {
        VStack(spacing: 18) {
            TransferRoute(animated: false)
            Text("Đồng bộ tin nhắn từ điện thoại?")
                .font(.system(size: 20, weight: .bold))
                .multilineTextAlignment(.center)
            VStack(spacing: 8) {
                Text(state.officialDescription ?? state.message
                     ?? "Để đảm bảo tính bảo mật, Zalo chỉ hiển thị tin nhắn từ sau lần đăng nhập đầu tiên trên thiết bị này.")
                    .font(.system(size: 13.5))
                    .foregroundStyle(.secondary)
                Text(state.officialSuggestion ?? "Đồng bộ từ điện thoại để xem đầy đủ nội dung các cuộc trò chuyện.")
                    .font(.system(size: 13.5))
                    .foregroundStyle(.secondary)
            }
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)

            VStack(spacing: 10) {
                Button { engine.respondToTransfer(accept: true) } label: {
                    Text("Đồng bộ ngay").frame(maxWidth: .infinity).padding(.vertical, 6)
                }
                .buttonStyle(.glassProminent)
                .controlSize(.large)

                Button { engine.respondToTransfer(accept: false) } label: {
                    Text("Để sau").frame(maxWidth: .infinity).padding(.vertical, 6)
                }
                .buttonStyle(.glass)
                .controlSize(.large)
            }
            .padding(.top, 4)
        }
    }

    // MARK: Waiting for the phone

    private var waiting: some View {
        VStack(spacing: 18) {
            TransferRoute(animated: true)
            Text("Mở Zalo trên điện thoại để xác nhận")
                .font(.system(size: 20, weight: .bold))
                .multilineTextAlignment(.center)
            Text(state.waitingDetail)
                .font(.system(size: 13.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Đang chờ xác nhận từ điện thoại…")
                    .font(.system(size: 12.5, weight: .medium))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 14).padding(.vertical, 8)
            .background(.primary.opacity(0.07), in: Capsule())
            secondaryActions
        }
    }

    // MARK: Transferring

    private var transferring: some View {
        VStack(spacing: 18) {
            ProgressRing(progress: state.progress)
                .frame(width: 112, height: 112)
            Text("Đang đồng bộ tin nhắn")
                .font(.system(size: 20, weight: .bold))
            VStack(spacing: 6) {
                Text(transferDetail)
                    .font(.system(size: 13.5, weight: .medium))
                    .foregroundStyle(.primary.opacity(0.85))
                    .contentTransition(.numericText())
                if let index = state.stageIndex, let count = state.stageCount {
                    Text("Bước \(index)/\(count) · \(state.stageTitle)")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }
                Text("Giữ Zalo mở trên điện thoại cho đến khi hoàn tất.")
                    .font(.system(size: 12))
                    .foregroundStyle(.tertiary)
                    .padding(.top, 2)
            }
            .multilineTextAlignment(.center)
            .animation(.smooth, value: transferDetail)
            secondaryActions
        }
    }

    private var transferDetail: String {
        if let received = state.receivedMessages, received > 0 { return "Đã nhận \(received.formatted()) tin nhắn" }
        if let total = state.totalConversations, total > 0 {
            return "Đã đồng bộ \(state.syncedConversations)/\(total) hội thoại"
        }
        if state.syncedConversations > 0 { return "Đã đồng bộ \(state.syncedConversations) hội thoại" }
        return state.stageTitle
    }

    /// "Ẩn" keeps the sync running in the background; "Hủy" stops it.
    private var secondaryActions: some View {
        HStack(spacing: 10) {
            Button { engine.dismissTransfer() } label: {
                Text("Ẩn").frame(maxWidth: .infinity).padding(.vertical, 4)
            }
            .buttonStyle(.glass)
            Button(role: .destructive) { engine.respondToTransfer(accept: false) } label: {
                Text("Hủy đồng bộ").frame(maxWidth: .infinity).padding(.vertical, 4)
            }
            .buttonStyle(.glass)
        }
        .controlSize(.large)
        .padding(.top, 4)
    }

    // MARK: Finished

    private var finished: some View {
        VStack(spacing: 16) {
            ZStack {
                Circle().fill(.tint.opacity(0.14)).frame(width: 96, height: 96)
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 58))
                    .foregroundStyle(.tint)
                    .symbolEffect(.bounce, options: .nonRepeating)
            }
            Text("Hoàn tất")
                .font(.system(size: 22, weight: .bold))
            Text("Tin nhắn từ điện thoại đã được đồng bộ sang máy tính.")
                .font(.system(size: 13.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button { engine.dismissTransfer() } label: {
                Text("Xong").frame(maxWidth: .infinity).padding(.vertical, 6)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .keyboardShortcut(.defaultAction)
            .padding(.top, 4)
        }
    }

    // MARK: Failed

    private func failed(_ reason: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 34))
                .foregroundStyle(.orange)
                .frame(width: 88, height: 88)
                .background(.orange.opacity(0.15), in: Circle())
            Text("Đồng bộ không thành công")
                .font(.system(size: 20, weight: .bold))
            Text(state.message ?? reason)
                .font(.system(size: 13.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            VStack(spacing: 10) {
                if state.canRetry {
                    Button { engine.respondToTransfer(accept: true) } label: {
                        Text("Thử lại").frame(maxWidth: .infinity).padding(.vertical, 6)
                    }
                    .buttonStyle(.glassProminent)
                    // No Return default: a retry pings the phone, so it takes a deliberate click.
                }
                Button { engine.dismissTransfer() } label: {
                    Text("Đóng").frame(maxWidth: .infinity).padding(.vertical, 6)
                }
                .buttonStyle(.glass)
            }
            .controlSize(.large)
            .padding(.top, 4)
        }
    }
}

// MARK: - Phone → computer illustration

/// iPhone ⟶ laptop. With `animated`, a pulse radiates from the phone and chevrons stream towards the computer.
/// Also used by the first-run setup's sync step.
struct TransferRoute: View {
    let animated: Bool

    var body: some View {
        HStack(spacing: 14) {
            device("iphone", pulsing: animated)
            chevrons
            device("laptopcomputer", pulsing: false)
        }
        .frame(height: 92)
    }

    private func device(_ symbol: String, pulsing: Bool) -> some View {
        ZStack {
            if pulsing { PulseRing() }
            Image(systemName: symbol)
                .font(.system(size: 34, weight: .regular))
                .foregroundStyle(.tint)
                .frame(width: 76, height: 76)
                .background(.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
        .frame(width: 92, height: 92)
    }

    @ViewBuilder
    private var chevrons: some View {
        if animated {
            TimelineView(.animation) { context in
                let t = context.date.timeIntervalSinceReferenceDate
                HStack(spacing: 2) {
                    ForEach(0..<3, id: \.self) { i in
                        let phase = (t * 1.4 - Double(i) * 0.28).truncatingRemainder(dividingBy: 1)
                        let wave = max(0, sin(phase < 0 ? (phase + 1) * .pi : phase * .pi))
                        Image(systemName: "chevron.right")
                            .font(.system(size: 17, weight: .bold))
                            .foregroundStyle(.tint)
                            .opacity(0.2 + 0.8 * wave)
                            .offset(x: 3 * wave)
                    }
                }
            }
        } else {
            Image(systemName: "arrow.right")
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(.tint.opacity(0.8))
        }
    }
}

private struct PulseRing: View {
    var body: some View {
        TimelineView(.animation) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            ZStack {
                ForEach(0..<2, id: \.self) { i in
                    let p = (t * 0.7 + Double(i) * 0.5).truncatingRemainder(dividingBy: 1)
                    RoundedRectangle(cornerRadius: 22 + 10 * p, style: .continuous)
                        .stroke(.tint.opacity(0.55 * (1 - p)), lineWidth: 2)
                        .frame(width: 76 + 30 * p, height: 76 + 30 * p)
                }
            }
        }
        .allowsHitTesting(false)
    }
}

// MARK: - Progress ring

/// Determinate ring with the percentage; a spinning arc while `progress` is unknown.
private struct ProgressRing: View {
    let progress: Double?

    var body: some View {
        ZStack {
            Circle().stroke(.primary.opacity(0.10), lineWidth: 9)
            if let progress {
                Circle()
                    .trim(from: 0, to: max(0.012, progress))
                    .stroke(.tint, style: StrokeStyle(lineWidth: 9, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .animation(.smooth(duration: 0.6), value: progress)
                Text("\(Int((progress * 100).rounded()))%")
                    .font(.system(size: 24, weight: .bold, design: .rounded))
                    .monospacedDigit()
                    .contentTransition(.numericText(value: progress))
                    .animation(.smooth, value: progress)
            } else {
                TimelineView(.animation) { context in
                    let t = context.date.timeIntervalSinceReferenceDate
                    Circle()
                        .trim(from: 0, to: 0.28)
                        .stroke(.tint, style: StrokeStyle(lineWidth: 9, lineCap: .round))
                        .rotationEffect(.degrees(t.truncatingRemainder(dividingBy: 1.1) / 1.1 * 360 - 90))
                }
                Image(systemName: "arrow.triangle.2.circlepath")
                    .font(.system(size: 26, weight: .medium))
                    .foregroundStyle(.tint)
                    .symbolEffect(.pulse)
            }
        }
        .padding(5)
        .background(.primary.opacity(0.05), in: Circle())
    }
}

// MARK: - Shared copy

extension MessageTransferState {
    /// Instruction while the phone has not confirmed yet, by official stage.
    var waitingDetail: String {
        switch stage {
        case "confirmMobile":
            "Yêu cầu đồng bộ đã được gửi. Hãy chấp nhận yêu cầu trong thông báo của Zalo trên điện thoại."
        case "waitForBackup":
            "Điện thoại đang chuẩn bị dữ liệu tin nhắn. Hãy giữ Zalo mở trên điện thoại."
        default:
            "Mở ứng dụng Zalo trên điện thoại, sau đó chọn xác nhận đồng bộ tin nhắn sang máy tính."
        }
    }

    /// What the running official stage is doing.
    var stageTitle: String {
        switch stage {
        case "conversationList": "Đang tải danh sách hội thoại"
        case "messages": "Đang đồng bộ tin nhắn gần đây"
        case "otherMessages": "Đang đồng bộ các tin nhắn cũ hơn"
        case "downloading": "Đang tải dữ liệu từ điện thoại"
        case "decrypting": "Đang giải mã dữ liệu"
        case "restoring": "Đang khôi phục tin nhắn"
        default: "Đang đồng bộ"
        }
    }
}
