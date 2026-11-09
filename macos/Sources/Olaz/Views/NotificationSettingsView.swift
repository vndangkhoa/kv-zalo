import SwiftUI

/// "Thông báo" settings sections. Embed inside the Settings `Form` (it contributes `Section`s):
///
///     Form { … NotificationSettingsView() … }
///
/// Toggles read/write the official client's own settings (`askNoti`, `z_sound`) and the official mute
/// service, so they behave exactly like Zalo PC's "Cài đặt ▸ Thông báo".
struct NotificationSettingsView: View {
    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        let settings = engine.notificationSettings
        Section {
            Toggle(isOn: Binding(get: { settings.enabled }, set: { engine.setNotificationsEnabled($0) })) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Thông báo tin nhắn mới")
                    Text("Nhận được thông báo mỗi khi có tin nhắn mới").font(.caption).foregroundStyle(.secondary)
                }
            }
            .disabled(!settings.isLoaded)

            Toggle("Phát âm thanh khi có tin nhắn & thông báo mới",
                   isOn: Binding(get: { settings.sound }, set: { engine.setNotificationSound($0) }))
                .disabled(!settings.isLoaded || !settings.enabled)

            Toggle(isOn: Binding(get: { settings.showsPreview }, set: { engine.setMessagePreview($0) })) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Hiện nội dung tin nhắn")
                    Text("Tắt để chỉ hiện “Bạn có tin nhắn mới”").font(.caption).foregroundStyle(.secondary)
                }
            }

            authorizationRow(settings.authorization)
        } header: {
            Text("Thông báo")
        } footer: {
            Text("Zalo quyết định khi nào báo tin mới (tắt thông báo hội thoại, nhóm hay cá nhân, đang mở ứng dụng…). Chế độ Tập trung / Không làm phiền của macOS vẫn được áp dụng.")
        }
        .task { engine.notificationSettingsDidAppear() }

        MutedConversationsSection()
    }

    @ViewBuilder
    private func authorizationRow(_ status: NotificationSettingsState.Authorization) -> some View {
        switch status {
        case .authorized, .unknown:
            EmptyView()
        case .notDetermined:
            LabeledContent("macOS chưa cho phép hiển thị thông báo") {
                Button("Cho phép") { Task { await engine.requestNotificationAuthorization() } }
            }
        case .denied:
            LabeledContent("Thông báo đang bị tắt trong Cài đặt hệ thống") {
                Button("Mở Cài đặt…") { engine.openSystemNotificationSettings() }
            }
        case .unavailable:
            Text("Thông báo chỉ hoạt động khi chạy Olaz.app.").font(.caption).foregroundStyle(.secondary)
        }
    }
}

/// Conversations whose notifications are off, with a way to turn them back on.
private struct MutedConversationsSection: View {
    @Environment(ZaloEngine.self) private var engine

    var body: some View {
        // Hidden chats stay out of the list (and so does anything while the hidden state is unknown).
        let muted = engine.conversationActions.hiddenKnown ? engine.notificationSettings.activeMutes().filter { !engine.conversationActions.isHidden($0.id) } : []
        if !muted.isEmpty {
            Section("Hội thoại đã tắt thông báo") {
                ForEach(muted, id: \.id) { entry in
                    LabeledContent {
                        Button("Bật lại") { engine.unmuteConversation(entry.id) }
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(name(of: entry.id))
                            Text(entry.info.description()).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }

    private func name(of id: String) -> String {
        engine.conversations.first { $0.id == id }?.displayName ?? (id.hasPrefix("g") ? "Nhóm" : "Người dùng Zalo")
    }
}

/// Per-conversation "Tắt thông báo" menu items (official order: 1 giờ, 4 giờ, đến 8 giờ sáng, đến khi mở lại).
/// Use inside `.contextMenu { ConversationMuteMenu(conversationId: id) }` or a toolbar `Menu`.
struct ConversationMuteMenu: View {
    @Environment(ZaloEngine.self) private var engine
    let conversationId: String

    var body: some View {
        if engine.isConversationMuted(conversationId) {
            Button("Bật thông báo", systemImage: "bell") { engine.unmuteConversation(conversationId) }
        } else {
            Menu("Tắt thông báo", systemImage: "bell.slash") {
                ForEach(MuteDuration.allCases) { duration in
                    Button(duration.title) { engine.muteConversation(conversationId, duration) }
                }
            }
        }
    }
}
