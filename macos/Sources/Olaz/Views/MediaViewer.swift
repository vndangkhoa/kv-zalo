import SwiftUI
import AppKit

/// What the media viewer shows: a chat photo (loaded through `RemoteImage`, official resolver fallback and E2EE included)
/// or a video that is already on disk, plus who sent it and when (for the top bar and the window title).
struct MediaViewerItem: Identifiable, Equatable {
    enum Media: Equatable {
        case photo(url: URL?, width: Int, height: Int, encrypted: Bool)
        /// `size` is the display size when known; it only sizes the window.
        case video(file: URL, size: CGSize?)
    }

    let id = UUID()
    var media: Media
    var conversationId: String
    var messageId: String
    var senderName: String
    var date: Date

    /// Pixel size of the media when known (the window takes its aspect ratio).
    var mediaSize: CGSize? {
        switch media {
        case .photo(_, let width, let height, _):
            width > 0 && height > 0 ? CGSize(width: width, height: height) : nil
        case .video(_, let size):
            size
        }
    }

    var isPhoto: Bool {
        if case .photo = media { return true }
        return false
    }

    /// Window title (Window menu, VoiceOver): the sender, or the kind of media when the sender is unknown.
    var windowTitle: String {
        senderName.isEmpty ? (isPhoto ? "Ảnh" : "Video") : senderName
    }
}

/// How big the viewer window opens: as large as the media's aspect ratio allows inside most of the screen.
enum MediaViewerLayout {
    /// Share of the screen's visible area the window may take.
    static let screenShare: CGFloat = 0.85
    static let minimumContent = CGSize(width: 720, height: 520)
    /// Share of the screen for media of unknown size.
    static let unknownShare: CGFloat = 0.7
    /// Band above a photo for the traffic lights and the top bar, which stays up (a video's bars fade over the picture).
    static let photoTopBar: CGFloat = 52

    /// `topInset`: the band above the media, added to the window so the media itself still gets the aspect-fit size.
    static func contentSize(media: CGSize?, available: CGSize, topInset: CGFloat = 0) -> CGSize {
        var size: CGSize
        if let media, media.width > 0, media.height > 0 {
            let scale = min(available.width * screenShare / media.width,
                            (available.height * screenShare - topInset) / media.height)
            size = CGSize(width: media.width * scale, height: media.height * scale + topInset)
        } else {
            size = CGSize(width: available.width * unknownShare, height: available.height * unknownShare)
        }
        // A thin strip or tiny clip still gets a usable window (letterboxed), but never one bigger than the screen.
        size.width = min(max(size.width, minimumContent.width), available.width)
        size.height = min(max(size.height, minimumContent.height), available.height)
        return CGSize(width: size.width.rounded(), height: size.height.rounded())
    }
}

/// The one viewer window. Titled like a document window, but its content runs under the transparent titlebar.
final class MediaViewerWindow: NSWindow {
    init(contentSize: NSSize) {
        super.init(contentRect: NSRect(origin: .zero, size: contentSize),
                   styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                   backing: .buffered, defer: false)
        titlebarAppearsTransparent = true
        titleVisibility = .hidden
        titlebarSeparatorStyle = .none
        appearance = NSAppearance(named: .darkAqua)
        backgroundColor = NSColor(white: 0.04, alpha: 1)
        isRestorable = false
        isReleasedWhenClosed = false
        tabbingMode = .disallowed
        collectionBehavior = [.fullScreenPrimary]
        contentMinSize = NSSize(width: 480, height: 320)
    }

    override func cancelOperation(_ sender: Any?) {
        MediaViewer.shared.escape()
    }
}

/// Where the top bar sits: right of the traffic lights and level with them (read from the real buttons, because their
/// position differs between macOS versions and full screen has none).
struct ViewerTitlebar: Equatable {
    var leading: CGFloat = 84
    var centerY: CGFloat = 22
}

/// What the viewer window currently shows. One instance lives as long as the app; `present` swaps the item.
@MainActor
@Observable
final class MediaViewerState {
    private(set) var item: MediaViewerItem?
    private(set) var zoom = ZoomController()
    private(set) var playback: VideoPlayback?
    var isFullScreen = false
    var titlebar = ViewerTitlebar()

    func present(_ item: MediaViewerItem) {
        clear()
        self.item = item
        zoom = ZoomController()
        if case .video(let file, _) = item.media { playback = VideoPlayback(url: file) }
    }

    /// Stops the player and lets go of the item (so the full-resolution image and the video are freed).
    func clear() {
        playback?.stop()
        playback = nil
        item = nil
    }
}

/// Quick Look-style viewer for chat photos and videos: one big reusable window that resizes to the media, with zoom for
/// photos and a full player (scrubber, volume) for videos.
///
/// Privacy: hidden (PIN-locked) chats exist, so the viewer closes itself the moment its conversation is locked, the
/// hidden-chat state is unknown, or the account signs out (see `MediaViewerContent`).
@MainActor
final class MediaViewer: NSObject, NSWindowDelegate {
    static let shared = MediaViewer()

    private let state = MediaViewerState()
    private var window: MediaViewerWindow?
    private var hosting: NSHostingView<AnyView>?
    private var keyMonitor: Any?
    /// A swap that happened in full screen: the frame to take when leaving it.
    private var pendingFrame: NSRect?

    func show(_ item: MediaViewerItem, engine: ZaloEngine, shell: ShellModel) {
        // Privacy: a locked hidden chat must never get as far as ordering the window front, not even for a frame.
        guard !MediaViewer.mustClose(conversationId: item.conversationId, engine: engine) else { return }
        let screen = (window?.isVisible == true ? window?.screen : nil) ?? NSApp.keyWindow?.screen ?? NSApp.mainWindow?.screen ?? NSScreen.main
        let available = screen?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let content = MediaViewerLayout.contentSize(media: item.mediaSize, available: available.size,
                                                    topInset: item.isPhoto ? MediaViewerLayout.photoTopBar : 0)
        let frame = NSRect(x: available.midX - content.width / 2, y: available.midY - content.height / 2,
                           width: content.width, height: content.height)

        let window = self.window ?? makeWindow(size: content)
        hosting?.rootView = AnyView(MediaViewerRoot(state: state, engine: engine, shell: shell))
        state.present(item)
        window.title = item.windowTitle

        if window.styleMask.contains(.fullScreen) {
            pendingFrame = frame
        } else {
            if window.isMiniaturized { window.deminiaturize(nil) }
            // Animated only when the window is already up; a fresh one just appears at its size.
            window.setFrame(frame, display: true, animate: window.isVisible)
        }
        installKeyMonitor()
        NSApp.activate()
        window.makeKeyAndOrderFront(nil)
        updateTitlebar()
    }

    func close() {
        window?.close()
    }

    /// The viewer may only stay open while its conversation is readable: not locked (hidden chat without its PIN), hidden
    /// chats known (any conversation could still turn out to be one), and an account signed in.
    nonisolated static func mustClose(locked: Bool, hiddenKnown: Bool, signedIn: Bool) -> Bool {
        locked || !hiddenKnown || !signedIn
    }

    static func mustClose(conversationId: String, engine: ZaloEngine) -> Bool {
        mustClose(locked: engine.isConversationLocked(conversationId),
                  hiddenKnown: engine.conversationActions.hiddenKnown,
                  signedIn: engine.profile != nil && !engine.phase.isLogin)
    }

    /// Esc: leaves full screen first, else closes.
    func escape() {
        guard let window else { return }
        if window.styleMask.contains(.fullScreen) { window.toggleFullScreen(nil) } else { close() }
    }

    func toggleFullScreen() {
        window?.toggleFullScreen(nil)
    }

    private func makeWindow(size: CGSize) -> MediaViewerWindow {
        let window = MediaViewerWindow(contentSize: size)
        let hosting = NSHostingView(rootView: AnyView(EmptyView()))
        // The window decides its size (aspect-fit to the media); the SwiftUI content must not push back.
        hosting.sizingOptions = []
        window.contentView = hosting
        window.delegate = self
        self.window = window
        self.hosting = hosting
        return window
    }

    // MARK: NSWindowDelegate

    func windowWillClose(_ notification: Notification) {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
        pendingFrame = nil
        state.clear()
        state.isFullScreen = false
        // Drop the SwiftUI content too: the player and the decoded photo go with it.
        hosting?.rootView = AnyView(EmptyView())
    }

    func windowDidEnterFullScreen(_ notification: Notification) {
        state.isFullScreen = true
        updateTitlebar()
    }

    func windowDidExitFullScreen(_ notification: Notification) {
        state.isFullScreen = false
        if let pendingFrame, let window {
            window.setFrame(pendingFrame, display: true, animate: true)
        }
        pendingFrame = nil
        updateTitlebar()
    }

    func windowDidResize(_ notification: Notification) {
        updateTitlebar()
    }

    /// Reads where the traffic lights are so the top bar lines up with them.
    private func updateTitlebar() {
        guard let window else { return }
        if window.styleMask.contains(.fullScreen) {
            state.titlebar = ViewerTitlebar(leading: 20, centerY: 40)
            return
        }
        guard let button = window.standardWindowButton(.zoomButton), let container = button.superview else { return }
        let frame = container.convert(button.frame, to: nil)
        guard frame.width > 0, window.frame.height > 0 else { return }
        let metrics = ViewerTitlebar(leading: frame.maxX + 14, centerY: window.frame.height - frame.midY)
        if metrics != state.titlebar { state.titlebar = metrics }
    }

    // MARK: Keys

    /// A local monitor, because the key view is the scroll view or the player and both would take arrows and space.
    private func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard event.window is MediaViewerWindow else { return event }
            let consumed = MainActor.assumeIsolated { self?.handle(event) ?? false }
            return consumed ? nil : event
        }
    }

    /// True when the key was ours.
    private func handle(_ event: NSEvent) -> Bool {
        guard let item = state.item else { return false }
        let flags = event.modifierFlags.intersection([.command, .option, .control, .shift])
        let key = event.charactersIgnoringModifiers?.lowercased() ?? ""
        if flags.isEmpty {
            if event.keyCode == 53 { escape(); return true }
            if key == "f" {
                if !event.isARepeat { toggleFullScreen() }
                return true
            }
            guard let playback = state.playback else { return false }
            switch event.keyCode {
            case 49: if !event.isARepeat { playback.togglePlay() }
            case 123: playback.skip(by: -5)
            case 124: playback.skip(by: 5)
            case 126: playback.adjustVolume(by: 0.1)
            case 125: playback.adjustVolume(by: -0.1)
            default:
                guard key == "m" else { return false }
                if !event.isARepeat { playback.toggleMute() }
            }
            return true
        }
        // Preview's zoom shortcuts (⌘+ comes as ⌘⇧= on most layouts).
        guard item.isPhoto, flags.contains(.command), flags.isSubset(of: [.command, .shift]) else { return false }
        switch key {
        case "+", "=": state.zoom.zoomIn()
        case "-", "_": state.zoom.zoomOut()
        case "0": state.zoom.actualSize()
        case "9": state.zoom.fit()
        default: return false
        }
        return true
    }
}

// MARK: - SwiftUI

/// Dark, tinted, Vietnamese root of the viewer window (the window itself is always dark, whatever the app's appearance).
private struct MediaViewerRoot: View {
    let state: MediaViewerState
    let engine: ZaloEngine
    let shell: ShellModel
    @AppStorage(OlazPrefs.accent) private var accent = OlazAccent.blue

    private var mustClose: Bool {
        state.item.map { MediaViewer.mustClose(conversationId: $0.conversationId, engine: engine) } ?? false
    }

    var body: some View {
        ZStack {
            Color(white: 0.04)
            // Privacy: while the chat is not readable nothing of it is rendered, not even for the frame before the window closes.
            if let item = state.item, !mustClose {
                MediaViewerContent(state: state, item: item).id(item.id)
            }
        }
        .onChange(of: mustClose, initial: true) { _, mustClose in
            if mustClose { Task { @MainActor in MediaViewer.shared.close() } }
        }
        .ignoresSafeArea()
        .environment(engine)
        .environment(shell)
        .environment(\.locale, Locale(identifier: "vi_VN"))
        .environment(\.colorScheme, .dark)
        .tint(accent.color)
    }
}

private struct MediaViewerContent: View {
    let state: MediaViewerState
    let item: MediaViewerItem
    @Environment(ZaloEngine.self) private var engine

    /// The player's bars and the top bar fade together; photos keep the top bar.
    private var barsVisible: Bool { state.playback?.barsVisible ?? true }

    var body: some View {
        ZStack(alignment: .top) {
            media
            WindowDragArea().frame(height: 36)
            topBar
        }
    }

    // MARK: Media

    @ViewBuilder private var media: some View {
        switch item.media {
        case .photo(let url, _, _, let encrypted):
            RemoteImage(url: url, contentMode: .fit, maxPixelSize: 6144) { ProgressView().controlSize(.large) }
                // A full-size decode must not push the chat's thumbnails out of the shared cache; it lives as long as the viewer.
                .uncached()
                .resolvingWith(first: encrypted) { force in
                    await engine.resolveMedia(conversationId: item.conversationId, messageId: item.messageId, kind: .photo, force: force)
                }
                .onFailure { reason, retry in
                    MediaFailureView(failure: reason, retry: retry)
                        .frame(width: 340, height: 220)
                        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                }
                .rendering { image in ZoomableImageView(image: image, zoom: state.zoom) }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                // The top bar gets its own band instead of covering the top of the photo.
                .padding(.top, MediaViewerLayout.photoTopBar)
        case .video:
            if let playback = state.playback {
                VideoPlayerView(playback: playback, fullScreen: FullScreenControl(isOn: state.isFullScreen) {
                    MediaViewer.shared.toggleFullScreen()
                })
            }
        }
    }

    // MARK: Top bar

    private var topBar: some View {
        HStack(alignment: .center, spacing: 12) {
            info
                .onHover { state.playback?.setPointerOver("top", $0) }
            Spacer(minLength: 12)
            if item.isPhoto { ZoomControls(zoom: state.zoom) }
        }
        .padding(.leading, state.titlebar.leading)
        .padding(.trailing, 16)
        .padding(.top, max(8, state.titlebar.centerY - 22))
        .opacity(barsVisible ? 1 : 0)
        .allowsHitTesting(barsVisible)
        .animation(.easeOut(duration: 0.2), value: barsVisible)
    }

    private var info: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(item.windowTitle)
                .font(.system(size: 13, weight: .semibold))
                .lineLimit(1)
            Text(subtitle)
                .font(.system(size: 11.5))
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
        .glassEffect(.regular, in: Capsule())
        .accessibilityElement(children: .combine)
    }

    /// "Hôm nay · 14:32", plus the group's name when it differs from the sender (in a 1-1 chat it is the same person).
    private var subtitle: String {
        var parts = ["\(ChatFormat.daySeparator(item.date)) · \(ChatFormat.time(item.date))"]
        // Through `ConversationDetail.summary` so the self conversation reads "Cloud của tôi", not the official "My Documents".
        if let name = ConversationDetail.summary(for: item.conversationId, engine: engine)?.displayName, name != item.senderName {
            parts.append(name)
        }
        return parts.joined(separator: " · ")
    }
}

/// A strip that drags the window (the titlebar is under the content, so nothing else would).
private struct WindowDragArea: NSViewRepresentable {
    final class DragView: NSView {
        override var mouseDownCanMoveWindow: Bool { true }
        override func mouseDown(with event: NSEvent) { window?.performDrag(with: event) }
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    }

    func makeNSView(context: Context) -> DragView { DragView() }
    func updateNSView(_ view: DragView, context: Context) {}
}
