import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import Olaz

/// Runs the real `group-create-adapter.js` in JavaScriptCore against fake official modules: the checks done before anything is sent,
/// the exact `createGroupV2` arguments, the quota and server error texts, the open / summary / avatar steps after creating.
@MainActor
struct GroupCreateScriptTests {
    private func harness() -> ScriptHarness {
        let h = ScriptHarness(prelude: """
        var calls = [], opened = [], dispatched = [];
        window.webkit = { messageHandlers: { zlog: { postMessage: function () {} } } };
        var conversations = {};
        window.__zaloStore = { getState: function () { return { user: { userId: '1' }, conversations: { alls: conversations } }; },
                               dispatch: function (a) { dispatched.push(a); } };
        var friends = [
          { userId: '2', isFr: 1, zaloName: 'An', displayName: 'An bạn thân', avatar: 'https://a/2.jpg' },
          { userId: '3', isFr: 1, zaloName: '', displayName: 'Bình', avatar: 'https://a/3.jpg' },
          { userId: '4', isFr: 1, zaloName: 'Chặn', displayName: 'Chặn' },
          { userId: '5', isFr: 0, zaloName: 'Người lạ' },
          { userId: '6', isFr: 1, zaloName: 'Dũng*', displayName: 'Dũng' },
          { userId: '7', isFr: 1, zaloName: 'Một cái tên rất dài', displayName: 'Dài' },
          { userId: '8', isFr: 1, zaloName: 'Zalo', type: 7 },
          { userId: '1', isFr: 1, zaloName: 'Tôi' }
        ];
        var friendMod = { getFriendsSync: function () { return friends; }, isBlocked: function (id) { return id === '4'; } };
        var check = { error: null };
        var privacy = { checkCreateCondition: function (t) { calls.push(['check', t]); return check.error ? Promise.reject(check.error) : Promise.resolve(true); },
                        getOwnedCommunities: function () { return []; } };
        var opener = { openConversation: function (id, p) { opened.push([id, p]); conversations[id] = { userId: id }; return Promise.resolve(); },
                       conversationWillOpen: function () {} };
        var io = { getFriendManagerModule: function () { return Promise.resolve({ default: friendMod }); }, getGroupManagerModule: function () {} };
        window.__zaloRequire = {};
        window.__zaloDI = function () { return [{ instance: opener }, { instance: privacy }, { instance: io }]; };
        var server = { answer: null, error: null };
        var fetcher = { createGroupV2: function () { calls.push(['create'].concat(Array.prototype.slice.call(arguments)));
                          return server.error ? Promise.reject(server.error) : Promise.resolve(server.answer); },
                        createGroupTopicv2: function () {}, createGroupNoticeCmd: function () {}, getGroupInfoByLink: function () {} };
        var api = { updateAvatar: function (id, size, cid, file, w, h) { calls.push(['avatar', id, size, cid, file.name, file.type, file.parts[0].length, w, h]); return Promise.resolve({}); },
                    getSocketUrl: function () {}, createGroupV2: function () {} };
        // The low-level API class: a function with statics named like the fetcher's methods (it must never be taken for the fetcher).
        var apiClass = function () {};
        apiClass.createGroupV2 = function (o) { calls.push(['api.createGroupV2', o]); return Promise.resolve({}); };
        apiClass.createGroupTopicv2 = function () {}; apiClass.getGroupInfoByLink = function () {};
        var clientIds = { _lastClientId: 100, next: function () { this._lastClientId += 1; return this._lastClientId; }, setGetTimeServerFunc: function () {} };
        var events = { addGroupEventMessages: function (id, msgs) { calls.push(['summary', id, msgs]); return Promise.resolve({ conversation: { userId: id }, messages: msgs }); } };
        var exportsList = [
          { openConvParam: { fromGroupCompose: function (c) { return { from: 'GroupCompose', conv: c.userId }; } } },
          { MEMBERS_TRACKING_SOURCE: { friend: -1, searchPhoneGlobal: 85 }, GROUPID_PREFIX: 'g', MSG_INFO: -4, GROUP_MEMBER_ERROR: { REACHED_LIMIT_GROUP: 173 } },
          { a: { MUTUAL_MESSAGES_BLOCKED: 'mutual-messages-blocked' },
            f: function (info) { return Object.keys(info).map(function (k) { var e = {}; e[k] = info[k]; return { group: k === '999' ? null : 'p' + k, errorMembers: e }; }); } },
          { FetchActions: { ADD_MESSAGES: 'ADD_MESSAGES' } },
          { default: apiClass },
          { default: fetcher }, { default: api }, clientIds, { default: events },
          { default: { getFullTimeFromMilisecond: function () { return '10:00 27/10/2026'; } } },
          { group_config: { max_group_name_length: 20, group_name_regex: '`|~|!|@|#|%|\\\\*|\\\\^|\\\\$' } },
          { settings: { group: { max_member: 3 } } },
          { default: { str: function (k) { return k === 'STR_CREATE_GROUP_MSG' ? 'Bạn vừa tạo nhóm' : k; }, trans: function () {}, currentLanguage: function () {} } }
        ];
        window.__zaloEachExport = function (req, visit) { exportsList.forEach(visit); };
        var atob = function (s) { return s === 'AQID' ? '\\u0001\\u0002\\u0003' : ''; };
        var Blob = function (parts, opts) { this.parts = parts; this.type = opts.type; };
        """)
        h.load("group-create-adapter.js")
        return h
    }

    private func json(_ h: ScriptHarness, _ expression: String) -> String {
        h.ctx.evaluateScript("JSON.stringify(\(expression))")?.toString() ?? ""
    }

    @Test func infoReportsTheOfficialLimits() {
        let h = harness()
        let info = h.run("info", "window.__zaloGroupCreate.info()")
        #expect(info?["status"] as? String == "ok")
        #expect(info?["maxMembers"] as? Int == 3)
        #expect(info?["maxNameLength"] as? Int == 20)
        #expect(info?["nameRegex"] as? String == "`|~|!|@|#|%|\\*|\\^|\\$")
        #expect(info?["blocked"] as? [String] == ["4"])
        #expect(info?["officialAccounts"] as? [String] == ["8"])
        #expect(h.failures.isEmpty)
    }

    @Test func picksAndNameAreCheckedBeforeAnythingIsSent() {
        let h = harness()
        // Me (1) and the duplicate are ignored: one person is left.
        let few = h.run("few", "window.__zaloGroupCreate.create({ name: '', members: ['2', '1', '2'] })")
        #expect(few?["status"] as? String == "invalid")
        #expect(few?["text"] as? String == "Không đủ số thành viên để tạo nhóm (tối thiếu là 3).")
        // A blocked pick or one that is no longer a friend is refused and named, never dropped silently.
        let blocked = h.run("blocked", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3', '4'] })")
        #expect(blocked?["text"] as? String == "Bỏ chặn để thêm người này")
        #expect(blocked?["dropped"] as? [String] == ["4"])
        let stranger = h.run("stranger", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3', '5'] })")
        #expect(stranger?["status"] as? String == "invalid")
        #expect(stranger?["dropped"] as? [String] == ["5"])
        let many = h.run("many", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3', '6', '7'] })")
        #expect(many?["text"] as? String == "Bạn chỉ có thể tạo nhóm với 3 người khác")
        let badName = h.run("name", "window.__zaloGroupCreate.create({ name: 'Nhóm #1', members: ['2', '3'] })")
        #expect(badName?["text"] as? String == "Tên nhóm chứa ký tự không hợp lệ. Vui lòng thử lại.")
        #expect(json(h, "calls") == "[]")
    }

    @Test func createSendsTheComposeArgumentsAndOpensTheGroup() {
        let h = harness()
        h.ctx.evaluateScript("server.answer = { conv: { userId: 'g77' }, isExistGroup: false, errorInfo: null, successMembers: ['2', '3'] };")
        let result = h.run("create", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })")
        #expect(result?["status"] as? String == "created")
        #expect(result?["convId"] as? String == "g77")
        #expect(result?["failed"] as? [String] == [])
        // Quota check first, then the compose's exact call: zaloName (else displayName) joined, my uid, next client id,
        // friend member types, nameChanged false, no label, NORMAL group.
        #expect(json(h, "calls") == #"[["check",1],["create","An, Bình",[{"id":"2","avatar":"https://a/2.jpg","dName":"An"},{"id":"3","avatar":"https://a/3.jpg","dName":"Bình"}],"1",101,[-1,-1],false,null,null,1]]"#)
        #expect(json(h, "opened") == #"[["g77",{"from":"GroupCompose","conv":"g77"}]]"#)
        #expect(h.failures.isEmpty)
    }

    @Test func typedNameIsSentAsChangedAndDefaultNameIsCleaned() {
        let h = harness()
        h.ctx.evaluateScript("server.answer = { conv: { userId: 'g1' } };")
        _ = h.run("typed", "window.__zaloGroupCreate.create({ name: '  Đội bóng ', members: ['2', '3'] })")
        #expect(json(h, "[calls[1][1], calls[1][6]]") == #"["Đội bóng",true]"#)
        // Untyped: "*" stripped, then cut at max_group_name_length (20) + "....".
        _ = h.run("default", "window.__zaloGroupCreate.create({ name: '', members: ['6', '7'] })")
        #expect(json(h, "[calls[3][1], calls[3][6]]") == #"["Dũng, Một cái tên rấ....",false]"#)
    }

    @Test func quotaCheckFailuresStopTheCreate() {
        let h = harness()
        h.ctx.evaluateScript("check.error = { error_code: 173, data: { error_message_localize: 'Bạn chỉ có thể tham gia đồng thời 500 nhóm và cộng đồng.' } };")
        let limit = h.run("limit", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })")
        #expect(limit?["status"] as? String == "failed")
        #expect(limit?["title"] as? String == "Không thể tạo nhóm/cộng đồng")
        #expect(limit?["text"] as? String == "Bạn chỉ có thể tham gia đồng thời 500 nhóm và cộng đồng.")
        #expect(limit?["close"] as? Bool == true)
        h.ctx.evaluateScript("check.error = { error_code: 1 };")
        let other = h.run("other", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })")
        #expect(other?["title"] == nil)
        #expect(other?["text"] as? String == "Có lỗi xảy ra. Vui lòng thử lại.")
        #expect(other?["close"] as? Bool == true)
        #expect(json(h, "calls.filter(function (c) { return c[0] === 'create'; }).length") == "0")
    }

    @Test func serverErrorsUseTheOfficialTexts() {
        let h = harness()
        let cases: [(String, String)] = [
            ("{ error_code: 175 }", "Tất cả thành viên đã chặn bạn."),
            ("{ error_code: 270, error_message: 'server text' }", "Bạn đã đạt giới hạn tạo nhóm trong ngày."),
            ("{ code: 'ERR_NO_NETWORK' }", "Không có mạng, vui lòng kiểm tra lại kết nối và thử lại"),
            ("{ error_code: 999, error_message: 'Lỗi máy chủ' }", "Lỗi máy chủ"),
            ("{ error_code: 999 }", "Tạo nhóm không thành công."),
        ]
        for (index, (error, text)) in cases.enumerated() {
            h.ctx.evaluateScript("server.error = \(error);")
            let result = h.run("e\(index)", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })")
            #expect(result?["status"] as? String == "failed")
            #expect(result?["text"] as? String == text)
        }
        #expect(json(h, "opened") == "[]")
    }

    @Test func membersNotAddedGetTheSummaryMessagesAndAreReported() {
        let h = harness()
        h.ctx.evaluateScript("server.answer = { conv: { userId: 'g78' }, errorInfo: { '1001': ['3'], '999': ['6'], '5': [] }, successMembers: ['2'] };")
        let result = h.run("partial", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3', '6'] })")
        #expect(result?["status"] as? String == "created")
        #expect(Set(result?["failed"] as? [String] ?? []) == ["3", "6"])
        // One info message per partition (empty codes dropped); the legacy partition also lists who was added.
        #expect(json(h, "calls[2][0]") == #""summary""#)
        #expect(json(h, "calls[2][2].map(function (m) { return [m.toUid, m.msgType, m.message, m.act, m.eventInfo]; })")
                == #"[["g78",-4,"Bạn vừa tạo nhóm","create_group_summary",{"errorMembers":{"999":["6"]},"successMembers":["2"]}],["g78",-4,"Bạn vừa tạo nhóm","create_group_summary",{"errorMembers":{"1001":["3"]}}]]"#)
        #expect(json(h, "dispatched.map(function (a) { return [a.type, a.payload.conversationId, a.payload.messages.length]; })") == #"[["ADD_MESSAGES","g78",2]]"#)
    }

    @Test func existingGroupIsOpenedWithTheOfficialNotice() {
        let h = harness()
        h.ctx.evaluateScript("server.answer = { conv: { userId: 'g79' }, isExistGroup: true };")
        let result = h.run("exists", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })")
        #expect(result?["status"] as? String == "exists")
        #expect(result?["convId"] as? String == "g79")
        #expect(result?["text"] as? String == "Nhóm bạn vừa tạo đã tồn tại")
        #expect(json(h, "opened.length") == "1")
    }

    @Test func pickedPhotoIsUploadedAsTheGroupAvatar() {
        let h = harness()
        h.ctx.evaluateScript("server.answer = { conv: { userId: 'g80' } };")
        _ = h.run("avatar", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'], avatar: { data: 'AQID', width: 640, height: 640 } })")
        #expect(json(h, "calls[2]") == #"["avatar","g80",160,"110:00 27/10/2026","change-avatar.jpg","image/jpeg",3,640,640]"#)
    }

    @Test func noAnswerInTimeIsPendingAndALateAnswerOpensNothing() {
        let h = harness()
        h.ctx.evaluateScript("var answer; fetcher.createGroupV2 = function () { calls.push(['create']); return new Promise(function (r) { answer = r; }); };")
        let pending = h.runUntilSettled("slow", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'], avatar: { data: 'AQID', width: 640, height: 640 } })")
        #expect(pending?["status"] as? String == "pending")
        #expect(pending?["text"] as? String == "Chưa nhận được phản hồi. Hãy kiểm tra danh sách hội thoại trước khi thử lại.")
        // Not busy any more, and a late answer still gets the summary and the avatar, but nothing is opened under Swift.
        h.ctx.evaluateScript("answer({ conv: { userId: 'g90' }, errorInfo: { '999': ['3'] } });")
        h.pump(steps: 5)
        #expect(json(h, "calls.map(function (c) { return c[0]; })") == #"["check","create","summary","avatar"]"#)
        #expect(json(h, "opened") == "[]")
        h.ctx.evaluateScript("fetcher.createGroupV2 = function () { return Promise.resolve({ conv: { userId: 'g91' } }); };")
        #expect(h.run("again", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })")?["status"] as? String == "created")
    }

    @Test func secondCreateWhileOneRunsIsRefused() {
        let h = harness()
        h.ctx.evaluateScript("fetcher.createGroupV2 = function () { return new Promise(function () {}); };")
        h.start("first", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })").pump(steps: 3)
        let second = h.run("second", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })", steps: 2)
        #expect(second?["status"] as? String == "busy")
        #expect(second?["text"] as? String == "Nhóm trước vẫn đang được tạo.")
    }

    @Test func theApiClassIsNeverTakenForTheFetcher() {
        let h = harness()
        h.ctx.evaluateScript("server.answer = { conv: { userId: 'g92' } };")
        #expect(h.run("create", "window.__zaloGroupCreate.create({ name: '', members: ['2', '3'] })")?["status"] as? String == "created")
        #expect(json(h, "calls.filter(function (c) { return c[0] === 'api.createGroupV2'; }).length") == "0")
    }
}

@MainActor
struct GroupComposeModelTests {
    @Test func limitsParseTheAdapterAnswer() {
        let limits = GroupCreateLimits(json: ["status": "ok", "maxMembers": 50, "maxNameLength": 30, "nameRegex": "x", "blocked": ["9"],
                                              "officialAccounts": ["8"]])
        #expect(limits?.maxMembers == 50)
        #expect(limits?.maxNameLength == 30)
        #expect(limits?.namePattern == "x")
        #expect(limits?.blocked == ["9"])
        #expect(limits?.officialAccounts == ["8"])
        #expect(GroupCreateLimits(json: ["status": "not-ready"]) == nil)
    }

    @Test func nameRulesFollowTheOfficialInput() {
        let limits = GroupCreateLimits()
        #expect(limits.isValidName("Gia đình 2026"))
        #expect(limits.isValidName(""))
        for bad in ["a`b", "~", "x!", "@team", "#1", "100%", "a*b", "a^b", "$"] { #expect(!limits.isValidName(bad), "\(bad)") }
        #expect(limits.clampedName(String(repeating: "á", count: 60)).utf16.count == 50)
        // An emoji is two UTF-16 units and is never split.
        #expect(limits.clampedName(String(repeating: "a", count: 49) + "😀") == String(repeating: "a", count: 49))
    }

    @Test func selectionRefusesBlockedAndOverLimitPicks() {
        var limits = GroupCreateLimits()
        limits.maxMembers = 2
        limits.blocked = ["b"]
        var selection = GroupComposeSelection()
        #expect(selection.toggle("a", limits: limits) == .added)
        #expect(!selection.canConfirm)
        #expect(selection.toggle("b", limits: limits) == .blocked)
        #expect(selection.toggle("c", limits: limits) == .added)
        #expect(selection.canConfirm)
        #expect(selection.toggle("d", limits: limits) == .overLimit)
        #expect(selection.toggle("a", limits: limits) == .removed)
        #expect(selection.ids == ["c"])
        selection.keep { $0 != "c" }
        #expect(selection.ids.isEmpty)
    }

    @Test func rosterListsRecentFriendChatsThenEveryFriend() {
        func friend(_ id: String, _ name: String, zalo: String = "") -> ContactsState.Friend {
            let key = ZaloEngine.contactFold(name)
            let first = key.first.map { String($0).uppercased() } ?? "#"
            let section = first.unicodeScalars.allSatisfy { ("A"..."Z").contains($0) } ? first : "#"
            return .init(id: id, name: name, zaloName: zalo, avatar: nil, lastActive: nil, online: false, section: section, sortKey: key)
        }
        func chat(_ id: String, group: Bool = false, minutesAgo: Double) -> ZaloEngine.ConversationSummary {
            .init(id: id, isGroup: group, name: id, avatar: nil, pinned: false, members: 0, unread: 0,
                  preview: .init(text: "", date: Date(timeIntervalSinceNow: -minutesAgo * 60), fromMe: false))
        }
        let friends = [friend("1", "Đức"), friend("2", "an"), friend("3", "Bảo", zalo: "Bao Zalo"), friend("4", "123"),
                       friend("5", "Châu"), friend("6", "Em"), friend("7", "Giang")]
        let chats = [chat("g1", group: true, minutesAgo: 1), chat("me", minutesAgo: 2), chat("9", minutesAgo: 3), chat("3", minutesAgo: 9),
                     chat("1", minutesAgo: 4), chat("2", minutesAgo: 5), chat("5", minutesAgo: 6), chat("6", minutesAgo: 7), chat("7", minutesAgo: 8)]
        let roster = GroupComposeRoster(friends: friends, conversations: chats, selfId: "me")
        // Groups, My Cloud and non-friends are skipped; the 5 latest by message time.
        #expect(roster.recent.map(\.id) == ["1", "2", "5", "6", "7"])
        // Official accounts are friends too, but never "recent".
        let withoutOA = GroupComposeRoster(friends: friends, conversations: chats, selfId: "me", officialAccounts: ["2"])
        #expect(withoutOA.recent.map(\.id) == ["1", "5", "6", "7", "3"])
        #expect(withoutOA.friends.count == 7)
        #expect(roster.friends.map(\.name) == ["an", "Bảo", "Châu", "Đức", "Em", "Giang", "123"])
        #expect(roster.sections.map(\.title) == ["A", "B", "C", "D", "E", "G", "#"])
        #expect(roster.matching("duc").map(\.id) == ["1"])
        #expect(roster.matching("bao zalo").map(\.id) == ["3"])
        #expect(roster.matching("  ").count == 7)
    }

    @Test func outcomesMapTheAdapterAnswers() {
        #expect(GroupCreateOutcome(json: ["status": "created", "convId": "g1", "failed": ["2"]]) == .created(id: "g1", failed: ["2"]))
        #expect(GroupCreateOutcome(json: ["status": "exists", "convId": "g1", "text": "t"]) == .exists(id: "g1", text: "t"))
        #expect(GroupCreateOutcome(json: ["status": "created"]) == .failed(title: nil, message: GroupCreateOutcome.defaultFailure))
        #expect(GroupCreateOutcome(json: ["status": "invalid", "text": "x"]) == .invalid("x"))
        #expect(GroupCreateOutcome(json: ["status": "invalid", "text": "x", "dropped": ["4"]]) == .invalid("x", dropped: ["4"]))
        #expect(GroupCreateOutcome(json: ["status": "failed", "title": "T", "text": "m"]) == .failed(title: "T", message: "m"))
        #expect(GroupCreateOutcome(json: ["status": "failed", "text": "m", "close": true]) == .failed(title: nil, message: "m", closes: true))
        #expect(GroupCreateOutcome(json: ["status": "busy", "text": "b"]) == .failed(title: nil, message: "b"))
        #expect(GroupCreateOutcome(json: ["status": "pending"]) == .pending(GroupCreateOutcome.noAnswerText))
        #expect(GroupCreateOutcome(json: ["status": "not-ready"]) == .failed(title: nil, message: GroupCreateOutcome.notReadyText))
        #expect(GroupCreateOutcome(json: nil) == .failed(title: nil, message: GroupCreateOutcome.defaultFailure))
    }

    @Test func notAddedNoticeNamesAtMostThree() {
        #expect(ZaloEngine.notAddedText(["An"]) == "Chưa thêm được An vào nhóm.")
        #expect(ZaloEngine.notAddedText(["A", "B", "C", "D", "E"]) == "Chưa thêm được A, B, C và 2 người khác vào nhóm.")
    }

    private func noise(width: Int, height: Int) -> CGImage {
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
        var seed: UInt32 = 7
        for y in stride(from: 0, to: height, by: 4) {
            for x in stride(from: 0, to: width, by: 4) {
                seed = seed &* 1_103_515_245 &+ 12345
                context.setFillColor(CGColor(red: CGFloat(seed >> 24 & 255) / 255, green: CGFloat(seed >> 16 & 255) / 255, blue: CGFloat(seed >> 8 & 255) / 255, alpha: 1))
                context.fill(CGRect(x: x, y: y, width: 4, height: 4))
            }
        }
        return context.makeImage()!
    }

    @Test func avatarIsTheCentreSquareWithinTheOfficialBounds() throws {
        let wide = try GroupAvatarImage.make(from: noise(width: 500, height: 300)).get()
        #expect(wide.side == 300)
        let big = try GroupAvatarImage.make(from: noise(width: 3000, height: 2000)).get()
        #expect(big.side == GroupAvatarImage.maxSide)
        #expect(big.jpeg.count <= GroupAvatarImage.maxBytes)
        #expect(big.jpeg.starts(with: [0xFF, 0xD8]))
        #expect(GroupAvatarImage.make(from: noise(width: 239, height: 800)) == .failure(.tooSmall))
    }

    private func jpegFile(_ image: CGImage, quality: Double = 0.9) throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("olaz-avatar-\(UUID().uuidString).jpg")
        let dest = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        #expect(CGImageDestinationFinalize(dest))
        return url
    }

    private func flat(width: Int, height: Int) -> CGImage {
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
        context.setFillColor(CGColor(gray: 0.5, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()!
    }

    /// The official size rule is about the picked file (7 KB minimum), not the re-encoded photo.
    @Test func pickedFileSizeDecidesNotTheReencodedPhoto() throws {
        let tiny = try jpegFile(flat(width: 300, height: 300))
        defer { try? FileManager.default.removeItem(at: tiny) }
        #expect(try tiny.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0 < GroupAvatarImage.minFileBytes)
        #expect(GroupAvatarImage.make(from: tiny) == .failure(.tooLight))
        // A flat image is fine when it comes from a big enough file, even if its re-encoded square is small.
        #expect((try? GroupAvatarImage.make(from: flat(width: 300, height: 300)).get())?.side == 300)
        let photo = try jpegFile(noise(width: 2000, height: 1500))
        defer { try? FileManager.default.removeItem(at: photo) }
        let made = try GroupAvatarImage.make(from: photo).get()
        #expect(made.side == GroupAvatarImage.maxSide)
        #expect(made.jpeg.count <= GroupAvatarImage.maxBytes)
    }
}
