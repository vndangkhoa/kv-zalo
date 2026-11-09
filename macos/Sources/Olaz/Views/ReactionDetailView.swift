import SwiftUI

/// "Biểu cảm" popover: who reacted to a message, mirroring the official Zalo PC popup
/// (tabs "Tất cả" + one per icon, rows with avatar, name and the icons they used).
/// Data comes from the official reaction cache via `ZaloEngine.reactors`.
struct ReactionDetailView: View {
    let conversationId: String
    let messageId: String
    /// The message's current reactions: the list reloads when they change while the popover is open.
    let reactions: [ChatMessage.Reaction]
    @Environment(ZaloEngine.self) private var engine

    private enum Phase: Equatable { case loading, loaded(ReactionDetail), failed }
    @State private var phase: Phase = .loading
    /// Reaction type of the selected tab; nil = "Tất cả".
    @State private var selectedType: String?

    var body: some View {
        VStack(spacing: 0) {
            Text("Biểu cảm")
                .font(.system(size: 14, weight: .semibold))
                .padding(.vertical, 10)
            Divider()
            switch phase {
            case .loading:
                ProgressView().controlSize(.small)
                    .frame(maxWidth: .infinity, minHeight: 120)
            case .failed:
                message("Không tải được danh sách biểu cảm", retry: true)
            case .loaded(let detail) where detail.isEmpty:
                message("Chưa có biểu cảm nào", retry: false)
            case .loaded(let detail):
                let selected = detail.tabs.contains { $0.icon.type == selectedType } ? selectedType : nil
                let rows = detail.rows(for: selected)
                tabs(detail, selected: selected)
                Divider()
                list(rows)
            }
        }
        .frame(width: 320)
        .task(id: Reload(messageId: messageId, reactions: reactions)) { await load() }
    }

    private struct Reload: Equatable { var messageId: String; var reactions: [ChatMessage.Reaction] }

    /// Keeps the current list on screen while refreshing; "loading" only shows before the first answer.
    private func load() async {
        if case .failed = phase { phase = .loading }
        if let detail = await engine.reactors(for: messageId, in: conversationId) {
            phase = .loaded(detail)
        } else if case .loaded = phase {
            // A failed refresh keeps what is shown.
        } else {
            phase = .failed
        }
    }

    private func message(_ text: String, retry: Bool) -> some View {
        VStack(spacing: 8) {
            Text(text).font(.system(size: 12.5)).foregroundStyle(.secondary)
            if retry { Button("Thử lại") { Task { await load() } }.controlSize(.small) }
        }
        .frame(maxWidth: .infinity, minHeight: 120)
    }

    // MARK: Tabs

    private func tabs(_ detail: ReactionDetail, selected: String?) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 2) {
                tab(icon: nil, total: detail.total, selected: selected == nil)
                ForEach(detail.tabs) { tab(icon: $0.icon, total: $0.total, selected: selected == $0.icon.type) }
            }
            .padding(.horizontal, 8)
        }
        .frame(height: 36)
    }

    private func tab(icon: ReactionIcon?, total: Int, selected: Bool) -> some View {
        Button { selectedType = icon?.type } label: {
            HStack(spacing: 4) {
                if let icon {
                    ReactionIconView(icon: icon, size: 18)
                } else {
                    Text("Tất cả").font(.system(size: 12.5, weight: .medium))
                }
                Text(ReactionDetail.shortCount(total)).font(.system(size: 12, weight: .medium))
            }
            .foregroundStyle(selected ? AnyShapeStyle(TintShapeStyle()) : AnyShapeStyle(Color.secondary))
            .padding(.horizontal, 10).frame(height: 36)
            .overlay(alignment: .bottom) {
                Rectangle().fill(selected ? AnyShapeStyle(TintShapeStyle()) : AnyShapeStyle(Color.clear)).frame(height: 2)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(icon == nil ? "Tất cả, \(total)" : "\(icon!.emoji), \(total)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    // MARK: Rows

    private func list(_ rows: [ReactionDetail.User]) -> some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                ForEach(rows) { row($0) }
            }
            .padding(.vertical, 4)
        }
        .frame(height: min(CGFloat(rows.count) * 48 + 8, 288))
    }

    private func row(_ user: ReactionDetail.User) -> some View {
        HStack(spacing: 10) {
            RemoteAvatar(url: user.avatar, name: user.name, size: 32)
            Text(user.name)
                .font(.system(size: 13.5, weight: .medium))
                .lineLimit(1)
            Spacer(minLength: 8)
            HStack(spacing: 4) {
                ForEach(user.icons, id: \.icon.type) { ReactionIconView(icon: $0.icon, size: 18) }
                Text(ReactionDetail.shortCount(user.total))
                    .font(.system(size: 12, weight: .medium)).foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .frame(height: 48)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(user.name), \(user.icons.map(\.icon.emoji).joined(separator: " ")) \(user.total)")
    }
}

/// An official reaction icon: its PNG from the install's `pc-dist`, or the icon text when there is no image.
struct ReactionIconView: View {
    let icon: ReactionIcon
    var size: CGFloat = 16
    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        Group {
            if let image = engine.reactionImage(icon.image) {
                Image(nsImage: image).resizable().interpolation(.high).aspectRatio(contentMode: .fit)
                    .frame(width: size, height: size)
            } else {
                Text(icon.emoji).font(.system(size: size * 0.85))
            }
        }
        .accessibilityLabel(icon.emoji)
    }
}
