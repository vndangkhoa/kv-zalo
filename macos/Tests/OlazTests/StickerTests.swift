import Testing
import Foundation
import JavaScriptCore
@testable import Olaz

/// Runs the real `card-actions-adapter.js` + `sticker-adapter.js` in JavaScriptCore against fake official sticker modules, with a fake
/// clock so the waits run instantly. Nothing here touches a network or the app.
private final class StickerHarness {
    let ctx: JSContext
    var failures: [String] = []

    init(setting: String = "1", e2ee: String? = "false", recents: String = "[]") {
        ctx = JSContext()!
        ctx.exceptionHandler = { [unowned self] _, e in failures.append(String(describing: e)) }
        ctx.evaluateScript("""
        var window = this; var location = { pathname: '/index.html' };
        var __now = 1.7e12, __timers = [];
        Date.now = function () { return __now; };
        var setTimeout = function (f, ms) { __timers.push({ f: f, at: __now + (ms || 0) }); return __timers.length; };
        var clearTimeout = function () {}; var setInterval = function () { return 0; }; var clearInterval = function () {};
        function __pump(step) {
          __now += step;
          var due = __timers.filter(function (t) { return t.at <= __now; });
          __timers = __timers.filter(function (t) { return t.at > __now; });
          due.sort(function (a, b) { return a.at - b.at; }).forEach(function (t) { t.f(); });
        }
        var __results = {};
        """)
        ctx.evaluateScript("""
        var calls = [], sent = [], flag = \(setting), downloaded = [10, 20], hidden = {}, gate = [];
        var recents = \(recents);
        var suggestKeywords = { 'chào': 'k1', 'chào bạn': 'k2' };
        var suggestFetches = 0, suggestReply = [{ cate_id: 10, sticker_id: 1 }, { cate_id: 10, sticker_id: 2 }, { cate_id: 20, sticker_id: 3 }];
        var extra = {};
        function st(c, id) { return Object.assign({ id: id, cateId: c, type: 7, stickerUrl: 'https://cdn.example/s?eid=' + id }, extra[c + '_' + id] || {}); }
        var mgr = {
          getDownloadedStickerCategoriesV2: async function () {
            return [{ id: 10, name: 'Mèo', iconUrl: 'https://cdn.example/i10', totalImage: 3 }, { id: 20, name: 'Chó', iconUrl: '//cdn.example/i20', totalImage: 1 }, { id: 0, name: 'Emoji', iconUrl: 'https://cdn.example/i0', totalImage: 2 }];
          },
          getDownloadedCateIds: async function () { calls.push('downloaded?'); return downloaded; },
          getRecentMediaItems: function () { return recents; },
          fetchCateIfNotEnoughSticker: async function (id) { return id === 10 ? [st(10, 1), st(10, 2), st(10, 3)] : id === 20 ? [st(20, 3)] : []; },
          getSingleCategory: async function () { return null; },
          getStickerIfNotExist: function (c, id) {
            calls.push('resolve');
            if (window.__gate) return window.__gate.then(function () { return st(c, id); });
            return Promise.resolve(c === 99 ? null : st(c, id));
          },
          isHideSticker: function (s) { return !!hidden[s.id]; },
          getSuggestionFromSentencesV2: async function (t) { var k = suggestKeywords[t.trim().toLowerCase()]; return { keyword: k || '', keywordType: 1 }; },
          getCacheStickerSearch: function () { return null; }, cacheStickerSearch: function () {}
        };
        var svc = {
          fetchStickersFromList: async function (list) { return list.map(function (x) { return st(x.cate_id, x.sticker_id); }); },
          fetchStickerCats: async function () { return []; },
          fetchStickersSuggestion: async function (kw) { suggestFetches++; return { sugg_sticker: suggestReply }; },
          searchSticker: async function (q) { return { searchData: [{ cate_id: 10, sticker_id: 2 }, { cate_id: 99, sticker_id: 9 }] }; }
        };
        var srcMod = { setStickerSrc: function (v) { calls.push('sgg:' + v); }, clearStickerSrc: function () { calls.push('sgg:clear'); } };
        var consts = { FRIEND_REQUEST_SRC_ECARD: 1, MAX_CHAR_SAY_HI: 150, SGG_SRC_CHATBAR: 1, SGG_SRC_SUGGESTION: 2, SGG_SRC_SEARCH_CHATBAR: 5, DEFAULT_SUGGEST_SSG_LEN: 9 };
        var flags = { e: 'auto_sticker', g: { getFlagForCurrentUser: function () { return flag; }, setFlagForCurrentUser: function (u, k, v) { calls.push('flag:' + k + ':' + v); flag = v; } } };
        function E2() {}
        E2._instance = new E2();
        E2.instance = function () { calls.push('instance()'); return E2._instance; };
        E2.prototype.isOnE2ee = function (id) { return \(e2ee ?? "undefined"); };
        var enumMod = { b: { Sticker: { SUGGESTION_DEFAULT: 101, RECENT_STICKER_SMALL_PANEL: 103, STICKER_SMALL_PANEL: 106, SEARCH_STICKER_SMALL_PANEL: 109, STICKER_BIG_PANEL: 107 } } };
        var stats = { hitHint: function () { calls.push('hit'); }, goodSuggest: function (t, s) { calls.push('good:' + t + ':' + s.id); } };
        var logger = { actionList: [], logAction: function (id) { calls.push('log:' + id); } };
        var bus = { send: function (a, v) { calls.push('bus:' + a + ':' + v); }, emit: function () {}, on: function () {}, once: function () {}, subscribe: function () {}, omitAll: function () {} };
        var lookalike = { send: function () { calls.push('wrongbus'); }, emit: function () {}, on: function () {}, once: function () {} };
        var throwOnSend = false;
        var controller = { convId: '1', _getConversationInfo: function () { return {}; },
          _sendSticker: function (s, src, n, sug) { if (throwOnSend) throw new Error('boom'); sent.push({ cate: s.cateId, id: s.id, src: src, sug: sug === true }); } };
        var core = {
          S: { cfg: { shortcut_suggest_config: { sticker_display: 2 }, sticker_post_typing: { debounce: 250 }, stickerUrl: 'https://cdn.example/bubble?eid=' } },
          safe: function (f, d) { try { var v = f(); return v === undefined ? d : v; } catch (e) { return d; } },
          str: function (v) { return typeof v === 'string' ? v : ''; }, own: function (o, k) { return Object.prototype.hasOwnProperty.call(o, k); },
          parseJSON: function (v) { if (typeof v !== 'string') return v; try { return JSON.parse(v); } catch (e) { return null; } },
          diag: function () {}, scrubErr: function () { return ''; }, isGroupId: function (s) { return /^g/.test(s); },
          isMeUid: function () { return false; }, meNow: function () { return '7'; }, nameOf: function () { return ''; }, avatarOf: function () { return ''; },
          MSG: { STICKER: 4 }, findMessage: function () { return null; }, ready: async function () { return true; },
          absUrl: function (v) { v = typeof v === 'string' ? v : ''; if (/^https?:\\/\\//.test(v)) return v; if (v.indexOf('//') === 0) return 'https:' + v; return ''; },
          sleep: function (ms) { return new Promise(function (r) { setTimeout(r, ms); }); },
          controller: function () { return controller; }, repairController: function () {}
        };
        window.__zaloChat = { __core: core };
        window.__zaloRequire = { c: {} };
        window.__zaloDI = function () { return []; };
        window.__zaloEachExport = function (req, visit) {
          visit(mgr); visit(svc); visit(srcMod); visit(consts); visit(flags); visit(enumMod); visit({ STICKER_EXT: { PNG: 0, JPG: 1, WEBP: 2 } });
          visit({ a: stats }); visit({ default: lookalike }); visit({ default: logger }); visit({ default: bus }); visit({ GeneralActions: { SETTING_CHANGED: 471 } });
          \(e2ee == nil ? "" : "visit({ a: E2 });")
        };
        """)
    }

    func load(_ name: String, file: StaticString = #filePath) {
        let url = URL(fileURLWithPath: "\(file)").deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/Olaz/HostScripts/\(name)")
        guard let source = try? String(contentsOf: url, encoding: .utf8) else { failures.append("missing \(name)"); return }
        ctx.evaluateScript(source)
    }

    static func make(setting: String = "1", e2ee: String? = "false", recents: String = "[]") -> StickerHarness {
        let h = StickerHarness(setting: setting, e2ee: e2ee, recents: recents)
        h.load("card-actions-adapter.js")
        h.load("sticker-adapter.js")
        return h
    }

    /// Runs `expression` (a promise) on the fake clock and returns its JSON.
    func run(_ expression: String, steps: Int = 60, step: Int = 500) -> Any? {
        ctx.evaluateScript("__results.r = undefined; Promise.resolve(\(expression)).then(function (r) { __results.r = JSON.stringify(r === undefined ? null : r); }, function (e) { __results.r = 'REJECTED ' + e; });")
        pump(steps: steps, step: step)
        guard let text = ctx.evaluateScript("__results.r")?.toString(), text != "undefined" else { return "UNSETTLED" }
        return try? JSONSerialization.jsonObject(with: Data(text.utf8), options: [.fragmentsAllowed])
    }

    func start(_ slot: String, _ expression: String) {
        ctx.evaluateScript("__results['\(slot)'] = undefined; Promise.resolve(\(expression)).then(function (r) { __results['\(slot)'] = JSON.stringify(r === undefined ? null : r); }, function (e) { __results['\(slot)'] = 'REJECTED ' + e; });")
    }

    func result(_ slot: String) -> [String: Any]? {
        guard let text = ctx.evaluateScript("__results['\(slot)']")?.toString(), text != "undefined" else { return nil }
        return (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any]
    }

    func pump(steps: Int = 60, step: Int = 500) {
        for _ in 0..<steps { ctx.evaluateScript("__pump(\(step))") }
    }

    func text(_ expression: String) -> String { ctx.evaluateScript(expression)?.toString() ?? "" }
}

@Suite struct StickerScriptTests {
    private let api = "window.__zaloSticker"

    @Test func installsOnlyWithTheCardActionInternals() {
        let h = StickerHarness.make()
        #expect(h.failures.isEmpty, "\(h.failures)")
        #expect(h.text("typeof \(api).send") == "function")
        let bare = StickerHarness(); bare.load("sticker-adapter.js")
        #expect(bare.text("typeof window.__zaloSticker") == "undefined", "without card-actions-adapter there is nothing to send with")
    }

    @Test func tabsKeepTheUsersOrderAndPutRecentStickersFirst() {
        let recents = #"[{"id":5,"cateId":10,"stickerUrl":"https://cdn.example/r5","recentItemType":"_r_s"},{"id":6,"cateId":10,"recentItemType":"_r_gf"},{"id":7,"catId":20}]"#
        let h = StickerHarness.make(recents: recents)
        let tabs = h.run("\(api).categories()") as? [[String: Any]]
        #expect(tabs?.map { $0["id"] as? Int } == [-2, 10, 20, 0])
        #expect(tabs?.first?["count"] as? Int == 2, "the GIF is not a sticker")
        #expect(tabs?[2]["icon"] as? String == "https://cdn.example/i20", "scheme-less icon links become https")
        let none = StickerHarness.make().run("\(api).categories()") as? [[String: Any]]
        #expect(none?.map { $0["id"] as? Int } == [10, 20, 0], "no recent tab when nothing was sent yet")
    }

    @Test func recentItemWithoutAUrlUsesTheBubbleUrlLogic() {
        let h = StickerHarness.make(recents: #"[{"id":8,"cateId":10}]"#)
        let cat = h.run("\(api).category(-2)") as? [String: Any]
        let list = cat?["stickers"] as? [[String: Any]]
        #expect(list?.first?["url"] as? String == "https://cdn.example/bubble?eid=8")
    }

    @Test func hiddenRecentStickersAreLeftOut() {
        let h = StickerHarness.make(recents: #"[{"id":5,"cateId":10,"stickerUrl":"https://cdn.example/r5"},{"id":6,"cateId":10,"stickerUrl":"https://cdn.example/r6"}]"#)
        h.ctx.evaluateScript("hidden[5] = true;")
        let cat = h.run("\(api).category(-2)") as? [String: Any]
        #expect((cat?["stickers"] as? [[String: Any]])?.compactMap { $0["id"] as? Int } == [6])
        let tabs = h.run("\(api).categories()") as? [[String: Any]]
        #expect(tabs?.first?["count"] as? Int == 1)
    }

    @Test func thumbnailsFollowTheOfficialFieldsAndAnimateOnlyWhereThereIsASource() {
        let h = StickerHarness.make()
        h.ctx.evaluateScript("""
        extra['10_1'] = { stickerWebpUrl: 'https://cdn.example/webp1', totalFrames: 16, stickerSpriteUrl: 'https://cdn.example/sprite1', duration: 60 };
        extra['10_2'] = { stickerGifUrl: 'https://cdn.example/g2.gif' };
        extra['10_3'] = { totalFrames: 1, stickerSpriteUrl: 'https://cdn.example/sprite3' };
        """)
        let list = (h.run("\(api).category(10)") as? [String: Any])?["stickers"] as? [[String: Any]]
        #expect(list?[0]["url"] as? String == "https://cdn.example/webp1", "stickerWebpUrl ?? stickerUrl")
        let sprite = list?[0]["anim"] as? [String: Any]
        #expect(sprite?["kind"] as? String == "sprite" && sprite?["frames"] as? Int == 16 && sprite?["ms"] as? Int == 60)
        #expect((list?[1]["anim"] as? [String: Any])?["kind"] as? String == "gif")
        #expect(list?[2]["anim"] == nil, "one frame: nothing to animate")
        #expect(list?[1]["url"] as? String == "https://cdn.example/s?eid=2")
    }

    @Test func webpStickersAnimateFromAnimatedWebpAndFramesDefaultTo250Ms() {
        let h = StickerHarness.make()
        h.ctx.evaluateScript("""
        extra['20_3'] = { ext: 2, stickerSpriteUrl: 'https://cdn.example/anim3.webp', totalFrames: 0 };
        extra['10_1'] = { totalFrames: 8, stickerSpriteUrl: 'https://cdn.example/sprite1' };
        """)
        let webp = (h.run("\(api).category(20)") as? [String: Any])?["stickers"] as? [[String: Any]]
        let anim = webp?.first?["anim"] as? [String: Any]
        #expect(anim?["kind"] as? String == "webp" && anim?["url"] as? String == "https://cdn.example/anim3.webp", "\(String(describing: anim))")
        let sheet = ((h.run("\(api).category(10)") as? [String: Any])?["stickers"] as? [[String: Any]])?.first?["anim"] as? [String: Any]
        #expect(sheet?["kind"] as? String == "sprite" && sheet?["ms"] as? Int == 250, "no duration: the official default of 250 ms")
    }

    @Test func categoryListsItsStickersAndRejectsAnUnknownId() {
        let h = StickerHarness.make()
        let cat = h.run("\(api).category(10)") as? [String: Any]
        #expect((cat?["stickers"] as? [[String: Any]])?.count == 3)
        #expect(h.run("\(api).category('abc')") == nil || h.run("\(api).category('abc')") is NSNull)
        #expect(h.run("\(api).category(-7)") is NSNull)
    }

    @Test func searchResolvesHitsAndDropsTheOnesThatAreGone() {
        let h = StickerHarness.make()
        let list = h.run("\(api).search('mèo')") as? [[String: Any]]
        #expect(list?.count == 1 && list?.first?["id"] as? Int == 2, "the hit of an unknown sticker is dropped")
        #expect((h.run("\(api).search('   ')") as? [Any])?.isEmpty == true)
    }

    // MARK: suggestions

    @Test func suggestionsUseTheOfficialKeywordMatchAndTheConfiguredCount() {
        let h = StickerHarness.make()
        let list = h.run("\(api).suggest('1', 'Chào')") as? [[String: Any]]
        #expect(list?.count == 2, "sticker_display = 2 in the fake config")
        #expect(h.text("String(suggestFetches)") == "1")
        // The same keyword again reuses the last result (the handler's lastProcessKeyword).
        _ = h.run("\(api).suggest('1', 'chào')")
        #expect(h.text("String(suggestFetches)") == "1")
        // Another keyword asks again; no keyword, several lines, "#…" and empty text never reach the server.
        _ = h.run("\(api).suggest('1', 'chào bạn')")
        #expect(h.text("String(suggestFetches)") == "2")
        for text in ["xyz", "chào\\nbạn", "#chào", "   ", ""] {
            #expect((h.run("\(api).suggest('1', \"\(text)\")") as? [Any])?.isEmpty == true, "\(text)")
        }
        #expect(h.text("String(suggestFetches)") == "2")
    }

    @Test func suggestionsAreOffWhenTheSettingIsOffAndNothingIsAsked() {
        let h = StickerHarness.make(setting: "false")
        #expect((h.run("\(api).suggest('1', 'chào')") as? [Any])?.isEmpty == true)
        #expect(h.text("String(suggestFetches)") == "0")
        #expect((h.run("\(api).suggestSetting()") as? [String: Any])?["enabled"] as? Bool == false)
    }

    @Test func encryptedChatsAndUnknownEncryptionNeverSendTextToTheServer() {
        for e2ee in ["true", nil] as [String?] {
            let h = StickerHarness.make(e2ee: e2ee)
            #expect((h.run("\(api).suggest('1', 'chào')") as? [Any])?.isEmpty == true, "\(e2ee ?? "no manager")")
            #expect(h.text("String(suggestFetches)") == "0")
        }
    }

    @Test func theEncryptionManagerIsReadFromItsInstanceFieldNeverCreatedByTheAdapter() {
        let h = StickerHarness.make()
        _ = h.run("\(api).suggest('1', 'chào')")
        #expect(!h.text("calls.join(',')").contains("instance()"), "instance() logs an official error before E2EE is initialised")
        let cold = StickerHarness.make()
        cold.ctx.evaluateScript("E2._instance = null;")
        #expect((cold.run("\(api).suggest('1', 'chào')") as? [Any])?.isEmpty == true, "manager not initialised → unknown → no suggestion")
        #expect(cold.text("String(suggestFetches)") == "0")
    }

    @Test func settingIsReadAndWrittenThroughTheOfficialFlag() {
        let h = StickerHarness.make()
        let now = h.run("\(api).suggestSetting()") as? [String: Any]
        #expect(now?["enabled"] as? Bool == true && now?["debounce"] as? Int == 250)
        let off = h.run("\(api).setSuggestSetting(false)") as? [String: Any]
        #expect(off?["ok"] as? Bool == true && off?["enabled"] as? Bool == false)
        #expect(h.text("calls.join(',')").contains("flag:auto_sticker:false"))
        #expect(h.text("calls.join(',')").contains("bus:471:false"), "the official toggle also sends GeneralActions.SETTING_CHANGED")
        #expect(!h.text("calls.join(',')").contains("wrongbus"), "an emitter without subscribe / omitAll is not the official bus")
        #expect((h.run("\(api).suggest('1', 'chào')") as? [Any])?.isEmpty == true)
    }

    // MARK: sending

    private func sendOne(_ h: StickerHarness, _ cate: Int, _ id: Int, _ source: String, conv: String = "1") -> [String: Any]? {
        h.run("\(api).send('\(conv)', {cateId: \(cate), id: \(id)}, '\(source)')") as? [String: Any]
    }

    @Test func eachSourceSendsWithTheOfficialSourceCodes() {
        let h = StickerHarness.make()
        #expect(sendOne(h, 10, 1, "category")?["status"] as? String == "done")
        #expect(sendOne(h, 10, 2, "recent")?["status"] as? String == "done")
        #expect(sendOne(h, 10, 3, "search")?["status"] as? String == "done")
        #expect(sendOne(h, 20, 3, "suggestion")?["status"] as? String == "done")
        let sent = h.text("JSON.stringify(sent)")
        #expect(sent == #"[{"cate":10,"id":1,"src":106,"sug":false},{"cate":10,"id":2,"src":103,"sug":false},{"cate":10,"id":3,"src":109,"sug":false},{"cate":20,"id":3,"src":101,"sug":true}]"#, "\(sent)")
        // setStickerSrc before each send: chat bar (1) for the panel and recent, search (5), suggestion (2)
        let sgg = h.text("calls.filter(function (c) { return c.indexOf('sgg:') === 0; }).join(',')")
        #expect(sgg == "sgg:1,sgg:1,sgg:5,sgg:2", "\(sgg)")
    }

    @Test func pickerTabsAlwaysUseThePanelSourceWithoutAskingForDownloads() {
        let h = StickerHarness.make()
        h.ctx.evaluateScript("downloaded = [];")   // even a set the manager does not list as downloaded: it came from a tab
        #expect(sendOne(h, 10, 1, "category")?["status"] as? String == "done")
        #expect(h.text("String(sent[0].src)") == "106")
        #expect(!h.text("calls.join(',')").contains("downloaded?"), "no download lookup (it could wait 5 s or fall back to the store source)")
    }

    @Test func theCategorySheetStillTellsStoreSetsFromDownloadedOnes() {
        let h = StickerHarness.make()
        h.ctx.evaluateScript("downloaded = [20];")
        let store = h.run("window.__zaloChat.stickerCategorySend('1', 10, 1)") as? [String: Any]
        #expect(store?["status"] as? String == "done")
        #expect(h.text("String(sent[0].src)") == "122")
        let own = h.run("window.__zaloChat.stickerCategorySend('1', 20, 3)") as? [String: Any]
        #expect(own?["status"] as? String == "done")
        #expect(h.text("String(sent[1].src)") == "106")
    }

    @Test func aSendThatThrowsAfterHandingOverIsPendingNotAnError() {
        let h = StickerHarness.make()
        h.ctx.evaluateScript("throwOnSend = true;")
        let r = sendOne(h, 10, 1, "category")
        #expect(r?["status"] as? String == "pending", "the sticker may be on its way: nobody is invited to send it again")
        let reason = r?["text"] as? String ?? ""
        #expect(!reason.isEmpty)
        #expect(h.text("calls.join(',')").contains("sgg:clear"), "the suggestion source set for the send is cleared again")
        let sheet = h.run("window.__zaloChat.stickerCategorySend('1', 10, 2)") as? [String: Any]
        #expect(sheet?["status"] as? String == "pending")
        guard case .pending = CardActionOutcome(json: r) else { Issue.record("a throw after the hand-over is pending, never a failure that invites a retry"); return }
    }

    @Test func suggestionSendsRecordTheOfficialStatsAndLogs() {
        let h = StickerHarness.make()
        _ = h.run("\(api).send('1', {cateId: 10, id: 2}, 'suggestion', {text: 'chào', index: 1})")
        let calls = h.text("calls.join(',')")
        #expect(calls.contains("log:107041202") && calls.contains("log:10704118") && calls.contains("log:10704117"))
        #expect(calls.contains("hit") && calls.contains("good:chào:2"))
        #expect(!calls.contains("log:10704119"), "there was typed text")
        // other sources do not touch the suggestion stats
        let h2 = StickerHarness.make()
        _ = sendOne(h2, 10, 1, "category")
        #expect(!h2.text("calls.join(',')").contains("hit"))
    }

    @Test func badInputsAreRefusedBeforeAnythingIsResolved() {
        let h = StickerHarness.make()
        #expect(sendOne(h, 10, 1, "forward")?["status"] as? String == "error")
        #expect(sendOne(h, 10, 0, "category")?["status"] as? String == "error")
        #expect(sendOne(h, 10, 1, "category", conv: "1; alert(1)")?["status"] as? String == "error")
        #expect(h.text("String(sent.length)") == "0" && h.text("String(calls.filter(function (c) { return c === 'resolve'; }).length)") == "0")
    }

    @Test func unknownAndHiddenStickersAreNotSent() {
        let h = StickerHarness.make()
        #expect(sendOne(h, 99, 5, "search")?["status"] as? String == "not-found")
        h.ctx.evaluateScript("hidden[1] = true;")
        #expect(sendOne(h, 10, 1, "category")?["status"] as? String == "not-found")
        #expect(h.text("String(sent.length)") == "0")
    }

    @Test func groupsWhoseLockIsUnknownAreNotSentTo() {
        let h = StickerHarness.make()
        let r = sendOne(h, 10, 1, "category", conv: "g5")
        #expect(r?["status"] as? String == "error", "no verify service → the lock cannot be checked, so nothing is sent")
        #expect(h.text("String(sent.length)") == "0")
    }

    @Test func oneSendAtATimePerConversation() {
        let h = StickerHarness.make()
        h.ctx.evaluateScript("var release; window.__gate = new Promise(function (r) { release = r; });")
        h.start("a", "\(api).send('1', {cateId: 10, id: 1}, 'category')")
        h.pump(steps: 2)
        h.start("b", "\(api).send('1', {cateId: 10, id: 2}, 'suggestion')")
        h.pump(steps: 2)
        #expect(h.result("b")?["status"] as? String == "busy", "the second click is refused while the first has not settled")
        h.ctx.evaluateScript("release();")
        h.pump(steps: 4)
        #expect(h.result("a")?["status"] as? String == "done")
        #expect(h.text("String(sent.length)") == "1")
        // another conversation has its own slot
        h.ctx.evaluateScript("window.__gate = null; controller.convId = '2';")
        #expect(sendOne(h, 10, 2, "category", conv: "2")?["status"] as? String == "done")
    }
}

@Suite struct StickerModelTests {
    private func json(_ text: String) throws -> Any {
        try JSONSerialization.jsonObject(with: Data(text.utf8), options: [.fragmentsAllowed])
    }

    @Test func stickersNeedIdsAndAWebUrlAndAreUnique() throws {
        let list = StickerEntry.list(json: try json(#"""
        [{"cateId":10,"id":1,"url":"https://cdn.example/a"},{"cateId":10,"id":1,"url":"https://cdn.example/dup"},
         {"cateId":0,"id":2,"url":"http://cdn.example/b"},{"cateId":10,"id":0,"url":"https://cdn.example/c"},
         {"cateId":-1,"id":3,"url":"https://cdn.example/d"},{"cateId":10,"id":4,"url":"file:///etc/passwd"},
         {"cateId":10,"id":5},{"id":6,"url":"https://cdn.example/e"},"x",null]
        """#))
        #expect(list.map(\.id) == ["10_1", "0_2"])
        #expect(list.first?.url.absoluteString == "https://cdn.example/a")
        #expect(StickerEntry.list(json: nil).isEmpty && StickerEntry.list(json: ["not", "rows"]).isEmpty)
    }

    @Test func listsAreCapped() throws {
        let rows = (1...800).map { #"{"cateId":1,"id":\#($0),"url":"https://cdn.example/\#($0)"}"# }.joined(separator: ",")
        #expect(StickerEntry.list(json: try json("[\(rows)]")).count == 500)
        #expect(StickerEntry.list(json: try json("[\(rows)]"), limit: 40).count == 40)
    }

    @Test func tabsKeepOrderAndTellUnavailableFromEmpty() throws {
        let tabs = try #require(StickerTab.list(json: try json(#"""
        [{"id":-2,"name":"Gần đây","icon":"","count":3},{"id":12556,"name":"Mèo","icon":"https://cdn.example/i","count":24},
         {"id":12556,"name":"dup","icon":"","count":1},{"id":-9,"name":"bad"},{"id":0,"name":"Emoji","icon":"javascript:alert(1)","count":-4}]
        """#)))
        #expect(tabs.map(\.id) == [-2, 12556, 0])
        #expect(tabs[0].isRecent && !tabs[1].isRecent)
        #expect(tabs[1].icon?.absoluteString == "https://cdn.example/i")
        #expect(tabs[2].icon == nil && tabs[2].count == 0)
        #expect(StickerTab.list(json: try json("[]"))?.isEmpty == true, "no sets is an answer")
        #expect(StickerTab.list(json: NSNull()) == nil && StickerTab.list(json: nil) == nil, "unavailable is not an empty list")
    }

    @Test func suggestSettingParsing() throws {
        let on = try #require(StickerSuggestSetting(json: try json(#"{"ok":true,"enabled":true,"debounce":250}"#)))
        #expect(on.enabled && on.debounce == 250)
        #expect(StickerSuggestSetting(json: try json(#"{"ok":true,"enabled":false,"debounce":99999}"#))?.debounce == 2000)
        #expect(StickerSuggestSetting(json: try json(#"{"ok":false,"enabled":true}"#)) == nil, "a setting that could not be read is unknown, not on")
        #expect(StickerSuggestSetting(json: nil) == nil)
    }

    @MainActor @Test func typingDelayNeverGoesBelowTheCoalescingFloor() throws {
        let state = StickerSuggestState()
        #expect(state.delay == StickerSuggestState.minimumDelay, "setting not loaded yet")
        state.apply(StickerSuggestSetting(json: try json(#"{"ok":true,"enabled":true,"debounce":0}"#)))
        #expect(state.delay == StickerSuggestState.minimumDelay, "the server config says 0 today")
        state.apply(StickerSuggestSetting(json: try json(#"{"ok":true,"enabled":true,"debounce":400}"#)))
        #expect(state.delay == 400)
        state.reset()
        #expect(state.setting == nil)
    }

    @Test func sendOutcomesUseTheCardActionStatuses() throws {
        #expect(CardActionOutcome(json: try json(#"{"status":"done"}"#)) == .done)
        guard case .locked = CardActionOutcome(json: try json(#"{"status":"locked"}"#)) else { Issue.record("locked"); return }
        guard case .failed = CardActionOutcome(json: try json(#"{"status":"busy"}"#)) else { Issue.record("busy"); return }
        guard case .failed = CardActionOutcome(json: try json(#"{"status":"not-found"}"#)) else { Issue.record("not-found"); return }
    }

    @Test func stickerAnimationsAreValidated() throws {
        let list = StickerEntry.list(json: try json(#"""
        [{"cateId":1,"id":1,"url":"https://cdn.example/a","anim":{"kind":"sprite","url":"https://cdn.example/s","frames":16,"ms":60}},
         {"cateId":1,"id":2,"url":"https://cdn.example/b","anim":{"kind":"sprite","url":"https://cdn.example/s","frames":1,"ms":60}},
         {"cateId":1,"id":3,"url":"https://cdn.example/c","anim":{"kind":"sprite","url":"file:///x","frames":8,"ms":60}},
         {"cateId":1,"id":4,"url":"https://cdn.example/d","anim":{"kind":"gif","url":"https://cdn.example/g.gif"}},
         {"cateId":1,"id":5,"url":"https://cdn.example/e","anim":{"kind":"sprite","url":"https://cdn.example/s","frames":8,"ms":1}}]
        """#))
        #expect(list[0].animation == .sprite(url: URL(string: "https://cdn.example/s")!, frames: 16, frameMs: 60))
        #expect(list[1].animation == nil && list[2].animation == nil)
        #expect(list[3].animation == .animated(url: URL(string: "https://cdn.example/g.gif")!))
        #expect(list[4].animation == .sprite(url: URL(string: "https://cdn.example/s")!, frames: 8, frameMs: 30))
        let webp = StickerEntry.list(json: try json(#"[{"cateId":1,"id":1,"url":"https://cdn.example/a","anim":{"kind":"webp","url":"https://cdn.example/w"}}]"#))
        #expect(webp.first?.animation == .animated(url: URL(string: "https://cdn.example/w")!), "an animated WebP plays like a GIF, never as a sprite sheet")
    }
}

/// The composer's suggestion strip: stale stickers never show, and a click takes the whole box first.
@Suite struct TypingStickerStateTests {
    private func sticker(_ id: Int) -> StickerEntry { StickerEntry(cateId: 1, stickerId: id, url: URL(string: "https://cdn.example/\(id)")!) }

    @Test func theStripShowsOnlyForTheTextItWasFoundFor() {
        var state = TypingStickerState()
        state.offer([sticker(1)], for: "chào")
        func visible(_ draft: String, mention: Bool = false, enabled: Bool? = true) -> Bool {
            state.isVisible(draft: draft, mentionListOpen: mention, settingEnabled: enabled)
        }
        #expect(visible("chào"))
        #expect(!visible("chào bạn"), "the draft moved on: the old stickers are stale")
        #expect(!visible("chà"))
        #expect(!visible("chào", mention: true))
        #expect(!visible("chào", enabled: false), "setting turned off")
        #expect(visible("chào", enabled: nil), "setting not read yet is not a no")
        state.offer([], for: "chào")
        #expect(!visible("chào"))
        state.offer([sticker(1)], for: "   ")
        #expect(!visible("   "), "blank draft")
    }

    @Test func dismissalHoldsUntilTheComposerResetsIt() {
        var state = TypingStickerState()
        state.offer([sticker(1)], for: "chào")
        state.dismissed = true
        #expect(!state.isVisible(draft: "chào", mentionListOpen: false, settingEnabled: true))
        state.dismissed = false   // the composer does this on every draft change
        #expect(state.isVisible(draft: "chào", mentionListOpen: false, settingEnabled: true))
    }

    @Test func aClickTakesTheWholeDraft() throws {
        var state = TypingStickerState()
        state.offer([sticker(1)], for: "chào")
        let draft = ComposerDraft(text: "chào", mentions: [Mention(uid: "9", pos: 0, len: 2)])
        let taken = state.take(draft)
        #expect(taken.draft == draft && taken.typed == "chào")
        #expect(state.stickers.isEmpty && state.suggestedFor == nil, "the strip is gone with the box")
    }

    @Test func aFailedSendGivesTheTextBackAfterWhatWasTypedMeanwhile() throws {
        var state = TypingStickerState()
        state.offer([sticker(1)], for: "chào @An")
        let taken = state.take(ComposerDraft(text: "chào @An", mentions: [Mention(uid: "9", pos: 5, len: 3)]))
        var store = FailedDraftStore()
        store.stash(taken.draft, for: "c1", silent: true)       // what the composer does when the send fails
        let backOrNil = store.take(for: "c1")
        let back = try #require(backOrNil)
        let merged = ComposerDraft(text: "xyz").appending(back)   // ChatView.restoreFailedDraft
        #expect(merged.text == "xyz\nchào @An")
        #expect(merged.mentions == [Mention(uid: "9", pos: 9, len: 3)], "mention offsets move with the text")
    }
}

/// Outcomes that must never re-arm a second send, and the draft hand-back after a failed strip send.
@Suite struct StickerSendOutcomeTests {
    private func sticker(_ id: Int) -> StickerEntry { StickerEntry(cateId: 1, stickerId: id, url: URL(string: "https://cdn.example/\(id)")!) }

    @Test func noAnswerIsPendingNotAFailure() throws {
        let page = try JSONSerialization.jsonObject(with: Data(#"{"status":"pending","text":"Có thể đã gửi"}"#.utf8))
        #expect(CardActionOutcome(json: page) == .pending("Có thể đã gửi"))
        #expect(CardActionOutcome.noAnswerText.contains("kiểm tra lại cuộc trò chuyện"), "the Swift-side cap says to look, not to retry")
        #expect(CardActionOutcome.pending("x").logName == "pending")
    }

    @Test func aPendingSendHoldsTheStripBackForThatTextOnly() {
        var state = TypingStickerState()
        state.offer([sticker(1)], for: "chào")
        _ = state.take(ComposerDraft(text: "chào"))
        state.hold("chào")
        state.clear()   // the draft is empty now: clearing keeps the hold
        state.offer([sticker(1)], for: "chào")
        #expect(state.stickers.isEmpty && state.suggestedFor == nil, "the cached suggestion for the same text does not come back")
        #expect(state.isHeld("chào"))
        state.offer([sticker(2)], for: "chào bạn")
        #expect(state.stickers.count == 1 && !state.isHeld("chào"), "other text is suggested for again")
    }

    @Test func aSilentStashGivesTheTextBackWithoutTheNotSentNotice() {
        var store = FailedDraftStore()
        store.stash(ComposerDraft(text: "chào"), for: "c1", silent: true)
        let silent = store.takeWithNotice(for: "c1")
        #expect(silent?.draft.text == "chào" && silent?.notice == false)
        // a normal failure keeps its notice, also when merged with a silent one
        store.stash(ComposerDraft(text: "a"), for: "c2", silent: true)
        store.stash(ComposerDraft(text: "b"), for: "c2")
        let merged = store.takeWithNotice(for: "c2")
        #expect(merged?.draft.text == "a\nb" && merged?.notice == true)
        store.stash(ComposerDraft(text: "y"), for: "c4", silent: true)
        _ = store.drain()
        store.stash(ComposerDraft(text: "z"), for: "c4")
        #expect(store.takeWithNotice(for: "c4")?.notice == true, "drain forgets the silent mark")
    }

    @Test func unsentDraftsAreForgottenAtLogout() throws {
        // `logOut()` clears cookies of the real app, so it is not run here: both reset points are checked in the source.
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/Olaz/ZaloRuntime/ZaloEngine.swift")
        let source = try String(contentsOf: url, encoding: .utf8)
        #expect(source.components(separatedBy: "failedDrafts = FailedDraftStore()").count - 1 >= 2, "logOut() and the .loggedOut handler")
    }
}

