import AppKit
import Foundation

// Native macOS notifications + notification settings. Bridged to `HostScripts/notification-adapter.js`
// (`window.__zaloNotification`; posts {page:'main', notification:{kind, …}}) and `NativeNotifier`.
//
// Who decides what:
//   * The official renderer decides *whether* a message notifies (global toggle, per-conversation mute incl.
//     timed mute, group/1-1/stranger rules, sound setting, "app is focused") and builds the text.
//   * `NativeNotifier` shows it as a UNUserNotificationCenter banner (`_noti_*` IPC channels).
//   * Settings live in the official client's storage (`askNoti`, `z_sound`) and mute service
//     (`MuteDataManager.onMute/onUnMute`, synced with the Zalo server); this file mirrors and drives them.

/// How long a conversation stays muted — the four choices of the official "Tắt thông báo" menu.
/// Raw values are the official `MuteDataManager.onMute(convId, mode)` modes.
enum MuteDuration: Int, CaseIterable, Identifiable, Sendable {
    case oneHour = 1
    case fourHours = 2
    case until8AM = 3
    case untilReopened = 4

    var id: Int { rawValue }

    /// Official strings `STR_MUTE_ONE_HOURS`, `STR_MUTE_FOUR_HOUR`, `STR_MUTE_TO_8_AM`, `STR_MUTE_NO_LIMIT`.
    var title: String {
        switch self {
        case .oneHour: "Trong 1 giờ"
        case .fourHours: "Trong 4 giờ"
        case .until8AM: "Cho đến 8:00 AM"
        case .untilReopened: "Cho đến khi được mở lại"
        }
    }

    /// When the mute ends, nil = until the user turns notifications back on.
    /// "Until 8 AM" is the next 8:00 local time (official `_getNowTo8Am`: today's 8:00, or tomorrow's once past).
    func expiry(from now: Date, calendar: Calendar = .current) -> Date? {
        switch self {
        case .oneHour: return now.addingTimeInterval(3600)
        case .fourHours: return now.addingTimeInterval(4 * 3600)
        case .until8AM:
            return calendar.nextDate(after: now, matching: DateComponents(hour: 8, minute: 0, second: 0), matchingPolicy: .nextTime)
        case .untilReopened: return nil
        }
    }
}

/// A conversation mute as the official `MuteDataManager` stores it (seconds; `duration` -1 = until reopened).
struct MuteInfo: Equatable, Sendable {
    var startTime: TimeInterval
    var duration: TimeInterval

    var isIndefinite: Bool { duration < 0 }
    /// Official `getHMTime`: start + duration.
    var until: Date? { isIndefinite ? nil : Date(timeIntervalSince1970: startTime + duration) }

    func isActive(at now: Date = .now) -> Bool {
        guard let until else { return true }
        return until > now
    }

    /// "Cho đến 14:30" / "Cho đến khi được mở lại" (official `STR_MUTE_TIME_LIMIT_V2` wording).
    func description(now: Date = .now, calendar: Calendar = .current) -> String {
        guard let until else { return "Cho đến khi được mở lại" }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "vi_VN")
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.dateFormat = "HH:mm"
        let time = formatter.string(from: until)
        if calendar.isDate(until, inSameDayAs: now) { return "Cho đến \(time)" }
        formatter.dateFormat = "dd/MM"
        return "Cho đến \(time) ngày \(formatter.string(from: until))"
    }

    /// Menu status line: "Đang tắt đến 14:30" / "Đang tắt đến 14:30 ngày 27/10" / "Đang tắt cho đến khi được mở lại".
    func statusText(now: Date = .now, calendar: Calendar = .current) -> String {
        guard until != nil else { return "Đang tắt cho đến khi được mở lại" }
        let text = description(now: now, calendar: calendar)   // "Cho đến …"
        return "Đang tắt đến" + text.dropFirst("Cho đến".count)
    }

    static func parse(_ json: [String: Any]) -> (id: String, info: MuteInfo)? {
        guard let id = json["id"] as? String, !id.isEmpty else { return nil }
        let start = (json["startTime"] as? NSNumber)?.doubleValue ?? 0
        let duration = (json["duration"] as? NSNumber)?.doubleValue ?? -1
        return (id, MuteInfo(startTime: start, duration: duration))
    }
}

/// Notification preferences mirrored from the official client.
struct NotificationSettingsState: Equatable {
    enum Authorization: String, Equatable, Sendable {
        /// Not queried yet.
        case unknown
        /// Running outside an app bundle (tests, probe).
        case unavailable
        case notDetermined, denied, authorized
    }

    static let previewDefaultsKey = "olaz.notifications.showsPreview"

    /// A settings snapshot from the official client has arrived.
    var isLoaded = false
    /// Official "Cài đặt thông báo ▸ Bật" (`askNoti`).
    var enabled = true
    /// Official "Phát âm thanh khi có tin nhắn & thông báo mới" (`z_sound`).
    var sound = false
    /// Whether `z_sound` was ever written (the official page defaults it on first visit).
    var hasSoundSetting = false
    /// Show message text in banners; off → "Bạn có tin nhắn mới" (native option, the official client
    /// only hides content while the app lock is on).
    var showsPreview = UserDefaults.standard.object(forKey: previewDefaultsKey) as? Bool ?? true
    /// macOS permission for banners.
    var authorization: Authorization = .unknown
    /// Conversation id → mute, as stored by the official mute service (may include expired entries).
    var mutes: [String: MuteInfo] = [:]

    /// Active mute of a conversation, nil when notifications are on.
    func mute(for conversationId: String, now: Date = .now) -> MuteInfo? {
        guard let mute = mutes[conversationId], mute.isActive(at: now) else { return nil }
        return mute
    }

    func isMuted(_ conversationId: String, now: Date = .now) -> Bool { mute(for: conversationId, now: now) != nil }

    /// Active mutes, longest-standing first.
    func activeMutes(now: Date = .now) -> [(id: String, info: MuteInfo)] {
        mutes.filter { $0.value.isActive(at: now) }
            .map { (id: $0.key, info: $0.value) }
            .sorted { ($0.info.startTime, $0.id) < ($1.info.startTime, $1.id) }
    }

    /// Applies a `{kind:'settings'}` payload from the adapter.
    mutating func apply(settings json: [String: Any]) {
        isLoaded = true
        if let value = json["enabled"] as? NSNumber { enabled = value.boolValue }
        if let value = json["sound"] as? NSNumber { sound = value.boolValue }
        if let value = json["hasSoundSetting"] as? NSNumber { hasSoundSetting = value.boolValue }
        var next: [String: MuteInfo] = [:]
        for entry in json["mutes"] as? [[String: Any]] ?? [] {
            if let parsed = MuteInfo.parse(entry) { next[parsed.id] = parsed.info }
        }
        mutes = next
    }
}

extension ZaloEngine {
    // MARK: Actions — settings

    /// Official toggle: notifications on/off (also switches the sound like the official page does).
    func setNotificationsEnabled(_ enabled: Bool) {
        notificationSettings.enabled = enabled
        notificationSettings.sound = enabled
        callChat("setEnabled", enabled, object: "__zaloNotification")
        if enabled { Task { await requestNotificationAuthorization() } }
    }

    func setNotificationSound(_ enabled: Bool) {
        notificationSettings.sound = enabled
        notificationSettings.hasSoundSetting = true
        callChat("setSound", enabled, object: "__zaloNotification")
    }

    func setMessagePreview(_ shown: Bool) {
        notificationSettings.showsPreview = shown
        UserDefaults.standard.set(shown, forKey: NotificationSettingsState.previewDefaultsKey)
    }

    /// Call when the settings UI appears: applies the official settings page's first-visit defaults.
    func notificationSettingsDidAppear() {
        callChat("reconcileDefaults", object: "__zaloNotification")
        callChat("refresh", object: "__zaloNotification")
        Task { await refreshNotificationAuthorization() }
    }

    // MARK: Actions — per-conversation mute (hook into list rows / chat header: `ConversationMuteMenu`)

    func isConversationMuted(_ id: String) -> Bool { notificationSettings.isMuted(id) }

    /// Official "Tắt thông báo" submenu → `MuteDataManager.onMute(convId, mode)`; syncs to the server.
    func muteConversation(_ id: String, _ duration: MuteDuration) {
        let now = Date.now
        // Optimistic: the official service confirms through the next settings snapshot.
        let until = duration.expiry(from: now)
        notificationSettings.mutes[id] = MuteInfo(startTime: now.timeIntervalSince1970,
                                                  duration: until.map { $0.timeIntervalSince(now) } ?? -1)
        callChat("mute", id, duration.rawValue, object: "__zaloNotification")
    }

    /// Official "Bật thông báo" → `MuteDataManager.onUnMute(convId)`.
    func unmuteConversation(_ id: String) {
        notificationSettings.mutes[id] = nil
        callChat("unmute", id, object: "__zaloNotification")
    }

    // MARK: macOS authorization

    @discardableResult
    func requestNotificationAuthorization() async -> NotificationSettingsState.Authorization {
        let result = await NativeNotifier.shared.requestAuthorization()
        applyAuthorization(result)
        return notificationSettings.authorization
    }

    func refreshNotificationAuthorization() async {
        applyAuthorization(await NativeNotifier.shared.authorizationStatus())
    }

    func openSystemNotificationSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") {
            NSWorkspace.shared.open(url)
        }
    }

    private func applyAuthorization(_ status: NativeNotifier.Authorization) {
        let mapped: NotificationSettingsState.Authorization = switch status {
        case .unavailable: .unavailable
        case .notDetermined: .notDetermined
        case .denied: .denied
        case .authorized: .authorized
        }
        if notificationSettings.authorization != mapped { notificationSettings.authorization = mapped }
        // The shim's `Notification.permission` is what the official code gates on.
        let permission = mapped == .authorized ? "granted" : mapped == .denied ? "denied" : "default"
        callChat("setPermission", permission, object: "__zaloNotification")
    }

    // MARK: Adapter events

    func handleNotification(state: [String: Any]) {
        guard let payload = state["notification"] as? [String: Any] else { return }
        switch payload["kind"] as? String {
        case "ready":
            // Renderer booted: bind the notifier, ask macOS for permission once, tell the shim.
            NativeNotifier.shared.attach(engine: self)
            Task {
                var status = await NativeNotifier.shared.authorizationStatus()
                if status == .notDetermined { status = await NativeNotifier.shared.requestAuthorization() }
                applyAuthorization(status)
                callChat("refresh", object: "__zaloNotification")
                // The page-load handler announces "focus" once the page finishes; restore the real state after it.
                try? await Task.sleep(for: .seconds(4))
                NativeNotifier.shared.syncFocusState()
            }
        case "requestPermission":
            // The official code asked (`Notification.requestPermission`): answer with the macOS decision.
            Task {
                let status = await NativeNotifier.shared.requestAuthorization()
                applyAuthorization(status)
                let permission = status == .authorized ? "granted" : status == .denied ? "denied" : "default"
                callChat("permissionResult", permission, object: "__zaloNotification")
            }
        case "settings":
            var next = notificationSettings
            next.apply(settings: payload)
            if next != notificationSettings { notificationSettings = next }
        case "sound":
            if let url = payload["url"] as? String { NativeNotifier.shared.playNotificationSound(url) }
        default:
            break
        }
    }
}
