import SwiftUI

/// Setup step 1: native QR login. The QR, scan detection and confirmation are produced by the official Zalo client
/// running underneath (`ZaloEngine`); this view only renders them. Copy follows the official language pack
/// (STR_LOGIN_QR_PAGE_*).
struct LoginView: View {
    @Environment(ZaloEngine.self) private var engine
    let state: ZaloEngine.LoginState

    private var isConfirming: Bool {
        if case .waitingConfirm = state.step { return true }
        return false
    }

    /// Smaller while the sign-out notice takes room, so the step still fits the card at the minimum window size.
    private var qrSize: CGFloat { engine.signOutNotice == nil ? 216 : 168 }

    var body: some View {
        VStack(spacing: 14) {
            if let notice = engine.signOutNotice {
                Label(notice, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.orange)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
                    .background(.orange.opacity(0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .transition(.blurReplace)
            }
            VStack(spacing: 6) {
                Text(heading)
                    .font(.system(size: 22, weight: .bold))
                Text(subtitle)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 380)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .contentTransition(.opacity)

            qrArea
                .frame(width: qrSize, height: qrSize)

            footer
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(.smooth(duration: 0.3), value: state.step)
        .animation(.smooth(duration: 0.3), value: engine.signOutNotice)
    }

    private var heading: String {
        isConfirming ? "Xác nhận trên điện thoại" : "Đăng nhập bằng Zalo"
    }

    private var subtitle: String {
        switch state.step {
        case .generating: "Đang tạo mã QR…"
        case .waitingScan where state.isSecondQR: "Đã quét lần 1. Tài khoản của bạn cần quét thêm mã mới dưới đây để xác nhận thiết bị."
        case .waitingScan: "Mở Zalo trên điện thoại, chạm biểu tượng quét mã QR rồi quét mã dưới đây."
        case .waitingConfirm: "Chọn Đăng nhập trên điện thoại của bạn để tiếp tục."
        case .expired: "Mã QR đã hết hạn."
        case .failed(let message): message
        }
    }

    // MARK: QR / confirmation

    @ViewBuilder
    private var qrArea: some View {
        switch state.step {
        case .waitingConfirm(let name, let avatar):
            VStack(spacing: 12) {
                RemoteAvatar(url: avatar, name: name, size: 84)
                Text(name)
                    .font(.system(size: 17, weight: .semibold))
                    .lineLimit(2)
                    .multilineTextAlignment(.center)
                Label("Đã quét mã", systemImage: "checkmark.circle.fill")
                    .font(.system(size: 12.5, weight: .medium))
                    .foregroundStyle(.tint)
            }
            .padding(16)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 24, style: .continuous))
            .transition(.blurReplace)
        case .failed:
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 34))
                    .foregroundStyle(.orange)
                Text("Chưa lấy được mã QR")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 24, style: .continuous))
            .transition(.blurReplace)
        default:
            ZStack {
                RoundedRectangle(cornerRadius: 24, style: .continuous)
                    .fill(.white)
                    .shadow(color: .black.opacity(0.12), radius: 14, y: 6)
                if let png = state.qrPNG, let image = NSImage(data: png) {
                    Image(nsImage: image)
                        .interpolation(.none)
                        .resizable()
                        .scaledToFit()
                        .padding(16)
                        .blur(radius: state.step == .expired ? 6 : 0)
                        .opacity(state.step == .expired ? 0.4 : 1)
                        .accessibilityLabel("Mã QR đăng nhập Zalo")
                } else {
                    ProgressView().colorScheme(.light)
                }
                if state.step == .expired {
                    Button {
                        engine.refreshQR()
                    } label: {
                        Label("Lấy mã mới", systemImage: "arrow.clockwise")
                    }
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                }
            }
            .transition(.blurReplace)
        }
    }

    // MARK: Below the QR

    @ViewBuilder
    private var footer: some View {
        switch state.step {
        case .waitingConfirm:
            VStack(spacing: 8) {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Đang chờ xác nhận trên điện thoại…")
                        .font(.system(size: 12.5))
                        .foregroundStyle(.secondary)
                }
                Button("Dùng mã QR khác") { engine.refreshQR() }
                    .buttonStyle(.link)
                    .font(.system(size: 12.5))
            }
        case .failed:
            Button("Thử lại") { engine.refreshQR() }
                .buttonStyle(.glassProminent)
                .controlSize(.large)
        default:
            VStack(alignment: .leading, spacing: 7) {
                InstructionRow(number: 1, text: "Mở Zalo trên điện thoại")
                InstructionRow(number: 2, text: "Nhấn biểu tượng QR ở thanh tìm kiếm")
                InstructionRow(number: 3, text: "Quét mã và xác nhận đăng nhập")
            }
        }
    }
}

private struct InstructionRow: View {
    let number: Int
    let text: String

    var body: some View {
        HStack(spacing: 10) {
            Text("\(number)")
                .font(.system(size: 11, weight: .bold, design: .rounded))
                .foregroundStyle(.white)
                .frame(width: 20, height: 20)
                .background(Circle().fill(.tint))
            Text(text)
                .font(.system(size: 12.5))
                .foregroundStyle(.secondary)
        }
    }
}

/// Setup step 1 after the QR was confirmed: the official client is logging in and loading.
struct SigningInView: View {
    var body: some View {
        VStack(spacing: 14) {
            ProgressView().controlSize(.large)
            Text("Đang đăng nhập…")
                .font(.system(size: 22, weight: .bold))
            Text("Đang tải dữ liệu từ Zalo. Việc này có thể mất một lúc.")
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
