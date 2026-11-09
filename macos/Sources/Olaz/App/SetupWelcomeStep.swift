import SwiftUI

/// Setup step 3: Olaz is ready. Lists what the quick tour covers; either button finishes the setup, the first one
/// also starts the tour in the main window.
struct SetupWelcomeStep: View {
    @Environment(ZaloEngine.self) private var engine
    let syncSkipped: Bool
    let onFinish: (_ startTour: Bool) -> Void

    private var lead: String {
        let tour = "Xem nhanh \(TourStep.allCases.count) điều giúp bạn dùng \(AppBrand.name) thuận tay nhất, chưa tới một phút."
        return syncSkipped ? "Bạn có thể đồng bộ tin nhắn cũ bất cứ lúc nào trong Cài đặt. \(tour)" : tour
    }

    var body: some View {
        VStack(spacing: 0) {
            avatar
            Text("\(AppBrand.name) đã sẵn sàng")
                .font(.system(size: 22, weight: .bold))
                .padding(.top, 12)
            Text(lead)
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 6)
            VStack(spacing: 0) {
                ForEach(TourStep.allCases) { step in
                    Label {
                        Text(step.title)
                            .font(.system(size: 13, weight: .medium))
                    } icon: {
                        Image(systemName: step.symbol)
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(.tint)
                            .frame(width: 26, height: 22)
                            .background(.tint.opacity(0.13), in: RoundedRectangle(cornerRadius: 7, style: .continuous))
                    }
                    .frame(maxWidth: .infinity, minHeight: 28, alignment: .leading)
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 6)
            .frame(maxWidth: 340)
            .background(.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .padding(.top, 14)
            VStack(spacing: 10) {
                Button("Bắt đầu hướng dẫn") { onFinish(true) }
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .keyboardShortcut(.defaultAction)
                Button("Bỏ qua, vào \(AppBrand.name)") { onFinish(false) }
                    .buttonStyle(.link)
                    .font(.system(size: 12.5))
            }
            .padding(.top, 16)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// The signed-in account with a check badge; the app icon until the profile arrives.
    @ViewBuilder
    private var avatar: some View {
        if let profile = engine.profile {
            RemoteAvatar(url: profile.avatar, name: profile.displayName, size: 64)
                .overlay(alignment: .bottomTrailing) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 22))
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(.white, .green)
                        .background(Circle().fill(.background).padding(2))
                        .offset(x: 4, y: 4)
                }
                .accessibilityLabel(profile.displayName)
        } else if let icon = OlazAppIcon.c.image {
            Image(nsImage: icon)
                .resizable()
                .scaledToFit()
                .frame(width: 64, height: 64)
                .accessibilityHidden(true)
        }
    }
}
