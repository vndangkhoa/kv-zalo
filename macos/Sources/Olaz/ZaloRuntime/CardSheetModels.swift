import Foundation

// Models of the card actions that open a native sheet (alias, group invite, sticker category, poll, side-bar tabs).
// The JS side (card-actions-adapter.js) answers with plain JSON; everything is validated here before it reaches a view.

/// Official `groupLinkRegEx`: `^https?://(zaloapp.com|zalo.me)/g/`.
enum GroupInviteLink {
    static func isInvite(_ url: URL?) -> Bool {
        guard let url = WebLink.sanitized(url), let host = url.host?.lowercased(), host == "zalo.me" || host == "zaloapp.com" else { return false }
        return url.path.hasPrefix("/g/")
    }
}

/// Where a side-bar action (`action.open.tab.chat`, `.tab.contact`, `.grouplist`) goes.
enum SidebarTarget: String, Sendable {
    case chats, contacts, groups
}

/// The pre-filled "Đặt tên gợi nhớ" dialog (official EDIT_ALIAS).
struct AliasDraft: Equatable, Sendable {
    var uid: String
    var name: String
    /// The card's `suggestName` (the dialog's initial text).
    var suggest: String
    var placeholder: String
    /// Official input limit: `group_config.max_group_name_length` (UTF-16 units).
    var max: Int
    /// Whether an alias is set now (nil = unknown). An empty confirm removes it.
    var hasAlias: Bool?

    static let fallbackMax = 50

    /// Control characters 0-7 are stripped, as the official `stripInvalidChar` does.
    static func sanitized(_ text: String) -> String {
        String(String.UnicodeScalarView(text.unicodeScalars.filter { $0.value > 7 }))
    }
}

/// What a card button asks the app to show.
enum CardSheet: Equatable, Sendable {
    case alias(AliasDraft)
    case groupInvite(URL)
    case stickers(Int)
    case poll(String)
    case tab(SidebarTarget)

    /// Parses an `effect` object; nil when it is not a sheet effect or is malformed.
    init?(effect: [String: Any]) {
        func digits(_ value: Any?) -> String? {
            let text = (value as? String) ?? (value as? NSNumber).map { String($0.int64Value) } ?? ""
            guard !text.isEmpty, text.count <= 24, text.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
            return text
        }
        func clip(_ value: Any?, _ limit: Int) -> String { String((value as? String ?? "").prefix(limit)) }
        switch effect["type"] as? String {
        case "alias":
            guard let uid = digits(effect["uid"]) else { return nil }
            let limit = (effect["max"] as? NSNumber).map { clampedInt($0.doubleValue) } ?? AliasDraft.fallbackMax
            let max = limit > 0 ? min(limit, 200) : AliasDraft.fallbackMax
            self = .alias(AliasDraft(uid: uid, name: clip(effect["name"], 200), suggest: AliasDraft.sanitized(clip(effect["suggest"], 400)),
                                     placeholder: clip(effect["placeholder"], 200), max: max,
                                     hasAlias: (effect["hasAlias"] as? NSNumber)?.boolValue))
        case "groupInvite":
            guard let url = (effect["url"] as? String).flatMap({ URL(string: $0) }), GroupInviteLink.isInvite(url) else { return nil }
            self = .groupInvite(url)
        case "stickers":
            guard let text = digits(effect["cateId"]), let id = Int(text), id > 0 else { return nil }
            self = .stickers(id)
        case "poll":
            guard let id = digits(effect["pollId"]) else { return nil }
            self = .poll(id)
        case "tab":
            guard let target = SidebarTarget(rawValue: effect["tab"] as? String ?? "") else { return nil }
            self = .tab(target)
        default:
            return nil
        }
    }
}

/// The group an invite link points at (official `getGroupInfoByLink`).
struct GroupInvitePreview: Equatable, Sendable {
    enum State: Sendable { case preview, member, error }

    struct Member: Identifiable, Equatable, Sendable {
        var id: String
        var name: String
        var avatar: URL?
        var admin: Bool
    }

    var state: State = .error
    /// Conversation id with its "g" prefix.
    var groupId = ""
    var name = ""
    var avatar: URL?
    var totalMember = 0
    var desc = ""
    var community = false
    var creatorName = ""
    var members: [Member] = []
    var text = ""

    init() {}

    init(json: Any?) {
        let root = json as? [String: Any] ?? [:]
        text = String((root["text"] as? String ?? "").prefix(300))
        let group = (root["groupId"] as? String ?? "")
        let validGroup = group.count > 1 && group.count <= 25 && group.hasPrefix("g") && group.dropFirst().allSatisfy { $0.isASCII && $0.isNumber }
        switch root["state"] as? String {
        case "member" where validGroup:
            state = .member
            groupId = group
        case "preview" where validGroup:
            state = .preview
            groupId = group
            name = String((root["name"] as? String ?? "").prefix(200))
            avatar = WebLink.sanitized((root["avatar"] as? String).flatMap { URL(string: $0) })
            totalMember = clampedInt((root["totalMember"] as? NSNumber)?.doubleValue ?? 0)
            desc = String((root["desc"] as? String ?? "").prefix(2000))
            community = (root["community"] as? NSNumber)?.boolValue ?? false
            creatorName = String((root["creatorName"] as? String ?? "").prefix(200))
            members = (root["members"] as? [[String: Any]] ?? []).prefix(12).compactMap { row in
                guard let id = row["id"] as? String, !id.isEmpty else { return nil }
                return Member(id: id, name: String((row["name"] as? String ?? "").prefix(200)),
                              avatar: WebLink.sanitized((row["avatar"] as? String).flatMap { URL(string: $0) }),
                              admin: (row["admin"] as? NSNumber)?.boolValue ?? false)
            }
        default:
            state = .error
            if text.isEmpty { text = "Đã có lỗi xảy ra. Vui lòng thử lại" }
        }
    }
}

/// Result of "Tham gia" on a group invite.
struct GroupJoinResult: Equatable, Sendable {
    enum State: Sendable { case joined, pending, already, error }
    var state: State = .error
    var groupId = ""
    var text = ""

    init(state: State = .error, groupId: String = "", text: String = "") {
        self.state = state; self.groupId = groupId; self.text = text
    }

    init(json: Any?) {
        let root = json as? [String: Any] ?? [:]
        let group = root["groupId"] as? String ?? ""
        let validGroup = group.count > 1 && group.count <= 25 && group.hasPrefix("g") && group.dropFirst().allSatisfy { $0.isASCII && $0.isNumber }
        let text = String((root["text"] as? String ?? "").prefix(300))
        switch root["state"] as? String {
        case "joined" where validGroup: self.init(state: .joined, groupId: group, text: text)
        case "already" where validGroup: self.init(state: .already, groupId: group, text: text)
        case "pending": self.init(state: .pending, text: text)
        default: self.init(state: .error, text: text.isEmpty ? "Vào nhóm không thành công." : text)
        }
    }
}

/// The stickers of one category (official sticker manager).
struct StickerCategory: Equatable, Sendable {
    struct Item: Identifiable, Equatable, Sendable {
        /// `<cateId>_<stickerId>`
        var ref: String
        var url: URL
        var id: String { ref }
        var stickerId: Int { Int(ref.split(separator: "_").last ?? "") ?? 0 }
    }

    var id: Int
    var name: String
    var icon: URL?
    var stickers: [Item]

    init?(json: Any?) {
        guard let root = json as? [String: Any], let id = (root["id"] as? NSNumber).map({ clampedInt($0.doubleValue) }), id > 0 else { return nil }
        let items = (root["stickers"] as? [[String: Any]] ?? []).prefix(300).compactMap { row -> Item? in
            guard let ref = row["ref"] as? String, ref.hasPrefix("\(id)_"), Int(ref.dropFirst("\(id)_".count)) != nil,
                  let url = WebLink.sanitized((row["url"] as? String).flatMap { URL(string: $0) }) else { return nil }
            return Item(ref: ref, url: url)
        }
        guard !items.isEmpty else { return nil }
        self.id = id
        name = String((root["name"] as? String ?? "").prefix(200))
        icon = WebLink.sanitized((root["icon"] as? String).flatMap { URL(string: $0) })
        stickers = items
    }
}

/// Hand-off of side-bar actions to the main window (MainShellView / ContactsView listen).
@MainActor enum SectionRouter {
    static let notification = Notification.Name("olaz.openSection")
    /// Set when the contacts screen is about to be created on its "groups" list.
    static var wantsGroupList = false

    static func open(_ target: SidebarTarget) {
        wantsGroupList = target == .groups
        NotificationCenter.default.post(name: notification, object: target.rawValue)
    }
}
