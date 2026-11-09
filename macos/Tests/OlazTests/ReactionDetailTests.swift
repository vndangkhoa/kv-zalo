import Testing
import Foundation
@testable import Olaz

@Suite struct ReactionDetailTests {
    private func icon(_ emoji: String, type: String, image: String = "") -> [String: Any] {
        ["type": type, "key": "k" + type, "emoji": emoji, "image": image]
    }

    private var json: [String: Any] {
        func entry(_ emoji: String, _ type: String, _ field: String, _ n: Int) -> [String: Any] {
            icon(emoji, type: type).merging([field: n]) { $1 }
        }
        return [
            "tabs": [entry("👍", "3", "total", 4), entry("❤️", "5", "total", 2)],
            "users": [
                ["uid": "1", "name": "Me", "avatar": "https://x/a.jpg", "isMe": true, "total": 3,
                 "icons": [entry("👍", "3", "count", 2), entry("❤️", "5", "count", 1)]],
                ["uid": "2", "name": "  ", "avatar": "", "isMe": false, "total": 3,
                 "icons": [entry("👍", "3", "count", 2), entry("❤️", "5", "count", 1)]],
                // Only a type without an icon: still listed, still counted.
                ["uid": "3", "name": "C", "avatar": "", "isMe": false, "total": 4, "icons": []],
            ],
        ]
    }

    @Test func parsesTabsAndUsers() throws {
        let detail = try #require(ReactionDetail(json: json))
        #expect(detail.tabs.map(\.icon.emoji) == ["👍", "❤️"])
        #expect(detail.tabs.map(\.icon.type) == ["3", "5"])
        #expect(detail.users.count == 3)
        #expect(detail.users[0].avatar == URL(string: "https://x/a.jpg"))
        #expect(detail.users[1].avatar == nil)
        // Blank names fall back to the official placeholder.
        #expect(detail.users[1].name == ReactionDetail.unknownName)
        // "Tất cả" counts types that have no icon.
        #expect(detail.total == 10)
        #expect(ReactionDetail(json: "nope") == nil)
        #expect(ReactionDetail(json: NSNull()) == nil)
        #expect(ReactionDetail(json: ["tabs": [], "users": []])?.isEmpty == true)
    }

    @Test func tabRowsFollowTheOfficialOrdering() throws {
        let detail = try #require(ReactionDetail(json: json))
        #expect(detail.rows(for: nil).map(\.uid) == ["1", "2", "3"])
        // "👍" tab: only people who used it, with their count of it.
        let thumbs = detail.rows(for: "3")
        #expect(thumbs.map(\.uid) == ["1", "2"])
        #expect(thumbs.allSatisfy { $0.total == 2 && $0.icons.count == 1 })
        #expect(detail.rows(for: "5").map(\.uid) == ["1", "2"])
        #expect(detail.rows(for: "99").isEmpty)
    }

    @Test func allTabShowsAtMostSixIcons() throws {
        let icons = (0..<8).map { icon("e\($0)", type: "\($0)").merging(["count": 8 - $0]) { $1 } }
        let detail = try #require(ReactionDetail(json: ["tabs": [], "users": [["uid": "1", "name": "A", "isMe": false, "total": 36, "icons": icons]]]))
        #expect(detail.rows(for: nil)[0].icons.map(\.icon.emoji) == (0..<6).map { "e\($0)" })
    }

    @Test func iconParsing() {
        // Image path and key survive; groups use the type, not the emoji.
        let parsed = ReactionIcon(json: ["type": "34", "key": "/-weak", "emoji": "/-weak", "image": "emoji/emoji-32/row-35-column-43.png"])
        #expect(parsed?.image == "emoji/emoji-32/row-35-column-43.png")
        #expect(parsed?.id == "34")
        // Nothing to show at all → not an icon.
        #expect(ReactionIcon(json: ["type": "9", "key": "", "emoji": ""]) == nil)
        // No emoji: the icon text is shown.
        #expect(ReactionIcon(json: ["type": "9", "key": ":d"])?.emoji == ":d")
    }

    @Test func shortCountMatchesOfficialLabels() {
        #expect(ReactionDetail.shortCount(0) == "0")
        #expect(ReactionDetail.shortCount(999) == "999")
        #expect(ReactionDetail.shortCount(1000) == "1k")
        #expect(ReactionDetail.shortCount(1001) == "1k+")
        #expect(ReactionDetail.shortCount(9000) == "9k")
        #expect(ReactionDetail.shortCount(9999) == "9k+")
        #expect(ReactionDetail.shortCount(10000) == "9k+")
        #expect(ReactionDetail.shortCount(25_000) == "9k+")
    }

    @Test func tooltipListsUpToThreeNames() {
        func chip(_ names: [String], people: Int) -> ChatMessage.Reaction {
            ChatMessage.Reaction(icon: ReactionIcon(type: "3", emoji: "👍"), count: people, includesMe: false, names: names, people: people)
        }
        #expect(chip([], people: 1).tooltip == nil || chip([], people: 1).tooltip == "1 người")
        #expect(chip([], people: 0).tooltip == nil)
        #expect(chip(["A"], people: 1).tooltip == "A")
        #expect(chip(["Bạn", "B"], people: 2).tooltip == "Bạn và B")
        #expect(chip(["A", "B", "C"], people: 3).tooltip == "A, B và C")
        #expect(chip(["A", "B", "C"], people: 5).tooltip == "A, B, C và 2 người khác")
        #expect(chip(["A"], people: 3).tooltip == "A và 2 người khác")
        #expect(chip([], people: 2).tooltip == "2 người")
    }
}
