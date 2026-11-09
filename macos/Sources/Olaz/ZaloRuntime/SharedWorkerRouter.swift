import Foundation
import os

private let routerLog = Logger(subsystem: "app.olaz", category: "shared-worker")

/// Swift port of main's `sharedWorkerManager` (`main-dist/main.js`, module `poPE`).
///
/// Zalo PC runs heavy background tasks (cloud message scan, offline sync, preview
/// updates…) in a hidden *shared worker* window (`shared-worker.html`). The main
/// window and that window each register handlers for task types
/// (`$zsharedWorker.register(type)`); `$zsharedWorker.runTask(task)` is routed by
/// Electron's main process to the window that registered `task.type`, and the
/// result/progress is routed back to the requesting window. Without this router the
/// start-up task `CLOUD_SCAN_V2` never ran, so the conversations had no recent
/// messages/previews and the offline-data pipeline never settled.
@MainActor
final class SharedWorkerRouter {
    private struct Pending {
        weak var requester: ZaloBridge?
        let requestId: Any
        let type: String
        let handlerId: Int
    }

    private final class WeakBridge {
        weak var bridge: ZaloBridge?
        init(_ bridge: ZaloBridge) { self.bridge = bridge }
    }

    private struct SenderEntry { weak var bridge: ZaloBridge?; let id: Int }
    private var senderIds: [ObjectIdentifier: SenderEntry] = [:]
    private var nextSenderId = 0
    private var handlers: [String: WeakBridge] = [:]
    private var pending: [String: Pending] = [:]
    private var lastRemoteConfig: Any?

    /// The hidden worker window (`registerWorker`); tasks fail with "Worker is not ready" until it exists.
    weak var workerBridge: ZaloBridge?
    var trace: ((String) -> Void)?

    func senderId(_ bridge: ZaloBridge) -> Int {
        // Ids are monotonic; an address reused by a new bridge gets a fresh id.
        senderIds = senderIds.filter { $0.value.bridge != nil }
        if let entry = senderIds[ObjectIdentifier(bridge)], entry.bridge === bridge { return entry.id }
        nextSenderId += 1
        senderIds[ObjectIdentifier(bridge)] = SenderEntry(bridge: bridge, id: nextSenderId)
        return nextSenderId
    }

    /// Drops everything that belonged to the worker window (it is being replaced or closed).
    func resetWorker(_ bridge: ZaloBridge) {
        if workerBridge === bridge { workerBridge = nil }
        for (type, weak) in handlers where weak.bridge == nil || weak.bridge === bridge { handlers.removeValue(forKey: type) }
        for (key, entry) in pending where entry.requester == nil { pending.removeValue(forKey: key) }
        for (key, entry) in pending where entry.handlerId == senderId(bridge) {
            pending.removeValue(forKey: key)
            respond(entry, result: nil, error: ["name": "WorkerInterrupted", "message": "Worker is interrupted"])
        }
    }

    func registerWorker(_ bridge: ZaloBridge) {
        workerBridge = bridge
        // A re-created worker starts from scratch: its predecessor's handlers are gone.
        for (type, weak) in handlers where weak.bridge === bridge { handlers.removeValue(forKey: type) }
    }

    /// Forgets per-account state (logout).
    func resetSession() {
        lastRemoteConfig = nil
        pending.removeAll()
    }

    /// Re-sends the last remote config when the worker page (re)loads (`handleRecoverRemoteConfig`).
    func workerDidFinishLoad() {
        if let lastRemoteConfig { workerBridge?.send("shared-worker-remote-config-change", [lastRemoteConfig]) }
    }

    // MARK: Handlers (ipcMain)

    func install(on bridge: ZaloBridge) {
        bridge.handle("show-shared-worker") { _ in nil }
        bridge.handle("hide-shared-worker") { _ in nil }
        bridge.handle("shared-worker-ping") { _ in nil }
        bridge.handle("shared-worker-dispose") { _ in nil }

        bridge.handle("shared-worker-handle") { [weak self, weak bridge] args in
            guard let self, let bridge, let type = args.string(0) else { return nil }
            self.handlers[type] = WeakBridge(bridge)
            self.trace?("register \(type) from #\(self.senderId(bridge))")
            return nil
        }

        bridge.handle("shared-worker-start") { [weak self, weak bridge] _ in
            guard let self, let bridge else { return nil }
            let sender = self.senderId(bridge)
            self.trace?("start from #\(sender)")
            // `onStartup`: work still pending on *another* window than the one that
            // just (re)started is declared interrupted.
            // (Only tasks already pending *now* count: a task issued by the fresh page within
            // the 100 ms window belongs to the new lifetime.)
            let before = Set(self.pending.keys)
            Task { @MainActor in
                try? await Task.sleep(for: .milliseconds(100))
                for (key, entry) in self.pending where before.contains(key) && entry.handlerId != sender {
                    self.pending.removeValue(forKey: key)
                    self.respond(entry, result: nil, error: ["name": "WorkerInterrupted", "message": "Worker is interrupted"])
                }
            }
            return nil
        }

        bridge.handle("shared-worker-run") { [weak self, weak bridge] args in
            guard let self, let bridge, let task = args.dict(0) else { return nil }
            let requestId = task["id"] ?? NSNull()
            let type = task["type"] as? String ?? ""
            guard self.workerBridge != nil else {
                self.reply(bridge, id: requestId, error: "Worker is not ready")
                return nil
            }
            guard let handler = self.handlers[type]?.bridge else {
                self.trace?("run \(type) -> unsupported (no handler)")
                self.reply(bridge, id: requestId, error: "Task type is not supported")
                return nil
            }
            let key = "\(self.senderId(bridge))_\(requestId)"
            self.pending[key] = Pending(requester: bridge, requestId: requestId, type: type, handlerId: self.senderId(handler))
            self.trace?("run \(type) id=\(key) #\(self.senderId(bridge)) -> #\(self.senderId(handler))")
            var routed = task
            routed["id"] = key
            handler.send("shared-worker-run", [routed])
            return nil
        }

        bridge.handle("shared-worker-resp") { [weak self] args in
            guard let self, let message = args.dict(0), let key = message["id"].map({ "\($0)" }),
                  let entry = self.pending.removeValue(forKey: key) else { return nil }
            let errorText = message["error"].map { error -> String in
                let wrapped: [String: Any] = ["e": error]
                if JSONSerialization.isValidJSONObject(wrapped), let data = try? JSONSerialization.data(withJSONObject: wrapped) {
                    return String(decoding: data, as: UTF8.self)
                }
                return String(describing: error)
            } ?? ""
            self.trace?("resp \(entry.type) id=\(key) \(message["error"] == nil ? "ok" : "error " + errorText.prefix(200))")
            self.respond(entry, result: message["result"], error: message["error"])
            return nil
        }

        bridge.handle("shared-worker-progress") { [weak self] args in
            guard let self, let message = args.dict(0), let key = message["id"].map({ "\($0)" }),
                  let entry = self.pending[key] else { return nil }
            var forwarded: [String: Any] = ["id": entry.requestId, "type": entry.type]
            if let progress = message["progress"] { forwarded["progress"] = progress }
            if let checkpoint = message["checkpoint"] { forwarded["checkpoint"] = checkpoint }
            entry.requester?.send("shared-worker-progress", [forwarded])
            return nil
        }

        bridge.handle("shared-worker-abort") { [weak self, weak bridge] args in
            guard let self, let bridge, let message = args.dict(0) else { return nil }
            let key = "\(self.senderId(bridge))_\(message["id"] ?? NSNull())"
            if let entry = self.pending.removeValue(forKey: key) { self.respond(entry, result: nil, error: "Aborted") }
            if let type = message["type"] as? String, let handler = self.handlers[type]?.bridge {
                handler.send("shared-worker-abort", [["id": key]])
            }
            return nil
        }

        bridge.handle("shared-worker-update-params") { [weak self, weak bridge] args in
            guard let self, let bridge, let message = args.dict(0), let type = message["type"] as? String,
                  let handler = self.handlers[type]?.bridge else { return nil }
            let key = "\(self.senderId(bridge))_\(message["id"] ?? NSNull())"
            handler.send("shared-worker-update-params", [["id": key, "type": type, "params": message["params"] ?? NSNull()]])
            return nil
        }

        bridge.handle("shared-worker-remote-config-change") { [weak self] args in
            // Renderer sends `{config}`; main stores/forwards the inner `config`.
            guard let self, let config = args.dict(0)?["config"] ?? args.first else { return nil }
            self.lastRemoteConfig = config
            self.workerBridge?.send("shared-worker-remote-config-change", [config])
            return nil
        }
    }

    // MARK: Replies

    private func respond(_ entry: Pending, result: Any?, error: Any?) {
        var message: [String: Any] = ["id": entry.requestId]
        if let result { message["result"] = result }
        if let error { message["error"] = error }
        entry.requester?.send("shared-worker-resp", [message])
    }

    private func reply(_ bridge: ZaloBridge, id: Any, error: String) {
        bridge.send("shared-worker-resp", [["id": id, "error": error]])
    }
}
