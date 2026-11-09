import SwiftUI

/// Shown in place of a photo / sticker / video thumbnail that cannot be loaded.
///
/// Two states, as in Zalo PC: *expired* (the CDN no longer has it and it is past Zalo's retention —
/// nothing to retry) and *unavailable* (network or server problem — "Thử lại" loads it again).
struct MediaFailureView: View {
    enum Subject { case photo, sticker, videoPreview }

    let failure: MediaFailure
    var subject: Subject = .photo
    /// Tight layout for small frames (stickers).
    var compact = false
    /// Keep the content in the upper part of the frame (video thumbnails have a play badge in the middle).
    var topAligned = false
    let retry: () -> Void

    static func title(_ failure: MediaFailure, subject: Subject) -> String {
        switch (failure, subject) {
        case (.expired, .photo): "Ảnh đã hết hạn"
        case (.expired, .sticker): "Sticker đã hết hạn"
        case (.expired, .videoPreview): "Video đã hết hạn"
        case (.unavailable, .photo): "Không tải được ảnh"
        case (.unavailable, .sticker): "Không tải được sticker"
        case (.unavailable, .videoPreview): "Không tải được ảnh xem trước"
        }
    }

    var body: some View {
        ZStack(alignment: topAligned ? .top : .center) {
            Rectangle().fill(.quaternary)
            VStack(spacing: compact ? 4 : 7) {
                Image(systemName: failure == .expired ? "clock.badge.exclamationmark" : "photo.badge.exclamationmark")
                    .font(.system(size: compact ? 18 : 24, weight: .regular))
                    .foregroundStyle(.secondary)
                if !compact {
                    Text(Self.title(failure, subject: subject))
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    if failure == .expired {
                        Text("Không còn trên máy chủ Zalo")
                            .font(.system(size: 11)).foregroundStyle(.tertiary)
                    }
                }
                if failure == .unavailable {
                    Button(action: retry) {
                        Label("Thử lại", systemImage: "arrow.clockwise").font(.system(size: 12, weight: .medium))
                    }
                    .buttonStyle(.glass)
                    .controlSize(.small)
                }
            }
            .padding(.horizontal, 8)
            .padding(.top, topAligned ? 6 : 8)
            .padding(.bottom, 8)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Self.title(failure, subject: subject))
    }
}
