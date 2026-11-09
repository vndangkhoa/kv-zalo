import Testing
import AppKit
@testable import Olaz

@Suite struct MediaViewerTests {
    // MARK: Window size

    @Test func portraitPhotoOpensNearlyScreenTall() {
        let screen = CGSize(width: 1512, height: 944)
        let size = MediaViewerLayout.contentSize(media: CGSize(width: 1080, height: 1920), available: screen)
        #expect(size.height == (screen.height * 0.85).rounded())
        #expect(size.width == MediaViewerLayout.minimumContent.width)   // 451 wide is below the minimum
    }

    @Test func landscapePhotoOpensNearlyScreenWide() {
        let screen = CGSize(width: 1512, height: 944)
        let size = MediaViewerLayout.contentSize(media: CGSize(width: 1920, height: 1080), available: screen)
        #expect(size.width == (screen.width * 0.85).rounded())
        #expect(abs(size.width / size.height - 1920.0 / 1080.0) < 0.01)
    }

    @Test func photoTopBarIsAddedAboveTheAspectFitMedia() {
        let screen = CGSize(width: 1512, height: 944)
        let inset = MediaViewerLayout.photoTopBar
        let size = MediaViewerLayout.contentSize(media: CGSize(width: 3060, height: 1388), available: screen, topInset: inset)
        #expect(abs(size.width / (size.height - inset) - 3060.0 / 1388.0) < 0.01)
        #expect(size.height <= (screen.height * 0.85).rounded())
    }

    @Test func smallMediaIsScaledUpToFillTheShare() {
        let screen = CGSize(width: 1512, height: 944)
        let size = MediaViewerLayout.contentSize(media: CGSize(width: 320, height: 240), available: screen)
        #expect(size.width > 1000)
        #expect(size.height <= screen.height * 0.85 + 1)
    }

    @Test func unknownSizeTakesMostOfTheScreen() {
        let screen = CGSize(width: 1512, height: 944)
        let size = MediaViewerLayout.contentSize(media: nil, available: screen)
        #expect(size == CGSize(width: (screen.width * 0.7).rounded(), height: (screen.height * 0.7).rounded()))
        #expect(MediaViewerLayout.contentSize(media: CGSize(width: 0, height: 100), available: screen) == size)
    }

    @Test func neverBiggerThanTheScreen() {
        let tiny = CGSize(width: 600, height: 400)
        let size = MediaViewerLayout.contentSize(media: CGSize(width: 1000, height: 1000), available: tiny)
        #expect(size.width <= tiny.width && size.height <= tiny.height)
    }

    @Test func itemTitleFallsBackToTheKindOfMedia() {
        let date = Date(timeIntervalSince1970: 0)
        let photo = MediaViewerItem(media: .photo(url: nil, width: 10, height: 20, encrypted: false),
                                    conversationId: "c", messageId: "m", senderName: "", date: date)
        let video = MediaViewerItem(media: .video(file: URL(fileURLWithPath: "/x.mp4"), size: CGSize(width: 4, height: 3)),
                                    conversationId: "c", messageId: "m", senderName: "An", date: date)
        #expect(photo.windowTitle == "Ảnh" && photo.isPhoto && photo.mediaSize == CGSize(width: 10, height: 20))
        #expect(video.windowTitle == "An" && !video.isPhoto && video.mediaSize == CGSize(width: 4, height: 3))
    }

    // MARK: Decoding

    /// A JPEG of `size` pixels carrying an EXIF orientation.
    private func jpeg(size: CGSize, orientation: Int?) throws -> Data {
        let context = try #require(CGContext(data: nil, width: Int(size.width), height: Int(size.height), bitsPerComponent: 8, bytesPerRow: 0,
                                             space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        context.fill(CGRect(origin: .zero, size: size))
        let image = try #require(context.makeImage())
        let data = NSMutableData()
        let destination = try #require(CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil))
        var properties: [CFString: Any] = [:]
        if let orientation { properties[kCGImagePropertyOrientation] = orientation }
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        #expect(CGImageDestinationFinalize(destination))
        return data as Data
    }

    @Test func rotatedPhotosAreUprightAtAnySize() throws {
        // Stored 400x200 with "rotate 90° clockwise": shown 200x400, also when it needs no downsampling (the viewer's 6144).
        let rotated = try jpeg(size: CGSize(width: 400, height: 200), orientation: 6)
        #expect(MediaLoader.decode(rotated, maxPixelSize: 6144)?.size == NSSize(width: 200, height: 400))
        #expect(MediaLoader.decode(rotated, maxPixelSize: 100)?.size == NSSize(width: 50, height: 100))
        let upright = try jpeg(size: CGSize(width: 400, height: 200), orientation: nil)
        #expect(MediaLoader.decode(upright, maxPixelSize: 6144)?.size == NSSize(width: 400, height: 200))
    }

    @Test func uncachedDecodesStayOutOfTheSharedMemoryCache() async throws {
        // A file under Caches/Olaz is an allowed local root, so no network is needed.
        let dir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("Olaz", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let file = dir.appendingPathComponent("viewer-test-\(UUID().uuidString).jpg")
        try jpeg(size: CGSize(width: 300, height: 200), orientation: nil).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let loader = MediaLoader(cacheDirectory: FileManager.default.temporaryDirectory.appendingPathComponent("olaz-test-\(UUID().uuidString)"))
        let big = try #require(await loader.image(for: file, maxPixelSize: 6144, cache: false))
        #expect(big.image.size == NSSize(width: 300, height: 200))
        #expect(MediaLoader.cachedImage(for: file, maxPixelSize: 6144) == nil)
        // Normal callers still fill the cache.
        #expect(await loader.image(for: file, maxPixelSize: 6144) != nil)
        #expect(MediaLoader.cachedImage(for: file, maxPixelSize: 6144) != nil)
    }

    // MARK: Privacy

    @Test func viewerClosesWhenItsChatIsNotReadable() {
        #expect(!MediaViewer.mustClose(locked: false, hiddenKnown: true, signedIn: true))
        #expect(MediaViewer.mustClose(locked: true, hiddenKnown: true, signedIn: true))
        #expect(MediaViewer.mustClose(locked: false, hiddenKnown: false, signedIn: true))
        #expect(MediaViewer.mustClose(locked: false, hiddenKnown: true, signedIn: false))
    }

    @MainActor @Test func hiddenChatLocksTheViewerUntilItsPINUnlocks() {
        let engine = ZaloEngine()
        // No hidden-chat state and no account yet: nothing may be shown.
        #expect(MediaViewer.mustClose(conversationId: "h1", engine: engine))
        engine.conversationActions.apply(json: ["hidden": ["enabled": true, "hasPin": true, "ids": ["h1"]]])
        #expect(engine.isConversationLocked("h1") && MediaViewer.mustClose(conversationId: "h1", engine: engine))
        engine.unlockHiddenConversation("h1")
        #expect(!engine.isConversationLocked("h1"))
        engine.conversationUI.relock()
        #expect(engine.isConversationLocked("h1") && MediaViewer.mustClose(conversationId: "h1", engine: engine))
    }

    /// Animated zooms take their time under a loaded test run: wait for `condition` instead of for a fixed delay.
    @MainActor private func settle(_ condition: () -> Bool) async {
        for _ in 0..<300 where !condition() { try? await Task.sleep(for: .milliseconds(10)) }
    }

    @MainActor @Test func lockedChatNeverOrdersTheViewerFront() {
        _ = NSApplication.shared
        let item = MediaViewerItem(media: .photo(url: nil, width: 10, height: 10, encrypted: false),
                                   conversationId: "h1", messageId: "m", senderName: "An", date: Date())
        MediaViewer.shared.show(item, engine: ZaloEngine(), shell: ShellModel())   // no account yet: must close
        #expect(!NSApp.windows.contains { $0 is MediaViewerWindow })
    }

    // MARK: Zoom

    @MainActor private func zoomView(_ image: CGSize, in frame: CGSize = CGSize(width: 800, height: 600)) -> ZoomScrollView {
        let view = ZoomScrollView(frame: NSRect(origin: .zero, size: frame))
        view.setImage(NSImage(size: image))
        return view
    }

    @MainActor @Test func imageStartsFittedAndCentered() {
        let view = zoomView(CGSize(width: 1600, height: 600))
        #expect(view.magnification == 0.5 && view.isFitted)
        // 1600x600 at 0.5 is 800x300: the clip view is 1600x1200 document units, the image sits in its middle.
        #expect(view.contentView.bounds.midX == 800 && view.contentView.bounds.midY == 300)
    }

    @Test @MainActor func imageSetBeforeTheViewHasASizeFitsOnceItGetsOne() {
        // SwiftUI creates the view at zero size and lays it out afterwards.
        let view = ZoomScrollView(frame: .zero)
        view.setImage(NSImage(size: NSSize(width: 1600, height: 1200)))
        view.setFrameSize(NSSize(width: 800, height: 600))
        #expect(view.magnification == 0.5 && view.isFitted)
    }

    @Test @MainActor func smallImagesAreScaledUpToFill() {
        let view = zoomView(CGSize(width: 400, height: 300))
        #expect(view.magnification == 2 && view.isFitted)
        #expect(view.minMagnification == 1)   // a small image can be shrunk to actual size, not below
    }

    @Test @MainActor func fittedImageFollowsWindowResizes() {
        let view = zoomView(CGSize(width: 1600, height: 1200))
        view.setFrameSize(NSSize(width: 400, height: 600))
        #expect(view.magnification == 0.25 && view.isFitted)
        view.setFrameSize(NSSize(width: 1600, height: 1200))
        #expect(view.magnification == 1 && view.isFitted)
    }

    @Test @MainActor func zoomedImageKeepsItsMagnificationOnResize() {
        let view = zoomView(CGSize(width: 1600, height: 1200))
        view.zoom(to: 2, anchoredAt: NSPoint(x: 400, y: 300))
        #expect(view.magnification == 2 && !view.isFitted)
        view.setFrameSize(NSSize(width: 1000, height: 700))
        #expect(view.magnification == 2 && !view.isFitted)
    }

    @Test @MainActor func pinchStopsTheImageFollowingFit() {
        let view = zoomView(CGSize(width: 1600, height: 1200))
        NotificationCenter.default.post(name: NSScrollView.willStartLiveMagnifyNotification, object: view)
        view.magnification = 1   // what the pinch does
        view.setFrameSize(NSSize(width: 1000, height: 700))
        #expect(view.magnification == 1 && !view.isFitted)
    }

    @Test @MainActor func aResizeToTheSameSizeDoesNotRefit() {
        let view = zoomView(CGSize(width: 1600, height: 1200))
        view.magnification = 1
        view.setFrameSize(view.frame.size)
        #expect(view.magnification == 1)
    }

    @Test @MainActor func smartMagnifyTogglesLikeADoubleClickAndFitsAgainAtFit() async throws {
        let view = zoomView(CGSize(width: 1600, height: 1200))
        let event = try #require(NSEvent.mouseEvent(with: .leftMouseDown, location: NSPoint(x: 400, y: 300), modifierFlags: [],
                                                    timestamp: 0, windowNumber: 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        view.smartMagnify(with: event)
        await settle { view.magnification == 1 }
        #expect(view.magnification == 1 && !view.isFitted)
        view.setFrameSize(NSSize(width: 1000, height: 700))
        #expect(view.magnification == 1)   // zoomed in: no longer follows the window
        view.smartMagnify(with: event)
        await settle { view.isFitted }
        try await Task.sleep(for: .milliseconds(150))   // the completion handler that re-arms fit-following
        #expect(view.isFitted)
        view.setFrameSize(NSSize(width: 400, height: 300))
        #expect(view.isFitted && view.magnification == 0.25)   // ... and fitted again, it does
    }

    @Test @MainActor func wheelZoomKeepsThePointUnderThePointer() {
        let view = zoomView(CGSize(width: 1600, height: 1200))
        let pointer = NSPoint(x: 100, y: 100)
        let before = view.contentView.convert(pointer, from: nil)
        view.zoom(to: 4, anchoredAt: pointer)
        let after = view.contentView.convert(pointer, from: nil)
        #expect(view.magnification == 4)
        #expect(abs(before.x - after.x) < 0.001 && abs(before.y - after.y) < 0.001)
    }

    @Test @MainActor func magnificationIsClamped() {
        let view = zoomView(CGSize(width: 1600, height: 1200))
        view.zoom(to: 100, anchoredAt: .zero)
        #expect(view.magnification == view.maxMagnification && view.maxMagnification >= 8)
        view.zoom(to: 0.01, anchoredAt: .zero)
        #expect(view.magnification == view.minMagnification)
    }

    @Test @MainActor func controllerMirrorsTheScrollViewAndGoesQuietOnTeardown() async throws {
        let controller = ZoomController()
        let view = ZoomScrollView(frame: NSRect(x: 0, y: 0, width: 800, height: 600))
        view.attach(controller)
        view.setImage(NSImage(size: NSSize(width: 1600, height: 1200)))
        try await Task.sleep(for: .milliseconds(80))
        #expect(controller.hasContent && controller.isFitted && controller.percent == 50)
        controller.actualSize()
        try await Task.sleep(for: .milliseconds(700))
        #expect(view.magnification == 1 && controller.percent == 100 && !controller.isFitted)
        controller.toggleActualSize()   // not fitted -> fit
        try await Task.sleep(for: .milliseconds(700))
        #expect(view.magnification == 0.5 && controller.isFitted)
        view.teardown()
        try await Task.sleep(for: .milliseconds(80))
        #expect(!controller.hasContent)
    }

    // MARK: Player

    /// One second of silence as a WAV: playable, audio only.
    private func silentWAV() throws -> URL {
        let rate = 8000, samples = rate
        var data = Data()
        func put32(_ v: Int) { withUnsafeBytes(of: UInt32(v).littleEndian) { data.append(contentsOf: $0) } }
        func put16(_ v: Int) { withUnsafeBytes(of: UInt16(v).littleEndian) { data.append(contentsOf: $0) } }
        data.append(contentsOf: Array("RIFF".utf8)); put32(36 + samples * 2)
        data.append(contentsOf: Array("WAVEfmt ".utf8)); put32(16); put16(1); put16(1); put32(rate); put32(rate * 2); put16(2); put16(16)
        data.append(contentsOf: Array("data".utf8)); put32(samples * 2)
        data.append(Data(count: samples * 2))
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("olaz-test-\(UUID().uuidString).wav")
        try data.write(to: url)
        return url
    }

    @MainActor private func waitUntilLoaded(_ playback: VideoPlayback) async {
        for _ in 0..<100 where playback.status == .loading { try? await Task.sleep(for: .milliseconds(50)) }
    }

    /// Runs `body` with the persisted volume / mute restored afterwards (the keys are shared by every test and player).
    @MainActor private func preservingVolumePrefs(_ body: () async throws -> Void) async rethrows {
        let defaults = UserDefaults.standard
        let volume = defaults.object(forKey: OlazPrefs.videoVolume), muted = defaults.object(forKey: OlazPrefs.videoMuted)
        defer {
            defaults.set(volume, forKey: OlazPrefs.videoVolume)
            defaults.set(muted, forKey: OlazPrefs.videoMuted)
        }
        try await body()
    }

    @Test func timeStrings() {
        #expect(VideoPlayback.timeString(0) == "0:00")
        #expect(VideoPlayback.timeString(65.9) == "1:05")
        #expect(VideoPlayback.timeString(3723) == "1:02:03")
        #expect(VideoPlayback.timeString(.nan) == "0:00")
        #expect(VideoPlayback.timeString(-4) == "0:00")
    }

    @Test @MainActor func audioFileLoadsAsAudioOnlyWithBarsAlwaysUp() async throws {
        let url = try silentWAV()
        defer { try? FileManager.default.removeItem(at: url) }
        let playback = VideoPlayback(url: url, autoplay: false)
        await waitUntilLoaded(playback)
        #expect(playback.status == .ready)
        #expect(!playback.hasVideo && playback.barsVisible)
        #expect(abs(playback.duration - 1) < 0.05)
        playback.skip(by: 5)   // clamps to the end and counts as ended
        #expect(playback.currentTime == playback.duration && playback.ended)
        playback.skip(by: -5)
        #expect(playback.currentTime == 0 && !playback.ended)
        playback.stop()
    }

    @MainActor private func waitUntilSeekLands(_ playback: VideoPlayback) async {
        for _ in 0..<200 where playback.isSeeking { try? await Task.sleep(for: .milliseconds(10)) }
    }

    @Test @MainActor func scrubEndsAtOnceAndPlaybackResumesEvenIfALaterSeekSupersedesTheExactOne() async throws {
        let url = try silentWAV()
        defer { try? FileManager.default.removeItem(at: url) }
        let playback = VideoPlayback(url: url, autoplay: false)
        await waitUntilLoaded(playback)
        playback.play()
        try #require(playback.isPlaying)
        playback.scrub(to: 0.3)
        #expect(playback.isScrubbing && !playback.isPlaying)
        playback.endScrub(at: 0.3)
        #expect(!playback.isScrubbing && playback.isSeeking)   // over at once; only the seek is still on its way
        playback.skip(by: 0.1)                                  // e.g. → right after letting go: cancels the exact seek
        await waitUntilSeekLands(playback)
        #expect(!playback.isSeeking && !playback.isScrubbing && playback.isPlaying)   // the newest seek resumed it
        playback.stop()
    }

    @Test @MainActor func scrubWhilePausedStaysPausedAndFollowsThePlayerAgain() async throws {
        let url = try silentWAV()
        defer { try? FileManager.default.removeItem(at: url) }
        let playback = VideoPlayback(url: url, autoplay: false)
        await waitUntilLoaded(playback)
        playback.scrub(to: 0.5)
        playback.endScrub(at: 0.5)
        await waitUntilSeekLands(playback)
        #expect(!playback.isScrubbing && !playback.isSeeking && !playback.isPlaying)
        #expect(abs(playback.currentTime - 0.5) < 0.01)
        playback.stop()
    }

    @Test @MainActor func pointerIsOnlyHiddenOverTheKeyWindowOfThePlayer() {
        _ = NSApplication.shared
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 400, height: 300), styleMask: [.titled], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        let inside = NSPoint(x: 200, y: 200)
        #expect(!VideoPlayback.mayHidePointer(in: nil, mouse: inside))     // no window of its own (yet)
        #expect(!VideoPlayback.mayHidePointer(in: window, mouse: inside))  // some other window is key
    }

    @Test @MainActor func unplayableFileFails() async {
        let playback = VideoPlayback(url: URL(fileURLWithPath: "/nonexistent/olaz.mp4"), autoplay: false)
        await waitUntilLoaded(playback)
        #expect(playback.status == .failed && playback.barsVisible)
        playback.stop()
    }

    @Test @MainActor func volumeAndMutePersist() async throws {
        try await preservingVolumePrefs {
            let url = try silentWAV()
            defer { try? FileManager.default.removeItem(at: url) }
            let playback = VideoPlayback(url: url, autoplay: false)
            playback.setVolume(0.4)
            #expect(playback.player.volume == 0.4 && !playback.isMuted)
            playback.toggleMute()
            #expect(playback.isMuted && playback.player.isMuted && playback.displayedVolume == 0)
            #expect(playback.volumeSymbol == "speaker.slash.fill")
            // The next video starts with the same level and mute.
            let next = VideoPlayback(url: url, autoplay: false)
            #expect(next.volume == 0.4 && next.isMuted)
            next.adjustVolume(by: 0.1)   // raising a muted player un-mutes it at its own level first
            #expect(!next.isMuted && abs(next.volume - 0.4) < 0.0001)
            next.adjustVolume(by: 0.1)
            #expect(abs(next.volume - 0.5) < 0.0001 && next.volumeSymbol == "speaker.wave.2.fill")
            next.setVolume(0)
            #expect(next.volumeSymbol == "speaker.slash.fill")
            next.toggleMute()   // un-mute from 0 brings back a audible level
            #expect(!next.isMuted && next.volume == 0.5)
            next.setVolume(1.5)
            #expect(next.volume == 1 && next.volumeSymbol == "speaker.wave.3.fill")
            playback.stop()
            next.stop()
        }
    }
}
