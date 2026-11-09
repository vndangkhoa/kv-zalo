import Foundation
import SwiftUI

// Conversation labels ("Phân loại"), list filters, stranger folder, My Documents id.
// Bridged to `HostScripts/conversations-adapter.js` (`window.__zaloConversations`; posts
// {page:'main', conversationExtras:{…}}). The filters, the label manager and the stranger folder are the
// official client's own (ConvListController, `_label`), see the adapter header for the entry points.

/// Official `FilterType` values the sidebar can request.
enum ConversationTypeFilter: Int, CaseIterable, Identifiable, Sendable {
    case all = 0, unread = 1, focused = 3, other = 4

    var id: Int { rawValue }
    var title: String {
        switch self {
        // The official default tab is "Tất cả" / "Ưu tiên"; Olaz always calls that list "Tin nhắn".
        case .all, .focused: ConversationExtras.inboxTitle
        case .unread: "Chưa đọc"
        case .other: "Khác"
        }
    }
}

struct ConversationExtras: Equatable {
    struct Label: Identifiable, Equatable, Sendable {
        let id: String
        let name: String
        /// Hex string as stored by the official client ("#0068ff").
        let colorHex: String
        let convIds: [String]
        var color: Color { Color(hex: colorHex) ?? ZaloTheme.blue }
    }

    struct Limits: Equatable, Sendable {
        var maxLabels = 15
        var maxNameLength = 30
        var palette: [String] = ["#D91B1B", "#F31BC8", "#FF6905", "#FAC000", "#4BC377", "#2AC5BB", "#0068FF", "#6F3FCF"]
    }

    /// The pseudo-row the official list shows for strangers I have not answered (`stranger_box`).
    struct StrangerBox: Equatable, Sendable {
        var chats = 0
        var unreadChats = 0
        var unreadMessages = 0
        var newestId: String?
    }

    /// Result of the latest label action (create / rename / delete / assign).
    struct Notice: Equatable, Sendable {
        let seq: Int
        let ok: Bool
        let text: String
    }

    /// What Olaz's own UI calls the default list (sidebar row, column title, menus), whatever the official tab is named.
    static let inboxTitle = "Tin nhắn"

    /// Official id of the list pseudo-row for the stranger folder.
    static let strangerBoxId = "stranger_box"

    /// True once the official controller has published at least once.
    var loaded = false
    var labels: [Label] = []
    var limits = Limits()
    var archivedEnabled = false
    /// The controller's current filter (echo of what Swift asked for).
    var typeFilter: Int = 0
    var labelFilterIds: [String] = []
    /// Ids of the list the controller computed for the current filter, in display order (may contain `stranger_box`).
    var listIds: [String] = []
    /// Every conversation with a non-friend.
    var strangerIds: Set<String> = []
    /// Folder contents: strangers I have not answered, newest first.
    var strangerFolderIds: [String] = []
    var strangerBox = StrangerBox()
    var unboxStranger = false
    var notice: Notice?

    /// conversation id → label id (a conversation has at most one label).
    var labelByConversation: [String: String] = [:]

    func label(of conversationId: String) -> Label? {
        guard let id = labelByConversation[conversationId] else { return nil }
        return labels.first { $0.id == id }
    }

    /// The label `destination` filters the inbox by; nil for every other destination and for a label that no longer exists.
    func label(filteredBy destination: SidebarDestination) -> Label? {
        guard case .label(let id) = destination else { return nil }
        return labels.first { $0.id == id }
    }

    var tabs: [ConversationTypeFilter] { archivedEnabled ? [.focused, .other, .unread] : [.all, .unread] }
    var defaultTab: ConversationTypeFilter { archivedEnabled ? .focused : .all }
}

extension Color {
    /// "#RRGGBB" / "RRGGBB".
    init?(hex: String) {
        var text = hex.trimmingCharacters(in: .whitespaces)
        if text.hasPrefix("#") { text.removeFirst() }
        guard text.count == 6, let value = UInt32(text, radix: 16) else { return nil }
        self.init(red: Double((value >> 16) & 0xFF) / 255, green: Double((value >> 8) & 0xFF) / 255, blue: Double(value & 0xFF) / 255)
    }
}

extension ZaloEngine {
    // MARK: Actions

    /// Drives the official ConvListController filter (type + label ids); the resulting list comes back as `listIds`.
    func setConversationFilter(_ type: ConversationTypeFilter, labelIds: [String] = []) {
        conversationExtras.typeFilter = type.rawValue
        conversationExtras.labelFilterIds = labelIds
        callChat("setFilter", type.rawValue, labelIds, object: "__zaloConversations")
    }

    /// Assigns the conversation to `labelId`, or removes its label when nil (official label manager).
    func setConversationLabel(_ conversationId: String, labelId: String?) {
        // Optimistic: the official client confirms through the next snapshot.
        var next = conversationExtras
        let previous = next.labelByConversation[conversationId]
        next.labelByConversation[conversationId] = labelId
        if previous != labelId { conversationExtras = next }
        callChat("setConvLabel", conversationId, labelId, object: "__zaloConversations")
    }

    func createConversationLabel(name: String, colorHex: String, conversationIds: [String] = []) {
        callChat("createLabel", name, colorHex, conversationIds, object: "__zaloConversations")
    }

    func updateConversationLabel(_ id: String, name: String, colorHex: String) {
        callChat("updateLabel", id, name, colorHex, object: "__zaloConversations")
    }

    func deleteConversationLabel(_ id: String) {
        callChat("deleteLabel", id, object: "__zaloConversations")
    }

    func reorderConversationLabels(_ ids: [String]) {
        callChat("reorderLabels", ids, object: "__zaloConversations")
    }

    // MARK: State

    func handleConversationExtras(state: [String: Any]) {
        guard let json = state["conversationExtras"] as? [String: Any] else { return }
        var next = conversationExtras
        next.loaded = true

        if let id = json["selfConversationId"] as? String, !id.isEmpty, id != selfConversationId { selfConversationId = id }

        if let rows = json["labels"] as? [[String: Any]] {
            next.labels = rows.compactMap { row in
                guard let id = row["id"] as? String else { return nil }
                return ConversationExtras.Label(
                    id: id, name: row["name"] as? String ?? "", colorHex: row["color"] as? String ?? "",
                    convIds: row["convIds"] as? [String] ?? [])
            }
            var map: [String: String] = [:]
            for label in next.labels { for conv in label.convIds { map[conv] = label.id } }
            next.labelByConversation = map
        }
        if let limits = json["limits"] as? [String: Any] {
            var l = next.limits
            if let n = (limits["maxLabels"] as? NSNumber)?.intValue, n > 0 { l.maxLabels = n }
            if let n = (limits["maxNameLength"] as? NSNumber)?.intValue, n > 0 { l.maxNameLength = n }
            if let p = limits["palette"] as? [String], !p.isEmpty { l.palette = p }
            next.limits = l
        }
        if let flag = json["archivedEnabled"] as? Bool { next.archivedEnabled = flag }
        if let filter = json["filter"] as? [String: Any] {
            next.typeFilter = (filter["type"] as? NSNumber)?.intValue ?? next.typeFilter
            next.labelFilterIds = filter["labelIds"] as? [String] ?? []
        }
        if let ids = json["listIds"] as? [String] { next.listIds = ids }
        if let ids = json["strangerIds"] as? [String] { next.strangerIds = Set(ids) }
        if let ids = json["strangerFolderIds"] as? [String] { next.strangerFolderIds = ids }
        if let box = json["strangerBox"] as? [String: Any] {
            next.strangerBox = ConversationExtras.StrangerBox(
                chats: (box["chats"] as? NSNumber)?.intValue ?? 0,
                unreadChats: (box["unreadChats"] as? NSNumber)?.intValue ?? 0,
                unreadMessages: (box["unreadMessages"] as? NSNumber)?.intValue ?? 0,
                newestId: (box["newestId"] as? String).flatMap { $0.isEmpty ? nil : $0 })
        }
        if let flag = json["unboxStranger"] as? Bool { next.unboxStranger = flag }
        if let action = json["action"] as? [String: Any], let seq = (action["seq"] as? NSNumber)?.intValue {
            next.notice = ConversationExtras.Notice(seq: seq, ok: action["ok"] as? Bool ?? false, text: action["text"] as? String ?? "")
        }
        if next != conversationExtras { conversationExtras = next }
    }
}
