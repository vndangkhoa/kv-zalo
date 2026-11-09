import SwiftUI

/// "Kho lưu trữ": every photo / video, file and link ever shared in the conversation, in place of the inspector's overview.
/// The lists are read page by page from the official media service (`SharedMediaModel`) and grouped by month; the next page is
/// requested by the spinner row under the last month for as long as it is on screen. The media found in the loaded messages is merged in
/// (only as far back as the store has been read), so what was just sent is here too.
struct SharedMediaBrowser: View {
    let conversationId: String
    let model: SharedMediaModel
    @Binding var kind: SharedMediaKind
    /// Back to the overview.
    let close: () -> Void

    @Environment(ZaloEngine.self) private var engine
    /// Width of the scroll view itself (not of its content, see `SharedMediaGrid`).
    @State private var outerWidth: CGFloat = 300

    var body: some View {
        VStack(spacing: 0) {
            navigation
            ScrollView {
                content
                    .padding(.horizontal, 16)
                    .padding(.top, 6)
                    .padding(.bottom, 20)
            }
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { outerWidth = $0 }
        }
        .onExitCommand(perform: close)
        // The overview has usually read the first page already; a tab it had not (or could not) read is read now.
        .task(id: kind) { await model.loadIfNeeded(kind, using: engine) }
    }

    // MARK: Navigation

    private var navigation: some View {
        VStack(spacing: 10) {
            ZStack {
                Text("Kho lưu trữ").font(.system(size: 14, weight: .semibold)).allowsHitTesting(false)
                HStack {
                    Button(action: close) {
                        HStack(spacing: 4) {
                            Image(systemName: "chevron.left").font(.system(size: 12, weight: .semibold))
                            Text("Thông tin").font(.system(size: 13))
                        }
                        .foregroundStyle(.tint)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Quay lại thông tin")
                    Spacer(minLength: 0)
                }
            }
            Picker("Loại nội dung", selection: $kind) {
                Text("Ảnh/Video").tag(SharedMediaKind.images)
                Text("Tệp").tag(SharedMediaKind.files)
                Text("Liên kết").tag(SharedMediaKind.links)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .padding(.bottom, 8)
    }

    // MARK: Content

    @ViewBuilder
    private var content: some View {
        let feed = model.feed(kind)
        let listing = model.listing(kind, loaded: engine.messages[conversationId] ?? [])
        if listing.items.isEmpty {
            placeholder(feed)
        } else {
            // One lazy stack for everything, so months of hundreds of rows build only what is on screen.
            LazyVStack(alignment: .leading, spacing: 0) {
                switch kind {
                case .images:
                    ForEach(listing.months) { month in
                        VStack(alignment: .leading, spacing: 6) {
                            monthTitle(month.title)
                            photoGrid(month)
                        }
                        .padding(.top, month.id == listing.months.first?.id ? 0 : 16)
                    }
                case .files, .links:
                    ForEach(listing.rows) { row in rowView(row) }
                }
                footer(feed)
            }
        }
    }

    private func monthTitle(_ title: String) -> some View {
        Text(title)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 4)
            .accessibilityAddTraits(.isHeader)
    }

    private func photoGrid(_ month: SharedMediaList.Month) -> some View {
        let layout = SharedMediaGrid.layout(width: SharedMediaGrid.contentWidth(outer: outerWidth, inset: 16))
        return LazyVGrid(columns: Array(repeating: GridItem(.fixed(layout.side), spacing: SharedMediaGrid.gap), count: layout.columns),
                         spacing: SharedMediaGrid.gap) {
            ForEach(month.items) { message in
                SharedMediaTile(message: message, conversationId: conversationId, side: layout.side)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func rowView(_ row: SharedMediaList.Row) -> some View {
        switch row.content {
        case .header(let title):
            monthTitle(title)
                .padding(.top, row.firstMonth ? 0 : 16)
                .padding(.bottom, 6)
        case .item(let message):
            InspectorSegment(isFirst: row.firstInMonth, isLast: row.lastInMonth, dividerInset: kind == .files ? 12 : 40) {
                if kind == .files { SharedFileRow(message: message) } else { SharedLinkRow(message: message) }
            }
        }
    }

    /// Nothing to list: the first read is out, it failed, or there is genuinely nothing shared.
    @ViewBuilder
    private func placeholder(_ feed: SharedMediaFeed) -> some View {
        Group {
            if feed.phase == .failed {
                VStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle").font(.system(size: 22)).foregroundStyle(.secondary)
                    Text("Không tải được dữ liệu").font(.system(size: 13)).foregroundStyle(.secondary)
                    retryButton
                }
            } else if feed.isPending || feed.phase == .loading {
                ProgressView().controlSize(.small)
            } else {
                Text(emptyText).font(.system(size: 13)).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, minHeight: 160)
    }

    private var emptyText: String {
        switch kind {
        case .images: "Chưa có ảnh hoặc video"
        case .files: "Chưa có tệp"
        case .links: "Chưa có liên kết"
        }
    }

    /// Under the last month. While older pages may exist this is a spinner row that asks for the next page when it appears and again each
    /// time a page lands while it is still on screen (the id is the list's position), so a tall window keeps filling; scrolling it away
    /// stops the asking. A failure replaces it with a retry — nothing is retried by itself.
    @ViewBuilder
    private func footer(_ feed: SharedMediaFeed) -> some View {
        if feed.phase == .failed {
            HStack(spacing: 8) {
                Text(feed.loaded ? "Không tải thêm được" : "Không tải được danh sách đầy đủ").font(.system(size: 12)).foregroundStyle(.secondary)
                retryButton
            }
            .frame(maxWidth: .infinity, minHeight: 36)
            .padding(.top, 8)
        } else if feed.phase == .loading || (feed.loaded && feed.more) {
            ProgressView().controlSize(.small)
                .frame(maxWidth: .infinity, minHeight: 36)
                .padding(.top, 8)
                .task(id: "\(kind.rawValue)|\(feed.cursor ?? "")|\(feed.items.count)") { requestMore() }
        }
    }

    private var retryButton: some View {
        Button("Thử lại") {
            let kind = kind
            Task { await model.retry(kind, using: engine) }
        }
        .buttonStyle(.plain)
        .font(.system(size: 12))
        .foregroundStyle(.tint)
    }

    private func requestMore() {
        let kind = kind
        Task { await model.loadMore(kind, using: engine) }
    }
}
