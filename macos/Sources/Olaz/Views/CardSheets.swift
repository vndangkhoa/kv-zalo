import SwiftUI

// Native versions of the dialogs that card buttons open in the official client (React modals that do not render in the headless web view):
// alias ("Đặt tên gợi nhớ", EDIT_ALIAS), group invite (GROUP_PREVIEW) and a sticker category (the sticker dock at that category).
// Opening a sheet only reads; every write (alias, join, sticker) runs from the sheet's own confirm / click, through the official services
// (`ZaloEngine+CardActions.swift` → card-actions-adapter.js). Message content, names and ids never go to the log.

// MARK: - Alias

struct AliasSheet: View {
    let request: AliasRequest
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var avatar: URL?
    @State private var saving = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var draft: AliasDraft { request.draft }
    private var displayName: String { draft.name.isEmpty ? "người này" : draft.name }
    private var isEmpty: Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    /// Empty field: confirming removes an existing alias (labelled so); with no alias there is nothing to confirm.
    private var removes: Bool { isEmpty && draft.hasAlias == true }
    private var nothingToSave: Bool { isEmpty && draft.hasAlias == false }

    var body: some View {
        VStack(spacing: 14) {
            RemoteAvatar(url: avatar, name: draft.name, size: 84)
            Text("Đặt tên gợi nhớ").font(.system(size: 17, weight: .bold))
            VStack(spacing: 3) {
                Text("Hãy đặt cho \(Text(displayName).bold()) một cái tên dễ nhớ.")
                Text("Lưu ý: Tên gợi nhớ sẽ chỉ hiển thị riêng với bạn.").foregroundStyle(.secondary)
            }
            .font(.system(size: 12.5))
            .multilineTextAlignment(.center)
            VStack(alignment: .trailing, spacing: 4) {
                TextField(draft.placeholder, text: $text)
                    .textFieldStyle(.plain)
                    .font(.system(size: 13.5))
                    .padding(.horizontal, 12).padding(.vertical, 9)
                    .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(.quaternary.opacity(0.5)))
                    .focused($focused)
                    .disabled(saving)
                    .onSubmit(save)
                    .onChange(of: text) { _, new in
                        let clean = AliasDraft.sanitized(new).prefixUTF16(draft.max)
                        if clean != new { text = clean }
                    }
                Text("\(text.utf16.count)/\(draft.max)").font(.system(size: 11)).foregroundStyle(.tertiary)
            }
            if let error { Text(error).font(.system(size: 12)).foregroundStyle(.orange).multilineTextAlignment(.center) }
            HStack(spacing: 10) {
                Button("Hủy") { dismiss() }.keyboardShortcut(.cancelAction).buttonStyle(.glass).disabled(saving)
                Button(action: save) {
                    if saving { ProgressView().controlSize(.small) } else { Text(removes ? "Xoá tên gợi nhớ" : "Xác nhận") }
                }
                .keyboardShortcut(.defaultAction).buttonStyle(.glassProminent).disabled(saving || nothingToSave)
            }
        }
        .padding(24)
        .frame(width: 360)
        .task {
            text = draft.suggest.prefixUTF16(draft.max)
            focused = true
            avatar = await engine.contactProfile(draft.uid)?.avatar
        }
    }

    /// Official `accept`: the trimmed text goes to `setAliasName(uid, name)`; an empty name removes the alias.
    private func save() {
        guard !saving, !nothingToSave else { return }
        saving = true
        error = nil
        let value = text
        Task {
            let result = await engine.saveAlias(uid: draft.uid, name: value)
            saving = false
            if result.ok {
                CardActionCenter.shared.show(result.text, in: request.conversationId)
                dismiss()
            } else {
                error = result.text.isEmpty ? "Có lỗi xảy ra, vui lòng thử lại sau" : result.text
            }
        }
    }
}

// MARK: - Group invite

struct GroupInviteSheet: View {
    let request: GroupInviteRequest
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var preview: GroupInvitePreview?
    @State private var joining = false
    @State private var error: String?

    private var community: Bool { preview?.community ?? false }
    /// This sheet is still the one on screen, in the conversation it was opened from.
    private var stillPresented: Bool {
        !Task.isCancelled && engine.openConversationId == request.conversationId && CardActionCenter.shared.groupInvite?.id == request.id
    }

    var body: some View {
        VStack(spacing: 14) {
            Text(community ? "Thông tin cộng đồng" : "Thông tin nhóm").font(.system(size: 15, weight: .semibold))
            if let preview {
                switch preview.state {
                case .preview: content(preview)
                case .member: ProgressView().controlSize(.small)
                case .error: Text(preview.text).font(.system(size: 12.5)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
            } else {
                ProgressView().controlSize(.small).padding(.vertical, 40)
            }
            if let error { Text(error).font(.system(size: 12)).foregroundStyle(.orange).multilineTextAlignment(.center) }
            HStack(spacing: 10) {
                Button("Đóng") { dismiss() }.keyboardShortcut(.cancelAction).buttonStyle(.glass).disabled(joining)
                if preview?.state == .preview {
                    Button(action: join) {
                        if joining { ProgressView().controlSize(.small) } else { Text("Tham gia") }
                    }
                    .keyboardShortcut(.defaultAction).buttonStyle(.glassProminent).disabled(joining)
                }
            }
        }
        .padding(24)
        .frame(width: 360)
        .task {
            let result = await engine.groupInvitePreview(request.url)
            // The sheet may have been closed / reset (another conversation opened) while this was loading: then nothing may navigate.
            guard stillPresented else { return }
            // Official: already a member → no dialog, the conversation opens.
            if result.state == .member {
                engine.openContactConversation(result.groupId)
                dismiss()
                return
            }
            preview = result
        }
    }

    @ViewBuilder private func content(_ group: GroupInvitePreview) -> some View {
        RemoteAvatar(url: group.avatar, name: group.name, size: 84)
        VStack(spacing: 4) {
            Text(group.name).font(.system(size: 17, weight: .bold)).multilineTextAlignment(.center).lineLimit(2)
            Text(subtitle(group)).font(.system(size: 12.5)).foregroundStyle(.secondary).multilineTextAlignment(.center)
        }
        if !group.desc.isEmpty {
            Text(group.desc).font(.system(size: 12.5)).multilineTextAlignment(.center).lineLimit(4).textSelection(.enabled)
        } else {
            Text(community ? "Bạn đang ở phòng chờ. Hãy tham gia chat để cùng trò chuyện với mọi người trong cộng đồng."
                 : "Bạn đang ở phòng chờ. Hãy tham gia chat để cùng trò chuyện với mọi người trong nhóm.")
                .font(.system(size: 12.5)).foregroundStyle(.secondary).multilineTextAlignment(.center)
        }
        if !group.members.isEmpty {
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(group.members) { member in
                        HStack(spacing: 10) {
                            RemoteAvatar(url: member.avatar, name: member.name, size: 28)
                            Text(member.name.isEmpty ? "Người dùng Zalo" : member.name).font(.system(size: 13)).lineLimit(1)
                            if member.admin { Image(systemName: "key.fill").font(.system(size: 10)).foregroundStyle(.secondary).help("Trưởng/phó nhóm") }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 150)
        }
    }

    private func subtitle(_ group: GroupInvitePreview) -> String {
        var text = "\(group.totalMember) thành viên"
        if !group.creatorName.isEmpty { text += " • Tạo bởi \(group.creatorName)" }
        return text
    }

    /// Official `_join`: only from this button. Approval-required groups answer "request sent"; "already a member" opens the group.
    private func join() {
        guard !joining else { return }
        joining = true
        error = nil
        Task {
            let result = await engine.joinGroup(request.url)
            joining = false
            switch result.state {
            case .joined, .already:
                // Joined for real; only take the user to the group while this sheet is still what they are looking at.
                if stillPresented { engine.openContactConversation(result.groupId) }
                dismiss()
            case .pending:
                CardActionCenter.shared.show(result.text, in: request.conversationId)
                dismiss()
            case .error:
                error = result.text
            }
        }
    }
}

// MARK: - Sticker category

/// A sticker category (`action.open.sticker`): the stickers come from the official sticker manager; a click sends that sticker to the
/// conversation the card is in, through the same chat-box send path as the e-card stickers.
struct StickerCategorySheet: View {
    let request: StickerCategoryRequest
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var category: StickerCategory?
    @State private var loading = true
    @State private var sending: String?
    @State private var error: String?

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                if let icon = category?.icon {
                    RemoteImage(url: icon, contentMode: .fit, maxPixelSize: 80) { Color.clear }.frame(width: 28, height: 28)
                }
                Text(category?.name.isEmpty == false ? category?.name ?? "" : "Bộ sticker").font(.system(size: 15, weight: .semibold)).lineLimit(1)
                Spacer()
                Button("Đóng") { dismiss() }.keyboardShortcut(.cancelAction).buttonStyle(.glass).controlSize(.small)
            }
            .padding(16)
            Divider()
            Group {
                if let category {
                    ScrollView {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 70, maximum: 90), spacing: 8)], spacing: 8) {
                            ForEach(category.stickers) { sticker in
                                Button { send(sticker, in: category) } label: {
                                    RemoteImage(url: sticker.url, contentMode: .fit, maxPixelSize: 200, animated: true) {
                                        Image(systemName: "face.smiling").foregroundStyle(.tertiary)
                                    }
                                    .frame(width: 64, height: 64)
                                    .opacity(sending == sticker.ref ? 0.4 : 1)
                                }
                                .buttonStyle(.plain)
                                .disabled(sending != nil)
                                .help("Gửi sticker này")
                            }
                        }
                        .padding(14)
                    }
                } else if loading {
                    ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    Text("Không tải được bộ sticker này. Hãy thử lại sau.").font(.system(size: 12.5)).foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            if let error {
                Text(error).font(.system(size: 12)).foregroundStyle(.orange).multilineTextAlignment(.center).padding(10)
            }
        }
        .frame(width: 420, height: 480)
        .task {
            // The sticker service may not be resolved yet right after launch: a few tries, then give up quietly.
            for attempt in 0..<3 {
                if let loaded = await engine.stickerCategory(request.cateId) { category = loaded; loading = false; return }
                if attempt < 2 { try? await Task.sleep(for: .seconds(1.5)) }
                if Task.isCancelled { return }
            }
            loading = false
        }
    }

    private func send(_ sticker: StickerCategory.Item, in category: StickerCategory) {
        guard sending == nil, engine.openConversationId == request.conversationId else { return }
        sending = sticker.ref
        error = nil
        Task {
            let outcome = await engine.sendCategorySticker(in: request.conversationId, category: category.id, sticker: sticker.stickerId)
            sending = nil
            switch outcome {
            case .done: dismiss()
            case .locked(let text), .failed(let text), .unsupported(let text), .pending(let text): error = text
            default: break
            }
        }
    }
}
