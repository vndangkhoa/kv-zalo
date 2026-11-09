import AppKit
import SwiftUI

/// User-visible product name. The package, bundle id and log subsystem still say Olaz until the rename.
enum AppBrand {
    static let name = "Olaz"
}

/// `@AppStorage` keys of the Olaz appearance and first-run settings.
enum OlazPrefs {
    /// `OlazAccent.rawValue`.
    static let accent = "olaz.accent"
    /// `OlazTextSize.rawValue`.
    static let textSize = "olaz.textSize"
    /// `ChatWallpaper.rawValue`.
    static let wallpaper = "olaz.wallpaper"
    /// `OlazAppIcon.rawValue`.
    static let appIcon = "olaz.appIcon"
    /// Video player volume, 0...1 (shared by the chat viewer and the zCloud preview; default 1).
    static let videoVolume = "olaz.video.volume"
    /// Video player muted (kept apart from the volume so unmuting restores the level).
    static let videoMuted = "olaz.video.muted"
    /// Set once the first-run setup (sign-in → phone sync → tour offer) was completed or skipped.
    static let didFinishSetup = "olaz.didFinishSetup"
    /// A fresh QR sign-in happened and the setup's sync / welcome steps are still due (survives a relaunch).
    static let setupPending = "olaz.setupPending"
}

/// Accent color picked in Settings ▸ Giao diện; applied with `.tint` at the window root, so views use `.tint` /
/// `TintShapeStyle` rather than a fixed blue.
enum OlazAccent: String, CaseIterable, Identifiable {
    case blue, teal, indigo, pink, orange, green, graphite

    var id: String { rawValue }

    var color: Color {
        switch self {
        case .blue: ZaloTheme.blue
        case .teal: Color(red: 0.0, green: 0.62, blue: 0.70)
        case .indigo: Color(red: 0.36, green: 0.34, blue: 0.84)
        case .pink: Color(red: 0.93, green: 0.25, blue: 0.48)
        case .orange: Color(red: 0.96, green: 0.50, blue: 0.10)
        case .green: Color(red: 0.16, green: 0.66, blue: 0.33)
        case .graphite: Color(red: 0.43, green: 0.45, blue: 0.49)
        }
    }

    var title: String {
        switch self {
        case .blue: "Xanh dương"
        case .teal: "Xanh ngọc"
        case .indigo: "Chàm"
        case .pink: "Hồng"
        case .orange: "Cam"
        case .green: "Xanh lá"
        case .graphite: "Than chì"
        }
    }
}

/// Chat text size (Settings ▸ Giao diện); a multiplier for message text, not for the window chrome.
enum OlazTextSize: String, CaseIterable, Identifiable {
    case small, standard, large, extraLarge

    var id: String { rawValue }

    var scale: CGFloat {
        switch self {
        case .small: 0.92
        case .standard: 1
        case .large: 1.12
        case .extraLarge: 1.26
        }
    }

    var title: String {
        switch self {
        case .small: "Nhỏ"
        case .standard: "Vừa"
        case .large: "Lớn"
        case .extraLarge: "Rất lớn"
        }
    }
}

/// Chat background (Settings ▸ Giao diện). Images ship in `Resources/Olaz/wallpaper-<name>.jpg`.
enum ChatWallpaper: String, CaseIterable, Identifiable {
    case none, mist, silk, lotus, night

    var id: String { rawValue }

    var title: String {
        switch self {
        case .none: "Không"
        case .mist: "Sương"
        case .silk: "Lụa"
        case .lotus: "Sen"
        case .night: "Đêm"
        }
    }

    @MainActor var image: NSImage? {
        self == .none ? nil : OlazAssets.image("wallpaper-\(rawValue).jpg")
    }
}

/// Alternate Dock icons (Settings ▸ Giao diện). `c` is the primary icon baked into the bundle's icns.
enum OlazAppIcon: String, CaseIterable, Identifiable {
    case c, a, b

    var id: String { rawValue }

    var title: String {
        switch self {
        case .c: "Bong bóng"
        case .a: "Cổ điển"
        case .b: "Tối giản"
        }
    }

    @MainActor var image: NSImage? { OlazAssets.image("AppIcon-\(rawValue).png") }
}

/// Images in `Resources/Olaz` (copied into the SwiftPM resource bundle), cached by name.
@MainActor
enum OlazAssets {
    private static var cache: [String: NSImage] = [:]

    static func image(_ name: String) -> NSImage? {
        if let cached = cache[name] { return cached }
        guard let url = ResourceLocator.url(for: "Olaz/\(name)", in: "Resources"),
              let image = NSImage(contentsOf: url) else { return nil }
        cache[name] = image
        return image
    }
}

/// Message text scale from Settings ▸ Giao diện ▸ Cỡ chữ.
extension EnvironmentValues {
    @Entry var olazTextScale: CGFloat = 1
}
