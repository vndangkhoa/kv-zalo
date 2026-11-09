import SwiftUI

/// "Gợi ý Sticker" settings section. Embed inside the Settings `Form` (it contributes a `Section`):
///
///     Form { … StickerSettingsSection() … }
///
/// The toggle reads / writes the official client's own setting (key `auto_sticker`, "Cài đặt ▸ Tiện ích ▸ Gợi ý Sticker"), the one the
/// official chat box checks before it suggests stickers, so it behaves like Zalo PC's. See sticker-adapter.js `suggestSetting`.
struct StickerSettingsSection: View {
    @Environment(ZaloEngine.self) private var engine
    private let state = StickerSuggestState.shared
    @State private var busy = false
    @State private var failed = false

    var body: some View {
        Section {
            Toggle(isOn: Binding(get: { state.setting?.enabled ?? true }, set: change)) {
                Text("Hiện gợi ý Sticker phù hợp với nội dung tin nhắn đang soạn")
            }
            .disabled(state.setting == nil || busy)
            if failed {
                Text("Không thể đổi cài đặt này lúc này. Hãy thử lại sau.").font(.caption).foregroundStyle(.orange)
            }
        } header: {
            Text("Gợi ý Sticker")
        }
        .task { await load() }
    }

    /// The sticker service may not be resolved yet right after launch: a few tries.
    private func load() async {
        for attempt in 0..<4 {
            await state.load(from: engine)
            if state.setting != nil || Task.isCancelled { return }
            if attempt < 3 { try? await Task.sleep(for: .seconds(1.5)) }
        }
    }

    private func change(_ on: Bool) {
        guard !busy else { return }
        busy = true
        failed = false
        Task {
            let now = await engine.setStickerSuggest(enabled: on)
            busy = false
            if now?.enabled != on { failed = true }
        }
    }
}
