import SwiftUI

/// Right pane: chat header + message list + Liquid Glass composer.
struct ChatView: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        if let conversation = store.selectedConversation {
            VStack(spacing: 0) {
                ChatHeader(conversation: conversation)
                Divider().opacity(0.4)
                MessageList(conversation: conversation)
                Composer(conversation: conversation)
            }
            .id(conversation.id)
        } else {
            ContentUnavailableView(
                "Chọn một hội thoại",
                systemImage: "bubble.left.and.bubble.right",
                description: Text("Quay về danh sách hội thoại và chọn người để bắt đầu trò chuyện")
            )
        }
    }
}

// MARK: - Header

struct ChatHeader: View {
    @Environment(AppStore.self) private var store
    let conversation: Conversation

    var body: some View {
        HStack(spacing: 12) {
            AvatarView(
                name: conversation.title, size: 38,
                isOnline: peer?.isOnline ?? false,
                kind: conversation.kind
            )
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(conversation.title)
                        .font(.system(size: 15, weight: .bold))
                    if conversation.isEndToEndEncrypted {
                        Image(systemName: "lock.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(ZaloTheme.blue)
                            .help("Hội thoại được mã hóa đầu cuối")
                    }
                }
                Text(subtitle)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            Spacer()

            GlassEffectContainer(spacing: 6) {
                HStack(spacing: 2) {
                    headerButton("phone.fill", "Cuộc gọi thoại") { call(video: false) }
                    headerButton("video.fill", "Cuộc gọi video") { call(video: true) }
                    headerButton("magnifyingglass", "Tìm trong hội thoại") {
                        store.showToast("Tìm trong hội thoại — bản demo")
                    }
                    headerButton("sidebar.trailing", "Thông tin hội thoại") {
                        store.showToast("Chat info — bản demo")
                    }
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
    }

    private var peer: User? {
        conversation.participantIds.first.flatMap { store.user($0) }
    }

    private var subtitle: String {
        switch conversation.kind {
        case .direct:
            if let typing = store.typingIn[conversation.id] { return "\(typing) đang nhập…" }
            if peer?.isOnline == true { return "Đang hoạt động" }
            return "Bạn bè"
        case .group(let n), .community(let n):
            if let typing = store.typingIn[conversation.id] { return "\(typing) đang nhập…" }
            return "\(n) thành viên"
        case .official:
            return "Tài khoản chính thức"
        }
    }

    private func headerButton(_ icon: String, _ help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 32, height: 30)
        }
        .buttonStyle(.plain)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .help(help)
    }

    private func call(video: Bool) {
        store.showToast("Cuộc gọi \(video ? "video" : "thoại") cần native module `zcall` — xem README")
    }
}

// MARK: - Message list

struct MessageList: View {
    @Environment(AppStore.self) private var store
    let conversation: Conversation

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 2) {
                    Text("Zalo — Nhắn gửi yêu thương 💙")
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                        .padding(.vertical, 10)

                    let messages = store.messages(in: conversation.id)
                    ForEach(Array(messages.enumerated()), id: \.element.id) { index, message in
                        if showsDateDivider(before: index, in: messages) {
                            dateDivider(message.sentAt)
                        }
                        MessageBubble(
                            message: message,
                            showSender: showsSender(before: index, in: messages)
                        )
                            .id(message.id)
                    }

                    if let typing = store.typingIn[conversation.id] {
                        HStack {
                            TypingDots()
                                .padding(.horizontal, 14)
                                .padding(.vertical, 10)
                                .background(
                                    RoundedRectangle(cornerRadius: 16, style: .continuous)
                                        .fill(.quaternary.opacity(0.4))
                                )
                            Text("\(typing) đang nhập…")
                                .font(.system(size: 11))
                                .foregroundStyle(.tertiary)
                            Spacer()
                        }
                        .padding(.horizontal, 14)
                        .padding(.top, 4)
                        .id("typing")
                    }
                }
                .padding(.vertical, 8)
            }
            .defaultScrollAnchor(.bottom)
            .onChange(of: store.messages(in: conversation.id).count) { _, _ in
                if let last = store.messages(in: conversation.id).last {
                    withAnimation(.smooth) { proxy.scrollTo(last.id, anchor: .bottom) }
                }
            }
            .onChange(of: store.typingIn[conversation.id]) { _, _ in
                withAnimation(.smooth) { proxy.scrollTo("typing", anchor: .bottom) }
            }
        }
        .background(Color(.windowBackgroundColor))
    }

    private func showsDateDivider(before index: Int, in messages: [Message]) -> Bool {
        guard index > 0 else { return false }
        let current = messages[index].sentAt
        let previous = messages[index - 1].sentAt
        return !Calendar.current.isDate(current, inSameDayAs: previous)
    }

    private func showsSender(before index: Int, in messages: [Message]) -> Bool {
        let message = messages[index]
        if message.isMine { return false }
        switch conversation.kind {
        case .direct, .official:
            return false
        case .group, .community:
            if index == 0 { return true }
            return messages[index - 1].senderId != message.senderId
        }
    }

    private func dateDivider(_ date: Date) -> some View {
        Text(date, format: .dateTime.weekday().day().month())
            .font(.system(size: 11, weight: .medium))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 12)
            .padding(.vertical, 5)
            .glassEffect(in: Capsule())
            .padding(.vertical, 6)
    }
}

// MARK: - Bubble

struct MessageBubble: View {
    @Environment(AppStore.self) private var store
    let message: Message
    let showSender: Bool

    var body: some View {
        Group {
            switch message.kind {
            case .system(let text):
                Text(text)
                    .font(.system(size: 11.5))
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 4)

            default:
                bubbleBody
                    .frame(
                        maxWidth: 460,
                        alignment: message.isMine ? .trailing : .leading
                    )
                    .padding(.horizontal, 14)
                    .padding(.vertical, 3)
            }
        }
    }

    @ViewBuilder
    private var bubbleBody: some View {
        VStack(alignment: message.isMine ? .trailing : .leading, spacing: 4) {
            if showSender, let sender = store.user(message.senderId) {
                Text(sender.displayName)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(ZaloTheme.blue)
                    .padding(.horizontal, 12)
            }

            content
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(
                    RoundedRectangle(cornerRadius: 16, style: .continuous)
                        .fill(bubbleColor)
                )
                .contextMenu {
                    Button("Trả lời") { store.showToast("Trả lời — bản demo") }
                    Button("Bình luận") { store.showToast("Bình luận — bản demo") }
                    Button("Chuyển tiếp") { store.showToast("Chuyển tiếp — bản demo") }
                    Divider()
                    Button("Thu hồi", role: .destructive) {
                        store.service.recallMessage(message.id)
                    }
                }

            HStack(spacing: 4) {
                ForEach(message.reactions, id: \.self) { reaction in
                    Text("\(reaction.emoji) \(reaction.count)")
                        .font(.system(size: 11))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Capsule().fill(.quinary))
                }

                Text(message.sentAt, format: .dateTime.hour().minute())
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)

                if message.isMine {
                    Image(systemName: statusIcon)
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(message.isSeen ? AnyShapeStyle(ZaloTheme.blue) : AnyShapeStyle(.tertiary))
                }
            }
            .padding(.horizontal, 6)
        }
    }

    private var statusIcon: String {
        message.isSeen ? "checkmark.circle.fill" : "checkmark"
    }

    private var bubbleColor: Color {
        if message.isMine { return ZaloTheme.blue }
        return Color.primary.opacity(0.06)
    }

    @ViewBuilder
    private var content: some View {
        switch message.kind {
        case .text(let text):
            Text(text)
                .font(.system(size: 13.5))
                .foregroundStyle(message.isMine ? .white : .primary)
                .textSelection(.enabled)

        case .reply(let quoted, let text):
            VStack(alignment: .leading, spacing: 6) {
                Text(quoted)
                    .font(.system(size: 11.5))
                    .lineLimit(2)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(message.isMine ? Color.white.opacity(0.18) : ZaloTheme.blue.opacity(0.1))
                    )
                Text(text)
                    .font(.system(size: 13.5))
                    .foregroundStyle(message.isMine ? .white : .primary)
            }

        case .sticker(let emoji):
            Text(emoji)
                .font(.system(size: 44))

        case .image(let name):
            VStack(spacing: 6) {
                ZStack {
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .fill(
                            LinearGradient(
                                colors: [.teal.opacity(0.5), .indigo.opacity(0.5)],
                                startPoint: .topLeading, endPoint: .bottomTrailing
                            )
                        )
                        .frame(width: 210, height: 140)
                    Image(systemName: name == "photo" ? "photo.on.rectangle.angled" : "photo")
                        .font(.system(size: 34))
                        .foregroundStyle(.white.opacity(0.9))
                }
                Text("Hình ảnh")
                    .font(.system(size: 11))
                    .foregroundStyle(message.isMine ? .white.opacity(0.85) : .secondary)
            }

        case .voice(let seconds):
            HStack(spacing: 10) {
                Button {
                    store.showToast("Phát tin nhắn thoại — bản demo")
                } label: {
                    Image(systemName: "play.circle.fill")
                        .font(.system(size: 26))
                        .foregroundStyle(message.isMine ? .white : ZaloTheme.blue)
                }
                .buttonStyle(.plain)
                VStack(alignment: .leading, spacing: 3) {
                    WaveformView(playing: false)
                        .frame(width: 110)
                    Text("0:\(String(format: "%02d", seconds))")
                        .font(.system(size: 10.5, weight: .medium).monospacedDigit())
                        .foregroundStyle(message.isMine ? .white.opacity(0.85) : .secondary)
                }
            }

        case .file(let name, let sizeKB):
            HStack(spacing: 10) {
                Image(systemName: "doc.fill")
                    .font(.system(size: 22))
                    .foregroundStyle(message.isMine ? .white : ZaloTheme.blue)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(.system(size: 12.5, weight: .medium))
                    Text("\(sizeKB / 1024) MB")
                        .font(.system(size: 10.5))
                        .foregroundStyle(message.isMine ? .white.opacity(0.8) : .secondary)
                }
                Button {
                    store.showToast("Tải tệp — bản demo")
                } label: {
                    Image(systemName: "arrow.down.circle.fill")
                        .font(.system(size: 20))
                }
                .buttonStyle(.plain)
            }

        case .call(let video, let missed):
            HStack(spacing: 10) {
                Image(systemName: missed
                      ? (video ? "video.slash.fill" : "phone.badge.xmark")
                      : (video ? "video.fill" : "phone.fill"))
                    .font(.system(size: 20))
                    .foregroundStyle(missed ? .red : ZaloTheme.blue)
                Text(missed
                     ? "Cuộc gọi \(video ? "video" : "thoại") nhỡ"
                     : "Cuộc gọi \(video ? "video" : "thoại")")
                    .font(.system(size: 13, weight: .medium))
                Image(systemName: "arrow.up.right")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
            }

        case .todo(let title, let due):
            HStack(spacing: 10) {
                Image(systemName: "checklist")
                    .font(.system(size: 20))
                    .foregroundStyle(message.isMine ? .white : ZaloTheme.blue)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.system(size: 12.5, weight: .medium))
                    Text("Hạn: \(due)")
                        .font(.system(size: 10.5))
                        .foregroundStyle(message.isMine ? .white.opacity(0.8) : .secondary)
                }
            }

        case .system(let text):
            Text(text)
        }
    }
}

// MARK: - Composer (Liquid Glass capsule)

struct Composer: View {
    @Environment(AppStore.self) private var store
    let conversation: Conversation
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                toolButton("face.smiling", "Sticker & emoji") { store.showToast("Sticker — bản demo") }
                toolButton("photo.on.rectangle", "Gửi hình ảnh") {
                    store.send(.image("photo"), to: conversation.id)
                }
                toolButton("paperclip", "Đính kèm tệp") {
                    store.send(.file(name: "BaoCao-Tuan.pdf", sizeKB: 1024), to: conversation.id)
                }
                toolButton("camera.metering.center.weighted", "Chụp màn hình") {
                    store.showToast("Chụp màn hình — bản demo")
                }

                TextField("Nhập tin nhắn", text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...4)
                    .focused($focused)
                    .onSubmit(sendMessage)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)

                if draft.trimmingCharacters(in: .whitespaces).isEmpty {
                    toolButton("mic.fill", "Ghi âm") {
                        store.send(.voice(seconds: 8), to: conversation.id)
                    }
                } else {
                    Button(action: sendMessage) {
                        Image(systemName: "paperplane.fill")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 32, height: 32)
                    }
                    .buttonStyle(.glassProminent)
                    .transition(.scale.combined(with: .opacity))
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .glassEffect(in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            .padding(.horizontal, 14)
            .padding(.top, 8)
            .padding(.bottom, 12)
            .animation(.snappy(duration: 0.25), value: draft.isEmpty)
        }
        .background(
            Rectangle().fill(.bar).ignoresSafeArea(edges: .bottom)
        )
    }

    private func sendMessage() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft = ""
        store.send(.text(text), to: conversation.id)
    }

    private func toolButton(_ icon: String, _ help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 30, height: 30)
        }
        .buttonStyle(.plain)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .help(help)
    }
}
