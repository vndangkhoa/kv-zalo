import SwiftUI
import AppKit

/// Why a remote image is not shown.
enum MediaFailure: Equatable, Sendable {
    /// Gone from the CDN and past Zalo's shelf life for media (official "expired" state).
    case expired
    /// Could not be loaded (network, server); retrying may work.
    case unavailable
}

/// Loads CDN media (avatars, photos, stickers) through `MediaLoader`
/// (URLCache + DNS-over-HTTPS fallback + downsampling + in-memory cache).
/// Shows `placeholder` while loading and fades the image in. With `animated: true`, multi-frame images
/// (GIF stickers) play via an `NSImageView`.
///
/// Failure handling is opt-in so avatars keep their initials placeholder:
/// * `.onFailure { reason, retry in … }` replaces the placeholder with a failure view once the image
///   cannot be loaded (instead of leaving a loading spinner up forever); `retry()` loads again.
/// * `.resolvingWith { force in … }` is asked for a local file when the plain URL failed
///   (the official client's media loader, see `ZaloEngine.resolveMedia`).
/// * `.rendering { image in … }` draws the loaded still image with the caller's view instead of a plain `Image`
///   (the media viewer's zoomable scroll view).
/// * `.uncached()` keeps the decoded image out of the shared memory cache (full-size viewer photos).
struct RemoteImage<Placeholder: View>: View {
    let url: URL?
    var contentMode: ContentMode = .fill
    /// Longest side, in pixels, still images are downsampled to.
    var maxPixelSize: CGFloat = 1024
    var animated: Bool = false
    @ViewBuilder var placeholder: () -> Placeholder
    fileprivate var failureContent: ((MediaFailure, @escaping () -> Void) -> AnyView)?
    fileprivate var resolver: ((_ force: Bool) async -> MediaResolution)?
    fileprivate var resolverFirst = false
    fileprivate var renderer: ((NSImage) -> AnyView)?
    fileprivate var cachesDecoded = true

    @State private var image: NSImage?
    @State private var isAnimated = false
    @State private var failure: MediaFailure?
    @State private var attempt = 0

    init(url: URL?, contentMode: ContentMode = .fill, maxPixelSize: CGFloat = 1024, animated: Bool = false,
         @ViewBuilder placeholder: @escaping () -> Placeholder) {
        self.url = url
        self.contentMode = contentMode
        self.maxPixelSize = maxPixelSize
        self.animated = animated
        self.placeholder = placeholder
        var initial: NSImage?
        var initialAnimated = false
        if let url {
            if animated, let anim = MediaLoader.cachedAnimatedImage(for: url) {
                initial = anim; initialAnimated = true
            } else {
                initial = MediaLoader.cachedImage(for: url, maxPixelSize: maxPixelSize)
            }
        }
        _image = State(initialValue: initial)
        _isAnimated = State(initialValue: initialAnimated)
    }

    /// Shows `content` when the image cannot be loaded; `retry` starts over.
    func onFailure<Failure: View>(@ViewBuilder _ content: @escaping (MediaFailure, _ retry: @escaping () -> Void) -> Failure) -> Self {
        var copy = self
        copy.failureContent = { reason, retry in AnyView(content(reason, retry)) }
        return copy
    }

    /// Asked for a local file (or a verdict) after the direct URL failed; `force` is true on explicit retries.
    /// `first`: skip the direct download and ask the resolver straight away (end-to-end encrypted media is
    /// unreadable at its CDN URL; only the official downloader decrypts it).
    func resolvingWith(first: Bool = false, _ resolver: @escaping (_ force: Bool) async -> MediaResolution) -> Self {
        var copy = self
        copy.resolver = resolver
        copy.resolverFirst = first
        return copy
    }

    /// Draws a loaded still image with `content` (animated images keep their own view).
    func rendering<Content: View>(@ViewBuilder _ content: @escaping (NSImage) -> Content) -> Self {
        var copy = self
        copy.renderer = { AnyView(content($0)) }
        return copy
    }

    /// Decodes without storing the result in the shared memory cache: a full-size photo (tens of MB) would push out the
    /// thumbnails and avatars the chat list scrolls over. It is freed with the view instead.
    func uncached() -> Self {
        var copy = self
        copy.cachesDecoded = false
        return copy
    }

    private struct LoadID: Equatable { let url: URL?; let attempt: Int }

    var body: some View {
        Group {
            if let image {
                if isAnimated {
                    AnimatedImageView(image: image, contentMode: contentMode).transition(.opacity)
                } else if let renderer {
                    renderer(image).transition(.opacity)
                } else {
                    Image(nsImage: image)
                        .resizable()
                        .aspectRatio(contentMode: contentMode)
                        .transition(.opacity)
                }
            } else if let failure, let failureContent {
                failureContent(failure) { attempt += 1 }
            } else {
                placeholder()
            }
        }
        .task(id: LoadID(url: url, attempt: attempt)) { await load() }
    }

    private func load() async {
        let retrying = attempt > 0
        failure = nil
        guard let url else {
            image = nil; isAnimated = false
            if failureContent != nil { failure = .unavailable }
            return
        }
        if retrying { await MediaLoader.shared.forgetFailures(for: url) }
        let tryAnimated = animated && !MediaLoader.isKnownStill(url)
        if tryAnimated, let cached = MediaLoader.cachedAnimatedImage(for: url) {
            image = cached; isAnimated = true
            return
        }
        if let cached = MediaLoader.cachedImage(for: url, maxPixelSize: maxPixelSize) {
            image = cached; isAnimated = false           // frame 0 now; may upgrade to animation below
            if !tryAnimated { return }
        } else {
            image = nil; isAnimated = false
        }
        if tryAnimated {
            if let anim = await MediaLoader.shared.animatedImage(for: url), !Task.isCancelled {
                withAnimation(.easeOut(duration: 0.18)) { image = anim.image; isAnimated = true }
                return
            }
            if image != nil { return }
        }
        let loaded = resolver != nil && resolverFirst ? nil : await MediaLoader.shared.image(for: url, maxPixelSize: maxPixelSize, cache: cachesDecoded)
        guard !Task.isCancelled else { return }
        if let loaded {
            withAnimation(.easeOut(duration: 0.18)) { image = loaded.image; isAnimated = false }
            return
        }
        guard failureContent != nil || resolver != nil else { return }
        // The CDN link is dead: let the official loader find another source, else report why.
        var reason = MediaFailure.unavailable
        if let resolver {
            switch await resolver(retrying) {
            case .file(let file):
                if let local = await MediaLoader.shared.image(for: file, maxPixelSize: maxPixelSize, cache: cachesDecoded), !Task.isCancelled {
                    withAnimation(.easeOut(duration: 0.18)) { image = local.image; isAnimated = false }
                    return
                }
            case .expired: reason = .expired
            case .unavailable, .unsupported: break
            }
        }
        guard !Task.isCancelled else { return }
        failure = reason
    }
}

/// `NSImageView` with `animates = true`, scaled to fit/fill (fill is clipped to the bounds).
struct AnimatedImageView: NSViewRepresentable {
    let image: NSImage
    var contentMode: ContentMode

    final class Container: NSView {
        let imageView = NSImageView()
        var fill = true
        override init(frame: NSRect) {
            super.init(frame: frame)
            wantsLayer = true
            layer?.masksToBounds = true
            imageView.animates = true
            imageView.imageScaling = .scaleAxesIndependently
            imageView.imageAlignment = .alignCenter
            addSubview(imageView)
        }
        required init?(coder: NSCoder) { fatalError() }
        override func layout() {
            super.layout()
            let size = imageView.image?.size ?? .zero
            guard size.width > 0, size.height > 0, bounds.width > 0, bounds.height > 0 else { return }
            let sx = bounds.width / size.width, sy = bounds.height / size.height
            let scale = fill ? max(sx, sy) : min(sx, sy)
            let w = size.width * scale, h = size.height * scale
            imageView.frame = NSRect(x: (bounds.width - w) / 2, y: (bounds.height - h) / 2, width: w, height: h)
        }
    }

    func makeNSView(context: Context) -> Container { Container(frame: .zero) }

    func updateNSView(_ view: Container, context: Context) {
        view.fill = contentMode == .fill
        if view.imageView.image !== image { view.imageView.image = image }
        view.imageView.animates = true
        view.needsLayout = true
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: Container, context: Context) -> CGSize? {
        proposal.replacingUnspecifiedDimensions(by: image.size)
    }
}

/// Avatar from Zalo's CDN with an initials fallback.
struct RemoteAvatar: View {
    let url: URL?
    let name: String
    var size: CGFloat = 40

    var body: some View {
        RemoteImage(url: url, contentMode: .fill, maxPixelSize: max(96, size * 3)) {
            ZStack {
                Circle().fill(.tint.opacity(0.18))
                Text(nameInitials(name))
                    .font(.system(size: size * 0.36, weight: .bold, design: .rounded))
                    .foregroundStyle(.tint)
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
    }
}
