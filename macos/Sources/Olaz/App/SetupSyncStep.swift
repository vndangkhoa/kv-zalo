import SwiftUI

/// Setup step 2: bring chat history over from the phone. A fresh Mac session only has "Cloud của tôi"; the rest lives
/// on the phone. Sync pings the phone, so it starts only from the "Đồng bộ ngay" button (or "Thử lại") and every
/// later state mirrors the official client's `engine.transfer` with the numbers it provides, nothing invented.
struct SetupSyncStep: View {
    @Environment(ZaloEngine.self) private var engine
    let onContinue: (SetupSyncOutcome) -> Void

    private enum Kind: Hashable { case intro, waiting, transferring, finished, failed }

    private var kind: Kind {
        switch engine.transfer?.phase {
        case nil, .offer?: .intro
        case .waitingForPhone?: .waiting
        case .transferring?: .transferring
        case .finished?: .finished
        case .failed?: .failed
        }
    }

    private var isOffered: Bool { engine.transfer?.phase == .offer }

    var body: some View {
        ZStack {
            current
                .id(kind)
                .transition(.blurReplace)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(.smooth(duration: 0.35), value: kind)
    }

    @ViewBuilder
    private var current: some View {
        if let transfer = engine.transfer {
            switch transfer.phase {
            case .offer: intro
            case .waitingForPhone: waiting(transfer)
            case .transferring: transferring(transfer)
            case .finished: finished(transfer)
            case .failed(let reason): failed(transfer, reason: reason)
            }
        } else {
            intro
        }
    }

    // MARK: Actions

    /// "Đồng bộ ngay": answers the official offer, or starts a manual sync when the client allows it.
    private func startSync() {
        if isOffered {
            engine.respondToTransfer(accept: true)
        } else if engine.canStartTransfer {
            engine.startTransfer()
        }
    }

    private func retry(_ transfer: MessageTransferState) {
        if transfer.canRetry {
            engine.respondToTransfer(accept: true)
        } else if engine.canStartTransfer {
            engine.dismissTransfer()
            engine.startTransfer()
        }
    }

    private func later() {
        switch engine.transfer?.phase {
        case .offer?: engine.respondToTransfer(accept: false)
        case .failed?: engine.dismissTransfer()
        default: break
        }
        onContinue(.skipped)
    }

    // MARK: Intro

    private var canStart: Bool { isOffered || engine.canStartTransfer }

    private var intro: some View {
        VStack(spacing: 0) {
            if let image = OlazAssets.image("illo-sync.png") {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 190)
                    .accessibilityHidden(true)
            }
            Text("Đưa lịch sử trò chuyện sang Mac")
                .font(.system(size: 21, weight: .bold))
                .padding(.top, 10)
            Text("Máy Mac này mới chỉ có “Cloud của tôi”. Lịch sử trò chuyện nằm trên điện thoại của bạn; đồng bộ sẽ gửi yêu cầu tới điện thoại và chờ bạn xác nhận ở đó.")
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 6)
            VStack(alignment: .leading, spacing: 7) {
                Bullet(symbol: "iphone", text: "Chỉ bắt đầu khi bạn chọn Đồng ý trên điện thoại.")
                Bullet(symbol: "clock", text: "Thường mất vài phút, tuỳ số lượng tin nhắn.")
                Bullet(symbol: "wifi", text: "Giữ Zalo mở và có mạng cho đến khi xong.")
            }
            .padding(.top, 14)
            VStack(spacing: 10) {
                Button(action: startSync) {
                    if canStart {
                        Text("Đồng bộ ngay")
                    } else {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("Đang chuẩn bị…")
                        }
                    }
                }
                .buttonStyle(.glassProminent)
                .controlSize(.large)
                .disabled(!canStart)
                if !canStart, let reason = engine.transferStartBlockedReason {
                    Text(ZaloEngine.transferBlockedText(reason))
                        .font(.system(size: 10.5))
                        .foregroundStyle(.tertiary)
                }
                Button("Để sau, chỉ nhận tin nhắn mới", action: later)
                    .buttonStyle(.link)
                    .font(.system(size: 12.5))
            }
            .padding(.top, 18)
        }
    }

    // MARK: Waiting for the phone

    private func waiting(_ transfer: MessageTransferState) -> some View {
        VStack(spacing: 16) {
            TransferRoute(animated: true)
            Text("Mở Zalo trên điện thoại để xác nhận")
                .font(.system(size: 21, weight: .bold))
                .multilineTextAlignment(.center)
            Text(transfer.waitingDetail)
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 380)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Đang chờ xác nhận từ điện thoại…")
                    .font(.system(size: 12.5, weight: .medium))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 14).padding(.vertical, 8)
            .background(.primary.opacity(0.07), in: Capsule())
            Button("Huỷ") { engine.respondToTransfer(accept: false) }
                .buttonStyle(.link)
                .font(.system(size: 12.5))
        }
    }

    // MARK: Transferring

    private func transferring(_ transfer: MessageTransferState) -> some View {
        VStack(spacing: 14) {
            TransferRoute(animated: true)
            Text("Đang nhận tin nhắn…")
                .font(.system(size: 21, weight: .bold))
            Text("Giữ Zalo mở trên điện thoại. \(AppBrand.name) sẽ mở ngay khi đồng bộ xong.")
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 380)
                .fixedSize(horizontal: false, vertical: true)
            VStack(spacing: 6) {
                if let progress = transfer.progress {
                    ProgressView(value: progress)
                        .animation(.smooth(duration: 0.6), value: progress)
                } else {
                    ProgressView()
                        .progressViewStyle(.linear)
                }
                HStack {
                    if let count = Self.countText(transfer) {
                        Text(count)
                            .contentTransition(.numericText())
                    }
                    Spacer()
                    if let progress = transfer.progress {
                        Text("\(Int((progress * 100).rounded()))%")
                            .contentTransition(.numericText(value: progress))
                    }
                }
                .font(.system(size: 12))
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .animation(.smooth, value: transfer.syncedConversations)
                HStack {
                    Text(transfer.stageTitle)
                    if let index = transfer.stageIndex, let count = transfer.stageCount {
                        Text("· Bước \(index)/\(count)")
                    }
                    Spacer()
                }
                .font(.system(size: 11.5))
                .foregroundStyle(.tertiary)
            }
            .frame(maxWidth: 380)
            Button("Huỷ đồng bộ", role: .destructive) { engine.respondToTransfer(accept: false) }
                .buttonStyle(.link)
                .font(.system(size: 12.5))
                .padding(.top, 4)
        }
    }

    /// "Đã nhận X/Y cuộc trò chuyện" with only the numbers the client reported.
    private static func countText(_ transfer: MessageTransferState) -> String? {
        if let received = transfer.receivedMessages, received > 0 {
            return "Đã nhận \(received.formatted()) tin nhắn"
        }
        if let total = transfer.totalConversations, total > 0 {
            return "Đã nhận \(transfer.syncedConversations)/\(total) cuộc trò chuyện"
        }
        if transfer.syncedConversations > 0 { return "Đã nhận \(transfer.syncedConversations) cuộc trò chuyện" }
        return nil
    }

    // MARK: Finished

    private func finished(_ transfer: MessageTransferState) -> some View {
        VStack(spacing: 14) {
            ZStack {
                Circle().fill(.tint.opacity(0.14)).frame(width: 96, height: 96)
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 58))
                    .foregroundStyle(.tint)
                    .symbolEffect(.bounce, options: .nonRepeating)
            }
            Text("Đã đồng bộ xong")
                .font(.system(size: 21, weight: .bold))
            Text(transfer.syncedConversations > 0
                 ? "\(transfer.syncedConversations) cuộc trò chuyện từ điện thoại đã có trên máy Mac này."
                 : "Tin nhắn từ điện thoại đã được đồng bộ sang máy Mac này.")
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 380)
            Button("Tiếp tục") {
                engine.dismissTransfer()
                onContinue(.done)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .keyboardShortcut(.defaultAction)
            .padding(.top, 6)
        }
    }

    // MARK: Failed

    private func failed(_ transfer: MessageTransferState, reason: String) -> some View {
        VStack(spacing: 14) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 34))
                .foregroundStyle(.orange)
                .frame(width: 88, height: 88)
                .background(.orange.opacity(0.15), in: Circle())
            Text("Đồng bộ không thành công")
                .font(.system(size: 21, weight: .bold))
            Text(transfer.message ?? reason)
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 380)
                .fixedSize(horizontal: false, vertical: true)
            VStack(spacing: 10) {
                Button("Thử lại") { retry(transfer) }
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .disabled(!transfer.canRetry && !engine.canStartTransfer)
                Button("Để sau", action: later)
                    .buttonStyle(.link)
                    .font(.system(size: 12.5))
            }
            .padding(.top, 6)
        }
    }
}

private struct Bullet: View {
    let symbol: String
    let text: String

    var body: some View {
        Label {
            Text(text)
                .font(.system(size: 12.5))
                .foregroundStyle(.secondary)
        } icon: {
            Image(systemName: symbol)
                .font(.system(size: 12))
                .foregroundStyle(.tint)
                .frame(width: 18)
        }
    }
}
