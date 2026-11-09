import Testing
import Foundation
@testable import Olaz

/// `ChatMessage.Kind(json:)` fed with JSON shaped exactly like `mapContent` in chat-adapter.js emits it.
@Suite struct MessageKindTests {
    private func kind(_ json: String) throws -> ChatMessage.Kind {
        let object = try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        return ChatMessage.Kind(json: object)
    }

    @Test func linkCard() throws {
        let k = try kind(#"{"type":"link","title":"Hay","url":"https://x.vn/a","thumbnail":"https://img/t.jpg","description":"Mô tả","source":"vnexpress.net"}"#)
        #expect(k == .link(title: "Hay", url: URL(string: "https://x.vn/a"), thumbnail: URL(string: "https://img/t.jpg"), description: "Mô tả", source: "vnexpress.net"))
        // The old shape (no description / source) still parses.
        #expect(try kind(#"{"type":"link","title":"T","url":"","thumbnail":""}"#) == .link(title: "T", url: nil, thumbnail: nil))
    }

    @Test func contactCard() throws {
        let k = try kind(#"{"type":"contactCard","name":"Lan","uid":"2","avatar":"https://t/a.jpg","phone":"09123","qr":"https://q","oaLink":"","isOA":false}"#)
        guard case .contactCard(let card) = k else { Issue.record("not a contact card"); return }
        #expect(card.name == "Lan" && card.uid == "2" && card.phone == "09123")
        #expect(card.avatar == URL(string: "https://t/a.jpg") && card.qr == URL(string: "https://q"))
        #expect(!card.isOfficialAccount)
        // A bare {type, name} (previous contract) still gives a card.
        #expect(try kind(#"{"type":"contactCard","name":"A"}"#) == .contactCard(.init(name: "A")))
        guard case .contactCard(let oa) = try kind(#"{"type":"contactCard","name":"Shop","uid":"o1","oaLink":"zalo.me/s/x","isOA":true}"#) else { return }
        #expect(oa.isOfficialAccount && oa.oaLink == "zalo.me/s/x")
    }

    @Test func calls() throws {
        let missed = try kind(#"{"type":"call","video":true,"missed":true,"duration":0,"title":"Bạn bị nhỡ","detail":"Cuộc gọi video"}"#)
        #expect(missed == .call(video: true, missed: true, duration: 0, title: "Bạn bị nhỡ", detail: "Cuộc gọi video"))
        #expect(try kind(#"{"type":"call","video":false,"missed":false,"duration":125}"#) == .call(video: false, missed: false, duration: 125))
    }

    @Test func locationNeedsValidCoordinates() throws {
        guard case .location(let ok) = try kind(#"{"type":"location","title":"Nhà","address":"123 ABC","hasCoordinates":true,"latitude":10.77,"longitude":106.7,"thumbnail":""}"#) else { Issue.record("no location"); return }
        #expect(ok.title == "Nhà" && ok.address == "123 ABC")
        #expect(ok.coordinate == .init(latitude: 10.77, longitude: 106.7))
        #expect(ok.mapURL?.absoluteString == "https://maps.google.com/maps?q=10.77,106.7&z=14&t=m")
        guard case .location(let none) = try kind(#"{"type":"location","title":"","address":"","hasCoordinates":false,"latitude":0,"longitude":0,"thumbnail":""}"#) else { return }
        #expect(none.coordinate == nil && none.mapURL == nil)
        guard case .location(let bad) = try kind(#"{"type":"location","hasCoordinates":true,"latitude":123,"longitude":0}"#) else { return }
        #expect(bad.coordinate == nil)
    }

    @Test func ecards() throws {
        guard case .ecard(let banner) = try kind(#"{"type":"ecard","style":"banner","image":"https://bg","thumb":"https://av","title":"Hi","description":"D","actions":["Gửi","Xem",""],"avatars":[],"reminderTitle":"","reminderStart":0,"reminderRepeat":""}"#) else { Issue.record("no ecard"); return }
        #expect(banner.style == .banner && banner.title == "Hi" && banner.actions == ["Gửi", "Xem"])
        #expect(banner.image == URL(string: "https://bg") && banner.reminderStart == nil)
        guard case .ecard(let friend) = try kind(#"{"type":"ecard","style":"friend","image":"","title":"Bạn và Lan đã trở thành bạn","avatars":["https://a/1","https://a/2"]}"#) else { return }
        #expect(friend.style == .friend && friend.avatars.count == 2 && friend.image == nil)
        guard case .ecard(let reminder) = try kind(#"{"type":"ecard","style":"reminder","reminderTitle":"Họp","reminderStart":1760000000000,"reminderRepeat":"Nhắc theo tuần"}"#) else { return }
        #expect(reminder.style == .reminder && reminder.reminderTitle == "Họp" && reminder.reminderRepeat == "Nhắc theo tuần")
        #expect(reminder.reminderStart == Date(timeIntervalSince1970: 1_760_000_000))
        // Unknown style falls back to the banner.
        guard case .ecard(let odd) = try kind(#"{"type":"ecard","style":"future"}"#) else { return }
        #expect(odd.style == .banner)
    }

    @Test func oaMessageDropsEmptyParts() throws {
        let k = try kind(#"{"type":"oaMessage","parts":[{"header":true,"title":"T","description":"D","thumb":"https://t","url":"https://h"},{"header":false,"title":"C","description":"","thumb":"","url":""},{"header":false,"title":"","thumb":""}]}"#)
        guard case .oaMessage(let parts) = k else { Issue.record("no oa"); return }
        #expect(parts.count == 2)
        #expect(parts[0].isHeader && parts[0].url == URL(string: "https://h") && parts[0].thumb != nil)
        #expect(!parts[1].isHeader && parts[1].url == nil)
    }

    @Test func pollStickerSetAndZInstant() throws {
        #expect(try kind(#"{"type":"poll","question":"Ăn gì?","header":"Lan đã tạo bình chọn","anonymous":false}"#) == .poll(.init(question: "Ăn gì?", header: "Lan đã tạo bình chọn")))
        #expect(try kind(#"{"type":"stickerSet","title":"Mèo","thumb":"https://s"}"#) == .stickerSet(title: "Mèo", thumbnail: URL(string: "https://s")))
        #expect(try kind(#"{"type":"zinstant","title":"","text":"Xin chào Lan","thumb":""}"#) == .zinstant(title: "", text: "Xin chào Lan", thumbnail: nil))
    }

    @Test func noticesAndPlaceholders() throws {
        #expect(try kind(#"{"type":"notice","symbol":"checklist","text":"Việc cần làm"}"#) == .notice(symbol: "checklist", text: "Việc cần làm"))
        #expect(try kind(#"{"type":"notice","text":"x"}"#) == .notice(symbol: "info.circle", text: "x"))
        #expect(try kind(#"{"type":"placeholder","symbol":"lock","text":"Đang chờ tin nhắn này"}"#) == .placeholder(symbol: "lock", text: "Đang chờ tin nhắn này"))
        #expect(ChatMessage.Kind.system.isStandalone && ChatMessage.Kind.notice(symbol: "a", text: "b").isStandalone)
        #expect(!ChatMessage.Kind.text.isStandalone)
        #expect(ChatMessage.Kind.ecard(.init()).isStandalone)
    }

    @Test func unknownAndMissingFieldsDegrade() throws {
        #expect(try kind(#"{"type":"hologram","rawType":"9999"}"#) == .unsupported(type: "9999"))
        #expect(try kind(#"{}"#) == .text)
        // Mistyped fields never crash.
        #expect(try kind(#"{"type":"link","title":5,"url":7,"thumbnail":[]}"#) == .link(title: "", url: nil, thumbnail: nil))
        guard case .ecard(let card) = try kind(#"{"type":"ecard","actions":"x","avatars":3,"reminderStart":"soon"}"#) else { return }
        #expect(card.actions.isEmpty && card.avatars.isEmpty && card.reminderStart == nil)
        #expect(try kind(#"{"type":"call","video":"yes","duration":"long"}"#) == .call(video: false, missed: false, duration: 0))
    }

    @Test func onlyWebLinksAreKept() throws {
        for bad in ["file:///etc/passwd", "smb://host/share", "javascript:alert(1)", "zalo://x", "ssh://h", "http://", "mailto:a@b.c"] {
            #expect(WebLink.sanitized(URL(string: bad)) == nil, "\(bad)")
        }
        #expect(WebLink.sanitized(URL(string: "HTTPS://Example.com/a")) != nil)
        #expect(WebLink.sanitized(URL(string: "http://example.com")) != nil)
        #expect(WebLink.sanitized(nil) == nil)
        // Parsing drops unsafe card links so the card is disabled.
        #expect(try kind(#"{"type":"link","title":"T","url":"file:///etc/passwd","thumbnail":""}"#) == .link(title: "T", url: nil, thumbnail: nil))
        guard case .oaMessage(let parts) = try kind(#"{"type":"oaMessage","parts":[{"header":true,"title":"T","url":"smb://h/x"}]}"#) else { Issue.record("no oa"); return }
        #expect(parts.first?.url == nil)
    }

    @Test func hugeNumbersDoNotTrap() throws {
        #expect(clampedInt(1e300) == 9_000_000_000_000_000)
        #expect(clampedInt(-1e300) == -9_000_000_000_000_000)
        #expect(clampedInt(.nan) == 0 && clampedInt(.infinity) == 0)
        #expect(clampedInt64(1e300) == 9_000_000_000_000_000)
        #expect(try kind(#"{"type":"call","video":false,"missed":false,"duration":1e300}"#) == .call(video: false, missed: false, duration: 9_000_000_000_000_000))
        guard case .photo(_, let w, let h) = try kind(#"{"type":"photo","url":"","width":1e300,"height":-1e300}"#) else { return }
        #expect(w > 0 && h < 0)
        guard case .file(_, let size, _) = try kind(#"{"type":"file","name":"f","size":1e300,"url":""}"#) else { return }
        #expect(size == 9_000_000_000_000_000)
        guard case .ecard(let card) = try kind(#"{"type":"ecard","reminderStart":1e300}"#) else { return }
        #expect(card.reminderStart == nil)
    }
}
