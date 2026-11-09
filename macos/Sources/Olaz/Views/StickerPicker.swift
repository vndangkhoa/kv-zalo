import SwiftUI

// The sticker picker (popover of the composer's sticker button) and the suggestion strip above the composer.
// Official counterparts: the sticker dock (`_changeStickerCat` ~10446802: tabs = recent first, then the downloaded sets in the user's order;
// `_sendSticker` ~10449524: source SEARCH_/RECENT_/"" + STICKER_SMALL_PANEL; `_startSearch` ~10454736: search debounced 500 ms) and the
// suggestion popover (module "it" ~8123800: horizontally scrolling stickers; Esc hides it; a click sends the sticker and clears the typed text).
// All data comes from the official sticker services through `ZaloEngine+Stickers.swift`; a send happens only from a click.

/// A sticker picture. Like the official dock, the picture is the still `stickerWebpUrl ?? stickerUrl`; where the sticker has an animated
/// source (sprite sheet / GIF) it plays while the pointer is on it (`animatesOnHover`). The chat bubble draws the still picture too.
private struct StickerThumb: View {
    let sticker: StickerEntry
    var size: CGFloat = 64
    var decodePixels: CGFloat = 160
    var animatesOnHover = true
    @State private var hovering = false

    var body: some View {
        ZStack {
            RemoteImage(url: sticker.url, contentMode: .fit, maxPixelSize: decodePixels) { placeholder }
            if animatesOnHover, hovering, let animation = sticker.animation {
                switch animation {
                case .sprite(let url, let frames, let frameMs):
                    StickerSpritePlayer(url: url, frames: frames, frameMs: frameMs, cellPixels: size * 2)
                case .animated(let url):
                    RemoteImage(url: url, contentMode: .fit, maxPixelSize: 320, animated: true) { Color.clear }
                }
            }
        }
        .frame(width: size, height: size)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }

    private var placeholder: some View {
        Image(systemName: "face.smiling").foregroundStyle(.tertiary)
    }
}

/// Plays a sprite sheet (`frames` equal pictures side by side, like the official `stickerSpriteUrl`) in a loop, `frameMs` per picture.
/// Nothing is drawn until the sheet has loaded and been cut into pictures; the clock ticks once per picture.
private struct StickerSpritePlayer: View {
    let url: URL
    let frames: Int
    let frameMs: Int
    /// Pixels one picture is shown at (the sheet is decoded at about `frames` × that, not at its full size).
    let cellPixels: CGFloat
    @State private var pictures: [CGImage] = []

    var body: some View {
        if !pictures.isEmpty {
            TimelineView(.periodic(from: .now, by: Double(max(frameMs, 1)) / 1000)) { context in
                let tick = Int(context.date.timeIntervalSinceReferenceDate * 1000) / max(frameMs, 1)
                Image(decorative: pictures[tick % pictures.count], scale: 1)
                    .resizable().aspectRatio(contentMode: .fit)
            }
        } else {
            Color.clear.task(id: url) { await load() }
        }
    }

    private func load() async {
        guard frames >= 2, let loaded = await MediaLoader.shared.image(for: url, maxPixelSize: CGFloat(frames) * cellPixels),
              let sheet = loaded.image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return }
        let width = sheet.width / frames
        // Not a strip of `frames` square pictures: show nothing animated rather than slices of something else.
        guard width > 0, abs(width - sheet.height) <= 2 else { return }
        let slices = (0..<frames).compactMap { sheet.cropping(to: CGRect(x: $0 * width, y: 0, width: width, height: sheet.height)) }
        if slices.count == frames { pictures = slices }
    }
}

// MARK: - Picker

struct StickerPicker: View {
    let conversationId: String
    /// Called after a sticker was sent (the popover closes).
    let onSent: () -> Void

    @Environment(ZaloEngine.self) private var engine
    @State private var tabs: [StickerTab] = []
    @State private var tabsState: LoadState = .loading
    @State private var selected: Int?
    @State private var stickers: [StickerEntry] = []
    @State private var gridState: LoadState = .loading
    @State private var query = ""
    @State private var found: [StickerEntry]?
    @State private var searchFailed = false
    @State private var sending: String?
    @State private var error: String?
    @FocusState private var searchFocused: Bool

    private enum LoadState { case loading, ready, failed }

    private var searching: Bool { !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        VStack(spacing: 0) {
            searchField
            Divider()
            content
            if let error {
                Text(error).font(.system(size: 12)).foregroundStyle(.orange).multilineTextAlignment(.center)
                    .padding(.horizontal, 12).padding(.vertical, 6)
            }
            if !searching, tabsState == .ready, !tabs.isEmpty {
                Divider()
                tabStrip
            }
        }
        .frame(width: 360, height: 420)
        .task { await loadTabs() }
        .task(id: selected) { await loadGrid() }
        .task(id: query) { await runSearch() }
    }

    // MARK: Parts

    private var searchField: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Tìm kiếm sticker", text: $query)
                .textFieldStyle(.plain)
                .focused($searchFocused)
                .help("Gõ từ khóa để tìm Sticker")
                .accessibilityLabel("Tìm kiếm sticker")
            if !query.isEmpty {
                Button("Xóa", systemImage: "xmark.circle.fill") { query = "" }
                    .labelStyle(.iconOnly).buttonStyle(.plain).foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 9)
    }

    @ViewBuilder private var content: some View {
        if searching {
            if let found {
                if found.isEmpty { message("Không có Sticker phù hợp. Vui lòng thử từ khóa khác.") } else { grid(found, source: .search) }
            } else if searchFailed {
                message("Không tìm được sticker. Hãy thử lại sau.")
            } else {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else {
            switch tabsState {
            case .loading: ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
            case .failed: message("Không tải được danh sách sticker. Hãy thử lại sau.")
            case .ready:
                if tabs.isEmpty {
                    message("Bạn chưa có bộ sticker nào.")
                } else {
                    switch gridState {
                    case .loading: ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
                    case .failed: message("Không tải được bộ sticker này. Hãy thử lại sau.")
                    case .ready:
                        if stickers.isEmpty {
                            message("Bộ sticker này chưa có sticker nào.")
                        } else {
                            grid(stickers, source: selected == StickerTab.recentId ? .recent : .category)
                        }
                    }
                }
            }
        }
    }

    private func message(_ text: String) -> some View {
        Text(text).font(.system(size: 12.5)).foregroundStyle(.secondary).multilineTextAlignment(.center)
            .padding(16).frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func grid(_ items: [StickerEntry], source: StickerSource) -> some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 70, maximum: 90), spacing: 6)], spacing: 6) {
                ForEach(items) { sticker in
                    Button { send(sticker, source: source) } label: {
                        StickerThumb(sticker: sticker)
                            .opacity(sending == sticker.id ? 0.4 : 1)
                    }
                    .buttonStyle(.plain)
                    .disabled(sending != nil)
                    .help("Gửi sticker này")
                    .accessibilityLabel("Sticker")
                }
            }
            .padding(10)
        }
        .id(source == .search ? "search" : "tab-\(selected ?? 0)")
    }

    private var tabStrip: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 4) {
                ForEach(tabs) { tab in
                    Button { selected = tab.id } label: {
                        tabIcon(tab)
                            .frame(width: 34, height: 34)
                            .background(RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(selected == tab.id ? AnyShapeStyle(.tint.opacity(0.16)) : AnyShapeStyle(.clear)))
                    }
                    .buttonStyle(.plain)
                    .help(tab.isRecent ? "Gần đây" : tab.name)
                    .accessibilityLabel(tab.isRecent ? "Gần đây" : tab.name)
                    .accessibilityAddTraits(selected == tab.id ? .isSelected : [])
                }
            }
            .padding(.horizontal, 8).padding(.vertical, 6)
        }
        .scrollIndicators(.never)
    }

    @ViewBuilder private func tabIcon(_ tab: StickerTab) -> some View {
        if tab.isRecent {
            Image(systemName: "clock").font(.system(size: 16)).foregroundStyle(.secondary)
        } else {
            RemoteImage(url: tab.icon, contentMode: .fit, maxPixelSize: 64) {
                Image(systemName: "face.smiling").foregroundStyle(.tertiary)
            }
            .frame(width: 26, height: 26)
        }
    }

    // MARK: Loading

    /// The sticker service may not be resolved yet right after launch: a few tries, then say so.
    private func loadTabs() async {
        for attempt in 0..<3 {
            if let list = await engine.stickerTabs() {
                tabs = list
                if selected == nil || !list.contains(where: { $0.id == selected }) { selected = list.first?.id }
                tabsState = .ready
                return
            }
            if attempt < 2 { try? await Task.sleep(for: .seconds(1.5)) }
            if Task.isCancelled { return }
        }
        tabsState = .failed
    }

    private func loadGrid() async {
        guard let id = selected else { return }
        gridState = .loading
        stickers = []
        for attempt in 0..<2 {
            if let list = await engine.stickers(inTab: id) {
                guard !Task.isCancelled else { return }
                stickers = list
                gridState = .ready
                return
            }
            if attempt < 1 { try? await Task.sleep(for: .seconds(1.5)) }
            if Task.isCancelled { return }
        }
        gridState = .failed
    }

    /// Official dock search: a pause of 500 ms after the last keystroke, then the keyword is searched.
    private func runSearch() async {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        found = nil
        searchFailed = false
        guard !text.isEmpty else { return }
        try? await Task.sleep(for: .milliseconds(500))
        guard !Task.isCancelled else { return }
        let result = await engine.searchStickers(text)
        guard !Task.isCancelled else { return }
        if let result { found = result } else { searchFailed = true }
    }

    // MARK: Sending

    private func send(_ sticker: StickerEntry, source: StickerSource) {
        guard sending == nil else { return }
        // The conversation on screen is not the one this picker was opened for (switched meanwhile): nothing is sent.
        guard engine.openConversationId == conversationId else { error = StickerNotice.notReady; return }
        sending = sticker.id
        error = nil
        Task {
            let outcome = await engine.sendSticker(sticker, in: conversationId, source: source)
            sending = nil
            switch outcome {
            case .done: onSent()
            case .locked(let text), .failed(let text), .unsupported(let text), .pending(let text): error = text
            default: break
            }
        }
    }
}

// MARK: - Suggestion strip

/// Stickers suggested for the text being typed. Horizontal, like the official popover (`stickerWebpUrl ?? stickerUrl` pictures, 65 px there);
/// Esc (handled by the composer) or × hides it.
struct TypingStickerStrip: View {
    let stickers: [StickerEntry]
    let sending: String?
    let pick: (StickerEntry, Int) -> Void
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 0) {
            ScrollView(.horizontal) {
                LazyHStack(spacing: 6) {
                    ForEach(Array(stickers.enumerated()), id: \.element.id) { index, sticker in
                        Button { pick(sticker, index) } label: {
                            StickerThumb(sticker: sticker, size: 56, decodePixels: 168, animatesOnHover: false)
                                .opacity(sending == sticker.id ? 0.4 : 1)
                        }
                        .buttonStyle(.plain)
                        .disabled(sending != nil)
                        .help("Gửi sticker này")
                        .accessibilityLabel("Gửi sticker gợi ý")
                    }
                }
                .padding(.horizontal, 8).padding(.vertical, 6)
            }
            .scrollIndicators(.never)
            Button("Ẩn gợi ý", systemImage: "xmark") { dismiss() }
                .labelStyle(.iconOnly).buttonStyle(.plain).foregroundStyle(.secondary)
                .padding(.horizontal, 10)
                .help("Bấm phím Esc để ẩn")
        }
        .frame(height: 70)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Gợi ý Sticker")
    }
}

/// Short notices of the sticker UI.
enum StickerNotice {
    /// A click while another conversation is on screen (the same wording as a conversation that is not ready, ZaloEngine+Stickers outcome).
    static let notReady = "Cuộc trò chuyện chưa sẵn sàng, hãy thử lại sau giây lát."
}
