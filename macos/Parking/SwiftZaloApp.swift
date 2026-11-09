import SwiftUI

@main
struct SwiftZaloApp: App {
    @State private var store = AppStore()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(store)
                .preferredColorScheme(store.prefersDark ? .dark : .light)
                .frame(minWidth: 1000, minHeight: 620)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(replacing: .newItem) {}
            SidebarCommands()
            CommandMenu("Tài khoản") {
                Button("Đăng xuất") { store.logOut() }
                    .keyboardShortcut("L", modifiers: [.command, .shift])
                    .disabled(!store.isLoggedIn)
            }
        }
    }
}

struct RootView: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        ZStack {
            if store.isLoggedIn {
                MainView()
            } else {
                LoginView()
            }
        }
        .animation(.smooth(duration: 0.45), value: store.isLoggedIn)
        .overlay(alignment: .bottom) {
            if let toast = store.toast {
                Text(toast)
                    .font(.system(size: 13, weight: .medium))
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .glassEffect(.regular.tint(ZaloTheme.blue.opacity(0.85)), in: Capsule())
                    .foregroundStyle(.white)
                    .padding(.bottom, 24)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
    }
}
