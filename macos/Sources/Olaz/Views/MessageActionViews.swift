import SwiftUI

// Native stand-ins for what the official message menu opens (ZaloEngine+MessageActions.swift): the SHARE_MSG_COMPOSE picker of
// "Chia sẻ", the "N tin nhắn đã xóa · Khôi phục" snackbar, the pinned bar above the chat (official topic banner) and the confirms.
// Attached once to the chat (`messageActionPresenter(conversationId:)`); nothing of one conversation shows in another (the engine drops
// a pending picker / confirm when another conversation opens, and each is presented only in its own conversation).

// MARK: - Chia sẻ

/// The conversations a message is forwarded to — the official SHARE_MSG_COMPOSE picker, native.
struct ForwardSheet: View {
    let request: ForwardRequest

    /// Official `forward_conv_limit` (live config and default: 100).
    static let maxTargets = 100

    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var search = ""
    @State private var selected: [String] = []
    @State private var busy = false
    @State private var failure: String?

    private var rows: [ZaloEngine.ConversationSummary] {
        let all = engine.visibleConversations.sorted { ($0.preview?.date ?? .distantPast) > ($1.preview?.date ?? .distantPast) }
        let needle = search.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return all }
        return all.filter { title(of: $0).range(of: needle, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
    }

    var body: some View {
        VStack(spacing: 0) {
            SheetHeader(title: "Chia sẻ") { dismiss() }
            Divider()
            VStack(alignment: .leading, spacing: 10) {
                ForwardPreview(message: request.message)
                TextField("Tìm kiếm hội thoại cần chia sẻ", text: $search).textFieldStyle(.roundedBorder)
            }
            .padding(16)
            List(rows) { conversation in
                row(conversation)
            }
            .listStyle(.plain)
            Divider()
            SheetFooter(message: failure, hint: selected.isEmpty ? "Chọn hội thoại để chia sẻ" : "Đã chọn \(selected.count)",
                        action: "Chia sẻ", busy: busy, disabled: selected.isEmpty, cancel: { dismiss() }) {
                Task { await send() }
            }
        }
        .frame(width: 440, height: 580)
    }

    private func row(_ conversation: ZaloEngine.ConversationSummary) -> some View {
        let isOn = selected.contains(conversation.id)
        return Button {
            if isOn { selected.removeAll { $0 == conversation.id } }
            else if selected.count < Self.maxTargets { selected.append(conversation.id) }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: isOn ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 16)).foregroundStyle(isOn ? AnyShapeStyle(.tint) : AnyShapeStyle(.tertiary))
                RemoteAvatar(url: conversation.avatar, name: conversation.displayName, size: 30)
                Text(title(of: conversation)).font(.system(size: 13)).lineLimit(1)
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!isOn && selected.count >= Self.maxTargets)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }

    private func title(of conversation: ZaloEngine.ConversationSummary) -> String {
        conversation.id == engine.selfConversationId ? "Cloud của tôi" : conversation.displayName
    }

    private func send() async {
        guard !selected.isEmpty, !busy else { return }
        busy = true; failure = nil
        let outcome = await engine.forward(request.message, to: selected)
        busy = false
        switch outcome {
        case .done(let text):
            CardActionCenter.shared.show(text ?? "Đã chia sẻ tin nhắn", in: request.conversationId)
            dismiss()
        case .pending(let text):
            CardActionCenter.shared.show(text, in: request.conversationId)
            dismiss()
        case .failed(let text):
            failure = text
        case .confirm:
            failure = "Có lỗi khi chia sẻ tin nhắn"
        }
    }
}

/// "Nội dung chia sẻ": what is being forwarded, as a small card.
private struct ForwardPreview: View {
    let message: ChatMessage

    var body: some View {
        HStack(spacing: 10) {
            if let thumbnail {
                RemoteImage(url: thumbnail, contentMode: .fill, maxPixelSize: 120) { Rectangle().fill(.quaternary) }
                    .frame(width: 44, height: 44)
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            }
            VStack(alignment: .leading, spacing: 2) {
                Text("Nội dung chia sẻ").font(.system(size: 11)).foregroundStyle(.secondary)
                Text(message.text.isEmpty ? "Tin nhắn" : message.text).font(.system(size: 12.5)).lineLimit(3)
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color.primary.opacity(0.05)))
    }

    private var thumbnail: URL? {
        switch message.kind {
        case .photo(let url, _, _): message.isE2EE ? nil : url
        case .video(let thumbnail, _, _): thumbnail
        case .gif(let url), .sticker(let url): url
        case .link(_, _, let thumbnail, _, _): thumbnail
        default: nil
        }
    }
}

// MARK: - Pinned bar

/// The conversation's pins above its messages (official topic banner): the newest pin, the others in a menu.
struct PinnedBar: View {
    let conversationId: String
    var open: (PinnedItem) -> Void

    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        if let list = engine.pins[conversationId], list.ready, let first = list.items.first {
            HStack(spacing: 10) {
                Image(systemName: "pin.fill")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.tint)
                    .rotationEffect(.degrees(35))
                Button { open(first) } label: {
                    HStack(spacing: 8) {
                        if let thumbnail = first.thumbnail {
                            RemoteImage(url: thumbnail, contentMode: .fill, maxPixelSize: 80) { Rectangle().fill(.quaternary) }
                                .frame(width: 28, height: 28)
                                .clipShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
                        }
                        VStack(alignment: .leading, spacing: 1) {
                            Text(caption(first)).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                            Text(first.title.isEmpty ? first.label : first.title).font(.system(size: 12.5)).lineLimit(1)
                        }
                        Spacer(minLength: 0)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(first.title)
                if list.items.count > 1 {
                    Menu {
                        ForEach(list.items) { item in
                            Menu(menuTitle(item)) {
                                Button(item.kind == .message ? "Xem tin nhắn" : "Xem", systemImage: "arrow.right.circle") { open(item) }
                                if list.canPin {
                                    Button("Bỏ ghim", systemImage: "pin.slash", role: .destructive) { engine.requestUnpin(item, in: conversationId) }
                                }
                            }
                        }
                    } label: {
                        Text("+\(list.items.count - 1) ghim").font(.system(size: 11.5, weight: .medium))
                    }
                    .menuStyle(.button)
                    .buttonStyle(.glass)
                    .controlSize(.small)
                    .fixedSize()
                    .help("Danh sách ghim (\(list.items.count))")
                } else if list.canPin {
                    Button("Bỏ ghim", systemImage: "pin.slash") { engine.requestUnpin(first, in: conversationId) }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Bỏ ghim")
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 7)
            .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .padding(.horizontal, 16).padding(.top, 6).padding(.bottom, 2)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Tin nhắn đã ghim")
        }
    }

    private func caption(_ item: PinnedItem) -> String {
        item.sender.isEmpty ? item.label : "\(item.label) của \(item.sender)"
    }

    private func menuTitle(_ item: PinnedItem) -> String {
        let text = item.title.isEmpty ? item.label : item.title
        return String(text.prefix(60))
    }
}

// MARK: - Undo of a delete

/// "N tin nhắn đã xóa · Khôi phục (s)": the official snackbar of a delete-for-me, counting down to its deadline.
private struct DeleteUndoBar: View {
    let undo: DeleteUndo
    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let left = max(0, Int(undo.deadline.timeIntervalSince(context.date).rounded(.up)))
            HStack(spacing: 10) {
                Image(systemName: "trash").font(.system(size: 12)).foregroundStyle(.secondary)
                Text("\(undo.count) tin nhắn đã xóa").font(.system(size: 12.5, weight: .medium))
                Button("Khôi phục (\(left))") { engine.undoDelete() }
                    .buttonStyle(.glass)
                    .controlSize(.small)
                Button("Đóng", systemImage: "xmark") { engine.dismissDeleteUndo() }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .help("Xóa ngay")
            }
            .padding(.leading, 14).padding(.trailing, 10).padding(.vertical, 6)
            .glassEffect(.regular, in: Capsule())
        }
    }
}

// MARK: - Presenter

private struct MessageActionPresenter: ViewModifier {
    let conversationId: String
    @Environment(ZaloEngine.self) private var engine

    func body(content: Content) -> some View {
        content
            .sheet(item: Binding(get: { engine.forwardRequest?.conversationId == conversationId ? engine.forwardRequest : nil },
                                 set: { engine.forwardRequest = $0 })) { request in
                ForwardSheet(request: request).cardNoticeOverlay()
            }
            // Answered only by its buttons (the dismissal binding must not answer first and turn an OK into a cancel).
            .alert(engine.messageConfirm?.title ?? "", isPresented: Binding(get: { engine.messageConfirm?.conversationId == conversationId }, set: { _ in }),
                   presenting: engine.messageConfirm) { confirm in
                Button(confirm.ok, role: confirm.destructive ? .destructive : nil) { engine.answerMessageConfirm(confirm, ok: true) }
                Button(confirm.cancel, role: .cancel) { engine.answerMessageConfirm(confirm, ok: false) }
            } message: { confirm in
                Text(confirm.message)
            }
            .overlay(alignment: .bottom) {
                if let undo = engine.deleteUndo, undo.conversationId == conversationId {
                    DeleteUndoBar(undo: undo)
                        .padding(.bottom, 84)
                        .transition(.opacity.combined(with: .move(edge: .bottom)))
                }
            }
            .animation(.snappy, value: engine.deleteUndo)
    }
}

extension View {
    /// Hosts the forward picker, the confirms and the delete undo of message actions (attach once, to the chat).
    func messageActionPresenter(conversationId: String) -> some View {
        modifier(MessageActionPresenter(conversationId: conversationId))
    }
}
