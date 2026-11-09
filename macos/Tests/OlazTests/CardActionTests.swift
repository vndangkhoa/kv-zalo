import Testing
import Foundation
@testable import Olaz

/// Parsing of the card-button JSON that card-actions-adapter.js produces / answers with.
@Suite struct CardActionTests {
    private func json(_ text: String) throws -> Any {
        try JSONSerialization.jsonObject(with: Data(text.utf8), options: [.fragmentsAllowed])
    }

    @Test func buttonsDropEntriesWithoutRefOrLabel() throws {
        let list = CardButton.list(json: try json(#"[{"ref":"ecard:0","label":" Gửi ","enabled":true,"help":""},{"ref":"","label":"x"},{"ref":"ecard:1","label":"  "},{"ref":"info:2","label":"Xem","enabled":false,"help":"Chưa hỗ trợ"}]"#))
        #expect(list == [
            CardButton(ref: "ecard:0", label: "Gửi", enabled: true),
            CardButton(ref: "info:2", label: "Xem", enabled: false, help: "Chưa hỗ trợ"),
        ])
        // Missing `enabled` means disabled; garbage degrades to nothing.
        #expect(CardButton.list(json: try json(#"[{"ref":"a","label":"B"}]"#)).first?.enabled == false)
        #expect(CardButton.list(json: try json(#""x""#)).isEmpty)
        #expect(CardButton.list(json: nil).isEmpty)
    }

    @Test func outcomes() throws {
        #expect(CardActionOutcome(json: try json(#"{"status":"done"}"#)) == .done)
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"openChat","id":"g123"}}"#)) == .openChat("g123"))
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"openProfile","uid":"42"}}"#)) == .openProfile("42"))
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"openURL","url":"https://zalo.me/g/abc"}}"#)) == .openURL(URL(string: "https://zalo.me/g/abc")!))
        #expect(CardActionOutcome(json: try json(#"{"status":"unsupported","text":"Chưa hỗ trợ đặt tên gợi nhớ"}"#)) == .unsupported("Chưa hỗ trợ đặt tên gợi nhớ"))
        #expect(CardActionOutcome(json: try json(#"{"status":"unsupported"}"#)) == .unsupported(CardActionOutcome.defaultUnsupported))
        #expect(CardActionOutcome(json: try json(#"{"status":"locked","text":"Chỉ trưởng/phó nhóm"}"#)) == .locked("Chỉ trưởng/phó nhóm"))
        guard case .failed = CardActionOutcome(json: try json(#"{"status":"not-open"}"#)) else { Issue.record("not-open"); return }
    }

    @Test func outcomeNeverOpensUnsafeLinksOrEmptyTargets() throws {
        // Only http(s) links are ever opened, whatever the JS side sends.
        for url in ["file:///etc/passwd", "smb://h/x", "javascript:alert(1)", "zalo://x", ""] {
            let outcome = CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"openURL","url":"\#(url)"}}"#))
            #expect(outcome == .unsupported(CardActionOutcome.defaultUnsupported))
        }
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"openChat","id":""}}"#)) == .unsupported(CardActionOutcome.defaultUnsupported))
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"launch"}}"#)) == .unsupported(CardActionOutcome.defaultUnsupported))
        guard case .failed = CardActionOutcome(json: nil) else { Issue.record("nil"); return }
        guard case .failed = CardActionOutcome(json: try json(#"{"nope":1}"#)) else { Issue.record("shape"); return }
    }

    @Test func suggestedStickersKeepOnlyWebImages() throws {
        let list = SuggestedSticker.list(json: try json(#"[{"ref":"stk:1_2","url":"https://s/1.png"},{"ref":"stk:1_3","url":"file:///x.png"},{"ref":"","url":"https://s/2.png"},{"ref":"stk:1_4"}]"#))
        #expect(list == [SuggestedSticker(ref: "stk:1_2", url: URL(string: "https://s/1.png")!)])
    }

    @Test func contactProfileAndFriendPrep() throws {
        let profile = try #require(ContactProfile(json: try json(#"{"uid":"7","name":"Lan","zaloName":"lan.z","avatar":"https://a/1.jpg","bio":"hi","isSelf":false,"isFriend":true}"#)))
        #expect(profile == ContactProfile(uid: "7", name: "Lan", zaloName: "lan.z", avatar: URL(string: "https://a/1.jpg"), bio: "hi", isSelf: false, isFriend: true))
        #expect(ContactProfile(json: try json(#"{"name":"x"}"#)) == nil)
        #expect(ContactProfile(json: nil) == nil)

        let ready = FriendRequestPrep(json: try json(#"{"state":"ready","defaultMessage":"Xin chào","maxMessage":150}"#))
        #expect(ready == FriendRequestPrep(state: .ready, defaultMessage: "Xin chào", maxMessage: 150))
        #expect(FriendRequestPrep(json: try json(#"{"state":"requested"}"#)).state == .requested)
        #expect(FriendRequestPrep(json: try json(#"{"state":"self"}"#)).state == .me)
        // Unknown / missing state is an error (the form is never shown); an absurd limit is capped.
        #expect(FriendRequestPrep(json: try json(#"{"state":"weird"}"#)).state == .error)
        #expect(FriendRequestPrep(json: nil).state == .error)
        #expect(FriendRequestPrep(json: try json(#"{"state":"ready","maxMessage":1e300}"#)).maxMessage == 500)
    }

    @Test func oaPartCarriesWhyItIsInert() throws {
        let object = try #require(try json(#"{"type":"oaMessage","parts":[{"header":false,"title":"Ưu đãi","url":"","help":"Chưa hỗ trợ trên ứng dụng này"}]}"#) as? [String: Any])
        guard case .oaMessage(let parts) = ChatMessage.Kind(json: object) else { Issue.record("no oa"); return }
        #expect(parts.first?.help == "Chưa hỗ trợ trên ứng dụng này")
        #expect(parts.first?.url == nil)
    }

    @Test func clipboardAndErrorOutcomes() throws {
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"copy","text":"0912"}}"#)) == .copy("0912"))
        #expect(CardActionOutcome(json: try json(#"{"status":"effect","effect":{"type":"copy","text":""}}"#)) == .unsupported(CardActionOutcome.defaultUnsupported))
        // A plain error keeps the adapter's own explanation (e.g. group lock state unknown, timeout).
        #expect(CardActionOutcome(json: try json(#"{"status":"error","text":"Hết thời gian chờ phản hồi."}"#)) == .failed("Hết thời gian chờ phản hồi."))
        #expect(CardActionOutcome(json: try json(#"{"status":"error"}"#)) == .failed("Không thực hiện được thao tác."))
    }

    @Test func profileKnowsOfficialAccounts() throws {
        let oa = try #require(ContactProfile(json: try json(#"{"uid":"9","name":"Shop","isOA":true}"#)))
        #expect(oa.isOfficialAccount)
        #expect(try #require(ContactProfile(json: try json(#"{"uid":"9","name":"Shop"}"#))).isOfficialAccount == false)
    }

    @Test func reminderSnapshot() throws {
        let invite = try #require(ReminderSnapshot(json: try json(#"{"variant":"invite","title":"Họp","start":1760000000000,"repeat":"Nhắc theo tuần","creatorName":"Lan","onlyMe":false,"isGroup":true,"mine":"","pending":"","canRsvp":true,"canView":true,"deleted":false,"sig":"s1"}"#)))
        #expect(invite.variant == .invite && invite.isDrawn)
        #expect(invite.showsRsvpButtons && !invite.showsRsvpStatus)
        #expect(invite.start == Date(timeIntervalSince1970: 1_760_000_000))
        let answered = try #require(ReminderSnapshot(json: try json(#"{"variant":"invite","isGroup":true,"canRsvp":true,"mine":"accept","sig":"s2"}"#)))
        #expect(answered.mine == .accept && answered.showsRsvpStatus && !answered.showsRsvpButtons)
        // Not answerable: deleted, only-me, 1-1, or the controller says no.
        for odd in [#"{"variant":"invite","isGroup":true,"canRsvp":true,"deleted":true}"#, #"{"variant":"invite","isGroup":true,"canRsvp":true,"onlyMe":true}"#,
                    #"{"variant":"invite","isGroup":false,"canRsvp":true}"#, #"{"variant":"invite","isGroup":true,"canRsvp":false}"#] {
            #expect(try #require(ReminderSnapshot(json: try json(odd))).showsRsvpButtons == false)
        }
        // Official hidden / pending draw nothing: not "drawn", so the static card stays. Unknown variants degrade to hidden.
        #expect(try #require(ReminderSnapshot(json: try json(#"{"variant":"hidden","sig":"hidden"}"#))).isDrawn == false)
        #expect(try #require(ReminderSnapshot(json: try json(#"{"variant":"pending"}"#))).isDrawn == false)
        #expect(try #require(ReminderSnapshot(json: try json(#"{"variant":"future"}"#))).variant == .hidden)
        #expect(try #require(ReminderSnapshot(json: try json(#"{"variant":"alert","start":1e300}"#))).start == nil)
        #expect(ReminderSnapshot(json: nil) == nil)
    }

    @Test func friendRequestMessageIsCutByUTF16() {
        // The official limit counts JS string units; an emoji is two.
        #expect("abcdef".prefixUTF16(3) == "abc")
        #expect("a😀b".prefixUTF16(2) == "a")
        #expect("a😀b".prefixUTF16(3) == "a😀")
        #expect("ngắn".prefixUTF16(150) == "ngắn")
        #expect("".prefixUTF16(0) == "")
    }
}
