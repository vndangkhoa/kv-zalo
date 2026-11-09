import SwiftUI

// "Phân loại" filter of the inbox: a round menu button right of the search field and a removable chip under it while a
// label is active. The sidebar has no rows for the labels; `ShellModel.destination` is `.label(id)` while one filters.

/// Glass round button beside the search field: picks the label that filters "Tin nhắn" ("Tất cả" clears it), or opens
/// the label manager. Same height as the search field; filled and tinted while a label is active.
struct ConversationListLabelFilter: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell
    @Binding var showLabelManager: Bool

    private var extras: ConversationExtras { engine.conversationExtras }

    var body: some View {
        let active = extras.label(filteredBy: shell.destination)
        let counts = UnreadCounts(engine: engine)
        Menu {
            Toggle("Tất cả", isOn: isOn(nil, active: active))
            Divider()
            if extras.labels.isEmpty {
                Button("Chưa có thẻ phân loại") {}.disabled(true)
            }
            ForEach(extras.labels) { label in
                Toggle(isOn: isOn(label, active: active)) {
                    LabelMenuTitle(label: label, unread: counts.unread(in: label))
                }
            }
            Divider()
            Button("Quản lý thẻ phân loại…", systemImage: "slider.horizontal.3") { showLabelManager = true }
        } label: {
            Image(systemName: active == nil ? "tag" : "tag.fill")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(active == nil ? AnyShapeStyle(.secondary) : AnyShapeStyle(.tint))
                .frame(width: 30, height: 30)
                .glassEffect(.regular.interactive(), in: Circle())
                .contentShape(Circle())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Lọc theo thẻ phân loại")
        .accessibilityLabel("Lọc theo thẻ phân loại")
        .accessibilityValue(active?.name ?? "Tất cả")
    }

    /// Check mark of one menu item. Choosing the checked label again goes back to the whole inbox, like "Tất cả".
    private func isOn(_ label: ConversationExtras.Label?, active: ConversationExtras.Label?) -> Binding<Bool> {
        Binding(get: { active?.id == label?.id },
                set: { on in shell.destination = on ? label.map { .label($0.id) } ?? .inbox : .inbox })
    }
}

/// "● Gia đình  ✕" under the search field while a label filters the inbox; ✕ goes back to the whole inbox.
struct ConversationListLabelChip: View {
    let label: ConversationExtras.Label
    let clear: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(label.color).frame(width: 8, height: 8)
            Text(label.name)
                .font(.system(size: 12, weight: .medium))
                .lineLimit(1)
            Button(action: clear) {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(.secondary)
                    .frame(width: 16, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Bỏ lọc")
            .accessibilityLabel("Bỏ lọc \(label.name)")
        }
        .padding(.leading, 10).padding(.trailing, 5)
        .frame(height: 24)
        .glassEffect(.regular, in: Capsule())
        .accessibilityElement(children: .contain)
    }
}
