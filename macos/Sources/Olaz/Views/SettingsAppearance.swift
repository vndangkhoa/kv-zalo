import SwiftUI

/// Settings ▸ Giao diện. Every control writes an `OlazPrefs` key; the window roots (`olazAppearance`) and the chat views read the
/// same keys, so changes show up live in the main window behind this one.
struct AppearanceSettings: View {
    @AppStorage(OlazPrefs.accent) private var accent = OlazAccent.blue
    @AppStorage(OlazPrefs.textSize) private var textSize = OlazTextSize.standard
    @AppStorage(OlazPrefs.wallpaper) private var wallpaper = ChatWallpaper.none
    @AppStorage(OlazPrefs.appIcon) private var appIcon = OlazAppIcon.c

    var body: some View {
        Section {
            LabeledContent("Màu nhấn") {
                HStack(spacing: 6) {
                    ForEach(OlazAccent.allCases) { option in
                        AccentSwatch(option: option, isSelected: accent == option) { accent = option }
                    }
                }
            }
        } footer: {
            Text("Dùng cho tin nhắn của bạn, nút và lựa chọn.")
        }

        Section("Cỡ chữ tin nhắn") {
            Picker("Cỡ chữ", selection: $textSize) {
                ForEach(OlazTextSize.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(maxWidth: .infinity)
            TextSizePreview(scale: textSize.scale, wallpaper: wallpaper)
        }

        Section("Hình nền trò chuyện") {
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 10), count: 5), spacing: 12) {
                ForEach(ChatWallpaper.allCases) { option in
                    WallpaperTile(option: option, isSelected: wallpaper == option) { wallpaper = option }
                }
            }
            .padding(.vertical, 4)
        }

        Section {
            HStack(spacing: 18) {
                ForEach(OlazAppIcon.allCases) { option in
                    AppIconTile(option: option, isSelected: appIcon == option) { appIcon = option }
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 4)
        } header: {
            Text("Biểu tượng ứng dụng")
        } footer: {
            Text("Biểu tượng hiện trên Dock khi \(AppBrand.name) đang chạy.")
        }
    }
}

/// Selection ring shared by the swatch, wallpaper and icon tiles.
private extension View {
    func selectionRing<S: InsettableShape>(_ shape: S, isSelected: Bool, lineWidth: CGFloat = 2.5) -> some View {
        overlay { if isSelected { shape.strokeBorder(.tint, lineWidth: lineWidth) } }
    }
}

private struct AccentSwatch: View {
    let option: OlazAccent
    let isSelected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Circle()
                .fill(option.color)
                .frame(width: 22, height: 22)
                .overlay { Circle().strokeBorder(.black.opacity(0.12), lineWidth: 0.5) }
                .overlay {
                    if isSelected { Image(systemName: "checkmark").font(.system(size: 10, weight: .heavy)).foregroundStyle(.white) }
                }
                .padding(3)
                .overlay { Circle().strokeBorder(isSelected ? AnyShapeStyle(.secondary) : AnyShapeStyle(.clear), lineWidth: 2) }
        }
        .buttonStyle(.plain)
        .help(option.title)
        .accessibilityLabel(option.title)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}

/// Two sample bubbles at the chosen text scale, on the chosen wallpaper, so both settings can be judged together.
private struct TextSizePreview: View {
    let scale: CGFloat
    let wallpaper: ChatWallpaper

    var body: some View {
        VStack(spacing: 8) {
            bubble("Chiều nay đi cà phê không?", mine: false)
            bubble("Được chứ, 3 giờ nhé!", mine: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity)
        .background {
            if let image = wallpaper.image {
                Image(nsImage: image).resizable().scaledToFill()
            } else {
                Color.primary.opacity(0.07)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(.separator, lineWidth: 0.5) }
        .animation(.smooth(duration: 0.2), value: scale)
        .accessibilityHidden(true)
    }

    private func bubble(_ text: String, mine: Bool) -> some View {
        HStack {
            if mine { Spacer(minLength: 40) }
            Text(text)
                .font(.system(size: 14 * scale))
                .foregroundStyle(mine ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
                .padding(.horizontal, 12).padding(.vertical, 7)
                .background(mine ? AnyShapeStyle(.tint) : AnyShapeStyle(Color(nsColor: .controlBackgroundColor)),
                            in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            if !mine { Spacer(minLength: 40) }
        }
    }
}

private struct WallpaperTile: View {
    let option: ChatWallpaper
    let isSelected: Bool
    let action: () -> Void

    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: 10, style: .continuous) }

    var body: some View {
        Button(action: action) {
            VStack(spacing: 5) {
                Color.clear
                    .aspectRatio(1.3, contentMode: .fit)
                    .overlay {
                        if let image = option.image {
                            Image(nsImage: image).resizable().scaledToFill()
                        } else {
                            Color.primary.opacity(0.07)
                        }
                    }
                    .overlay { miniBubbles }
                    .clipShape(shape)
                    .overlay { shape.strokeBorder(.separator, lineWidth: 0.5) }
                    .selectionRing(shape, isSelected: isSelected)
                Text(option.title)
                    .font(.caption)
                    .foregroundStyle(isSelected ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Hình nền \(option.title)")
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    /// Hints how bubbles sit on the wallpaper, like the system's wallpaper pickers.
    private var miniBubbles: some View {
        VStack(alignment: .leading, spacing: 4) {
            Capsule().fill(Color(nsColor: .controlBackgroundColor).opacity(option == .night ? 0.35 : 0.9)).frame(width: 26, height: 7)
            Capsule().fill(.tint).frame(width: 22, height: 7).frame(maxWidth: .infinity, alignment: .trailing)
        }
        .padding(8)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
    }
}

private struct AppIconTile: View {
    let option: OlazAppIcon
    let isSelected: Bool
    let action: () -> Void

    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: 18, style: .continuous) }

    var body: some View {
        Button(action: action) {
            VStack(spacing: 5) {
                Group {
                    if let image = option.image {
                        Image(nsImage: image).resizable().interpolation(.high)
                    } else {
                        Color.clear
                    }
                }
                .frame(width: 64, height: 64)
                .padding(6)
                .background { if isSelected { shape.fill(.tint.opacity(0.14)) } }
                .selectionRing(shape, isSelected: isSelected, lineWidth: 2)
                Text(option.title)
                    .font(.caption)
                    .foregroundStyle(isSelected ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Biểu tượng \(option.title)")
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}
