import SwiftUI
import CoreImage.CIFilterBuiltins

/// Recreation of `login.html`: "Quét mã QR để đăng nhập".
/// Flow strings mirror STR_LOGIN_QR_PAGE_* in the language pack.
struct LoginView: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        ZStack {
            background
            HStack(spacing: 0) {
                marketingPane
                Divider().opacity(0.15)
                qrPane
            }
        }
        .frame(minWidth: 980, minHeight: 620)
        .task {
            if store.isLoggedOut { await store.beginQrLogin() }
        }
    }

    private var background: some View {
        ZStack {
            Color(.windowBackgroundColor)
            Circle().fill(ZaloTheme.blue.opacity(0.16)).blur(radius: 90).frame(width: 560, height: 560).offset(x: -260, y: -180)
            Circle().fill(Color.cyan.opacity(0.12)).blur(radius: 110).frame(width: 480, height: 480).offset(x: 300, y: 220)
            Circle().fill(Color.purple.opacity(0.10)).blur(radius: 100).frame(width: 420, height: 420).offset(x: 120, y: -260)
        }
        .ignoresSafeArea()
    }

    // MARK: Left marketing pane (login.html rotating slides)

    private var marketingPane: some View {
        VStack(alignment: .leading, spacing: 28) {
            ZaloWordmark(size: 40)
            Spacer()
            VStack(alignment: .leading, spacing: 26) {
                LoginFeatureRow(
                    icon: "hand.raised.fill",
                    text1: "Chỉ cần kéo thả để gửi tin, hình ảnh, video",
                    text2: "cho đồng nghiệp. Nhận tập tin nhanh hơn gấp 5 lần"
                )
                LoginFeatureRow(
                    icon: "person.3.fill",
                    text1: "Cùng thảo luận công việc với đồng nghiệp",
                    text2: "hoặc “tám” với bạn bè nhanh chóng dễ dàng"
                )
                LoginFeatureRow(
                    icon: "camera.metering.center.weighted",
                    text1: "Chụp và chia sẻ màn hình chỉ với",
                    text2: "một click chuột hoặc phím tắt"
                )
            }
            Spacer()
            Text("Zalo — Nhắn gửi yêu thương 💙")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(.secondary)
        }
        .padding(56)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: Right QR pane — the Liquid Glass centerpiece

    private var qrPane: some View {
        VStack(spacing: 22) {
            GlassEffectContainer(spacing: 24) {
                VStack(spacing: 18) {
                    ZaloWordmark(size: 22)

                    Text("Đăng nhập bản demo")
                        .font(.system(size: 17, weight: .bold))

                    Group {
                        switch store.serviceState {
                        case .qrWaiting(let payload):
                            QRCodeView(payload: payload)
                                .frame(width: 232, height: 232)
                        case .qrScanned(let device):
                            scannedState(device: device)
                        default:
                            ProgressView().frame(width: 232, height: 232)
                        }
                    }
                    .glassEffect(in: RoundedRectangle(cornerRadius: 24, style: .continuous))

                    if case .qrWaiting = store.serviceState {
                        Button {
                            store.simulateScan(deviceName: "iPhone của Bạn")
                        } label: {
                            Label("Mô phỏng: quét mã bằng điện thoại", systemImage: "iphone.gen3")
                        }
                        .buttonStyle(.glass)
                        .controlSize(.large)
                    }

                    Text("Chỉ mô phỏng cục bộ. Mã QR này không đăng nhập tài khoản Zalo thật.")
                        .multilineTextAlignment(.center)
                        .font(.system(size: 12.5))
                        .foregroundStyle(.secondary)
                }
                .padding(28)
                .frame(width: 380)
            }
        }
        .padding(48)
        .frame(maxWidth: .infinity)
    }

    private func scannedState(device: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 52))
                .foregroundStyle(ZaloTheme.blue)
            Text("Quét mã đăng nhập thành công")
                .font(.system(size: 15, weight: .semibold))
                .multilineTextAlignment(.center)
            Text("Chọn “Vào bản demo” để xem dữ liệu mẫu")
                .font(.system(size: 12.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button {
                Task { await store.simulateConfirm() }
            } label: {
                Text("Vào bản demo")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            Text("Chờ xác nhận từ \(device)…")
                .font(.system(size: 11))
                .foregroundStyle(.tertiary)
        }
        .frame(width: 232, height: 232)
    }
}

private struct LoginFeatureRow: View {
    let icon: String
    let text1: String
    let text2: String

    var body: some View {
        HStack(alignment: .top, spacing: 18) {
            Image(systemName: icon)
                .font(.system(size: 22))
                .foregroundStyle(ZaloTheme.blue)
                .frame(width: 44, height: 44)
                .background(
                    Circle().fill(ZaloTheme.blue.opacity(0.1))
                )
            VStack(alignment: .leading, spacing: 3) {
                Text(text1).font(.system(size: 14, weight: .medium))
                Text(text2).font(.system(size: 14, weight: .medium))
                .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - QR generator (CoreImage)

struct QRCodeView: View {
    let payload: String
    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image)
                    .interpolation(.none)
                    .resizable()
                    .scaledToFit()
                    .padding(18)
                    .background(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            } else {
                ProgressView()
            }
        }
        .task(id: payload) {
            let filter = CIFilter.qrCodeGenerator()
            filter.message = Data(payload.utf8)
            filter.correctionLevel = "M"
            if let output = filter.outputImage {
                let scaled = output.transformed(
                    by: CGAffineTransform(scaleX: 10, y: 10)
                )
                let rep = NSCIImageRep(ciImage: scaled)
                let nsImage = NSImage(size: rep.size)
                nsImage.addRepresentation(rep)
                image = nsImage
            }
        }
    }
}
