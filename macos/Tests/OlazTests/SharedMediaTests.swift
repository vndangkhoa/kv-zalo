import Testing
import Foundation
@testable import Olaz

// Shared media of a conversation ("Kho lưu trữ"): the pure list logic, page decoding, the paging model, and the real
// `media-store-adapter.js` (plus the `resolveMedia` fallback of `chat-adapter.js`) in JavaScriptCore against a fake official media service.
// Every fixture is synthetic: no real names, ids or URLs.

private func message(_ id: String, _ kind: ChatMessage.Kind, at seconds: Double, status: ChatMessage.Status = .sent, mine: Bool = false) -> ChatMessage {
    ChatMessage(id: id, conversationId: "c1", senderId: "u1", senderName: "Test User", senderAvatar: nil, isMine: mine, kind: kind, text: "",
                date: Date(timeIntervalSince1970: seconds), status: status, quote: nil, reactions: [], isE2EE: false)
}

private func photo(_ id: String, at seconds: Double, status: ChatMessage.Status = .sent) -> ChatMessage {
    message(id, .photo(url: URL(string: "https://cdn.example.test/p/\(id).jpg"), width: 10, height: 10), at: seconds, status: status)
}

private func file(_ id: String, at seconds: Double) -> ChatMessage {
    message(id, .file(name: "tep-\(id).pdf", size: 1024, url: URL(string: "https://cdn.example.test/f/\(id)")), at: seconds)
}

private func link(_ id: String, at seconds: Double, url: String? = "https://site.example.test/a") -> ChatMessage {
    message(id, .link(title: "Link \(id)", url: url.flatMap(URL.init(string:)), thumbnail: nil), at: seconds)
}

private var utc: Calendar {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    return calendar
}

// MARK: - List logic

@Suite struct SharedMediaListTests {
    @Test func kindsTakeTheirOwnMessages() {
        #expect(SharedMediaKind.images.accepts(photo("a", at: 1).kind))
        #expect(SharedMediaKind.images.accepts(.video(thumbnail: nil, duration: 3)))
        #expect(!SharedMediaKind.images.accepts(file("f", at: 1).kind))
        #expect(SharedMediaKind.files.accepts(file("f", at: 1).kind))
        #expect(SharedMediaKind.links.accepts(link("l", at: 1).kind))
        // A link card whose address cannot be opened is not a shared link.
        #expect(!SharedMediaKind.links.accepts(link("l", at: 1, url: nil).kind))
        #expect(!SharedMediaKind.links.accepts(.text))
    }

    @Test func mergeKeepsOneCopyPerIdNewestFirst() {
        let stored = [photo("1", at: 100), photo("2", at: 300), file("f", at: 999), photo("3", at: 200)]
        // "2" is also loaded (the loaded copy wins), "4" was just sent and the store does not know it yet.
        var loaded2 = photo("2", at: 300)
        loaded2.senderName = "Loaded"
        let loaded = [photo("0", at: 50), loaded2, photo("4", at: 400, status: .sending), file("g", at: 10)]
        let merged = SharedMediaList.merged(stored: stored, loaded: loaded, kind: .images)
        #expect(merged.map(\.id) == ["4", "2", "3", "1", "0"])
        #expect(merged.first { $0.id == "2" }?.senderName == "Loaded")
        #expect(SharedMediaList.merged(stored: stored, loaded: loaded, kind: .files).map(\.id) == ["f", "g"])
    }

    @Test func aFailedSendIsNotSharedContent() {
        let merged = SharedMediaList.merged(stored: [], loaded: [photo("ok", at: 1), photo("bad", at: 2, status: .failed)], kind: .images)
        #expect(merged.map(\.id) == ["ok"])
    }

    @Test func equalTimesOrderByIdWhateverTheInputOrder() {
        let a = [photo("99", at: 5), photo("100", at: 5), photo("101", at: 5)]
        let expected = ["101", "100", "99"]
        #expect(SharedMediaList.newestFirst(a).map(\.id) == expected)
        #expect(SharedMediaList.newestFirst(a.reversed()).map(\.id) == expected)
    }

    @Test func previewHoldsTheLatestFewOfEachList() {
        let photos = (1...20).map { photo("p\($0)", at: Double($0)) }
        let shown = SharedMediaList.preview(.images, stored: photos, loaded: [])
        #expect(shown.count == 9 && shown.first?.id == "p20" && shown.last?.id == "p12")
        let files = (1...10).map { file("f\($0)", at: Double($0)) }
        #expect(SharedMediaList.preview(.files, stored: files, loaded: []).count == 4)
        let links = (1...10).map { link("l\($0)", at: Double($0)) }
        #expect(SharedMediaList.preview(.links, stored: [], loaded: links).map(\.id) == ["l10", "l9", "l8", "l7"])
    }

    @Test func monthsGroupConsecutiveItemsAndTitleThem() {
        // 2026-10-27, 2026-10-01, 2026-09-30 (UTC), 2025-10-15.
        let items = [photo("a", at: 1_793_059_200), photo("b", at: 1_790_812_800), photo("c", at: 1_790_726_400), photo("d", at: 1_760_486_400)]
        let months = SharedMediaList.months(items, calendar: utc)
        #expect(months.map(\.title) == ["Tháng 10, 2026", "Tháng 9, 2026", "Tháng 10, 2025"])
        #expect(months.map { $0.items.map(\.id) } == [["a", "b"], ["c"], ["d"]])
        #expect(Set(months.map(\.id)).count == 3, "the same month of another year is a different section")
        #expect(SharedMediaList.months([], calendar: utc).isEmpty)
    }

    @Test func gridKeepsThreeColumnsAtTheDefaultWidthAndGrowsWhenWider() {
        let narrow = SharedMediaGrid.layout(width: 268)
        #expect(narrow.columns == 3 && narrow.side * 3 + SharedMediaGrid.gap * 2 <= 268)
        let narrowest = SharedMediaGrid.layout(width: 228)
        #expect(narrowest.columns == 3)
        let wide = SharedMediaGrid.layout(width: 408)
        #expect(wide.columns == 4 && wide.side * 4 + SharedMediaGrid.gap * 3 <= 408)
        #expect(SharedMediaGrid.layout(width: 0).columns == 3)
    }

    /// The tile width comes from the scroll view's own width with room kept for a classic scroll bar, so it is the same whether the bar is
    /// showing or not (a width read from the content flipped the bar on and off until AppKit crashed).
    @MainActor @Test func gridWidthDoesNotDependOnTheScrollBar() {
        let width = SharedMediaGrid.contentWidth(outer: 300, inset: 16)
        #expect(width == 300 - 32 - SharedMediaGrid.scrollerReserve)
        #expect(SharedMediaGrid.contentWidth(outer: 10, inset: 16) == 0)
    }
}

// MARK: - Page decoding

@Suite struct SharedMediaPageTests {
    private func json(_ text: String) throws -> Any { try JSONSerialization.jsonObject(with: Data(text.utf8)) }

    @Test func decodesAPageShapedLikeTheAdapterEmitsIt() throws {
        let page = try #require(SharedMediaPage(json: try json("""
        {"items":[
          {"id":"5001","conversationId":"u2","senderId":"me1","senderName":"Test Me","senderAvatar":"","isMine":true,
           "kind":{"type":"photo","url":"https://cdn.example.test/p/n1.jpg","width":1280,"height":960},"text":"[Hình ảnh]",
           "date":1760000000000,"status":"sent","quote":null,"reactions":[],"mentions":[],"isE2EE":false},
          {"id":"1000000000000000020","conversationId":"u2","senderId":"u2","senderName":"Test Friend","senderAvatar":"https://cdn.example.test/a.jpg","isMine":false,
           "kind":{"type":"video","thumbnail":"https://cdn.example.test/p/t2.jpg","url":"https://cdn.example.test/p/v2.mp4","duration":13},"text":"[Video]",
           "date":1760000100000,"status":"sent","quote":null,"reactions":[],"mentions":[],"isE2EE":false},
          {"id":"f1","conversationId":"u2","senderId":"u2","senderName":"","senderAvatar":"","isMine":false,
           "kind":{"type":"file","name":"tai-lieu.pdf","size":2048,"url":"https://cdn.example.test/f/1"},"text":"[Tệp]","date":1760000200000,"status":"sent"},
          {"id":"l1","kind":{"type":"link","title":"Trang thử","url":"https://site.example.test/x","thumbnail":"","description":"Mô tả","source":"site.example.test"},"date":1760000300000},
          {"kind":{"type":"photo"}}
        ],"cursor":"1000000000000000020","more":true}
        """), conversationId: "u2"))
        #expect(page.items.map(\.id) == ["5001", "1000000000000000020", "f1", "l1"], "an entry without an id is dropped")
        #expect(page.cursor == "1000000000000000020" && page.more)
        #expect(page.items[0].isMine && page.items[0].kind == .photo(url: URL(string: "https://cdn.example.test/p/n1.jpg"), width: 1280, height: 960))
        #expect(page.items[0].date == Date(timeIntervalSince1970: 1_760_000_000))
        #expect(page.items[1].kind == .video(thumbnail: URL(string: "https://cdn.example.test/p/t2.jpg"), duration: 13, url: URL(string: "https://cdn.example.test/p/v2.mp4")))
        #expect(page.items[2].kind == .file(name: "tai-lieu.pdf", size: 2048, url: URL(string: "https://cdn.example.test/f/1")))
        #expect(page.items[3].kind == .link(title: "Trang thử", url: URL(string: "https://site.example.test/x"), thumbnail: nil, description: "Mô tả", source: "site.example.test"))
        #expect(page.items[3].conversationId == "u2", "the conversation falls back to the one asked for")
    }

    @Test func nullOrMalformedIsNotAPage() throws {
        #expect(SharedMediaPage(json: nil, conversationId: "u2") == nil)
        #expect(SharedMediaPage(json: NSNull(), conversationId: "u2") == nil)
        #expect(SharedMediaPage(json: try json(#"{"cursor":"1"}"#), conversationId: "u2") == nil)
        // An empty page is an answer: nothing (more) shared.
        let end = try #require(SharedMediaPage(json: try json(#"{"items":[],"cursor":null,"more":false}"#), conversationId: "u2"))
        #expect(end.items.isEmpty && end.cursor == nil && !end.more)
    }
}

// MARK: - Paging model

@MainActor private final class FakeSource: SharedMediaSource {
    var pages: [SharedMediaKind: [SharedMediaPage?]] = [:]
    private(set) var calls: [(kind: SharedMediaKind, cursor: String?)] = []

    func sharedMedia(in conversationId: String, kind: SharedMediaKind, after cursor: String?, limit: Int) async -> SharedMediaPage? {
        calls.append((kind, cursor))
        guard var queue = pages[kind], !queue.isEmpty else { return SharedMediaPage(items: [], cursor: nil, more: false) }
        let next = queue.removeFirst()
        pages[kind] = queue
        return next
    }
}

@MainActor @Suite struct SharedMediaModelTests {
    @Test func feedStopsWhenAPageBringsNothingNew() {
        var feed = SharedMediaFeed()
        #expect(feed.isPending)
        feed.append(SharedMediaPage(items: [photo("3", at: 3), photo("2", at: 2)], cursor: "2", more: true))
        #expect(feed.loaded && !feed.isPending && feed.more && feed.cursor == "2")
        feed.append(SharedMediaPage(items: [photo("1", at: 1)], cursor: "1", more: true))
        #expect(feed.items.map(\.id) == ["3", "2", "1"] && feed.more && feed.cursor == "1")
        // The same ids again (a cursor that did not move): the end, whatever the page claims.
        feed.append(SharedMediaPage(items: [photo("1", at: 1)], cursor: "1", more: true))
        #expect(feed.items.count == 3 && !feed.more)
    }

    @Test func aFirstPageOfNothingIsAnAnswer() {
        var feed = SharedMediaFeed()
        feed.append(SharedMediaPage(items: [], cursor: nil, more: false))
        #expect(feed.loaded && !feed.isPending && feed.items.isEmpty && !feed.more)
    }

    @Test func reloadingTheFirstPageKeepsHowDeepTheListWasRead() {
        var feed = SharedMediaFeed()
        feed.append(SharedMediaPage(items: [photo("3", at: 3), photo("2", at: 2)], cursor: "2", more: true))
        feed.append(SharedMediaPage(items: [photo("1", at: 1)], cursor: "1", more: true))
        feed.merge(firstPage: SharedMediaPage(items: [photo("4", at: 4), photo("3", at: 3)], cursor: "3", more: true))
        #expect(feed.items.map(\.id) == ["4", "3", "2", "1"])
        #expect(feed.cursor == "1" && feed.more)
    }

    @Test func overviewReadsEveryListAndTheFullPageContinuesFromThere() async {
        let source = FakeSource()
        source.pages[.images] = [
            SharedMediaPage(items: [photo("3", at: 3), photo("2", at: 2)], cursor: "2", more: true),
            SharedMediaPage(items: [photo("1", at: 1)], cursor: "1", more: true),
            SharedMediaPage(items: [], cursor: nil, more: false),
        ]
        source.pages[.files] = [SharedMediaPage(items: [file("f1", at: 1)], cursor: "f1", more: true)]
        let model = SharedMediaModel(conversationId: "c1")
        #expect(!model.hasLoaded && model.feed(.images).isPending)
        await model.refresh(using: source)
        #expect(model.hasLoaded)
        #expect(model.feed(.images).items.map(\.id) == ["3", "2"])
        #expect(model.feed(.files).items.map(\.id) == ["f1"])
        #expect(model.feed(.links).loaded && model.feed(.links).items.isEmpty)

        await model.loadMore(.images, using: source)
        #expect(model.feed(.images).items.map(\.id) == ["3", "2", "1"])
        #expect(source.calls.last?.kind == .images && source.calls.last?.cursor == "2", "the next page starts after the oldest id read")
        // An empty page ends the list; asking again does nothing.
        await model.loadMore(.images, using: source)
        #expect(!model.feed(.images).more)
        let asked = source.calls.count
        await model.loadMore(.images, using: source)
        #expect(source.calls.count == asked)
    }

    @Test func aFailedFirstReadCanBeRetriedAndAFailedPageKeepsTheItems() async {
        let source = FakeSource()
        source.pages[.images] = [nil, SharedMediaPage(items: [photo("2", at: 2)], cursor: "2", more: true), nil,
                                 SharedMediaPage(items: [photo("1", at: 1)], cursor: "1", more: false)]
        let model = SharedMediaModel(conversationId: "c1")
        await model.refresh(using: source)
        #expect(model.feed(.images).phase == .failed && !model.feed(.images).isPending, "the overview falls back to the loaded messages")
        await model.retry(.images, using: source)
        #expect(model.feed(.images).phase == .idle && model.feed(.images).items.map(\.id) == ["2"])
        // The next page fails: the items stay, nothing is retried by itself, the retry button asks the same page again.
        await model.loadMore(.images, using: source)
        #expect(model.feed(.images).phase == .failed && model.feed(.images).items.map(\.id) == ["2"])
        await model.loadMore(.images, using: source)
        #expect(model.feed(.images).phase == .failed, "a scroll event does not retry on its own")
        await model.retry(.images, using: source)
        #expect(model.feed(.images).items.map(\.id) == ["2", "1"] && !model.feed(.images).more)
    }

    @Test func aQuietReloadAddsNewItemsWithoutShowingProgress() async {
        let source = FakeSource()
        source.pages[.images] = [SharedMediaPage(items: [photo("1", at: 1)], cursor: "1", more: true),
                                 SharedMediaPage(items: [photo("2", at: 2), photo("1", at: 1)], cursor: "1", more: true)]
        let model = SharedMediaModel(conversationId: "c1")
        await model.refresh(using: source)
        await model.refresh(using: source)
        #expect(model.feed(.images).items.map(\.id) == ["2", "1"] && model.feed(.images).phase == .idle)
        // A reload that fails changes nothing.
        source.pages[.images] = [nil]
        await model.refresh(using: source)
        #expect(model.feed(.images).items.map(\.id) == ["2", "1"] && model.feed(.images).phase == .idle)
    }

    @Test func aTabIsReadOnceWhenOpened() async {
        let source = FakeSource()
        source.pages[.links] = [SharedMediaPage(items: [link("l1", at: 1)], cursor: "l1", more: true)]
        let model = SharedMediaModel(conversationId: "c1")
        await model.loadIfNeeded(.links, using: source)
        await model.loadIfNeeded(.links, using: source)
        #expect(source.calls.count == 1 && model.feed(.links).items.map(\.id) == ["l1"])
    }
}

// MARK: - media-store-adapter.js (JavaScriptCore, fake official media service)

@Suite struct MediaStoreAdapterTests {
    /// Fake service in webpack module "dEOl" (`withModule`) or only reachable by scanning `__zaloEachExport` (`scanOnly`); every call is recorded
    /// in `calls` and answered from the `replies` queue (an `Error` rejects). `active` conversations start in the service's active set.
    private func harness(replies: String, active: [String] = [], withModule: Bool = true, scanOnly: Bool = false) -> ScriptHarness {
        let h = ScriptHarness(prelude: """
        window.__zaloStore = { getState: function () { return { user: { userId: 'me1', avatar: 'https://cdn.example.test/me.jpg' } }; } };
        window.__zaloChat = { __core: {
          isMeUid: function (u) { return u === '0' || u === 'me1'; }, meNow: function () { return 'me1'; }, myName: function () { return 'Test Me'; },
          nameOf: function (u) { return u === 'u2' ? 'Test Friend' : ''; }, avatarOf: function (u) { return u === 'u2' ? 'https://cdn.example.test/u2.jpg' : ''; },
          absUrl: function (v) { var u = typeof v === 'string' ? v.trim() : ''; return /^https?:\\/\\/[^\\s/]/i.test(u) ? u : ''; }
        } };
        var calls = [], activated = [], deactivated = [], replies = \(replies);
        var svc = {
          _activeConversationList: new Set([\(active.map { "'\($0)'" }.joined(separator: ", "))]),
          activateConversation: function (c) { activated.push(c); this._activeConversationList.add(c); },
          deactivateConversation: function (c) { deactivated.push(c); this._activeConversationList.delete(c); },
          getChatMedia: async function (conv, kind, last, limit, options) {
            calls.push({ conv: conv, kind: kind, last: last, limit: limit, options: options, active: this._activeConversationList.has(conv) });
            var r = replies.shift();
            if (r === 'throw') throw new Error('boom');
            return r;
          }
        };
        var mod = { MEDIA_TYPE: { images: { type: 'image' }, files: { type: 'file' }, links: { type: 'link' } }, default: svc };
        window.__zaloRequire = { c: \(withModule && !scanOnly ? "{ dEOl: { exports: mod } }" : "{ other: { exports: { x: 1 } } }") };
        window.__zaloEachExport = function (req, fn) {
          fn(null); fn({ nothing: true }); fn({ MEDIA_TYPE: {}, default: {} });
          \(scanOnly ? "fn(mod);" : "")
        };
        """)
        h.load("media-store-adapter.js")
        return h
    }

    private func item(_ fields: String) -> String { "{ userId: 'u2', \(fields) }" }

    /// Newest-first expectations are checked on the ids: a mixed page of every kind of image item.
    private let photoPage = """
    { items: [
      { mediaId: 'm1', msgId: '9990', cliMsgId: '5001', fromUid: '0', userId: 'u2', sendDttm: 1760000000000, type: 'image', subType: 1, width: 640, height: 480,
        message: { title: '', normalUrl: 'https://cdn.example.test/p/n1.jpg', hdUrl: 'https://cdn.example.test/p/h1.jpg', thumbUrl: 'https://cdn.example.test/p/t1.jpg', params: '{"width":1280,"height":960,"hd":"x"}' } },
      { mediaId: 'm2', msgId: '10020', cliMsgId: 7002, fromUid: 'u2', userId: 'u2', sendDttm: 1760000100000, type: 'image', subType: 2,
        message: { thumbUrl: 'https://cdn.example.test/p/t2.jpg', oriUrl: 'https://cdn.example.test/p/v2.mp4', params: '{"duration":12500}' } },
      { mediaId: 'm3', msgId: '10010', cliMsgId: '7003', fromUid: 'u2', userId: 'u2', sendDttm: 1760000050000, type: 'image', subType: 1,
        message: { hdUrl: 'https://cdn.example.test/p/h3.jpg', params: '{}' }, width: 300, height: 200 },
      { mediaId: 'm4', msgId: '10030', cliMsgId: '7004', fromUid: 'u2', userId: 'u2', sendDttm: 1760000020000, type: 'image', subType: 3,
        message: { href: 'https://cdn.example.test/p/d4.png', params: '{"width":100,"height":50}' } },
      { mediaId: 'm5', msgId: '10040_x', cliMsgId: '7005', fromUid: 'u2', userId: 'u2', sendDttm: 1760000010000, type: 'image', subType: 1,
        message: { thumbUrl: 'https://cdn.example.test/p/t5.jpg' } },
      { mediaId: 'bad', msgId: '10050', type: 'sticker' },
      null
    ], isSupportedLoadMediaFromMessage: false }
    """

    @Test func mapsEveryKindOfImageItemToTheChatMessageShape() throws {
        let h = harness(replies: "[\(photoPage)]")
        let page = try #require(h.run("p", "window.__zaloMediaStore.page('u2', 'images', null, 30)"))
        let items = try #require(page["items"] as? [[String: Any]])
        // Newest first; my own message is keyed by the client message id, the others by the server msgId (a fake id falls back to the client id).
        #expect(items.map { $0["id"] as? String } == ["10020", "10010", "10030", "7005", "5001"])
        let video = items[0], photo = items[1], doodle = items[2], thumbOnly = items[3], mine = items[4]
        #expect((video["kind"] as? [String: Any])?["type"] as? String == "video")
        #expect((video["kind"] as? [String: Any])?["duration"] as? Int == 13, "milliseconds in the params become seconds")
        #expect((video["kind"] as? [String: Any])?["url"] as? String == "https://cdn.example.test/p/v2.mp4")
        #expect((video["kind"] as? [String: Any])?["thumbnail"] as? String == "https://cdn.example.test/p/t2.jpg")
        #expect(video["senderName"] as? String == "Test Friend" && video["isMine"] as? Bool == false && video["status"] as? String == "sent")
        #expect((photo["kind"] as? [String: Any])?["url"] as? String == "https://cdn.example.test/p/h3.jpg", "normalUrl, then hdUrl, oriUrl, thumbUrl")
        #expect((photo["kind"] as? [String: Any])?["width"] as? Int == 300 && (photo["kind"] as? [String: Any])?["height"] as? Int == 200)
        #expect((mine["kind"] as? [String: Any])?["url"] as? String == "https://cdn.example.test/p/n1.jpg")
        #expect((mine["kind"] as? [String: Any])?["width"] as? Int == 1280 && (mine["kind"] as? [String: Any])?["height"] as? Int == 960, "the params size wins")
        #expect(mine["isMine"] as? Bool == true && mine["senderId"] as? String == "me1" && mine["senderName"] as? String == "Test Me")
        #expect(mine["senderAvatar"] as? String == "https://cdn.example.test/me.jpg")
        #expect(mine["date"] as? Double == 1_760_000_000_000 && mine["conversationId"] as? String == "u2")
        #expect((doodle["kind"] as? [String: Any])?["type"] as? String == "photo" && (doodle["kind"] as? [String: Any])?["url"] as? String == "https://cdn.example.test/p/d4.png")
        #expect((thumbOnly["kind"] as? [String: Any])?["url"] as? String == "https://cdn.example.test/p/t5.jpg")
        // The cursor is the oldest server msgId in the DB's order (shorter ids first): "9990" before "10010".
        #expect(page["cursor"] as? String == "9990" && page["more"] as? Bool == true)
    }

    @Test func mapsFilesAndLinks() throws {
        let files = harness(replies: """
        [{ items: [
          { msgId: '2000', cliMsgId: '1', fromUid: 'u2', userId: 'u2', sendDttm: 1760000000, type: 'file', message: { title: 'tai-lieu.pdf', href: 'https://cdn.example.test/f/1', params: '{"fileSize":"2048","fType":0}' } },
          { msgId: '2001', cliMsgId: '2', fromUid: 'u2', userId: 'u2', sendDttm: 1760000100000, type: 'file', message: { title: 'thu-muc', href: 'https://cdn.example.test/f/2', params: '{"fileSize":0,"fType":2}' } }
        ] }]
        """)
        let filePage = try #require(files.run("f", "window.__zaloMediaStore.page('u2', 'files', null, 20)"))
        let fileItems = try #require(filePage["items"] as? [[String: Any]])
        #expect(fileItems.map { $0["id"] as? String } == ["2001", "2000"])
        #expect((fileItems[1]["kind"] as? [String: Any])?["name"] as? String == "tai-lieu.pdf")
        #expect((fileItems[1]["kind"] as? [String: Any])?["size"] as? Int == 2048)
        #expect(fileItems[1]["date"] as? Double == 1_760_000_000_000, "a time in seconds becomes milliseconds")
        #expect(fileItems[0]["text"] as? String == "[Thư mục]")

        let links = harness(replies: """
        [{ items: [
          { msgId: '3000', cliMsgId: '1', fromUid: 'u2', userId: 'u2', sendDttm: 1760000000000, type: 'link',
            message: { title: 'xem cai nay', description: 'Mô tả ngắn', href: 'https://www.site.example.test/a?b=1', thumb: '//cdn.example.test/t.jpg', params: '{"mediaTitle":"Trang thử","src":"site.example.test"}' } },
          { msgId: '3001', cliMsgId: '2', fromUid: 'u2', userId: 'u2', sendDttm: 1760000100000, type: 'link', message: { title: 'plain', href: 'https://www.site.example.test/c', params: '{}' } },
          { msgId: '3002', cliMsgId: '3', fromUid: 'u2', userId: 'u2', sendDttm: 1760000200000, type: 'link', message: { title: 'khong mo duoc', href: 'smb://host/share', params: '{}' } }
        ] }]
        """)
        let linkPage = try #require(links.run("l", "window.__zaloMediaStore.page('u2', 'links', null, 20)"))
        let linkItems = try #require(linkPage["items"] as? [[String: Any]])
        #expect(linkItems.map { $0["id"] as? String } == ["3001", "3000"], "a link that cannot be opened (not http/https) is not listed")
        let first = try #require(linkItems[1]["kind"] as? [String: Any])
        #expect(first["title"] as? String == "Trang thử" && first["url"] as? String == "https://www.site.example.test/a?b=1")
        #expect(first["source"] as? String == "site.example.test" && first["description"] as? String == "Mô tả ngắn")
        #expect(first["thumbnail"] as? String == "", "no absolute thumbnail: left empty, as in the chat cards")
        let second = try #require(linkItems[0]["kind"] as? [String: Any])
        #expect(second["title"] as? String == "https://www.site.example.test/c" && second["source"] as? String == "site.example.test")
    }

    @Test func pagesWithTheCursorAndStopsOnAnEmptyPage() throws {
        let h = harness(replies: "[\(photoPage), { items: [{ msgId: '9980', cliMsgId: '1', fromUid: 'u2', userId: 'u2', sendDttm: 1750000000000, type: 'image', subType: 1, message: { normalUrl: 'https://cdn.example.test/p/old.jpg' } }] }, { items: [] }]")
        _ = h.run("a", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        let second = try #require(h.run("b", "window.__zaloMediaStore.page('u2', 'images', '9990', 30)"))
        #expect(h.text("JSON.stringify(calls[1])").contains(#""last":"9990""#) && h.text("JSON.stringify(calls[0].last)") == "null")
        #expect(second["cursor"] as? String == "9980" && second["more"] as? Bool == true)
        let end = h.run("c", "window.__zaloMediaStore.page('u2', 'images', '9980', 30)")
        #expect((end?["items"] as? [Any])?.isEmpty == true && end?["more"] as? Bool == false && end?["cursor"] is NSNull)
        #expect(h.text("calls.map(function (c) { return c.kind + ':' + c.limit + ':' + JSON.stringify(c.options); }).join('|')")
                == "images:30:{}|images:30:{}|images:30:{}", "plain reads pass no option")
    }

    @Test func aPageThatDoesNotMovePastTheCursorEnds() throws {
        let h = harness(replies: "[{ items: [{ msgId: '9990', cliMsgId: '1', fromUid: 'u2', userId: 'u2', sendDttm: 1760000000000, type: 'image', subType: 1, message: { normalUrl: 'https://cdn.example.test/p/a.jpg' } }] }]")
        let page = try #require(h.run("a", "window.__zaloMediaStore.page('u2', 'images', '9990', 30)"))
        #expect(page["more"] as? Bool == false)
    }

    @Test func cloudMessageHistoryIsNeverRead() throws {
        // A result that offers "load media from cloud messages" is still just the end of the local list: no second read, no option passed.
        let h = harness(replies: "[{ items: [], isSupportedLoadMediaFromMessage: true, hasMore: true }]")
        let page = try #require(h.run("a", "window.__zaloMediaStore.page('me1', 'images', '4000', 30)"))
        #expect((page["items"] as? [Any])?.isEmpty == true && page["more"] as? Bool == false && page["cursor"] is NSNull)
        #expect(h.number("calls.length") == 1)
        #expect(h.text("JSON.stringify(calls[0].options)") == "{}")
    }

    @Test func theConversationIsActiveOnlyWhileReading() {
        let h = harness(replies: "[\(photoPage), { items: [] }]")
        _ = h.run("a", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        #expect(h.text("JSON.stringify([activated, deactivated, calls[0].active, svc._activeConversationList.size])") == #"[["u2"],["u2"],true,0]"#)
        // Reads that overlap share one activation.
        let both = harness(replies: "[{ items: [] }, { items: [] }]")
        both.start("x", "Promise.all([window.__zaloMediaStore.page('u2', 'images', null, 30), window.__zaloMediaStore.page('u2', 'files', null, 30)])")
        both.pump()
        #expect(both.text("JSON.stringify([activated, deactivated])") == #"[["u2"],["u2"]]"#)
        // One the official client had active is left active.
        let held = harness(replies: "[{ items: [] }]", active: ["u2"])
        _ = held.run("z", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        #expect(held.text("JSON.stringify([activated, deactivated])") == "[[],[]]")
    }

    @Test func aFailingOrMissingServiceAnswersNullAndStillLetsGo() {
        let failing = harness(replies: "['throw']")
        failing.start("a", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        failing.pump()
        #expect(failing.isSettled("a") && failing.result("a") == nil && failing.text("__results['a']") == "null")
        #expect(failing.text("JSON.stringify([activated, deactivated])") == #"[["u2"],["u2"]]"#)

        let missing = harness(replies: "[]", withModule: false)
        missing.start("b", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        missing.pump()
        #expect(missing.text("__results['b']") == "null", "no service anywhere: null, not an empty page")
        #expect(missing.number("calls.length") == 0)

        let unknown = harness(replies: "[{ items: [] }]")
        unknown.start("c", "window.__zaloMediaStore.page('u2', 'stickers', null, 30)")
        unknown.pump()
        #expect(unknown.text("__results['c']") == "null" && unknown.number("calls.length") == 0)
    }

    @Test func findsTheServiceByShapeWhenTheModuleIdChanged() throws {
        let h = harness(replies: "[{ items: [] }]", scanOnly: true)
        let page = try #require(h.run("a", "window.__zaloMediaStore.page('u2', 'files', null, 30)"))
        #expect((page["items"] as? [Any])?.isEmpty == true && h.number("calls.length") == 1)
    }

    @Test func remembersRawItemsByEveryIdTheyAreKnownBy() throws {
        let h = harness(replies: "[\(photoPage)]")
        _ = h.run("a", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        // Listed id (mine: client id), server msgId, client id; another conversation does not see it.
        #expect(h.text("window.__zaloMediaStore.item('u2', '5001').mediaId") == "m1")
        #expect(h.text("window.__zaloMediaStore.item('u2', '9990').mediaId") == "m1")
        #expect(h.text("window.__zaloMediaStore.item('u2', '10020').message.oriUrl") == "https://cdn.example.test/p/v2.mp4")
        #expect(h.text("String(window.__zaloMediaStore.item('u3', '10020'))") == "null")
        #expect(h.text("String(window.__zaloMediaStore.item('u2', 'nope'))") == "null")
    }

    /// `count` image items of conversation `conv`, numbered from `first` (client id = the number, server msgId = 100000 + number).
    private func imageItems(_ conv: String, from first: Int, count: Int) -> String {
        "(function () { var items = []; for (var i = 0; i < \(count); i++) { var n = \(first) + i; items.push({ msgId: String(100000 + n), cliMsgId: String(n), fromUid: 'u2', userId: '\(conv)', sendDttm: 1760000000000 + n, type: 'image', subType: 1, message: { normalUrl: 'https://cdn.example.test/p/' + n } }); } return { items: items }; })()"
    }

    @Test func onlyPhotosAndVideosAreRemembered() {
        let h = harness(replies: """
        [{ items: [{ msgId: '2000', cliMsgId: '1', fromUid: 'u2', userId: 'u2', sendDttm: 1760000000000, type: 'file', message: { title: 'a.pdf', href: 'https://cdn.example.test/f/1', params: '{}' } }] },
         { items: [{ msgId: '3000', cliMsgId: '2', fromUid: 'u2', userId: 'u2', sendDttm: 1760000000000, type: 'link', message: { href: 'https://site.example.test/a', params: '{}' } }] },
         \(photoPage)]
        """)
        _ = h.run("f", "window.__zaloMediaStore.page('u2', 'files', null, 30)")
        _ = h.run("l", "window.__zaloMediaStore.page('u2', 'links', null, 30)")
        #expect(h.text("String(window.__zaloMediaStore.item('u2', '2000'))") == "null" && h.text("String(window.__zaloMediaStore.item('u2', '3000'))") == "null")
        _ = h.run("i", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        #expect(h.text("window.__zaloMediaStore.item('u2', '5001').mediaId") == "m1")
    }

    @Test func theRawItemStoreKeepsTheLatestSixHundredPerConversation() {
        let h = harness(replies: "[\(imageItems("u2", from: 1, count: 700))]")
        _ = h.run("a", "window.__zaloMediaStore.page('u2', 'images', null, 100)")
        // The first 100 listed are gone — by every id they were known by — the rest are still found.
        #expect(h.text("String(window.__zaloMediaStore.item('u2', '100'))") == "null")
        #expect(h.text("String(window.__zaloMediaStore.item('u2', '100100'))") == "null")
        #expect(h.text("window.__zaloMediaStore.item('u2', '101').message.normalUrl") == "https://cdn.example.test/p/101")
        #expect(h.text("window.__zaloMediaStore.item('u2', '100700').message.normalUrl") == "https://cdn.example.test/p/700")
    }

    @Test func theRawItemStoreKeepsTheLastFewConversations() {
        let replies = ["c1", "c2", "c3", "c4", "c1", "c5"].enumerated().map { imageItems($1, from: ($0 + 1) * 10, count: 1) }.joined(separator: ", ")
        let h = harness(replies: "[\(replies)]")
        for (index, conv) in ["c1", "c2", "c3", "c4", "c1", "c5"].enumerated() {
            _ = h.run("p\(index)", "window.__zaloMediaStore.page('\(conv)', 'images', null, 30)")
        }
        // c1 was listed again after c2…c4, so c2 is the least recently used and went when c5 came.
        #expect(h.text("String(window.__zaloMediaStore.item('c2', '20'))") == "null")
        #expect(h.text("window.__zaloMediaStore.item('c1', '50').message.normalUrl") == "https://cdn.example.test/p/50")
        #expect(h.text("window.__zaloMediaStore.item('c3', '30').message.normalUrl") == "https://cdn.example.test/p/30")
        #expect(h.text("window.__zaloMediaStore.item('c5', '60').message.normalUrl") == "https://cdn.example.test/p/60")
    }

    @Test func adaptersPageDecodesInSwift() throws {
        let h = harness(replies: "[\(photoPage)]")
        h.start("a", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        h.pump()
        let text = h.text("__results['a']")
        let object = try JSONSerialization.jsonObject(with: Data(text.utf8))
        let page = try #require(SharedMediaPage(json: object, conversationId: "u2"))
        #expect(page.items.map(\.id) == ["10020", "10010", "10030", "7005", "5001"] && page.cursor == "9990" && page.more)
        #expect(page.items.allSatisfy { SharedMediaKind.images.accepts($0.kind) })
        #expect(page.items[4].isMine && page.items[4].senderName == "Test Me")
    }
}

// MARK: - resolveMedia for a photo the chat box has not loaded (chat-adapter.js + media-store-adapter.js together)

@Suite struct MediaStoreResolveTests {
    /// The real chat adapter and media-store adapter over fake official services: enough of the DI container, the media modules and the
    /// store for `resolveMedia` to reach the downloader's candidate lookup, which records the message it is handed in `seen`.
    private func harness() -> ScriptHarness {
        let h = ScriptHarness(prelude: """
        var seen = [], files = { '/tmp/olaz-test/h1.jpg': true }, videoCalls = [], pcloudNoKey = false;
        window.__zaloStore = { getState: function () { return { user: { userId: 'me1' }, conversations: { alls: {} } }; }, subscribe: function () {} };
        var friends = { getDName: function () { return ''; }, getAvatar: function () { return ''; } };
        var hub = {
          broadCastMessage: function () {}, chatboxController: { convId: 'u2' }, openConversation: function () {}, conversationWillOpen: function () {},
          getFriendManagerModule: function () { return Promise.resolve({ default: friends }); },
          getGroupManagerModule: function () { return Promise.resolve({ default: {} }); }
        };
        var svc = { _activeConversationList: new Set(), activateConversation: function () {}, deactivateConversation: function () {},
          getChatMedia: async function () { return { items: [
            { msgId: '9990', cliMsgId: '5001', fromUid: '0', userId: 'u2', sendDttm: 1760000000000, type: 'image', subType: 1,
              message: { hdUrl: 'https://cdn.example.test/p/h1.jpg', thumbUrl: 'https://cdn.example.test/p/t1.jpg' } },
            { msgId: '10020', cliMsgId: '7002', fromUid: 'u2', userId: 'u2', sendDttm: 1760000100000, type: 'image', subType: 2,
              message: { thumbUrl: 'https://cdn.example.test/p/t2.jpg', oriUrl: 'https://cdn.example.test/p/v2.mp4' } }
          ] }; } };
        // The host's catch-all placeholder: any member is a function (so it passes every shape test), calling one gives nothing.
        var catchAll = new Proxy(function () {}, { get: function (t, k) { return typeof k === 'symbol' ? undefined : catchAll; }, apply: function () { return undefined; } });
        var exportsList = [
          catchAll,
          { cacheMessageLoader: new Map(), getLastMessagesForConversation: function () {} },
          { mapIdTyping: {}, addItemTyping: function () {}, removeItemTyping: function () {} },
          { generateReactMsg: function () {}, updateReactionListener: [], addUpdateReactListener: function () {}, getMessageReactions: function () {} },
          { sendQueue: [], sendReaction: function () {} },
          { stickerUrl: 'https://cdn.example.test/s?id=', sendToMeId: 'me1' },
          { a: { QualityOnly: ['q'] }, b: { HDUrl: 'hdUrl', ThumbUrl: 'thumbUrl' },
            d: function (types, msg) { /* uniqBy */ seen.push(msg); return [{ url: 'https://cdn.example.test/p/h1.jpg', localPath: '/tmp/olaz-test/h1.jpg', type: 'hdUrl' }]; } },
          { loadPC: function loadPC() { /* LoadPhotoPC */ return Promise.resolve(); } },
          { default: { fileExists: async function (p) { /* access */ return !!files[p]; }, getEncodedPathUrl: function () {} } },
          // The inline video player's download: CloudService ("/1VS"), the entry builder ("uP4a"), constants ("JprO"), utils ("z0WU"), checkPCloudError ("WKSW").
          { CloudService: { getInstance: function () { return { downloadWithMultiSrc: function (how, msg, opts) {
              videoCalls.push({ how: how, msg: msg, opts: opts });
              return Promise.resolve({ savePath: '/tmp/olaz-test/ZaloDownloads/video/7002_u2_u2', isCached: false, downloadError: null }); } }; } } },
          { c: function (view) { this.view = view; this.getBuilder = function () { var e = { view: view };
              return { withType: function (v) { e.type = v; return this; }, withMode: function (v) { e.mode = v; return this; },
                       withCode: function (v) { e.code = v; return this; }, withConvType: function (v) { e.convType = v; return this; }, build: function () { return e; } }; }; },
            a: { ChatBoxView: 'ChatBoxView' }, b: { 1: 3, 2: 2, 3: 1 }, d: { CBVVideoInline: 8 }, e: { Manual: 1 }, f: { Video: 3 } },
          { default: { constants: { DownloadType: { Video: 'Video' }, srcAction: { Dev: 'Dev' } } } },
          { default: { getConversationType: function (id) { return String(id).startsWith('g') ? 2 : 3; }, isValidMsgId: function () { return true; } } },
          { a: function r(e) { /* isFreev2 sendToMeId */ return pcloudNoKey; }, b: async function l(e) { return pcloudNoKey; } }
        ];
        window.__zaloRequire = { c: { dEOl: { exports: { MEDIA_TYPE: {}, default: svc } } } };
        window.__zaloDI = function () { return [{ instance: hub }]; };
        window.__zaloEachExport = function (req, fn) { exportsList.forEach(fn); };
        """)
        h.load("chat-adapter.js")
        h.load("media-store-adapter.js")
        return h
    }

    @Test func aListedPhotoOutsideTheLoadedMessagesIsHandedToTheOfficialDownloader() throws {
        let h = harness()
        #expect(h.failures.isEmpty, "both scripts load: \(h.failures)")
        _ = h.run("list", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        // Mine (listed by its client id) …
        let photo = h.run("p", "window.__zaloChat.resolveMedia('u2', '5001', 'photo')")
        #expect(photo?["state"] as? String == "ok" && photo?["path"] as? String == "/tmp/olaz-test/h1.jpg", "\(String(describing: photo))")
        let sent = h.text("JSON.stringify(seen[0])")
        let seenMessage = try #require(try JSONSerialization.jsonObject(with: Data(sent.utf8)) as? [String: Any])
        #expect(seenMessage["msgType"] as? Int == 2 && seenMessage["toUid"] as? String == "u2" && seenMessage["fromUid"] as? String == "0")
        #expect(seenMessage["msgId"] as? String == "9990" && seenMessage["cliMsgId"] as? String == "5001")
        #expect((seenMessage["message"] as? [String: Any])?["hdUrl"] as? String == "https://cdn.example.test/p/h1.jpg")
        // … and a video thumbnail of someone else's message.
        let video = h.run("v", "window.__zaloChat.resolveMedia('u2', '10020', 'videoThumb')")
        #expect(video?["state"] as? String == "ok")
        let videoMessage = try #require(try JSONSerialization.jsonObject(with: Data(h.text("JSON.stringify(seen[1])").utf8)) as? [String: Any])
        #expect(videoMessage["msgType"] as? Int == 18 && videoMessage["fromUid"] as? String == "u2")
    }

    @Test func aVideoIsTheCopyTheOfficialInlinePlayerDownloads() throws {
        let h = harness()
        _ = h.run("list", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        let video = h.run("v", "window.__zaloChat.resolveMedia('u2', '10020', 'video')")
        #expect(video?["state"] as? String == "ok" && video?["path"] as? String == "/tmp/olaz-test/ZaloDownloads/video/7002_u2_u2", "\(String(describing: video))")
        // Exactly the call `openFile` makes: by message, the CBVVideoInline entry, a cached Dev download without progress UI.
        let call = try #require(try JSONSerialization.jsonObject(with: Data(h.text("JSON.stringify(videoCalls[0])").utf8)) as? [String: Any])
        #expect(call["how"] as? String == "runByMsg")
        let msg = try #require(call["msg"] as? [String: Any])
        #expect(msg["msgType"] as? Int == 18 && msg["msgId"] as? String == "10020" && msg["toUid"] as? String == "u2")
        let opts = try #require(call["opts"] as? [String: Any])
        let entry = try #require(opts["entry"] as? [String: Any])
        #expect(entry["view"] as? String == "ChatBoxView" && entry["type"] as? Int == 3 && entry["mode"] as? Int == 1 && entry["code"] as? Int == 8 && entry["convType"] as? Int == 1)
        #expect(opts["type"] as? String == "Video")
        let options = try #require(opts["options"] as? [String: Any])
        #expect(options["caching"] as? Bool == true && options["duplicate"] as? Bool == false && options["srcAction"] as? String == "Dev")
        #expect((options["showProgress"] as? [String: Any])?["progressBar"] as? Bool == false)
        // A photo is not a video.
        let photo = h.run("p", "window.__zaloChat.resolveMedia('u2', '5001', 'video')")
        #expect(photo?["state"] as? String == "unavailable" && photo?["reason"] as? String == "message" && h.number("videoCalls.length") == 1)
    }

    @Test func aPersonalCloudVideoWithoutItsKeyIsNotDownloaded() {
        let h = harness()
        _ = h.run("list", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        _ = h.text("pcloudNoKey = true")
        let video = h.run("v", "window.__zaloChat.resolveMedia('u2', '10020', 'video')")
        #expect(video?["state"] as? String == "unavailable" && video?["reason"] as? String == "pcloud")
        #expect(h.number("videoCalls.length") == 0)
    }

    @Test func anUnlistedMessageIsStillUnavailable() {
        let h = harness()
        _ = h.run("list", "window.__zaloMediaStore.page('u2', 'images', null, 30)")
        let result = h.run("x", "window.__zaloChat.resolveMedia('u2', 'never-listed', 'photo')")
        #expect(result?["state"] as? String == "unavailable" && result?["reason"] as? String == "message")
        #expect(h.number("seen.length") == 0)
    }
}

// MARK: - Recalled / deleted media, the loaded window, the flat rows, the caches

@Suite struct SharedMediaDeletionTests {
    private func recalled(_ id: String, at seconds: Double) -> ChatMessage { message(id, .recalled, at: seconds) }

    @Test func aLoadedMessageThatNoLongerQualifiesDropsTheStoredItem() {
        let stored = [photo("1", at: 100), photo("2", at: 200), file("f", at: 150)]
        // "2" was recalled (the loaded copy is now a recalled notice), "1" is an ordinary text now, "f" is unrelated to photos.
        let loaded = [recalled("2", at: 200), message("1", .text, at: 100), photo("3", at: 300)]
        #expect(SharedMediaList.merged(stored: stored, loaded: loaded, kind: .images).map(\.id) == ["3"])
        #expect(SharedMediaList.merged(stored: stored, loaded: [recalled("f", at: 150)], kind: .files).isEmpty)
        // A copy that failed to send drops it as well; an unrelated loaded text does nothing.
        #expect(SharedMediaList.merged(stored: stored, loaded: [photo("1", at: 100, status: .failed), message("x", .text, at: 1)], kind: .images).map(\.id) == ["2"])
        // The overview's top few follow the same rule.
        #expect(SharedMediaList.preview(.images, stored: stored, loaded: loaded).map(\.id) == ["3"])
    }

    @Test func loadedMessagesOlderThanTheStoreHasReadAreLeftOut() {
        let stored = [photo("5", at: 500), photo("4", at: 400)]
        let loaded = [photo("6", at: 600), photo("4", at: 400), photo("3", at: 300), photo("1", at: 100)]
        let merged = SharedMediaList.merged(stored: stored, loaded: loaded, kind: .images, loadedSince: Date(timeIntervalSince1970: 400))
        #expect(merged.map(\.id) == ["6", "5", "4"])
        // No limit: all of them.
        #expect(SharedMediaList.merged(stored: stored, loaded: loaded, kind: .images).map(\.id) == ["6", "5", "4", "3", "1"])
        // … but an out-of-window loaded copy still drops a stored item with its id.
        #expect(SharedMediaList.merged(stored: stored, loaded: [recalled("4", at: 400)], kind: .images, loadedSince: Date(timeIntervalSince1970: 450)).map(\.id) == ["5"])
    }

    @Test func theFeedKnowsWhichLoadedMessagesCount() {
        var feed = SharedMediaFeed()
        #expect(feed.loadedSince == nil, "nothing read yet: the loaded messages are all there is")
        feed.append(SharedMediaPage(items: [photo("3", at: 300), photo("2", at: 200)], cursor: "2", more: true))
        #expect(feed.loadedSince == Date(timeIntervalSince1970: 200))
        feed.append(SharedMediaPage(items: [photo("1", at: 100)], cursor: "1", more: false))
        #expect(feed.loadedSince == nil, "the end of the store: every loaded message counts")
        var failed = SharedMediaFeed()
        failed.phase = .failed
        #expect(failed.loadedSince == nil && failed.didFail && !failed.isPending)
    }

    @Test func reloadingRemovesWhatTheNewestWindowNoLongerHas() {
        var feed = SharedMediaFeed()
        feed.append(SharedMediaPage(items: (6...9).map { photo("\($0)", at: Double($0)) }.reversed(), cursor: "6", more: true))
        feed.append(SharedMediaPage(items: (1...5).map { photo("\($0)", at: Double($0)) }.reversed(), cursor: "1", more: true))
        // 8 was deleted; 5 and 6 were cut off by the page (older than its oldest item 7) and stay.
        let before = feed.revision
        feed.merge(firstPage: SharedMediaPage(items: [photo("9", at: 9), photo("7", at: 7)], cursor: "7", more: true))
        #expect(feed.items.map(\.id) == ["9", "7", "6", "5", "4", "3", "2", "1"])
        #expect(feed.cursor == "1" && feed.more, "how deep the list was read is kept")
        #expect(feed.revision > before)
        // Nothing changed: the revision stays (derived lists are not rebuilt).
        let same = feed.revision
        feed.merge(firstPage: SharedMediaPage(items: [photo("9", at: 9), photo("7", at: 7)], cursor: "7", more: true))
        #expect(feed.revision == same)
    }

    @Test func aBurstSharingTheBoundaryTimeIsNotLostToThePageLimit() {
        // Ten photos of one send share a time; the page limit cut the burst: ids 10…6 are in the page, 5…1 are older ids of the same burst.
        func burst(_ ids: [Int]) -> [ChatMessage] { ids.map { photo("\(1000 + $0)", at: 50) } }
        var feed = SharedMediaFeed()
        feed.append(SharedMediaPage(items: burst(Array(1...10)) + [photo("500", at: 10)], cursor: "500", more: true))
        feed.merge(firstPage: SharedMediaPage(items: [photo("2000", at: 60)] + burst(Array(6...10)), cursor: "1006", more: true))
        #expect(Set(feed.items.map(\.id)) == Set(["2000", "1006", "1007", "1008", "1009", "1010", "1001", "1002", "1003", "1004", "1005", "500"]))
        // A member of the burst that sorts above the page's oldest item and is missing from the page was deleted.
        feed.merge(firstPage: SharedMediaPage(items: [photo("2000", at: 60), photo("1010", at: 50), photo("1008", at: 50), photo("1006", at: 50)], cursor: "1006", more: true))
        #expect(!feed.items.map(\.id).contains("1009") && !feed.items.map(\.id).contains("1007"))
        #expect(feed.items.map(\.id).contains("1005"), "older ids of the burst stay")
    }

    @Test func aPageWithNothingLeftEmptiesTheListAndAnEmptyListTakesThePagesCursor() {
        var gone = SharedMediaFeed()
        gone.append(SharedMediaPage(items: [photo("2", at: 2), photo("1", at: 1)], cursor: "1", more: true))
        gone.merge(firstPage: SharedMediaPage(items: [], cursor: nil, more: false))
        #expect(gone.items.isEmpty && !gone.more)

        // The inspector opened before the sync filled the database: the first page was empty, a later one is not.
        var late = SharedMediaFeed()
        late.append(SharedMediaPage(items: [], cursor: nil, more: false))
        #expect(!late.more)
        late.merge(firstPage: SharedMediaPage(items: [photo("3", at: 3), photo("2", at: 2)], cursor: "2", more: true))
        #expect(late.items.map(\.id) == ["3", "2"] && late.cursor == "2" && late.more)

        // A page that shares nothing with what is held (everything held is older): its cursor takes over, held older items stay.
        var apart = SharedMediaFeed()
        apart.append(SharedMediaPage(items: [photo("2", at: 20), photo("1", at: 10)], cursor: "1", more: false))
        apart.merge(firstPage: SharedMediaPage(items: [photo("9", at: 90), photo("8", at: 80)], cursor: "8", more: true))
        #expect(apart.items.map(\.id) == ["9", "8", "2", "1"] && apart.cursor == "8" && apart.more)
    }
}

@Suite struct SharedMediaDerivedTests {
    @Test func previewEqualsTheFirstOfTheFullMerge() {
        // Deterministic pseudo-random lists with overlaps, ties and non-qualifying loaded copies.
        var seed: UInt64 = 12345
        func next(_ bound: Int) -> Int {
            seed = seed &* 6364136223846793005 &+ 1442695040888963407
            return Int((seed >> 33) % UInt64(bound))
        }
        for _ in 0..<40 {
            let stored = (0..<60).map { _ in photo("s\(next(80))", at: Double(next(30))) }
            // Loaded messages have unique ids (the chat list merges by id); some overlap the stored ones.
            var ids = Array(0..<80)
            for i in ids.indices.reversed() { ids.swapAt(i, next(i + 1)) }
            var loaded: [ChatMessage] = ids[0..<30].map { photo("s\($0)", at: Double(next(30))) }
            loaded += ids[30..<40].map { message("s\($0)", next(2) == 0 ? .text : .recalled, at: Double(next(30))) }
            let unique = Dictionary(stored.map { ($0.id, $0) }, uniquingKeysWith: { $1 }).values.map { $0 }
            let full = SharedMediaList.merged(stored: unique, loaded: loaded, kind: .images)
            #expect(SharedMediaList.preview(.images, stored: unique, loaded: loaded).map(\.id) == Array(full.prefix(9)).map(\.id))
        }
    }

    @Test func rowsFlattenMonthsIntoHeadersAndItems() {
        let items = [photo("a", at: 1_793_059_200), photo("b", at: 1_790_812_800), photo("c", at: 1_790_726_400)]
        let rows = SharedMediaList.rows(SharedMediaList.months(items, calendar: utc))
        #expect(rows.map(\.id) == ["month-2026-10", "a", "b", "month-2026-9", "c"])
        #expect(rows[0].content == .header("Tháng 10, 2026") && rows[0].firstMonth && !rows[3].firstMonth)
        #expect(rows[1].firstInMonth && !rows[1].lastInMonth && !rows[2].firstInMonth && rows[2].lastInMonth)
        #expect(rows[4].firstInMonth && rows[4].lastInMonth, "a month of one row is both ends")
        #expect(Set(rows.map(\.id)).count == rows.count)
    }

    @MainActor @Test func derivedListsAreRebuiltOnlyWhenTheirInputsChange() async {
        let source = FakeSource()
        source.pages[.images] = [SharedMediaPage(items: [photo("2", at: 2), photo("1", at: 1)], cursor: "1", more: true)]
        let model = SharedMediaModel(conversationId: "c1")
        await model.refresh(using: source)
        let loaded = [photo("3", at: 3), message("t", .text, at: 4)]
        let first = model.preview(.images, loaded: loaded)
        #expect(first.map(\.id) == ["3", "2", "1"])
        let listing = model.listing(.images, loaded: loaded)
        #expect(listing.items.map(\.id) == ["3", "2", "1"] && listing.rows.count == listing.months.count + 3)
        let builds = model.derivedBuilds
        // Same inputs, even with the status (not the list) changing: served from the cache.
        for _ in 0..<5 { _ = model.preview(.images, loaded: loaded); _ = model.listing(.images, loaded: loaded) }
        #expect(model.derivedBuilds == builds)
        // New loaded messages rebuild; so does a feed change; an unchanged reload does not.
        #expect(model.preview(.images, loaded: loaded + [photo("5", at: 5)]).map(\.id) == ["5", "3", "2", "1"])
        let afterLoaded = model.derivedBuilds
        #expect(afterLoaded == builds + 1)
        source.pages[.images] = [SharedMediaPage(items: [photo("2", at: 2), photo("1", at: 1)], cursor: "1", more: true)]
        await model.refresh(using: source)
        _ = model.preview(.images, loaded: loaded + [photo("5", at: 5)])
        #expect(model.derivedBuilds == afterLoaded, "a reload that changed nothing keeps the caches")
        source.pages[.images] = [SharedMediaPage(items: [photo("9", at: 9), photo("2", at: 2)], cursor: "2", more: true)]
        await model.refresh(using: source)
        #expect(model.preview(.images, loaded: loaded + [photo("5", at: 5)]).map(\.id).first == "9")
        #expect(model.derivedBuilds == afterLoaded + 1)
    }

    @MainActor @Test func aFirstPageThatWasEmptyDoesNotStopTheListForGood() async {
        let source = FakeSource()
        source.pages[.images] = [SharedMediaPage(items: [], cursor: nil, more: false),
                                 SharedMediaPage(items: [photo("3", at: 3), photo("2", at: 2)], cursor: "2", more: true),
                                 SharedMediaPage(items: [photo("1", at: 1)], cursor: "1", more: true)]
        let model = SharedMediaModel(conversationId: "c1")
        await model.refresh(using: source)
        #expect(model.feed(.images).loaded && model.feed(.images).items.isEmpty && !model.feed(.images).more)
        await model.refresh(using: source)
        #expect(model.feed(.images).more && model.feed(.images).cursor == "2")
        await model.loadMore(.images, using: source)
        #expect(model.feed(.images).items.map(\.id) == ["3", "2", "1"])
    }
}
