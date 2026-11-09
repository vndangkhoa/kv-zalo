import SwiftUI

/// Brand tokens extracted from the original app (see REVERSE-ENGINEERING-NOTES.md §4).
enum ZaloTheme {
    /// #0068FF — Zalo CTA/brand blue.
    static let blue = Color(red: 0, green: 0x68 / 255, blue: 1)
    /// #005AE0 — pressed variant.
    static let bluePressed = Color(red: 0, green: 0x5A / 255, blue: 0xE0 / 255)
    /// #E5F1FF — tonal background.
    static let blueTonal = Color(red: 0xE5 / 255, green: 0xF1 / 255, blue: 1)
    /// #081B3A — primary ink on light.
    static let ink = Color(red: 0x08 / 255, green: 0x1B / 255, blue: 0x3A / 255)
    /// #22262B — dark-mode window background used by the original app.
    static let darkBase = Color(red: 0x22 / 255, green: 0x26 / 255, blue: 0x2B / 255)
    static let darkElevated = Color(red: 0x2A / 255, green: 0x2F / 255, blue: 0x35 / 255)

    static func bubble(_ isMine: Bool, dark: Bool) -> Color {
        isMine ? blue : (dark ? darkElevated : Color(white: 0.95))
    }
}

/// Zalo wordmark recreated in text (traces `pc-dist/assets/logo-new.svg`).
struct ZaloWordmark: View {
    var size: CGFloat = 28
    var body: some View {
        Text("Zalo")
            .font(.system(size: size, weight: .heavy, design: .rounded))
            .foregroundStyle(ZaloTheme.blue)
            .tracking(-0.5)
    }
}

enum ZaloTypography {
    static func title(_ s: String) -> Text { Text(s).font(.system(size: 15, weight: .semibold)) }
}
