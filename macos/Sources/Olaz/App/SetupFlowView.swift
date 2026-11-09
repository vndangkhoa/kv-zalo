import SwiftUI

/// The three first-run steps. `signIn` is derived from the engine phase; `sync` and `welcome` only exist while the
/// session is ready and the setup is still pending.
enum SetupStage: Int, CaseIterable {
    case signIn, sync, welcome
}

/// How step 2 ended, for the stepper and the welcome copy.
enum SetupSyncOutcome {
    case done, skipped
}

/// First-run setup inside the main window: sign-in (QR) → sync history from the phone → welcome. Shows one step at a
/// time in a `SetupCard`; `onFinish(startTour)` hands over to the main window.
struct SetupFlowView: View {
    @Environment(ZaloEngine.self) private var engine
    @State private var afterSignIn: SetupStage = .sync
    @State private var syncOutcome: SetupSyncOutcome?
    let onFinish: (_ startTour: Bool) -> Void

    private var stage: SetupStage {
        if case .ready = engine.phase { afterSignIn } else { .signIn }
    }

    /// Identity of the visible step; changes animate, QR refreshes within a step do not.
    private var contentKey: String {
        switch engine.phase {
        case .login: "login"
        case .ready: afterSignIn == .welcome ? "welcome" : "sync"
        default: "signingIn"
        }
    }

    var body: some View {
        SetupCard(stage: stage, syncOutcome: syncOutcome, contentKey: contentKey) {
            switch engine.phase {
            case .login(let state):
                LoginView(state: state)
            case .ready:
                if afterSignIn == .welcome {
                    SetupWelcomeStep(syncSkipped: syncOutcome == .skipped, onFinish: onFinish)
                } else {
                    SetupSyncStep { outcome in
                        syncOutcome = outcome
                        afterSignIn = .welcome
                    }
                }
            default:
                SigningInView()
            }
        }
        // Logging out (or being signed out) mid-setup starts over at the QR code.
        .onChange(of: engine.phase.isLogin) { _, isLogin in
            if isLogin {
                afterSignIn = .sync
                syncOutcome = nil
            }
        }
    }
}

// MARK: - Card

/// Setup-Assistant style card on the brand backdrop: app icon, stepper and the honest footnote on the left, the
/// current step on the right. Sized to the window (the main window is at least 960 × 620).
struct SetupCard<Content: View>: View {
    let stage: SetupStage
    let syncOutcome: SetupSyncOutcome?
    let contentKey: String
    @ViewBuilder var content: Content

    private let sidebarWidth: CGFloat = 276
    private let corner: CGFloat = 30

    var body: some View {
        GeometryReader { geometry in
            let width = min(880, max(760, geometry.size.width - 96))
            let height = min(600, max(500, geometry.size.height - 110))
            HStack(spacing: 0) {
                SetupSidebar(stage: stage, syncOutcome: syncOutcome)
                    .frame(width: sidebarWidth)
                ZStack {
                    content
                        .id(contentKey)
                        .transition(.blurReplace)
                }
                .animation(.smooth(duration: 0.4), value: contentKey)
                .padding(.horizontal, 36)
                .padding(.vertical, 22)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(.background, in: RoundedRectangle(cornerRadius: corner - 8, style: .continuous))
                .overlay { RoundedRectangle(cornerRadius: corner - 8, style: .continuous).strokeBorder(.separator.opacity(0.5)) }
                .padding([.top, .trailing, .bottom], 8)
            }
            .frame(width: width, height: height)
            .glassEffect(.regular, in: RoundedRectangle(cornerRadius: corner, style: .continuous))
            .shadow(color: .black.opacity(0.18), radius: 30, y: 12)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(BrandBackground())
    }
}

// MARK: - Sidebar

private struct SetupSidebar: View {
    let stage: SetupStage
    let syncOutcome: SetupSyncOutcome?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let icon = OlazAppIcon.c.image {
                Image(nsImage: icon)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 64, height: 64)
                    .accessibilityHidden(true)
            }
            Text("Chào mừng đến \(AppBrand.name)")
                .font(.system(size: 21, weight: .bold))
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 12)
            SetupStepper(stage: stage, syncOutcome: syncOutcome)
                .padding(.top, 26)
            Spacer(minLength: 12)
            Label {
                Text("\(AppBrand.name) là ứng dụng độc lập, không liên kết với VNG hay Zalo. Ứng dụng chạy phần lõi chính thức của Zalo PC, tải trực tiếp từ VNG; tin nhắn đi thẳng tới máy chủ Zalo.")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } icon: {
                Image(systemName: "lock.shield.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.green)
            }
            .labelStyle(TopAlignedLabelStyle())
        }
        .padding(.horizontal, 26)
        .padding(.top, 30)
        .padding(.bottom, 22)
        .frame(maxHeight: .infinity, alignment: .topLeading)
    }
}

/// Icon beside a multi-line text, aligned to the first line.
private struct TopAlignedLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 7) {
            configuration.icon
            configuration.title
        }
    }
}

// MARK: - Stepper

private struct SetupStepper: View {
    let stage: SetupStage
    let syncOutcome: SetupSyncOutcome?

    private enum StepState: Equatable {
        case done, current, upcoming, skipped

        var accessibilityText: String {
            switch self {
            case .done: "đã xong"
            case .current: "đang thực hiện"
            case .upcoming: "sắp tới"
            case .skipped: "đã để sau"
            }
        }
    }

    private struct Item {
        let stage: SetupStage
        let title: String
        let subtitle: String
    }

    private var items: [Item] {
        [
            Item(stage: .signIn, title: "Đăng nhập", subtitle: "Quét mã QR bằng Zalo"),
            Item(stage: .sync, title: "Đồng bộ tin nhắn", subtitle: syncOutcome == .skipped ? "Đã để sau" : "Từ điện thoại sang Mac"),
            Item(stage: .welcome, title: "Làm quen với \(AppBrand.name)", subtitle: "Hướng dẫn nhanh, 1 phút"),
        ]
    }

    private func state(of item: SetupStage) -> StepState {
        if item == stage { return .current }
        if item.rawValue > stage.rawValue { return .upcoming }
        return item == .sync && syncOutcome == .skipped ? .skipped : .done
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(items, id: \.stage) { item in
                let itemState = state(of: item.stage)
                HStack(alignment: .top, spacing: 12) {
                    Dot(number: item.stage.rawValue + 1, state: itemState)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(item.title)
                            .font(.system(size: 13, weight: itemState == .upcoming ? .medium : .semibold))
                            .foregroundStyle(itemState == .upcoming || itemState == .skipped ? .secondary : .primary)
                        Text(item.subtitle)
                            .font(.system(size: 11.5))
                            .foregroundStyle(.secondary)
                    }
                    .frame(minHeight: 24, alignment: .leading)
                    .padding(.bottom, item.stage == .welcome ? 0 : 22)
                }
                .background(alignment: .topLeading) {
                    if item.stage != .welcome {
                        Rectangle()
                            .fill(itemState == .done ? AnyShapeStyle(.tint) : AnyShapeStyle(.quaternary))
                            .frame(width: 2)
                            .padding(.top, 28)
                            .padding(.leading, 11)
                    }
                }
                .accessibilityElement(children: .combine)
                .accessibilityValue(itemState.accessibilityText)
            }
        }
        .animation(.smooth(duration: 0.35), value: stage)
        .animation(.smooth(duration: 0.35), value: syncOutcome)
    }

    private struct Dot: View {
        let number: Int
        let state: StepState

        var body: some View {
            ZStack {
                switch state {
                case .done:
                    Circle().fill(.tint)
                    Image(systemName: "checkmark").font(.system(size: 10.5, weight: .heavy)).foregroundStyle(.white)
                case .current:
                    Circle().fill(.tint.opacity(0.22)).padding(-4)
                    Circle().fill(.tint)
                    Text("\(number)").font(.system(size: 11.5, weight: .bold)).foregroundStyle(.white)
                case .upcoming:
                    Circle().fill(.quaternary)
                    Text("\(number)").font(.system(size: 11.5, weight: .bold)).foregroundStyle(.secondary)
                case .skipped:
                    Circle().fill(.quaternary)
                    Image(systemName: "minus").font(.system(size: 10.5, weight: .heavy)).foregroundStyle(.secondary)
                }
            }
            .frame(width: 24, height: 24)
        }
    }
}
