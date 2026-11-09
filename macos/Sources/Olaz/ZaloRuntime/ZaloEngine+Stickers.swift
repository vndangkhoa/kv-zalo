import Foundation

// Stickers: the picker of the composer and the suggestions while typing, bridged to `HostScripts/sticker-adapter.js`
// (`window.__zaloSticker`; the official entry points are listed in that file's header). Every call goes to the official sticker services.
// Sending runs only from a click on a sticker (picker or suggestion); nothing else here writes. Typed text goes to the JS side only when the
// "Gợi ý Sticker" setting is on, and never for end-to-end encrypted chats. No ids, names or content go to the log.
//
// Swift → JS (all async, see the adapter header for the shapes):
//   categories() → [{id, name, icon, count}]   category(id) → {id, stickers:[{cateId, id, url}]}   search(text) → [Sticker]
//   suggest(convId, text) → [Sticker]   send(convId, {cateId, id}, source) → {status, text?}   suggestSetting() / setSuggestSetting(on)

/// Where a sticker animates from, when it has an animated source (sticker-adapter.js `animOf`): a sprite sheet (`frames` pictures of the
/// sticker side by side, `frameMs` each) or an animated image (a GIF, or the animated WebP of `ext === WEBP` stickers). The still picture is
/// `StickerEntry.url`.
enum StickerAnimation: Hashable, Sendable {
    case sprite(url: URL, frames: Int, frameMs: Int)
    case animated(url: URL)

    init?(json: Any?) {
        guard let row = json as? [String: Any], let url = WebLink.sanitized((row["url"] as? String).flatMap { URL(string: $0) }) else { return nil }
        switch row["kind"] as? String {
        case "sprite":
            let frames = (row["frames"] as? NSNumber).map { clampedInt($0.doubleValue) } ?? 0
            let ms = (row["ms"] as? NSNumber).map { clampedInt($0.doubleValue) } ?? 0
            guard (2...200).contains(frames) else { return nil }
            self = .sprite(url: url, frames: frames, frameMs: min(max(ms, 30), 1000))
        case "gif", "webp": self = .animated(url: url)
        default: return nil
        }
    }
}

/// A sticker as the picker and the suggestion strip draw it; `url` is an http(s) CDN link (the still picture, same endpoint as the chat bubble).
struct StickerEntry: Identifiable, Hashable, Sendable {
    var cateId: Int
    var stickerId: Int
    var url: URL
    var animation: StickerAnimation?
    var id: String { "\(cateId)_\(stickerId)" }

    init(cateId: Int, stickerId: Int, url: URL, animation: StickerAnimation? = nil) {
        self.cateId = cateId
        self.stickerId = stickerId
        self.url = url
        self.animation = animation
    }

    /// Rows that are not a sticker (no ids, no web URL) are dropped; at most `limit` are kept, each sticker once.
    static func list(json: Any?, limit: Int = 500) -> [StickerEntry] {
        var seen = Set<String>()
        var out: [StickerEntry] = []
        for case let row as [String: Any] in (json as? [Any] ?? []) {
            guard let cate = (row["cateId"] as? NSNumber).map({ clampedInt($0.doubleValue) }), cate >= 0,
                  let sticker = (row["id"] as? NSNumber).map({ clampedInt($0.doubleValue) }), sticker > 0,
                  let url = WebLink.sanitized((row["url"] as? String).flatMap { URL(string: $0) }) else { continue }
            let entry = StickerEntry(cateId: cate, stickerId: sticker, url: url, animation: StickerAnimation(json: row["anim"]))
            guard seen.insert(entry.id).inserted else { continue }
            out.append(entry)
            if out.count >= limit { break }
        }
        return out
    }
}

/// One tab of the picker: a downloaded sticker set, or the recent stickers (`recentId`, always first when present).
struct StickerTab: Identifiable, Hashable, Sendable {
    static let recentId = -2

    var id: Int
    var name: String
    var icon: URL?
    var count: Int
    var isRecent: Bool { id == Self.recentId }

    /// nil = the list could not be read (not the same as "no sets").
    static func list(json: Any?) -> [StickerTab]? {
        guard let rows = json as? [Any] else { return nil }
        var seen = Set<Int>()
        var out: [StickerTab] = []
        for case let row as [String: Any] in rows {
            guard let id = (row["id"] as? NSNumber).map({ clampedInt($0.doubleValue) }), id >= recentId, seen.insert(id).inserted else { continue }
            out.append(StickerTab(
                id: id, name: String((row["name"] as? String ?? "").prefix(200)),
                icon: WebLink.sanitized((row["icon"] as? String).flatMap { URL(string: $0) }),
                count: max(0, min(10_000, (row["count"] as? NSNumber).map { clampedInt($0.doubleValue) } ?? 0))))
            if out.count >= 200 { break }
        }
        return out
    }
}

/// Where a sticker is sent from; it decides the send source the official client records (106 / 103 / 109 / 101).
enum StickerSource: String, Sendable {
    case category, recent, search, suggestion
}

/// The "Gợi ý Sticker" setting plus the official typing debounce (`sticker_post_typing.debounce`, milliseconds).
struct StickerSuggestSetting: Equatable, Sendable {
    var enabled: Bool
    var debounce: Int

    init?(json: Any?) {
        guard let root = json as? [String: Any], (root["ok"] as? Bool) == true, let enabled = root["enabled"] as? Bool else { return nil }
        self.enabled = enabled
        debounce = max(0, min(2000, (root["debounce"] as? NSNumber).map { clampedInt($0.doubleValue) } ?? 0))
    }
}

/// The composer's sticker suggestions for the text being typed: what was offered, for which text, and whether the user hid it.
/// The strip shows only while the draft still is the text the stickers were found for (a click on stale stickers must never take newer text),
/// and a click takes the whole box first (official `_sendSuggestionSticker` → `_clearBoxChatAndClosePopup`).
struct TypingStickerState: Equatable {
    private(set) var stickers: [StickerEntry] = []
    private(set) var suggestedFor: String?
    /// Esc / × hid the strip; the composer resets it when the draft changes.
    var dismissed = false
    /// The text a send for which no answer came: it may still be on its way, so no strip is offered for that same text again
    /// (typing something else releases it).
    private(set) var held: String?

    /// What a click took from the box: the whole draft (to give back if the send fails) and the text the sticker was suggested for.
    struct Taken: Equatable {
        var draft: ComposerDraft
        var typed: String
    }

    mutating func offer(_ found: [StickerEntry], for text: String) {
        if let held, text == held { stickers = []; suggestedFor = nil; return }
        held = nil
        stickers = found
        suggestedFor = found.isEmpty ? nil : text
    }

    /// A send for `text` did not answer in time: keep the strip away for that text.
    mutating func hold(_ text: String) { held = text }

    func isHeld(_ text: String) -> Bool { held == text }

    /// Empties the strip; a hold stays until a different text gets suggestions.
    mutating func clear() {
        stickers = []
        suggestedFor = nil
    }

    /// `settingEnabled`: nil = not known yet (not a "no").
    func isVisible(draft: String, mentionListOpen: Bool, settingEnabled: Bool?) -> Bool {
        !stickers.isEmpty && suggestedFor == draft && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !dismissed && !mentionListOpen && settingEnabled != false
    }

    /// The click: clears the strip and returns what to send and what to give back. (The composer checks that the strip is showing.)
    mutating func take(_ draft: ComposerDraft) -> Taken {
        let taken = Taken(draft: draft, typed: suggestedFor ?? draft.text)
        clear()
        return taken
    }
}

/// What the composer and the settings page share about the suggestion setting, so a change applies to open chats at once.
@MainActor @Observable final class StickerSuggestState {
    static let shared = StickerSuggestState()
    private(set) var setting: StickerSuggestSetting?
    @ObservationIgnored private var lastAttempt: Date?
    @ObservationIgnored private var loading = false
    /// Never faster than this between keystrokes, whatever the server config says (it is 0 today): typing bursts are coalesced.
    static let minimumDelay = 120
    /// A failed read is not repeated sooner than this (the composer asks on typing; the service may not be resolved yet right after launch).
    static let retryInterval: TimeInterval = 5

    func load(from engine: ZaloEngine) async {
        if let fresh = await engine.stickerSuggestSetting() { setting = fresh }
    }

    /// Reads the setting when it is not known yet, at most once per `retryInterval` and never twice at once.
    func loadIfNeeded(from engine: ZaloEngine) async {
        guard setting == nil, !loading, lastAttempt.map({ Date().timeIntervalSince($0) >= Self.retryInterval }) ?? true else { return }
        loading = true
        lastAttempt = Date()
        await load(from: engine)
        loading = false
    }

    func apply(_ value: StickerSuggestSetting?) { setting = value }

    func reset() {
        setting = nil
        lastAttempt = nil
    }

    /// Milliseconds to wait after the last keystroke before asking.
    var delay: Int { max(Self.minimumDelay, setting?.debounce ?? 0) }
}

extension ZaloEngine {
    /// The user's downloaded sticker sets in the official order, "Gần đây" first when there are recent stickers. nil = unavailable.
    func stickerTabs() async -> [StickerTab]? {
        let answer = await raceTimeout(60) { [self] in JSBox(value: await callSticker("categories", [])) }
        return StickerTab.list(json: answer?.value)
    }

    /// The stickers of one tab (`StickerTab.recentId` = recent stickers). nil = unavailable.
    func stickers(inTab id: Int) async -> [StickerEntry]? {
        let answer = await raceTimeout(60) { [self] in JSBox(value: await callSticker("category", [id])) }
        guard let root = answer?.value as? [String: Any], (root["id"] as? NSNumber)?.intValue == id else { return nil }
        return StickerEntry.list(json: root["stickers"])
    }

    /// Official dock search (`searchSticker(keyword, 40)`). nil = could not be asked; [] = nothing matches.
    func searchStickers(_ text: String) async -> [StickerEntry]? {
        let answer = await raceTimeout(40) { [self] in JSBox(value: await callSticker("search", [text])) }
        guard let rows = answer?.value as? [Any] else { return nil }
        return StickerEntry.list(json: rows, limit: 40)
    }

    /// Stickers the official suggestion logic offers for the text being typed; [] when the setting is off, the chat is end-to-end
    /// encrypted or nothing matches.
    func typingStickerSuggestions(in conversationId: String, text: String) async -> [StickerEntry] {
        let answer = await raceTimeout(25) { [self] in JSBox(value: await callSticker("suggest", [conversationId, text])) }
        return StickerEntry.list(json: answer?.value, limit: 30)
    }

    /// Sends one sticker to `conversationId` through the official chat-box path (the user clicked it). The answer is the usual card-action
    /// outcome: `.done`, `.locked` (only admins may post), or `.failed` with the reason. For a suggestion, `typed` is the text it was
    /// suggested for and `index` its position in the strip (official suggestion stats / action logs).
    func sendSticker(_ sticker: StickerEntry, in conversationId: String, source: StickerSource, typed: String? = nil, index: Int = -1) async -> CardActionOutcome {
        let target: [String: Any] = ["cateId": sticker.cateId, "id": sticker.stickerId]
        let context: [String: Any] = source == .suggestion ? ["text": typed ?? "", "index": index] : [:]
        // The JS side answers within ~40 s and keeps the slot taken until the send settles (a retry is refused meanwhile).
        let answer = await raceTimeout(90) { [self] in JSBox(value: await callSticker("send", [conversationId, target, source.rawValue, context])) }
        guard let answer else { return .pending(CardActionOutcome.noAnswerText) }
        let outcome = CardActionOutcome(json: answer.value)
        if case .done = outcome {} else { EngineLog.shared.write("sticker send \(source.rawValue) → \(outcome.logName)") }
        return outcome
    }

    func stickerSuggestSetting() async -> StickerSuggestSetting? {
        let answer = await raceTimeout(15) { [self] in JSBox(value: await callSticker("suggestSetting", [])) }
        return StickerSuggestSetting(json: answer?.value)
    }

    /// Turns "Gợi ý Sticker" on or off (the official `setFlagForCurrentUser`) and returns the value now in force; nil = could not be changed.
    func setStickerSuggest(enabled: Bool) async -> StickerSuggestSetting? {
        let answer = await raceTimeout(15) { [self] in JSBox(value: await callSticker("setSuggestSetting", [enabled])) }
        guard let root = answer?.value as? [String: Any], (root["ok"] as? Bool) == true else { return nil }
        let now = await stickerSuggestSetting()
        StickerSuggestState.shared.apply(now)
        return now
    }

    /// Logout: forget the suggestions and the setting of the previous session.
    func resetStickers() {
        StickerSuggestState.shared.reset()
        Task { _ = await callSticker("reset", []) }
    }

    private func callSticker(_ method: String, _ args: [Any]) async -> Any? {
        guard let data = try? JSONSerialization.data(withJSONObject: args, options: [.fragmentsAllowed]),
              let json = String(data: data, encoding: .utf8) else { return nil }
        let script = "const c = window.__zaloSticker; if (!c || !c.\(method)) return null; return await c.\(method)(...\(json));"
        return await host?.evaluate(script)
    }
}
