import SwiftUI

// Building blocks shared by the chat inspector (ChatInspector.swift) and its "Kho lưu trữ" page (SharedMediaBrowser.swift).

/// Rounded list background of the inspector's sections.
struct InspectorList<Content: View>: View {
    @ViewBuilder let content: Content

    init(@ViewBuilder content: () -> Content) { self.content = content() }

    var body: some View {
        VStack(spacing: 0) { content }
            .frame(maxWidth: .infinity)
            .background(Color.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

/// One row of a long inspector list as its own background segment: the rows of a month, drawn one after another in a lazy stack, still read
/// as one rounded list (rounded at the month's first and last row only, a divider between rows).
struct InspectorSegment<Content: View>: View {
    let isFirst: Bool
    let isLast: Bool
    /// Where the divider above the row starts.
    var dividerInset: CGFloat = 12
    @ViewBuilder let content: Content

    init(isFirst: Bool, isLast: Bool, dividerInset: CGFloat = 12, @ViewBuilder content: () -> Content) {
        self.isFirst = isFirst
        self.isLast = isLast
        self.dividerInset = dividerInset
        self.content = content()
    }

    var body: some View {
        VStack(spacing: 0) {
            if !isFirst { Divider().padding(.leading, dividerInset) }
            content
        }
        .frame(maxWidth: .infinity)
        .background(Color.primary.opacity(0.05), in: UnevenRoundedRectangle(
            topLeadingRadius: isFirst ? 12 : 0, bottomLeadingRadius: isLast ? 12 : 0,
            bottomTrailingRadius: isLast ? 12 : 0, topTrailingRadius: isFirst ? 12 : 0, style: .continuous))
    }
}

/// A tappable row of an inspector list: symbol, title, optional second line, chevron.
struct InspectorRow: View {
    let symbol: String
    let title: String
    var subtitle = ""
    var lines = 1
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: symbol).font(.system(size: 14)).foregroundStyle(.tint).frame(width: 18)
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(.system(size: 13)).lineLimit(lines).multilineTextAlignment(.leading)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 12).padding(.vertical, 7)
            .frame(minHeight: 38)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// The one-line "nothing here" text inside an inspector list.
struct InspectorEmptyLine: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, minHeight: 38, alignment: .leading)
            .padding(.horizontal, 12)
    }
}

/// A photo or video as a square grid tile; tapping it opens the media viewer.
struct SharedMediaTile: View {
    let message: ChatMessage
    let conversationId: String
    let side: CGFloat

    var body: some View {
        switch message.kind {
        case .photo(let url, let width, let height):
            PhotoView(url: url, width: width, height: height, conversationId: conversationId, messageId: message.id,
                      senderName: message.isMine ? "Bạn" : message.senderName, date: message.date,
                      encrypted: message.isE2EE || url?.query?.contains("e2esession") == true, tile: side)
        case .video(let thumbnail, let duration, let url):
            VideoThumb(thumbnail: thumbnail, duration: duration, url: url, conversationId: conversationId, messageId: message.id,
                       senderName: message.isMine ? "Bạn" : message.senderName, date: message.date,
                       encrypted: message.isE2EE || url?.query?.contains("e2esession") == true, tile: side)
        default:
            EmptyView()
        }
    }
}

/// A file as a row of an inspector list (the chat's file card).
struct SharedFileRow: View {
    let message: ChatMessage

    var body: some View {
        if case .file(let name, let size, let url) = message.kind {
            FileCard(name: name, size: size, url: url, mine: false)
                .padding(.horizontal, 10).padding(.vertical, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A link as a row of an inspector list: its title and site, opened in the browser.
struct SharedLinkRow: View {
    let message: ChatMessage

    var body: some View {
        if case .link(let title, let url, _, _, let source) = message.kind {
            let site = source.isEmpty ? ChatFormat.host(url) : source
            InspectorRow(symbol: "link", title: title.isEmpty ? (url?.absoluteString ?? "Liên kết") : title, subtitle: site, lines: 2) {
                WebLink.open(url)
            }
            .disabled(WebLink.sanitized(url) == nil)
        }
    }
}
