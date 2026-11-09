import SwiftUI

/// Content column for zCloud: the storage summary and the zCloud screens (bound to `ShellModel.cloudRoute`).
struct CloudListColumn: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell

    private var cloud: CloudState { engine.cloud }

    var body: some View {
        @Bindable var shell = shell
        List(selection: $shell.cloudRoute) {
            Section {
                CloudStorageCard(plan: cloud.plan, loaded: cloud.loaded, media: cloud.media, myCloud: cloud.myCloudTypes)
            }
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
            .selectionDisabled()
            Section {
                if cloud.hasMediaScreens {
                    row("Tin nhắn media", "photo.on.rectangle.angled", .media)
                    row("Theo trò chuyện", "bubble.left.and.bubble.right.fill", .conversations)
                }
                row("Cloud của tôi", "folder.fill", .myCloud)
            }
            Section {
                row("Thông tin gói", "externaldrive.fill", .plan)
                row("Cài đặt zCloud", "gearshape.fill", .settings)
            }
        }
        .columnTitle("zCloud", subtitle: cloud.plan.map { "\(CloudFormat.size($0.usage + $0.family)) / \(CloudFormat.size($0.quota))" } ?? "")
        .task { engine.refreshCloud() }
        // Free accounts only have "Cloud của tôi": leave the hidden media screens.
        .onChange(of: cloud.hasMediaScreens, initial: true) { _, hasMedia in
            if !hasMedia, shell.cloudRoute == .media || shell.cloudRoute == .conversations { shell.cloudRoute = .myCloud }
        }
    }

    private func row(_ title: String, _ symbol: String, _ route: CloudRoute) -> some View {
        Label {
            Text(title).font(.system(size: 13.5, weight: .medium))
        } icon: {
            Image(systemName: symbol).foregroundStyle(.tint)
        }
        .padding(.vertical, 3)
        .tag(route)
    }
}

/// Detail column for zCloud: the screen picked in `ShellModel.cloudRoute`.
struct CloudDetailColumn: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell

    var body: some View {
        switch shell.cloudRoute ?? .media {
        case .media:
            if engine.cloud.hasMediaScreens { CloudMediaView(scope: .all).id("media") } else { CloudMyCloudScreen().id("my") }
        case .conversations: CloudConversationsView()
        case .myCloud: CloudMyCloudScreen().id("my")
        case .plan: CloudPlanView()
        case .settings: CloudSettingsView()
        }
    }
}
