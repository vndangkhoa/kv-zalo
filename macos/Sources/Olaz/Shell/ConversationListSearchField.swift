import SwiftUI

/// Glass search capsule pinned above the conversation list. Focuses when the shell asks for it (⌘F, ⌥⌘F) and Esc clears it.
struct ConversationListSearchField: View {
    @Environment(ShellModel.self) private var shell
    @FocusState private var focused: Bool

    var body: some View {
        @Bindable var shell = shell
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 12.5, weight: .medium))
                .foregroundStyle(.secondary)
            TextField("Tìm kiếm", text: $shell.search)
                .textFieldStyle(.plain)
                .font(.system(size: 13))
                .focused($focused)
                .onExitCommand {
                    if shell.search.isEmpty { focused = false } else { shell.search = "" }
                }
            if shell.search.isEmpty {
                Text("⌘K")
                    .font(.system(size: 11))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            } else {
                Button { shell.search = "" } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Xoá")
                .accessibilityLabel("Xoá nội dung tìm kiếm")
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 30)
        .glassEffect(.regular.interactive(), in: Capsule())
        .overlay(Capsule().strokeBorder(.tint.opacity(focused ? 0.9 : 0), lineWidth: 1.5))
        .animation(.smooth(duration: 0.15), value: focused)
        .contentShape(Capsule())
        .onTapGesture { focused = true }
        .onChange(of: shell.searchFocusTick) { _, _ in focused = true }
        .tourAnchor(.search)
    }
}
