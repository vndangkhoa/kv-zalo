import AppKit
import Foundation

// Card buttons (e-card actions, suggested stickers, contact card, info-message links, reminder cards), bridged to
// `HostScripts/card-actions-adapter.js` (`window.__zaloChat.performAction` & co.; contract in ChatModels.swift).
// Every call runs only after a click or when a card is shown; nothing here is fired on its own. Message content never goes to the log.
extension ZaloEngine {
    /// Runs the official handler of the card button `ref` on message `messageId` and returns what happened.
    /// Effects that need app UI (open a chat / link, copy) are applied here; opening a profile is left to the caller.
    func performCardAction(in conversationId: String, message messageId: String, ref: String) async -> CardActionOutcome {
        // The JS side always answers within ~40 s (card-actions-adapter.js `guarded`): a run that is slower is reported as "pending", it keeps
        // its slot there and a retry is refused until it settles. So the button stays busy until JS really answers; this cap (a dead page)
        // must never invite a retry that could send twice, hence its wording.
        let answer = await raceTimeout(90) { [self] in JSBox(value: await callCard("performAction", [conversationId, messageId, ref])) }
        guard let answer else { return .pending(CardActionOutcome.noAnswerText) }
        let outcome = CardActionOutcome(json: answer.value)
        // The answer can arrive after the user moved to another conversation: effects then belong to a screen that is gone and are dropped.
        if openConversationId != conversationId {
            switch outcome {
            case .openChat, .openURL, .copy, .present(.tab):
                EngineLog.shared.write("card action \(ref.split(separator: ":").first.map(String.init) ?? "-") → dropped (conversation changed)")
                return .done
            default: break
            }
        }
        switch outcome {
        case .openChat(let id): openContactConversation(id)
        case .openURL(let url): WebLink.open(url)
        case .copy(let text):
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
        case .present(.tab(let target)): SectionRouter.open(target)
        case .failed, .locked, .unsupported, .pending: EngineLog.shared.write("card action \(ref.split(separator: ":").first.map(String.init) ?? "-") → \(outcome.logName)")
        default: break
        }
        return outcome
    }

    /// Logout: forget group previews and in-flight slots of the previous session (best effort).
    func resetCardState() {
        Task { _ = await callCard("cardReset", []) }
    }

    /// The stickers an e-card suggests (official `fetchStickersFromList` over the card's `stickerList`).
    func suggestedStickers(in conversationId: String, message messageId: String) async -> [SuggestedSticker] {
        SuggestedSticker.list(json: await callCard("cardStickers", [conversationId, messageId]))
    }

    /// Local friend data for a contact card: is it me, is it a friend (no network).
    func contactCardState(_ uid: String) async -> (isSelf: Bool, isFriend: Bool) {
        let root = await callCard("contactState", [uid]) as? [String: Any] ?? [:]
        return ((root["isSelf"] as? NSNumber)?.boolValue ?? false, (root["isFriend"] as? NSNumber)?.boolValue ?? false)
    }

    func contactProfile(_ uid: String) async -> ContactProfile? {
        ContactProfile(json: await callCard("contactProfile", [uid]))
    }

    /// The official "KẾT BẠN" steps before the add-friend form (status check, request source, default message). Sends nothing.
    /// `via`: how the form was reached — see `FriendRequestOrigin`.
    func prepareFriendRequest(to uid: String, from conversationId: String, via: FriendRequestOrigin) async -> FriendRequestPrep {
        let answer = await raceTimeout(20) { [self] in JSBox(value: await callCard("prepareFriendRequest", [uid, conversationId, via.rawValue])) }
        return FriendRequestPrep(json: answer?.value)
    }

    /// Sends the friend request the user confirmed (official `requestAddFriend` through the contacts adapter) and waits for its result.
    /// Never reports success without the adapter's answer. The adapter answers within ~60 s; `pending` means the request is still
    /// in flight there (a retry is refused until it settles), so the caller must not offer to send again.
    func submitFriendRequest(to uid: String, message: String) async -> (ok: Bool, text: String, pending: Bool) {
        let args = [uid, message] as [Any]
        guard let data = try? JSONSerialization.data(withJSONObject: args, options: [.fragmentsAllowed]),
              let json = String(data: data, encoding: .utf8) else { return (false, "Không thể gửi lời mời kết bạn.", false) }
        let script = "const c = window.__zaloContacts; if (!c || !c.sendRequest) return 'missing'; return await c.sendRequest(...\(json));"
        let answer = await raceTimeout(90) { [self] in JSBox(value: await host?.evaluate(script)) }
        guard let answer else { return (false, "Chưa nhận được phản hồi. Hãy kiểm tra mục “Đã gửi” trong Lời mời kết bạn trước khi thử lại.", true) }
        guard let root = answer.value as? [String: Any], let ok = root["ok"] as? Bool else {
            return (false, "Chưa sẵn sàng gửi lời mời kết bạn, hãy thử lại sau giây lát.", false)
        }
        return (ok, root["text"] as? String ?? (ok ? "Đã gửi lời mời kết bạn." : "Không thể gửi lời mời kết bạn."), (root["pending"] as? Bool) ?? false)
    }

    // MARK: Native sheets of card actions (alias, group invite, sticker category)

    /// Sets (or, with an empty name, removes) the alias of a person after the user confirmed the dialog (official `setAliasName`).
    func saveAlias(uid: String, name: String) async -> (ok: Bool, text: String) {
        let answer = await raceTimeout(90) { [self] in JSBox(value: await callCard("aliasSave", [uid, name])) }
        guard let root = answer?.value as? [String: Any], let ok = root["ok"] as? Bool else {
            return (false, "Chưa nhận được phản hồi. Hãy kiểm tra lại tên gợi nhớ trước khi thử lại.")
        }
        return (ok, String((root["text"] as? String ?? "").prefix(300)))
    }

    /// The group an invite link points at (official `getGroupInfoByLink`; a read, nothing is joined).
    func groupInvitePreview(_ url: URL) async -> GroupInvitePreview {
        let answer = await raceTimeout(60) { [self] in JSBox(value: await callCard("groupPreview", [url.absoluteString])) }
        return GroupInvitePreview(json: answer?.value)
    }

    /// "Tham gia": joins through the official group manager. Only called from the button.
    func joinGroup(_ url: URL) async -> GroupJoinResult {
        let answer = await raceTimeout(90) { [self] in JSBox(value: await callCard("groupJoin", [url.absoluteString])) }
        guard let answer else { return GroupJoinResult(state: .error, text: "Chưa nhận được phản hồi. Hãy kiểm tra danh sách nhóm trước khi thử lại.") }
        return GroupJoinResult(json: answer.value)
    }

    /// Category info and sticker list from the official sticker manager; nil when unavailable.
    func stickerCategory(_ id: Int) async -> StickerCategory? {
        let answer = await raceTimeout(60) { [self] in JSBox(value: await callCard("stickerCategory", [id])) }
        return StickerCategory(json: answer?.value)
    }

    /// Sends one sticker of the category to `conversationId` (the user clicked it); same safe send path as the e-card stickers.
    func sendCategorySticker(in conversationId: String, category: Int, sticker: Int) async -> CardActionOutcome {
        let answer = await raceTimeout(90) { [self] in JSBox(value: await callCard("stickerCategorySend", [conversationId, category, sticker])) }
        guard let answer else { return .pending(CardActionOutcome.noAnswerText) }
        return CardActionOutcome(json: answer.value)
    }

    /// Reminder card state from the official reminder controller. Returns when the state differs from `signature`
    /// (or after ~25 s with the unchanged state); nil when the controller is not available.
    func reminderWatch(in conversationId: String, message messageId: String, signature: String) async -> ReminderSnapshot? {
        ReminderSnapshot(json: await callCard("reminderWatch", [conversationId, messageId, signature]))
    }

    /// RSVP to a reminder (official controller `respond`); only called from a click.
    func respondToReminder(in conversationId: String, message messageId: String, accept: Bool) async -> (ok: Bool, text: String, snapshot: ReminderSnapshot?) {
        let answer = await raceTimeout(40) { [self] in JSBox(value: await callCard("reminderRespond", [conversationId, messageId, accept ? "accept" : "reject"])) }
        // No answer is not a failure: the answer may still land (the page keeps the reminder "pending" until it settles).
        guard let root = answer?.value as? [String: Any] else { return (false, "Đang xử lý, hãy kiểm tra lại sau.", nil) }
        let ok = root["ok"] as? Bool ?? false
        return (ok, root["text"] as? String ?? "", ReminderSnapshot(json: root["snapshot"]))
    }

    /// Who accepted / declined a group reminder (official responders page: reminder-detail-service `listResponders`, a read).
    /// Only called when the user opens the list. nil = could not be read.
    func reminderResponders(in conversationId: String, message messageId: String) async -> ReminderResponders? {
        let answer = await raceTimeout(40) { [self] in JSBox(value: await callCard("reminderResponders", [conversationId, messageId])) }
        return ReminderResponders(json: answer?.value)
    }

    private func callCard(_ method: String, _ args: [Any]) async -> Any? {
        guard let data = try? JSONSerialization.data(withJSONObject: args, options: [.fragmentsAllowed]),
              let json = String(data: data, encoding: .utf8) else { return nil }
        let script = "const c = window.__zaloChat; if (!c || !c.\(method)) return null; return await c.\(method)(...\(json));"
        return await host?.evaluate(script)
    }
}

/// How the add-friend form was reached; it decides which request source the official client records (card-actions-adapter.js).
enum FriendRequestOrigin: String, Sendable {
    /// The contact card's KẾT BẠN (`_onAddFriend`).
    case add
    /// The profile opened by tapping the card (`_showUserInfo` with the e-card source).
    case card
    /// A profile action (`action.open.profile`): no source is recorded.
    case none
}

/// A JSON value from the page, handed back from a timed call (read once on the main actor).
struct JSBox: @unchecked Sendable { let value: Any? }

/// Result of a call that may never answer; nil = the time ran out (the late answer is dropped).
private final class OnceGate<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<T?, Never>?
    init(_ continuation: CheckedContinuation<T?, Never>) { self.continuation = continuation }
    func resume(_ value: T?) {
        lock.lock()
        let next = continuation
        continuation = nil
        lock.unlock()
        next?.resume(returning: value)
    }
}

/// Runs `operation`; if it has not answered after `seconds`, returns nil and ignores its late answer.
@MainActor
func raceTimeout<T: Sendable>(_ seconds: Double, _ operation: @escaping @MainActor () async -> T) async -> T? {
    await withCheckedContinuation { (continuation: CheckedContinuation<T?, Never>) in
        let gate = OnceGate(continuation)
        Task { @MainActor in gate.resume(await operation()) }
        Task { try? await Task.sleep(for: .seconds(seconds)); gate.resume(nil) }
    }
}

extension CardActionOutcome {
    /// Status word for the log (never the message text).
    var logName: String {
        switch self {
        case .done: "done"
        case .openChat: "openChat"
        case .openProfile: "openProfile"
        case .openURL: "openURL"
        case .copy: "copy"
        case .present: "present"
        case .unsupported: "unsupported"
        case .locked: "locked"
        case .failed: "failed"
        case .pending: "pending"
        }
    }
}
