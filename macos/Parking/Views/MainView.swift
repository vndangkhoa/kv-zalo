import SwiftUI

/// Main window: icon nav rail + list pane + content pane (index.html layout).
struct MainView: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        HStack(spacing: 0) {
            NavRail()
            Divider().opacity(0.4)
            pane(for: store.tab)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Color(.windowBackgroundColor))
    }

    @ViewBuilder
    private func pane(for tab: NavTab) -> some View {
        switch tab {
        case .messages:
            HStack(spacing: 0) {
                ConversationListView()
                Divider().opacity(0.4)
                ChatView()
            }
        case .contacts:
            ContactsView()
        case .calls:
            CallsView()
        case .todo:
            TodoView()
        case .cloud:
            CloudView()
        case .settings:
            SettingsView()
        }
    }
}

/// Left icon rail with a Liquid Glass morphing selection pill.
struct NavRail: View {
    @Environment(AppStore.self) private var store
    @Namespace private var glassNS

    var body: some View {
        VStack(spacing: 0) {
            GlassEffectContainer(spacing: 10) {
                VStack(spacing: 6) {
                    VStack(spacing: 4) {
                        railButton(.messages, badge: totalUnread)
                        railButton(.contacts)
                        railButton(.calls)
                    }
                    .padding(.top, 14)

                    VStack(spacing: 4) {
                        railButton(.todo)
                        railButton(.cloud)
                    }
                    .padding(.top, 10)

                    Spacer(minLength: 8)

                    railButton(.settings)
                }
                .frame(maxHeight: .infinity, alignment: .top)
                .frame(width: 68)
            }

            // Account menu sits OUTSIDE the glass container so clicks always register.
            Menu {
                Button("Hồ sơ của bạn") { store.showToast("Hồ sơ — bản demo giao diện") }
                Divider()
                Button("Đăng xuất", role: .destructive) { store.logOut() }
            } label: {
                VStack(spacing: 3) {
                    AvatarView(name: store.loggedInUser?.displayName ?? "Bạn", size: 34)
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 7, weight: .bold))
                        .foregroundStyle(.tertiary)
                }
            }
            .menuIndicator(.hidden)
            .fixedSize()
            .frame(width: 68, height: 52)
            .help("Tài khoản — Đăng xuất (⌘⇧L)")
            .padding(.bottom, 12)
        }
        .frame(width: 68)
        .background(.bar)
    }

    private var totalUnread: Int {
        store.conversations.reduce(0) { $0 + $1.unreadCount }
    }

    private func railButton(_ tab: NavTab, badge: Int = 0) -> some View {
        let selected = store.tab == tab
        return Button {
            withAnimation(.smooth(duration: 0.35)) { store.tab = tab }
        } label: {
            ZStack(alignment: .topTrailing) {
                Image(systemName: tab.icon)
                    .font(.system(size: 16, weight: .semibold))
                    .symbolEffect(.bounce, value: selected)
                    .foregroundStyle(selected ? AnyShapeStyle(ZaloTheme.blue) : AnyShapeStyle(.secondary))
                    .frame(width: 44, height: 36)
                if badge > 0 {
                    UnreadBadge(count: badge)
                        .offset(x: 8, y: -2)
                }
            }
            .frame(width: 52, height: 40)
        }
        .buttonStyle(.plain)
        .glassEffect(.regular.interactive(), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .glassEffectID(tab.id, in: glassNS)
        .help(tab.title)
    }
}
