import SwiftUI
import AppKit
import AVFoundation
import UniformTypeIdentifiers
import ImageIO

// Shared helpers for the zCloud screens: formatting, thumbnails, staging downloaded files, saving.

@MainActor
enum CloudFormat {
    private static let bytes: ByteCountFormatter = {
        let f = ByteCountFormatter()
        f.countStyle = .binary           // Zalo shows 100 GiB as "100 GB"
        f.includesUnit = true
        f.isAdaptive = true
        return f
    }()
    private static let day: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "vi_VN")
        f.dateFormat = "dd/MM/yyyy"
        return f
    }()
    private static let dayTime: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "vi_VN")
        f.dateFormat = "HH:mm dd/MM/yyyy"
        return f
    }()

    static func size(_ value: Int64) -> String {
        let text = bytes.string(fromByteCount: max(0, value))
        return text.replacingOccurrences(of: "Zero KB", with: "0 KB").replacingOccurrences(of: "bytes", with: "B")
    }
    static func date(_ value: Date?) -> String { value.map { day.string(from: $0) } ?? "—" }
    static func dateTime(_ value: Date) -> String { dayTime.string(from: value) }

    static func duration(_ seconds: TimeInterval) -> String {
        let total = Int(seconds.rounded())
        return String(format: "%d:%02d", total / 60, total % 60)
    }
}

extension CloudItem {
    /// Name to show: the sent file name, else a generic label for the kind.
    var displayName: String {
        if !title.isEmpty { return title }
        switch kind {
        case .photo: return "Ảnh"
        case .video: return "Video"
        case .voice: return "Tin nhắn thoại"
        case .file: return "File"
        case .other: return "Nội dung"
        }
    }

    var conversationLabel: String { conversationName.isEmpty ? "Trò chuyện" : conversationName }

    /// SF Symbol for a file by its extension.
    var fileSymbol: String {
        switch ext.lowercased() {
        case "pdf": "doc.richtext.fill"
        case "doc", "docx", "rtf", "pages": "doc.text.fill"
        case "xls", "xlsx", "csv", "numbers": "tablecells.fill"
        case "ppt", "pptx", "key": "play.rectangle.on.rectangle.fill"
        case "zip", "rar", "7z", "gz", "tar": "doc.zipper"
        case "mp3", "m4a", "wav", "aac", "flac": "music.note"
        case "mp4", "mov", "mkv", "avi": "film.fill"
        case "jpg", "jpeg", "png", "gif", "heic", "webp": "photo.fill"
        case "txt", "md": "doc.plaintext.fill"
        default: "doc.fill"
        }
    }
}

// MARK: - Container sniffing / staging

enum CloudContainer: String, Sendable {
    case jxl, jpeg, png, gif, webp, heic, mp4, m4a, aac, mp3, amr, ogg, wav, pdf, unknown

    var fileExtension: String? { self == .unknown ? nil : rawValue }

    init(fileAt url: URL) {
        guard let handle = try? FileHandle(forReadingFrom: url) else { self = .unknown; return }
        defer { try? handle.close() }
        let head = [UInt8]((try? handle.read(upToCount: 16)) ?? Data())
        func starts(_ bytes: [UInt8], at offset: Int = 0) -> Bool {
            head.count >= offset + bytes.count && Array(head[offset..<offset + bytes.count]) == bytes
        }
        if starts([0xFF, 0x0A]) || starts([0, 0, 0, 0x0C, 0x4A, 0x58, 0x4C, 0x20]) { self = .jxl }
        else if starts([0xFF, 0xD8, 0xFF]) { self = .jpeg }
        else if starts([0x89, 0x50, 0x4E, 0x47]) { self = .png }
        else if starts([0x47, 0x49, 0x46, 0x38]) { self = .gif }
        else if starts([0x52, 0x49, 0x46, 0x46]) && starts([0x57, 0x45, 0x42, 0x50], at: 8) { self = .webp }
        else if starts([0x52, 0x49, 0x46, 0x46]) && starts([0x57, 0x41, 0x56, 0x45], at: 8) { self = .wav }
        else if starts([0x66, 0x74, 0x79, 0x70], at: 4) {
            let brand = String(decoding: head[8..<min(12, head.count)], as: UTF8.self)
            if brand.hasPrefix("heic") || brand.hasPrefix("heix") || brand.hasPrefix("mif1") { self = .heic }
            else if brand.hasPrefix("M4A") { self = .m4a }
            else { self = .mp4 }
        }
        else if starts([0x25, 0x50, 0x44, 0x46]) { self = .pdf }
        else if starts([0x4F, 0x67, 0x67, 0x53]) { self = .ogg }
        else if starts([0x23, 0x21, 0x41, 0x4D, 0x52]) { self = .amr }
        else if starts([0x49, 0x44, 0x33]) || (head.count > 1 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 && head[1] & 0x06 != 0) { self = .mp3 }
        else if head.count > 1 && head[0] == 0xFF && head[1] & 0xF6 == 0xF0 { self = .aac }
        else { self = .unknown }
    }
}

enum CloudFiles {
    /// `Caches/Olaz/zcloud-preview` — covered by `MediaLoader.allowedLocalRoots`.
    static let stagingRoot: URL = {
        let dir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Olaz/zcloud-preview", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        purge(dir)
        return dir
    }()

    private static func purge(_ dir: URL) {
        let cutoff = Date().addingTimeInterval(-2 * 86400)
        let urls = (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.contentModificationDateKey])) ?? []
        for url in urls {
            let modified = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate ?? .distantPast
            if modified < cutoff { try? FileManager.default.removeItem(at: url) }
        }
    }

    /// Removes every staged copy (sign-out: nothing of the previous account stays in the caches).
    static func purgeAll() {
        let urls = (try? FileManager.default.contentsOfDirectory(at: stagingRoot, includingPropertiesForKeys: nil)) ?? []
        for url in urls { try? FileManager.default.removeItem(at: url) }
    }

    /// Files whose type could run code when opened (the extension comes from the sender, so it is untrusted):
    /// same rule as the chat file card.
    static func mayExecute(_ file: URL, displayName: String) -> Bool {
        let blocked: Set<String> = ["command", "tool", "terminal", "fileloc", "webloc", "inetloc", "pkg", "mpkg", "app",
                                    "scpt", "scptd", "workflow", "action", "sh", "bash", "zsh", "csh", "ksh", "jar", "dylib", "bundle"]
        let packageTypes = ["com.apple.installer-package-archive", "com.apple.finder.fileloc", "com.apple.terminal.shell-script",
                            "com.apple.web-internet-location", "com.apple.application-script-binary"].compactMap { UTType($0) }
        for ext in Set([file.pathExtension.lowercased(), (displayName as NSString).pathExtension.lowercased()]) where !ext.isEmpty {
            if blocked.contains(ext) { return true }
            guard let type = UTType(filenameExtension: ext) else { continue }
            if type.conforms(to: .executable) || type.conforms(to: .applicationBundle) || type.conforms(to: .package)
                || type.conforms(to: .shellScript) || type.conforms(to: .script) || type.conforms(to: .internetLocation)
                || packageTypes.contains(where: { type.conforms(to: $0) }) {
                return true
            }
        }
        return false
    }

    /// A same-content copy (APFS clone) of the official cache file with a real name + extension, so AVFoundation,
    /// QuickLook and other apps recognise it. The official cache file stays untouched.
    static func stage(_ file: URL, name: String, ext: String?) -> URL? {
        let fm = FileManager.default
        let folder = stagingRoot.appendingPathComponent(String(abs(file.lastPathComponent.hashValue), radix: 36), isDirectory: true)
        try? fm.createDirectory(at: folder, withIntermediateDirectories: true)
        var base = name.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: "-")
        if base.isEmpty { base = "zcloud" }
        if let ext, !ext.isEmpty, (base as NSString).pathExtension.lowercased() != ext.lowercased() { base += "." + ext }
        let target = folder.appendingPathComponent(base)
        if fm.fileExists(atPath: target.path) { return target }
        do {
            try fm.copyItem(at: file, to: target)
            // Gatekeeper must vet anything opened from outside the app.
            MediaLoader.applyQuarantine(to: target, origin: URL(string: "https://zcld.chat.zalo.me/")!)
            return target
        } catch { return nil }
    }

    /// Name + extension to use outside zCloud.
    static func outputName(item: CloudItem, container: CloudContainer) -> (name: String, ext: String?) {
        var ext = container.fileExtension
        switch item.kind {
        case .file:
            let own = item.ext.isEmpty ? (item.title as NSString).pathExtension : item.ext
            if !own.isEmpty { ext = own }
            let stem = (item.title as NSString).pathExtension.lowercased() == ext?.lowercased() ? (item.title as NSString).deletingPathExtension : item.title
            return (stem.isEmpty ? "File zCloud" : stem, ext)
        case .photo:
            return ("Ảnh zCloud \(stamp(item.date))", ext)
        case .video:
            return ("Video zCloud \(stamp(item.date))", ext ?? "mp4")
        case .voice:
            return ("Tin nhắn thoại \(stamp(item.date))", ext ?? "m4a")
        case .other:
            return ("zCloud \(stamp(item.date))", ext)
        }
    }

    private static func stamp(_ date: Date?) -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd HH.mm.ss"
        return f.string(from: date ?? Date())
    }

    /// Re-encodes a still image (JPEG XL, HEIC…) to `type` with ImageIO.
    static func convertImage(at url: URL, to type: UTType, destination: URL) -> Bool {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              let dest = CGImageDestinationCreateWithURL(destination as CFURL, type.identifier as CFString, 1, nil) else { return false }
        CGImageDestinationAddImageFromSource(dest, source, 0, [kCGImageDestinationLossyCompressionQuality: 0.95] as CFDictionary)
        return CGImageDestinationFinalize(dest)
    }
}

// MARK: - Save panel

/// "Lưu về…" — NSSavePanel (location chosen by the user), then copies / converts the downloaded file.
@MainActor
enum CloudSaver {
    @MainActor private final class FormatPicker: NSObject {
        let popup = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 220, height: 26), pullsDown: false)
        weak var panel: NSSavePanel?
        let options: [(title: String, type: UTType?, ext: String)]
        init(options: [(String, UTType?, String)]) {
            self.options = options.map { (title: $0.0, type: $0.1, ext: $0.2) }
            super.init()
            options.forEach { popup.addItem(withTitle: $0.0) }
            popup.target = self
            popup.action = #selector(changed)
        }
        @objc func changed() {
            guard let panel else { return }
            let option = options[popup.indexOfSelectedItem]
            let stem = (panel.nameFieldStringValue as NSString).deletingPathExtension
            // "Keep original" must not inherit the previous choice's type (it would rename a JPEG XL file to .jpg).
            panel.allowedContentTypes = option.type.map { [$0] } ?? (UTType(filenameExtension: option.ext).map { [$0] } ?? [])
            panel.nameFieldStringValue = stem + "." + option.ext
        }
        var selected: (title: String, type: UTType?, ext: String) { options[popup.indexOfSelectedItem] }
    }

    enum Outcome: Equatable {
        case saved
        case cancelled
        case failed(String)
    }

    static func save(_ file: CloudFile, item: CloudItem, window: NSWindow? = nil) async -> Outcome {
        let container = CloudContainer(fileAt: file.url)
        let output = CloudFiles.outputName(item: item, container: container)
        let panel = NSSavePanel()
        panel.title = "Lưu về máy"
        panel.prompt = "Lưu"
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        var picker: FormatPicker?
        let convertible = item.kind == .photo && container != .jpeg && container != .png && container != .gif
        if convertible {
            let original = container.fileExtension ?? "jxl"
            let fp = FormatPicker(options: [("JPEG (khuyên dùng)", .jpeg, "jpg"), ("PNG", .png, "png"), ("Giữ nguyên định dạng gốc", nil, original)])
            fp.panel = panel
            let label = NSTextField(labelWithString: "Định dạng:")
            let stack = NSStackView(views: [label, fp.popup])
            stack.spacing = 8
            stack.edgeInsets = NSEdgeInsets(top: 8, left: 0, bottom: 8, right: 0)
            panel.accessoryView = stack
            picker = fp
            panel.allowedContentTypes = [.jpeg]
            panel.nameFieldStringValue = output.name + ".jpg"
        } else {
            panel.nameFieldStringValue = output.ext.map { output.name + "." + $0 } ?? output.name
            if let ext = output.ext, let type = UTType(filenameExtension: ext) { panel.allowedContentTypes = [type] }
        }
        let response: NSApplication.ModalResponse = await withCheckedContinuation { continuation in
            if let window = window ?? NSApp.keyWindow { panel.beginSheetModal(for: window) { continuation.resume(returning: $0) } }
            else { continuation.resume(returning: panel.runModal()) }
        }
        guard response == .OK, let target = panel.url else { return .cancelled }
        let source = file.url
        let chosen = picker?.selected
        let error: String? = await Task.detached(priority: .userInitiated) {
            let fm = FileManager.default
            do {
                // Written beside the target first, so a failure never destroys a file the user chose to replace.
                let folder = try fm.url(for: .itemReplacementDirectory, in: .userDomainMask, appropriateFor: target, create: true)
                defer { try? fm.removeItem(at: folder) }
                let temp = folder.appendingPathComponent(target.lastPathComponent)
                if let chosen, let type = chosen.type {
                    guard CloudFiles.convertImage(at: source, to: type, destination: temp) else { return "Không chuyển đổi được ảnh sang \(chosen.title)." }
                } else {
                    try fm.copyItem(at: source, to: temp)
                }
                if fm.fileExists(atPath: target.path) { _ = try fm.replaceItemAt(target, withItemAt: temp) }
                else { try fm.moveItem(at: temp, to: target) }
                return nil
            } catch {
                return "Không lưu được tệp: \(error.localizedDescription)"
            }
        }.value
        return error.map { .failed($0) } ?? .saved
    }
}

// MARK: - Thumbnails

/// Loads grid / list thumbnails: the official downloader fetches the item (photos are small; videos only up to a
/// size), then it is decoded (JPEG XL included) or a video frame is grabbed. Throttled and cancel-aware.
@MainActor
final class CloudThumbnails {
    static let shared = CloudThumbnails()

    enum Result {
        case image(NSImage)
        case expired
        case missing
    }

    private var resolver = MediaResolver(maxConcurrent: 3)
    private let cache = NSCache<NSString, NSImage>()

    private init() {
        cache.countLimit = 600
        cache.totalCostLimit = 192 * 1024 * 1024        // bytes of decoded pixels
    }

    private func cacheKey(_ item: CloudItem, _ pixelSize: CGFloat) -> NSString { "\(item.id)@\(Int(pixelSize))" as NSString }

    func cached(_ item: CloudItem, pixelSize: CGFloat) -> NSImage? { cache.object(forKey: cacheKey(item, pixelSize)) }

    func reset() { cache.removeAllObjects(); resolver = MediaResolver(maxConcurrent: 3) }

    func load(_ item: CloudItem, engine: ZaloEngine, pixelSize: CGFloat = 420, force: Bool = false) async -> Result {
        if let image = cached(item, pixelSize: pixelSize) { return .image(image) }
        let key = "cloud|" + item.id
        let resolution = await resolver.resolve(key: key, force: force) { [engine] in await engine.cloudThumbnail(for: item.id) }
        if Task.isCancelled { return .missing }
        switch resolution {
        case .expired: return .expired
        case .unavailable, .unsupported: return .missing
        case .file(let url):
            var image: NSImage?
            if item.kind == .video { image = await Self.videoFrame(url, name: item.id) }
            else { image = await MediaLoader.shared.image(for: url, maxPixelSize: pixelSize)?.image }
            if let image {
                let cost = max(1, Int(image.size.width * image.size.height) * 4)
                cache.setObject(image, forKey: cacheKey(item, pixelSize), cost: cost)
                return .image(image)
            }
            return .missing
        }
    }

    private static func videoFrame(_ url: URL, name: String) async -> NSImage? {
        let container = CloudContainer(fileAt: url)
        guard let staged = CloudFiles.stage(url, name: "video", ext: container.fileExtension ?? "mp4") else { return nil }
        let generator = AVAssetImageGenerator(asset: AVURLAsset(url: staged))
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 640, height: 640)
        let time = CMTime(seconds: 0.4, preferredTimescale: 600)
        guard let result = try? await generator.image(at: time) else {
            guard let first = try? await generator.image(at: .zero) else { return nil }
            return NSImage(cgImage: first.image, size: NSSize(width: first.image.width, height: first.image.height))
        }
        return NSImage(cgImage: result.image, size: NSSize(width: result.image.width, height: result.image.height))
    }
}

/// Square-ish thumbnail of a photo / video; other kinds show their icon.
struct CloudThumbnailView: View {
    @Environment(ZaloEngine.self) private var engine
    let item: CloudItem
    var contentMode: ContentMode = .fill
    var pixelSize: CGFloat = 420

    @State private var image: NSImage?
    @State private var state: CloudThumbnails.Result?
    @State private var attempt = 0

    @ViewBuilder private var stateView: some View {
        if let image {
            Image(nsImage: image).resizable().aspectRatio(contentMode: contentMode)
        } else if case .expired = state {
            VStack(spacing: 4) {
                Image(systemName: "clock.badge.xmark").font(.system(size: 20))
                Text("Đã hết hạn").font(.system(size: 10.5))
            }
            .foregroundStyle(.secondary)
        } else if case .missing = state {
            Button { attempt += 1; state = nil } label: {
                VStack(spacing: 4) {
                    Image(systemName: item.kind.symbol).font(.system(size: 22))
                    if item.kind == .photo { Text("Thử lại").font(.system(size: 10.5)) }
                }
                .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
        } else {
            ProgressView().controlSize(.small)
        }
    }

    var body: some View {
        Rectangle().fill(.quaternary.opacity(0.5)).overlay { stateView }.clipped()
        .task(id: "\(item.id)#\(attempt)") {
            if let hit = CloudThumbnails.shared.cached(item, pixelSize: pixelSize) { image = hit; return }
            image = nil
            let result = await CloudThumbnails.shared.load(item, engine: engine, pixelSize: pixelSize, force: attempt > 0)
            guard !Task.isCancelled else { return }
            if case .image(let loaded) = result { withAnimation(.easeOut(duration: 0.15)) { image = loaded } }
            state = result
        }
    }
}
