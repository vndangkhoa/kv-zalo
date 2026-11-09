import AppKit
import SwiftUI

/// Colors and shapes shared by the chat views (bubbles, cards, composer, inspector).
enum ChatPalette {
    /// Fill of other people's bubbles and of cards: opaque so text stays readable over a wallpaper, light gray
    /// in light mode and raised gray in dark mode (as in Messages).
    static let incoming = Color(nsColor: NSColor(name: nil) { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.23, alpha: 1)
            : NSColor(red: 233 / 255, green: 233 / 255, blue: 235 / 255, alpha: 1)
    })

    /// The accent picked in Settings as a `Color`, for the places that cannot take the `.tint` shape style
    /// (glass tint, `AttributedString` runs, `NSColor`).
    static func accent(_ stored: String) -> Color { (OlazAccent(rawValue: stored) ?? .blue).color }
}

/// A chat bubble: continuous rounded rectangle with per-corner radii (tighter between bubbles of one run)
/// and, on the last bubble of a run, the Messages tail. The tail overhangs the bounds by `overhang`, so it only
/// shows when the shape is used as a background (never clipped to the frame).
struct BubbleShape: Shape {
    var mine: Bool
    var tail: Bool
    var topLeading: CGFloat = 18
    var topTrailing: CGFloat = 18
    var bottomLeading: CGFloat = 18
    var bottomTrailing: CGFloat = 18

    static let overhang: CGFloat = 5
    private static let tailSize = CGSize(width: 11, height: 17)

    func path(in rect: CGRect) -> Path {
        let body = UnevenRoundedRectangle(topLeadingRadius: topLeading, bottomLeadingRadius: bottomLeading,
                                          bottomTrailingRadius: bottomTrailing, topTrailingRadius: topTrailing,
                                          style: .continuous).path(in: rect)
        guard tail, rect.height > Self.tailSize.height + 4 else { return body }
        let (w, h) = (Self.tailSize.width, Self.tailSize.height)
        // Tail drawn for the leading side, mirrored for my bubbles.
        func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            let top = rect.maxY - h
            return mine ? CGPoint(x: rect.maxX + Self.overhang - x, y: top + y) : CGPoint(x: rect.minX - Self.overhang + x, y: top + y)
        }
        var sliver = Path()
        sliver.move(to: point(w, 0))
        sliver.addCurve(to: point(0, h), control1: point(w, 8), control2: point(7, 14))
        sliver.addLine(to: point(w, h))
        sliver.closeSubpath()
        // A union, not two overlapping subpaths: the winding of the rounded rectangle is not ours to rely on.
        return body.union(sliver)
    }
}

/// The chat wallpaper chosen in Settings, drawn behind the whole chat pane. A wallpaper made for the other
/// appearance (night in light mode, the light ones in dark mode) is faded so bubbles stay legible.
struct ChatWallpaperBackground: View {
    @AppStorage(OlazPrefs.wallpaper) private var stored = ChatWallpaper.none.rawValue
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let wallpaper = ChatWallpaper(rawValue: stored) ?? .none
        if let image = wallpaper.image {
            let matchesAppearance = (wallpaper == .night) == (scheme == .dark)
            Color.clear
                .overlay { Image(nsImage: image).resizable().scaledToFill() }
                .clipped()
                .opacity(matchesAppearance ? 1 : (scheme == .dark ? 0.12 : 0.16))
                .ignoresSafeArea()
                .accessibilityHidden(true)
        }
    }
}

extension View {
    /// Small secondary text drawn straight on the wallpaper (sender names, delivery status) gets a material
    /// backing so it stays readable; without a wallpaper it is left alone.
    func legibleOnWallpaper(_ active: Bool) -> some View {
        modifier(WallpaperBacking(active: active))
    }
}

private struct WallpaperBacking: ViewModifier {
    let active: Bool

    func body(content: Content) -> some View {
        if active {
            content.padding(.horizontal, 7).padding(.vertical, 2).background(.regularMaterial, in: Capsule())
        } else {
            content
        }
    }
}
