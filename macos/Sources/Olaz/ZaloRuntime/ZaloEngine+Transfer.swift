import Foundation

// Phone → PC message transfer, bridged to `HostScripts/transfer-adapter.js` (`window.__zaloTransfer`).
// The adapter observes the official transfer controller / sync services and reports `TransferJSON`
// (format documented at the top of the adapter); the engine only mirrors it.
extension ZaloEngine {
    /// Offer: accept = "Đồng bộ ngay", decline = "Để sau". Failed: accept = retry. Waiting/transferring:
    /// decline cancels the running sync.
    func respondToTransfer(accept: Bool) {
        callChat("respond", accept, transfer?.phase.adapterName, object: "__zaloTransfer")
    }

    func dismissTransfer() {
        // The adapter ignores the call (and re-posts the real state) if the phase moved on since the card was drawn.
        callChat("dismiss", transfer?.phase.adapterName, object: "__zaloTransfer")
        transfer = nil
    }

    /// True when the official client allows a manual phone → PC sync right now (Settings ▸ "Đồng bộ tin nhắn"
    /// guards). Observable: backed by `TransferAvailability.shared`, so no stored property is needed here.


    /// Starts a manual sync with the same call as the official Settings card (sync-v2 `sync(SETTING)`).
    /// The official state machine then drives `transfer` (waitingForPhone → transferring → finished/failed).
    func startTransfer() { callChat("start", object: "__zaloTransfer") }

    /// Why "Đồng bộ ngay" is off, in words, for the official `canSync` result name the adapter reports.
    static func transferBlockedText(_ reason: String) -> String {
        switch reason {
        // The official sync machine is not idle: right after launch it runs its own catch-up sync with the phone
        // (`START_AUTO_SYNC`), and a manual sync can only start when that is over.
        case "INVALID_STATE", "SYNC_RUNNING": "Zalo đang tự đồng bộ với điện thoại. Bạn có thể đồng bộ lại khi xong."
        case "WORKER_NOT_READY": "Zalo đang chuẩn bị dữ liệu, hãy chờ giây lát."
        case "DB_MIGRATING": "Zalo đang sắp xếp lại dữ liệu trên máy, hãy chờ giây lát."
        case "MOBILE_NOT_SUPPORT": "Zalo trên điện thoại chưa hỗ trợ đồng bộ này. Hãy cập nhật Zalo trên điện thoại."
        case "DISABLED", "SYNC_SRC_DISABLED": "Tài khoản này chưa được Zalo bật đồng bộ tin nhắn từ điện thoại."
        case "NO_PERMISSION": "Thiết bị này chưa được phép đồng bộ tin nhắn."
        default: "Chưa thể đồng bộ lúc này, hãy thử lại sau."
        }
    }

    /// `{page:'main', transferAvailable:{canStart, reason}}` from the adapter.
    func handleTransferAvailability(state: [String: Any]) {
        let json = state["transferAvailable"] as? [String: Any] ?? [:]
        canStartTransfer = json["canStart"] as? Bool ?? false
        transferStartBlockedReason = json["reason"] as? String
    }

    func handleTransfer(state: [String: Any]) {
        guard let json = state["transfer"] as? [String: Any],
              let phaseName = json["phase"] as? String else {
            transfer = nil
            return
        }
        let errorName = json["errorName"] as? String
        let errorCode = Self.int(json["errorCode"])
        let phase: MessageTransferState.Phase
        switch phaseName {
        case "offer": phase = .offer
        case "waitingForPhone": phase = .waitingForPhone
        case "transferring": phase = .transferring
        case "finished": phase = .finished
        case "failed": phase = .failed(Self.transferErrorText(code: errorCode, name: errorName))
        default:
            EngineLog.shared.write("transfer: unknown phase \(phaseName)")
            return
        }
        func text(_ key: String) -> String? {
            guard let value = json[key] as? String, !value.isEmpty else { return nil }
            return value
        }
        let next = MessageTransferState(
            phase: phase,
            progress: Self.double(json["progress"]).map { min(1, max(0, $0)) },
            syncedConversations: Self.int(json["syncedConversations"]) ?? 0,
            totalConversations: Self.int(json["totalConversations"]),
            message: text("message"),
            stage: text("stage"),
            stageIndex: Self.int(json["stageIndex"]),
            stageCount: Self.int(json["stageCount"]),
            stageProgress: Self.double(json["stageProgress"]).map { min(1, max(0, $0)) },
            receivedMessages: Self.int(json["receivedMessages"]),
            errorCode: errorCode,
            errorName: errorName,
            canRetry: json["canRetry"] as? Bool ?? false,
            officialTitle: text("officialTitle"),
            officialDescription: text("officialDescription"),
            officialSuggestion: text("officialSuggestion")
        )
        if transfer != next { transfer = next }
    }

    private static func int(_ value: Any?) -> Int? {
        if let n = value as? NSNumber, !(value is NSNull) { return n.intValue }
        return nil
    }

    private static func double(_ value: Any?) -> Double? {
        if let n = value as? NSNumber, !(value is NSNull) { return n.doubleValue }
        return nil
    }

    /// Vietnamese wording for the official sync error codes (`Wle.c`); unknown codes fall back to the code.
    static func transferErrorText(code: Int?, name: String?) -> String {
        switch name ?? "" {
        case "NO_NETWORK", "ERR_CONNECTION_TIMED_OUT", "REQUEST_SOCKET_CLOSED":
            return "Không có kết nối mạng. Hãy kiểm tra mạng rồi thử lại."
        case "MOBILE_LOW_STORAGE":
            return "Bộ nhớ điện thoại không đủ để đồng bộ."
        case "PC_LOW_STORAGE":
            return "Bộ nhớ trên máy tính không đủ để đồng bộ."
        case "MOBILE_BUSY", "MOBILE_BUSY_SYNCING", "MOBILE_BUSY_MANUAL_BACKUP":
            return "Điện thoại đang bận. Hãy thử lại sau ít phút."
        case "MOBILE_LOGGED_OUT":
            return "Zalo trên điện thoại đã đăng xuất."
        case "USER_REJECT_BACKUP", "MOBILE_USER_CANCELS_SYNC":
            return "Yêu cầu đồng bộ đã bị từ chối trên điện thoại."
        case "USER_DONT_CONFIRM", "SYNC_REQUEST_TIMEOUT_NO_NEED_CONFIRM", "MOBILE_DONT_AUTO_CONFIRM", "REQUEST_TIMEOUT":
            return "Điện thoại chưa xác nhận yêu cầu đồng bộ."
        case "MOBILE_IDLE_TIMEOUT", "WAKE_UP_TIMEOUT":
            return "Zalo trên điện thoại đã ngừng phản hồi. Hãy mở Zalo trên điện thoại rồi thử lại."
        case "BATCH_TIMEOUT", "DOWNLOAD_TIMEOUT":
            return "Quá trình đồng bộ bị gián đoạn do quá thời gian chờ."
        case "CLIENT_NOT_SUPPORT":
            return "Phiên bản Zalo trên điện thoại chưa hỗ trợ đồng bộ này."
        default:
            if let code { return "Đồng bộ không thành công (mã lỗi \(code))." }
            return "Đồng bộ không thành công."
        }
    }
}

extension MessageTransferState.Phase {
    /// Phase name as used by `transfer-adapter.js` (TransferJSON.phase).
    var adapterName: String {
        switch self {
        case .offer: "offer"
        case .waitingForPhone: "waitingForPhone"
        case .transferring: "transferring"
        case .finished: "finished"
        case .failed: "failed"
        }
    }
}
