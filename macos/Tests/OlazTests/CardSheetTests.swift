import Testing
import Foundation
@testable import Olaz

/// Parsing of what the card sheets (alias, group invite, sticker category, poll, tabs) receive from card-actions-adapter.js.
@Suite struct CardSheetTests {
    private func json(_ text: String) throws -> Any {
        try JSONSerialization.jsonObject(with: Data(text.utf8), options: [.fragmentsAllowed])
    }

    @Test func groupInviteLinksFollowTheOfficialRegex() {
        for good in ["https://zalo.me/g/abc123", "http://zaloapp.com/g/x", "https://ZALO.me/g/x?y=1"] {
            #expect(GroupInviteLink.isInvite(URL(string: good)), "\(good)")
        }
        for bad in ["https://zalo.me/g", "https://zalo.me/gx/abc", "https://evil.example/g/abc", "https://zalo.me.evil.example/g/abc",
                    "https://user@evil.example/zalo.me/g/abc", "zalo://zalo.me/g/abc", "file:///g/abc", "https://zalo.me/+84123"] {
            #expect(!GroupInviteLink.isInvite(URL(string: bad)), "\(bad)")
        }
        #expect(!GroupInviteLink.isInvite(nil))
    }

    @Test func sheetEffectsAreValidated() throws {
        func outcome(_ effect: String) throws -> CardActionOutcome { CardActionOutcome(json: try json(#"{"status":"effect","effect":\#(effect)}"#)) }
        #expect(try outcome(#"{"type":"groupInvite","url":"https://zalo.me/g/abc"}"#) == .present(.groupInvite(URL(string: "https://zalo.me/g/abc")!)))
        // Anything that is not a group link never becomes a group sheet (and never a browser launch).
        #expect(try outcome(#"{"type":"groupInvite","url":"https://example.com/g/abc"}"#) == .unsupported(CardActionOutcome.defaultUnsupported))
        #expect(try outcome(#"{"type":"groupInvite","url":"file:///etc/passwd"}"#) == .unsupported(CardActionOutcome.defaultUnsupported))
        #expect(try outcome(#"{"type":"stickers","cateId":"12345"}"#) == .present(.stickers(12345)))
        #expect(try outcome(#"{"type":"stickers","cateId":"0"}"#) == .unsupported(CardActionOutcome.defaultUnsupported))
        #expect(try outcome(#"{"type":"stickers","cateId":"1; drop"}"#) == .unsupported(CardActionOutcome.defaultUnsupported))
        #expect(try outcome(#"{"type":"poll","pollId":"987654321"}"#) == .present(.poll("987654321")))
        #expect(try outcome(#"{"type":"poll","pollId":"x"}"#) == .unsupported(CardActionOutcome.defaultUnsupported))
        #expect(try outcome(#"{"type":"tab","tab":"groups"}"#) == .present(.tab(.groups)))
        #expect(try outcome(#"{"type":"tab","tab":"settings"}"#) == .unsupported(CardActionOutcome.defaultUnsupported))
    }

    @Test func aliasDraftKeepsTheOfficialLimits() throws {
        let draft = try #require(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"alias","uid":"42","name":"Lan","suggest":"Lan\u0001 Anh","placeholder":"lan.z","max":50}}"#)))
        guard case .present(.alias(let alias)) = draft else { Issue.record("no alias"); return }
        #expect(alias == AliasDraft(uid: "42", name: "Lan", suggest: "Lan Anh", placeholder: "lan.z", max: 50, hasAlias: nil))
        guard case .present(.alias(let withAlias)) = CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"alias","uid":"42","hasAlias":true}}"#)) else { Issue.record("hasAlias"); return }
        #expect(withAlias.hasAlias == true)
        // Not a user id, or a conversation id of a group: no dialog. A silly limit falls back / is capped.
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"alias","uid":"g42"}}"#)) == .unsupported(CardActionOutcome.defaultUnsupported))
        guard case .present(.alias(let capped)) = CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"alias","uid":"7","max":1e9}}"#)) else { Issue.record("capped"); return }
        #expect(capped.max == 200)
        guard case .present(.alias(let zero)) = CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"alias","uid":"7","max":0}}"#)) else { Issue.record("zero"); return }
        #expect(zero.max == AliasDraft.fallbackMax)
        #expect(AliasDraft.sanitized("a\u{0}b\u{7}c\u{8}d") == "abc\u{8}d")
    }

    @Test func busyAndPendingNeverStartAnything() throws {
        #expect(CardActionOutcome(json: try json(#"{"status":"busy","text":"Đang xử lý"}"#)) == .failed("Đang xử lý"))
        // pending: no answer yet, it may still happen. Its own case, so nothing treats it as a failure to retry.
        guard case .pending(let text) = CardActionOutcome(json: try json(#"{"status":"pending"}"#)) else { Issue.record("pending"); return }
        #expect(!text.isEmpty)
        #expect(CardActionOutcome(json: try json(#"{"status":"pending","text":"Chờ"}"#)) == .pending("Chờ"))
    }

    @Test func groupPreviewStates() throws {
        let preview = GroupInvitePreview(json: try json(#"""
        {"state":"preview","groupId":"g100","name":"Nhóm A","avatar":"https://a/1.jpg","totalMember":3,"desc":"Mô tả","community":false,"creatorName":"Lan",
         "members":[{"id":"1","name":"Lan","avatar":"https://a/2.jpg","admin":true},{"id":"","name":"x"},{"id":"2","name":"Minh","avatar":"file:///x","admin":false}]}
        """#))
        #expect(preview.state == .preview && preview.groupId == "g100" && preview.totalMember == 3 && preview.creatorName == "Lan")
        #expect(preview.members.map(\.id) == ["1", "2"])
        #expect(preview.members[0].admin && preview.members[1].avatar == nil)
        #expect(GroupInvitePreview(json: try json(#"{"state":"member","groupId":"g5"}"#)).state == .member)
        // A group id that is not "g<digits>" never opens anything.
        #expect(GroupInvitePreview(json: try json(#"{"state":"member","groupId":"../x"}"#)).state == .error)
        #expect(GroupInvitePreview(json: try json(#"{"state":"preview","groupId":"100"}"#)).state == .error)
        let failed = GroupInvitePreview(json: try json(#"{"state":"error","text":"Link tham gia đã hết hạn."}"#))
        #expect(failed.state == .error && failed.text == "Link tham gia đã hết hạn.")
        #expect(GroupInvitePreview(json: nil).state == .error && !GroupInvitePreview(json: nil).text.isEmpty)
    }

    @Test func groupJoinResults() throws {
        #expect(GroupJoinResult(json: try json(#"{"state":"joined","groupId":"g9"}"#)) == GroupJoinResult(state: .joined, groupId: "g9"))
        #expect(GroupJoinResult(json: try json(#"{"state":"already","groupId":"g9","text":"Đã là thành viên của nhóm"}"#)).state == .already)
        #expect(GroupJoinResult(json: try json(#"{"state":"pending","text":"Yêu cầu của bạn đã được gửi"}"#)).state == .pending)
        // "joined" without a valid group id cannot open a conversation: treated as an error.
        #expect(GroupJoinResult(json: try json(#"{"state":"joined","groupId":"9"}"#)).state == .error)
        #expect(GroupJoinResult(json: try json(#"{"state":"error"}"#)).text == "Vào nhóm không thành công.")
        #expect(GroupJoinResult(json: nil).state == .error)
    }

    @Test func stickerCategoryKeepsOnlyItsOwnWebStickers() throws {
        let category = try #require(StickerCategory(json: try json(#"""
        {"id":321,"name":"Bộ","icon":"https://s/i.png","stickers":[{"ref":"321_1","url":"https://s/1.png"},{"ref":"321_x","url":"https://s/2.png"},
         {"ref":"999_3","url":"https://s/3.png"},{"ref":"321_4","url":"file:///x.png"},{"ref":"321_5","url":"https://s/5.webp"}]}
        """#)))
        #expect(category.id == 321 && category.name == "Bộ")
        #expect(category.stickers.map(\.ref) == ["321_1", "321_5"])
        #expect(category.stickers.map(\.stickerId) == [1, 5])
        #expect(StickerCategory(json: try json(#"{"id":321,"stickers":[]}"#)) == nil)
        #expect(StickerCategory(json: try json(#"{"id":0,"stickers":[{"ref":"0_1","url":"https://s/1.png"}]}"#)) == nil)
        #expect(StickerCategory(json: nil) == nil)
    }
}
