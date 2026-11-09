import AppKit
import Foundation

// Group calls: the participant picker. The official renderer opens its GROUP_CALL modal (invisible here) for two
// things — the chat header's video button on a group, and the native call helper's "add participant" request.
// HostScripts/call-adapter.js hands both to the app as `{kind:'picker'}` and runs the modal's own `onConfirm` /
// `onCancel` when the user answers (`pickerConfirm` → `callHelper.makeCall(ids, isVideo, true, …)` for a new call,
// `callHelper.sendAddUserResponse(groupId, callees)` for an add-participant request).
//
// The list, the limit and the rules are the renderer's (see the adapter); this file only models and answers.

/// What the renderer's GROUP_CALL modal would show.
struct GroupCallPicker: Equatable, Identifiable {
    struct Member: Equatable, Identifiable {
        let id: String
        var name: String
        var avatar: URL?
    }

    /// Request token (single use).
    let id: String
    /// The helper asked to add people to a running call (instead of starting one).
    var adding: Bool
    /// nil: unknown (add-participant: the renderer does not know whether the running call has video).
    var video: Bool?
    var groupName: String
    var groupAvatar: URL?
    /// Official limit: `maxUsers - 1` (10 when the server sends no `maxUsers`).
    var maxSelectable: Int
    /// Already in the call (add-participant): shown ticked and disabled, not counted.
    var existing: Set<String>
    /// Group-privacy notice of the modal header ("Bạn chỉ có thể gọi cho trưởng/phó nhóm.").
    var note: String?
    var members: [Member]

    var title: String { adding ? "Thêm người vào cuộc gọi" : "Tạo cuộc gọi" }
    var confirmTitle: String { adding ? "Thêm" : "Gọi" }
    var searchPlaceholder: String { "Tìm kiếm thành viên" }

    init(id: String, adding: Bool, video: Bool?, groupName: String, groupAvatar: URL? = nil, maxSelectable: Int,
         existing: Set<String> = [], note: String? = nil, members: [Member]) {
        self.id = id
        self.adding = adding
        self.video = video
        self.groupName = groupName
        self.groupAvatar = groupAvatar
        self.maxSelectable = maxSelectable
        self.existing = existing
        self.note = note
        self.members = members
    }

    /// `{kind:'picker', token, add, video, group:{id,name,avatar}, max, existing, note, members:[{id,name,avatar}]}`.
    init?(json: [String: Any]) {
        guard let token = json["token"] as? String, !token.isEmpty else { return nil }
        let group = json["group"] as? [String: Any] ?? [:]
        let rows = (json["members"] as? [[String: Any]] ?? []).compactMap { row -> Member? in
            guard let id = row["id"] as? String, !id.isEmpty else { return nil }
            let name = (row["name"] as? String ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return Member(id: id, name: name.isEmpty ? "Người dùng Zalo" : name,
                          avatar: (row["avatar"] as? String).flatMap { $0.isEmpty ? nil : URL(string: $0) })
        }
        let name = (group["name"] as? String ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        self.init(id: token,
                  adding: json["add"] as? Bool ?? false,
                  video: json["video"] as? Bool,
                  groupName: name.isEmpty ? "Nhóm" : name,
                  groupAvatar: (group["avatar"] as? String).flatMap { $0.isEmpty ? nil : URL(string: $0) },
                  maxSelectable: max(0, (json["max"] as? NSNumber)?.intValue ?? 10),
                  existing: Set((json["existing"] as? [Any] ?? []).compactMap { $0 as? String }),
                  note: (json["note"] as? String).flatMap { $0.isEmpty ? nil : $0 },
                  members: rows)
    }

    /// Official over-limit toast (`STR_MAX_1_MEM_INVITE_TOAST` / `STR_MAX_MEM_INVITE_TOAST`, same text).
    var limitMessage: String { "Bạn chỉ có thể mời \(maxSelectable) người khác" }

    /// Members whose name contains `query` (case / diacritic insensitive; Vietnamese `đ` folds to `d`).
    func members(matching query: String) -> [Member] {
        let needle = Self.fold(query)
        guard !needle.isEmpty else { return members }
        return members.filter { Self.fold($0.name).contains(needle) }
    }

    static func fold(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "đ", with: "d").replacingOccurrences(of: "Đ", with: "d")
            .folding(options: [.caseInsensitive, .diacriticInsensitive], locale: Locale(identifier: "vi"))
    }

    /// The modal's `onItemClick`: a new item beyond `max_mem` is refused (the toast shows), removing always works.
    /// Members already in the call and ids that are not in the list never change the selection.
    func toggled(_ memberId: String, in selection: [String]) -> (selection: [String], overLimit: Bool) {
        guard !existing.contains(memberId), members.contains(where: { $0.id == memberId }) else { return (selection, false) }
        if let index = selection.firstIndex(of: memberId) {
            var next = selection
            next.remove(at: index)
            return (next, false)
        }
        if selection.count >= maxSelectable { return (selection, true) }
        return (selection + [memberId], false)
    }
}

extension ZaloEngine {
    /// The user confirmed: runs the modal's `onConfirm` in the renderer (starts the call / answers the helper).
    func confirmGroupPicker(_ picker: GroupCallPicker, selected: [String]) {
        guard call.picker?.id == picker.id else { return }
        call.picker = nil
        let ids = selected.filter { id in picker.members.contains { $0.id == id } && !picker.existing.contains(id) }
        guard !ids.isEmpty, ids.count <= picker.maxSelectable else {
            Task { _ = await self.pickerCall("pickerCancel", picker.id) }
            return
        }
        Task { [weak self] in
            guard let self else { return }
            guard let result = await self.pickerCall("pickerConfirm", picker.id, ids) as? String,
                  !["started", "sent", "cancelled", "missing"].contains(result) else { return }
            if result == "stale" {
                self.call.notice = CallState.Notice(title: "Chưa gọi được",
                                                    message: "Yêu cầu đã hết hạn (cuộc gọi đã kết thúc hoặc có yêu cầu mới). Hãy thử lại.")
            } else if let message = Self.callFailureMessage(result, group: true) {
                self.call.notice = CallState.Notice(title: "Chưa gọi được", message: message)
            }
        }
    }

    /// Cancel / dismiss: `onCancel` — an add-participant request is answered with nobody.
    func cancelGroupPicker(_ picker: GroupCallPicker) {
        guard call.picker?.id == picker.id else { return }
        call.picker = nil
        Task { _ = await self.pickerCall("pickerCancel", picker.id) }
    }

    private func pickerCall(_ method: String, _ args: Any...) async -> Any? {
        guard let data = try? JSONSerialization.data(withJSONObject: args, options: [.fragmentsAllowed]),
              let json = String(data: data, encoding: .utf8) else { return nil }
        let script = "const c = window.__zaloCall; if (!c || !c.\(method)) return 'missing'; return await c.\(method)(...\(json));"
        let result = await host?.evaluate(script)
        if let text = result as? String, text == "missing" { EngineLog.shared.write("call \(method) → missing") }
        return result
    }
}
