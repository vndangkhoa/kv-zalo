import SwiftUI
import Observation

/// The pure parts of the shared-media lists (inspector overview and "Kho lưu trữ" page): what belongs in a list, how store
/// pages and the messages loaded in the chat are merged, and how a list is grouped by month.
enum SharedMediaList {
    /// How many items the inspector overview shows per list: photos / videos in a 3-column grid, files and links as rows.
    static func previewLimit(_ kind: SharedMediaKind) -> Int {
        switch kind {
        case .images: 9
        case .files, .links: 4
        }
    }

    /// Whether a loaded chat message is shared content of this list. A message that failed to send is not shared; a recalled one
    /// (kind `.recalled`) is no longer media at all.
    static func qualifies(_ message: ChatMessage, _ kind: SharedMediaKind) -> Bool {
        kind.accepts(message.kind) && message.status != .failed
    }

    /// Newest first; equal times fall back to the id (shorter first, as the official DB orders msgIds) so the order never depends on input order.
    static func precedes(_ a: ChatMessage, _ b: ChatMessage) -> Bool {
        if a.date != b.date { return a.date > b.date }
        return a.id.count != b.id.count ? a.id.count > b.id.count : a.id > b.id
    }

    static func newestFirst(_ items: [ChatMessage]) -> [ChatMessage] {
        items.sorted(by: precedes)
    }

    /// The media-store items plus the media found in the loaded messages (a photo just sent shows up before the store knows it),
    /// one entry per message id — the loaded copy wins — newest first. A loaded message is the truth about its own id: when it no
    /// longer qualifies (recalled, deleted for everyone, failed) the stored item with that id is dropped too.
    /// `loadedSince`: loaded messages older than this are not added (the store has not paged that far yet, so a month would look
    /// complete while it is not); they still replace or drop a stored item with their id.
    static func merged(stored: [ChatMessage], loaded: [ChatMessage], kind: SharedMediaKind, loadedSince: Date? = nil) -> [ChatMessage] {
        var byId: [String: ChatMessage] = [:]
        for message in stored where kind.accepts(message.kind) { byId[message.id] = message }
        for message in loaded {
            if qualifies(message, kind) {
                if let loadedSince, message.date < loadedSince { continue }
                byId[message.id] = message
            } else {
                byId[message.id] = nil
            }
        }
        return newestFirst(Array(byId.values))
    }

    /// The overview's items of one list: what `merged(...).prefix(previewLimit)` gives, found without sorting everything.
    static func preview(_ kind: SharedMediaKind, stored: [ChatMessage], loaded: [ChatMessage]) -> [ChatMessage] {
        let limit = previewLimit(kind)
        var top: [ChatMessage] = []
        func offer(_ message: ChatMessage) {
            if top.count == limit, let last = top.last, !precedes(message, last) { return }
            let at = top.firstIndex { precedes(message, $0) } ?? top.count
            top.insert(message, at: at)
            if top.count > limit { top.removeLast() }
        }
        var loadedIds = Set<String>(minimumCapacity: loaded.count)
        for message in loaded {
            loadedIds.insert(message.id)
            if qualifies(message, kind) { offer(message) }
        }
        for message in stored where kind.accepts(message.kind) && !loadedIds.contains(message.id) { offer(message) }
        return top
    }

    /// One calendar month of a list ("Tháng 10, 2026").
    struct Month: Identifiable, Equatable {
        let id: String
        let title: String
        var items: [ChatMessage]
    }

    static func monthTitle(_ date: Date, calendar: Calendar = .current) -> String {
        let parts = calendar.dateComponents([.year, .month], from: date)
        return "Tháng \(parts.month ?? 1), \(parts.year ?? 1970)"
    }

    /// Groups newest-first `items` into consecutive months (the order is kept).
    static func months(_ items: [ChatMessage], calendar: Calendar = .current) -> [Month] {
        var result: [Month] = []
        for message in items {
            let parts = calendar.dateComponents([.year, .month], from: message.date)
            let id = "\(parts.year ?? 0)-\(parts.month ?? 0)"
            if result.last?.id == id {
                result[result.count - 1].items.append(message)
            } else {
                result.append(Month(id: id, title: monthTitle(message.date, calendar: calendar), items: [message]))
            }
        }
        return result
    }

    /// The months of a row list flattened — a header, then its items — so one lazy stack builds only the rows on screen.
    struct Row: Identifiable, Equatable {
        enum Content: Equatable {
            case header(String)
            case item(ChatMessage)
        }
        let id: String
        let content: Content
        /// The header of the first month (nothing above it).
        var firstMonth = false
        var firstInMonth = false
        var lastInMonth = false
    }

    static func rows(_ months: [Month]) -> [Row] {
        var rows: [Row] = []
        for (index, month) in months.enumerated() {
            rows.append(Row(id: "month-" + month.id, content: .header(month.title), firstMonth: index == 0))
            for (at, message) in month.items.enumerated() {
                rows.append(Row(id: message.id, content: .item(message), firstInMonth: at == 0, lastInMonth: at == month.items.count - 1))
            }
        }
        return rows
    }

    /// Everything the full page draws for one list, derived once per change of its inputs.
    struct Listing: Equatable {
        var items: [ChatMessage] = []
        var months: [Month] = []
        var rows: [Row] = []
    }
}

/// Tile geometry of the storage page's photo grid: three columns at the inspector's default width, more when it is wider.
///
/// Tiles are sized from the scroll view's own width less `scrollerReserve`, never from the width of its content: with classic scroll
/// bars that width changes when the bar appears, square tiles then change the content height, the bar disappears again… AppKit gives
/// up on that layout loop by crashing.
enum SharedMediaGrid {
    static let gap: CGFloat = 4

    /// Width a classic scroll bar takes from the content (0 with overlay scroll bars), kept free whether the bar shows or not.
    @MainActor static var scrollerReserve: CGFloat {
        NSScroller.preferredScrollerStyle == .legacy ? NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy) : 0
    }

    /// Content width of a scroll view `outer` points wide with `inset` of padding on each side.
    @MainActor static func contentWidth(outer: CGFloat, inset: CGFloat) -> CGFloat {
        max(0, outer - inset * 2 - scrollerReserve)
    }
    static let minimumTile: CGFloat = 84

    static func layout(width: CGFloat) -> (columns: Int, side: CGFloat) {
        guard width.isFinite, width > 0 else { return (3, minimumTile) }
        let columns = max(3, Int(((width + gap) / (minimumTile + gap)).rounded(.down)))
        let side = ((width - gap * CGFloat(columns - 1)) / CGFloat(columns)).rounded(.down)
        return (columns, max(side, 32))
    }
}

/// What has been read of one list so far: the items of the pages (newest first), where the next page starts, and the state of the
/// read in progress.
struct SharedMediaFeed: Equatable {
    enum Phase: Equatable { case idle, loading, failed }

    private(set) var items: [ChatMessage] = []
    /// Pass as `after` for the next page.
    private(set) var cursor: String?
    /// Whether an older page may exist.
    private(set) var more = true
    var phase: Phase = .idle
    /// A page arrived at least once (an empty one included).
    private(set) var loaded = false
    /// Bumps whenever `items`, `cursor`, `more` or `loaded` change (what derived lists are cached by).
    private(set) var revision = 0

    /// Nothing is known yet and nothing has failed: the overview shows a spinner instead of "nothing shared".
    var isPending: Bool { !loaded && phase != .failed }

    /// The first read failed: nothing is known, which is not the same as nothing shared.
    var didFail: Bool { !loaded && phase == .failed }

    /// Loaded chat messages older than this are left out of the full page: the store has not paged that far, and a month built from the
    /// few loaded messages alone would look complete. nil = all of them count (nothing read yet, the end reached, or the store unavailable).
    var loadedSince: Date? { loaded && more ? items.last?.date : nil }

    /// The next (older) page. Stops asking when it brought no id that was not already here.
    mutating func append(_ page: SharedMediaPage) {
        let before = items.count
        union(page.items)
        cursor = page.cursor
        more = page.more && page.cursor != nil && items.count > before
        phase = .idle
        loaded = true
        revision += 1
    }

    /// The newest page again (a reload after a new message). It is the newest window of what the service holds: items held inside that
    /// window and missing from the page are gone (recalled, deleted for everyone or for me), the others join. How deep the list has been
    /// read stays as it was — unless nothing held is connected to the page (the list was empty, or everything in it is gone, or the page
    /// is entirely new), where the page's own cursor takes over.
    mutating func merge(firstPage page: SharedMediaPage) {
        guard loaded else { append(page); return }
        let pageIds = Set(page.items.map(\.id))
        let connected = items.contains { pageIds.contains($0.id) }
        let held = items.count
        if let boundary = SharedMediaList.newestFirst(page.items).last {
            // "Inside the window" is the list's own order (time, then id), so a burst sharing the boundary's time but cut off by the
            // page limit stays.
            items.removeAll { !pageIds.contains($0.id) && SharedMediaList.precedes($0, boundary) }
        } else if !page.more && page.cursor == nil {
            items.removeAll()
        }
        let removed = items.count != held
        if items.isEmpty || !connected {
            append(page)
        } else if union(page.items) || removed {
            revision += 1
        }
    }

    /// Adds the items whose ids are not here yet; whether there were any.
    @discardableResult
    private mutating func union(_ incoming: [ChatMessage]) -> Bool {
        var seen = Set(items.map(\.id))
        let fresh = incoming.filter { seen.insert($0.id).inserted }
        guard !fresh.isEmpty else { return false }
        items = SharedMediaList.newestFirst(items + fresh)
        return true
    }
}

/// The three lists of one conversation's storage, read page by page from the official media service.
/// Owned by the inspector, so the overview's first pages are what the full page starts with.
@MainActor @Observable
final class SharedMediaModel {
    static let pageSize = 30

    let conversationId: String
    private(set) var feeds: [SharedMediaKind: SharedMediaFeed] = [:]
    @ObservationIgnored private var refreshing: Set<SharedMediaKind> = []
    /// Derived lists by the inputs they were built from (a feed revision and the loaded messages), like the chat's row cache.
    @ObservationIgnored private var previews: [SharedMediaKind: (revision: Int, loaded: [ChatMessage], items: [ChatMessage])] = [:]
    @ObservationIgnored private var listings: [SharedMediaKind: (revision: Int, loaded: [ChatMessage], listing: SharedMediaList.Listing)] = [:]
    /// How many times a derived list was built (tests).
    @ObservationIgnored private(set) var derivedBuilds = 0

    init(conversationId: String) { self.conversationId = conversationId }

    func feed(_ kind: SharedMediaKind) -> SharedMediaFeed { feeds[kind] ?? SharedMediaFeed() }

    /// The overview's items of one list: the newest few of the store's first page and the loaded messages.
    func preview(_ kind: SharedMediaKind, loaded: [ChatMessage]) -> [ChatMessage] {
        let feed = feed(kind)
        if let hit = previews[kind], hit.revision == feed.revision, hit.loaded == loaded { return hit.items }
        derivedBuilds += 1
        let items = SharedMediaList.preview(kind, stored: feed.items, loaded: loaded)
        previews[kind] = (feed.revision, loaded, items)
        return items
    }

    /// The whole list of the full page, grouped by month.
    func listing(_ kind: SharedMediaKind, loaded: [ChatMessage]) -> SharedMediaList.Listing {
        let feed = feed(kind)
        if let hit = listings[kind], hit.revision == feed.revision, hit.loaded == loaded { return hit.listing }
        derivedBuilds += 1
        let items = SharedMediaList.merged(stored: feed.items, loaded: loaded, kind: kind, loadedSince: feed.loadedSince)
        let months = SharedMediaList.months(items)
        let listing = SharedMediaList.Listing(items: items, months: months, rows: SharedMediaList.rows(months))
        listings[kind] = (feed.revision, loaded, listing)
        return listing
    }

    /// Some list has been read: later reads are reloads and do not show a spinner.
    var hasLoaded: Bool { feeds.values.contains { $0.loaded } }

    /// The newest page of every list: the first read, and the reload after a new message.
    func refresh(using source: any SharedMediaSource) async {
        let reads = SharedMediaKind.allCases.map { kind in Task { await reload(kind, source) } }
        for read in reads { await read.value }
    }

    /// The newest page of one list unless it has been read (the full page opening a tab).
    func loadIfNeeded(_ kind: SharedMediaKind, using source: any SharedMediaSource) async {
        guard !feed(kind).loaded else { return }
        await reload(kind, source)
    }

    private func reload(_ kind: SharedMediaKind, _ source: any SharedMediaSource) async {
        let current = feed(kind)
        guard current.phase != .loading, refreshing.insert(kind).inserted else { return }
        defer { refreshing.remove(kind) }
        // A list that was read before reloads quietly; a first read (or a retry) shows its state.
        let quiet = current.loaded
        if !quiet { feeds[kind, default: SharedMediaFeed()].phase = .loading }
        let page = await source.sharedMedia(in: conversationId, kind: kind, after: nil, limit: Self.pageSize)
        if let page {
            feeds[kind, default: SharedMediaFeed()].merge(firstPage: page)
        } else if !quiet {
            feeds[kind, default: SharedMediaFeed()].phase = .failed
        }
    }

    /// The next older page of a list, when the last item came into view (`retrying`: the "Thử lại" button after a failure).
    func loadMore(_ kind: SharedMediaKind, using source: any SharedMediaSource, retrying: Bool = false) async {
        let current = feed(kind)
        guard current.loaded, current.more, let cursor = current.cursor,
              current.phase == .idle || (retrying && current.phase == .failed) else { return }
        feeds[kind, default: SharedMediaFeed()].phase = .loading
        let page = await source.sharedMedia(in: conversationId, kind: kind, after: cursor, limit: Self.pageSize)
        if let page {
            feeds[kind, default: SharedMediaFeed()].append(page)
        } else {
            feeds[kind, default: SharedMediaFeed()].phase = .failed
        }
    }

    /// The "Thử lại" button: the page that failed again, or the first read.
    func retry(_ kind: SharedMediaKind, using source: any SharedMediaSource) async {
        if feed(kind).loaded { await loadMore(kind, using: source, retrying: true) } else { await reload(kind, source) }
    }
}
