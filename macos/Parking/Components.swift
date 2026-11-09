import SwiftUI

// MARK: - Avatar

struct AvatarView: View {
    let name: String
    var size: CGFloat = 40
    var isOnline: Bool = false
    var kind: ConversationKind? = nil

    private var hue: Double {
        let scalars = name.unicodeScalars.map(\.value)
        let sum = scalars.reduce(0, +)
        return Double(sum % 360) / 360
    }

    private var badgeIcon: String? {
        switch kind {
        case .group: "person.2.fill"
        case .community: "person.3.fill"
        case .official: "checkmark.seal.fill"
        default: nil
        }
    }

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            ZStack {
                Circle().fill(
                    Color(hue: hue, saturation: 0.42, brightness: 0.9)
                )
                Text(nameInitials(name))
                    .font(.system(size: size * 0.36, weight: .bold, design: .rounded))
                    .foregroundStyle(Color(hue: hue, saturation: 0.75, brightness: 0.32))
            }
            .frame(width: size, height: size)

            if isOnline && kind == nil {
                Circle()
                    .fill(Color.green)
                    .frame(width: size * 0.28, height: size * 0.28)
                    .overlay(Circle().strokeBar())
                    .padding(1)
            }

            if let badgeIcon, size >= 36 {
                Image(systemName: badgeIcon)
                    .font(.system(size: size * 0.22, weight: .semibold))
                    .foregroundStyle(.white)
                    .padding(size * 0.09)
                    .background(Circle().fill(ZaloTheme.blue))
                    .offset(x: size * 0.02, y: size * 0.02)
            }
        }
    }
}

extension Shape {
    func strokeBar() -> some View {
        stroke(.background, lineWidth: 2)
    }
}

// MARK: - Unread badge

struct UnreadBadge: View {
    let count: Int
    var body: some View {
        Text("\(min(count, 99))")
            .font(.system(size: 11, weight: .bold, design: .rounded))
            .foregroundStyle(.white)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Capsule().fill(ZaloTheme.blue))
    }
}

// MARK: - Label chip ("Phân loại")

struct LabelChip: View {
    let label: ConversationLabel
    var body: some View {
        Text(label.name)
            .font(.system(size: 10, weight: .medium))
            .foregroundStyle(Color(hex: label.colorHex))
            .padding(.horizontal, 6)
            .padding(.vertical, 1.5)
            .background(
                Capsule().fill(Color(hex: label.colorHex).opacity(0.14))
            )
    }
}

// MARK: - Typing indicator

struct TypingDots: View {
    @State private var phase = false
    var body: some View {
        HStack(spacing: 3) {
            ForEach(0..<3, id: \.self) { i in
                Circle()
                    .frame(width: 5, height: 5)
                    .foregroundStyle(.tertiary)
                    .offset(y: phase ? -2.5 : 2.5)
                    .animation(
                        .easeInOut(duration: 0.45).repeatForever().delay(Double(i) * 0.14),
                        value: phase
                    )
            }
        }
        .onAppear { phase = true }
    }
}

// MARK: - Color helper

extension Color {
    init(hex: String) {
        var value: UInt64 = 0
        let cleaned = hex.replacingOccurrences(of: "#", with: "")
        Scanner(string: cleaned).scanHexInt64(&value)
        self.init(
            red: Double((value >> 16) & 0xFF) / 255,
            green: Double((value >> 8) & 0xFF) / 255,
            blue: Double(value & 0xFF) / 255
        )
    }
}

// MARK: - Voice message waveform

struct WaveformView: View {
    var bars: Int = 22
    var playing: Bool = false
    @State private var animate = false

    var body: some View {
        HStack(spacing: 2.5) {
            ForEach(0..<bars, id: \.self) { i in
                let base = 0.35 + 0.6 * abs(sin(Double(i) * 1.7))
                Capsule()
                    .fill(Color.primary.opacity(0.55))
                    .frame(width: 2.5, height: 16 * (playing && animate ? (0.5 + base) : base))
                    .animation(
                        playing ? .easeInOut(duration: 0.4).repeatForever().delay(Double(i % 5) * 0.06) : .default,
                        value: animate
                    )
            }
        }
        .onAppear { animate = true }
    }
}
