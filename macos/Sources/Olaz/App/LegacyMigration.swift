import AppKit
import os

private let migrationLog = Logger(subsystem: "app.olaz", category: "migration")

/// One-time hand-over of the signed-in session from the SwiftZalo builds (bundle id `dev.swiftzalo.app`) to Olaz.
///
/// macOS keys an app's cookies (`HTTPCookieStorage`), WebKit storage (the official client's IndexedDB message
/// database) and preferences by bundle id, so a new id alone would sign the user out and lose the history synced
/// from the phone. Runs first thing in `main.swift`, before anything opens those stores. Directories are cloned
/// (APFS copy-on-write, so the 700 MB WebKit store costs no extra space) instead of moved: the old app keeps
/// working as a fallback until the user deletes it.
@MainActor
enum LegacyMigration {
    static let legacyBundleId = "dev.swiftzalo.app"
    private static let doneKey = "olaz.migratedFromSwiftZalo"

    /// Old preference keys renamed with the app.
    private static let renamedKeys = [
        "swiftzalo.appearance": "olaz.appearance",
        "swiftzalo.imei": "olaz.imei",
        "swiftzalo.notifications.showsPreview": "olaz.notifications.showsPreview",
        "SwiftZaloDevEval": "OlazDevEval",
    ]

    static func runIfNeeded() {
        guard let bundleId = Bundle.main.bundleIdentifier, bundleId != legacyBundleId else { return }
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: doneKey) else { return }
        let items = storageItems(newBundleId: bundleId)
        let legacyDefaults = defaults.persistentDomain(forName: legacyBundleId)
        guard legacyDefaults != nil || items.contains(where: { exists($0.from) }) else {
            defaults.set(true, forKey: doneKey)   // fresh install: nothing to bring over
            return
        }
        // The old app keeps these databases open; cloning them mid-write could tear the SQLite WAL files.
        guard quitLegacyApp() else { exit(0) }
        do {
            for item in items where exists(item.from) { try clone(item.from, to: item.to) }
            for (key, value) in legacyDefaults ?? [:] where !key.hasPrefix("NSWindow Frame") && !key.hasPrefix("NSSplitView") {
                // The legacy value wins: until this migration the new domain only got values from launches that ran
                // before it (defaults registered, a pref changed in a first unmigrated start), never a deliberate choice.
                defaults.set(value, forKey: renamedKeys[key] ?? key)
            }
            defaults.set(true, forKey: doneKey)
            migrationLog.notice("session moved from \(legacyBundleId, privacy: .public)")
        } catch {
            migrationLog.error("migration failed: \(error.localizedDescription, privacy: .public)")
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = "Không chuyển được dữ liệu từ SwiftZalo"
            alert.informativeText = "\(error.localizedDescription)\n\nDữ liệu cũ vẫn còn nguyên. Bạn có thể thoát rồi mở lại \(AppBrand.name) để thử lại, hoặc tiếp tục và đăng nhập lại bằng mã QR."
            alert.addButton(withTitle: "Thoát")
            alert.addButton(withTitle: "Tiếp tục và đăng nhập lại")
            activate()
            if alert.runModal() == .alertFirstButtonReturn { exit(0) }
            defaults.set(true, forKey: doneKey)
        }
    }

    private struct Item { let from: URL; let to: URL }

    private static func storageItems(newBundleId: String) -> [Item] {
        let library = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        func pair(_ dir: String, _ old: String, _ new: String) -> Item {
            Item(from: library.appending(path: dir).appending(path: old), to: library.appending(path: dir).appending(path: new))
        }
        return [
            pair("Application Support", "SwiftZalo", "Olaz"),
            pair("WebKit", legacyBundleId, newBundleId),
            pair("HTTPStorages", legacyBundleId, newBundleId),
            pair("HTTPStorages", legacyBundleId + ".binarycookies", newBundleId + ".binarycookies"),
        ]
    }

    private static func exists(_ url: URL) -> Bool { FileManager.default.fileExists(atPath: url.path) }

    /// Clones into a temporary sibling, then swaps it in, so an interrupted run never leaves a half-copied store
    /// under the real name. Whatever already sat there (an earlier unmigrated launch) is kept beside it.
    private static func clone(_ source: URL, to destination: URL) throws {
        let fm = FileManager.default
        try fm.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        let staging = destination.deletingLastPathComponent().appending(path: ".\(destination.lastPathComponent).migrating-\(UUID().uuidString)")
        do {
            try fm.copyItem(at: source, to: staging)
            if exists(destination) {
                let stamp = Int(Date().timeIntervalSince1970)
                try fm.moveItem(at: destination, to: destination.deletingLastPathComponent().appending(path: "\(destination.lastPathComponent).before-migration-\(stamp)"))
            }
            try fm.moveItem(at: staging, to: destination)
        } catch {
            // A half copy (disk full…) would otherwise sit beside the real store, hundreds of MB, until someone finds it.
            try? fm.removeItem(at: staging)
            throw error
        }
    }

    /// Asks the user to let the old app quit (it may be in a call), then waits for it. False = don't continue.
    private static func quitLegacyApp() -> Bool {
        var running = NSRunningApplication.runningApplications(withBundleIdentifier: legacyBundleId)
        guard !running.isEmpty else { return true }
        let alert = NSAlert()
        alert.messageText = "SwiftZalo đang mở"
        alert.informativeText = "\(AppBrand.name) sẽ chuyển phiên đăng nhập và tin nhắn đã đồng bộ từ SwiftZalo sang. SwiftZalo cần được đóng trước; nếu đang có cuộc gọi, cuộc gọi sẽ kết thúc."
        alert.addButton(withTitle: "Đóng SwiftZalo và tiếp tục")
        alert.addButton(withTitle: "Để sau")
        activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return false }
        running.forEach { $0.terminate() }
        let deadline = Date().addingTimeInterval(15)
        while Date() < deadline {
            running.removeAll { $0.isTerminated }
            if running.isEmpty {
                // WebKit's helper processes can outlive the app for a moment and still hold the databases open.
                let settled = Date().addingTimeInterval(2)
                while Date() < settled { RunLoop.current.run(until: Date().addingTimeInterval(0.2)) }
                return true
            }
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        let failed = NSAlert()
        failed.messageText = "SwiftZalo chưa đóng"
        failed.informativeText = "Hãy thoát SwiftZalo rồi mở lại \(AppBrand.name)."
        failed.runModal()
        return false
    }

    private static func activate() {
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate()
    }
}
