import Foundation
import WebKit
import os

private let netLog = Logger(subsystem: "app.olaz", category: "network")

/// Performs the official renderer's http(s) and ws(s) traffic (see `network-shim.js`).
///
/// Swift owns the transport: one persistent cookie jar, the client user agent,
/// and no CORS (the Electron renderer had none either — it ran from file://).
@MainActor
final class NetworkProxy: NSObject, WKScriptMessageHandlerWithReply {
    static let handlerName = "znet"
    static let websocketOrigin = "file://"

    let session: URLSession
    /// WebSockets use a delegate session so `open` fires on the real handshake.
    private let socketSession: URLSession
    private let socketEvents = WebSocketEvents()
    private var openedSockets: Set<Int> = []
    private let userAgent: String
    private var sockets: [Int: URLSessionWebSocketTask] = [:]
    weak var webView: WKWebView?

    /// Optional observer for diagnostics (method, url, status).
    var onRequestCompleted: ((String, URL, Int) -> Void)?
    /// Observes response bodies for selected URLs (e.g. the QR login long-polls).
    var responseObserver: ((URL, Int, Data) -> Void)?
    /// Realtime-channel diagnostics: lifecycle + frame headers only (never payloads).
    var socketTrace: ((String) -> Void)?
    private var frameCounts: [Int: Int] = [:]

    /// Zalo realtime frames start with `[version u8][cmd u16 LE][subCmd u8]`.
    private func describeFrame(_ data: Data) -> String {
        guard data.count >= 4 else { return "len=\(data.count)" }
        let b = [UInt8](data.prefix(4))
        let cmd = Int(b[1]) | Int(b[2]) << 8
        return "v=\(b[0]) cmd=\(cmd) sub=\(b[3]) len=\(data.count)"
    }

    private func traceFrame(_ id: Int, _ direction: String, _ data: Data) {
        let n = frameCounts[id, default: 0] + 1
        frameCounts[id] = n
        if n <= 80 { socketTrace?("ws#\(id) \(direction) \(describeFrame(data))") }
    }

    init(userAgent: String, cookieStorage: HTTPCookieStorage = .shared) {
        self.userAgent = userAgent
        let config = URLSessionConfiguration.default
        config.httpCookieStorage = cookieStorage
        config.httpCookieAcceptPolicy = .always
        config.httpShouldSetCookies = true
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.httpAdditionalHeaders = ["User-Agent": userAgent]
        session = URLSession(configuration: config)
        socketSession = URLSession(configuration: config, delegate: socketEvents, delegateQueue: nil)
        super.init()
        socketEvents.onOpen = { [weak self] taskID, proto in
            Task { @MainActor in self?.socketDidOpen(taskID: taskID, protocol: proto) }
        }
        socketEvents.onClose = { [weak self] taskID, code, reason in
            Task { @MainActor in self?.socketDidClose(taskID: taskID, code: code, reason: reason) }
        }
    }

    private func socketID(forTask taskID: Int) -> Int? {
        sockets.first { $0.value.taskIdentifier == taskID }?.key
    }

    private func socketDidOpen(taskID: Int, protocol proto: String?) {
        guard let id = socketID(forTask: taskID) else { return }
        openedSockets.insert(id)
        socketTrace?("ws#\(id) handshake ok")
        emit(id, "open", ["protocol": proto ?? ""])
    }

    private func socketDidClose(taskID: Int, code: Int, reason: String) {
        guard let id = socketID(forTask: taskID) else { return }
        sockets.removeValue(forKey: id)
        openedSockets.remove(id)
        socketTrace?("ws#\(id) closed by server code=\(code) reason=\(reason.prefix(80))")
        emit(id, "close", ["code": code, "reason": reason, "wasClean": true])
    }

    func userContentController(
        _ userContentController: WKUserContentController,
        didReceive message: WKScriptMessage,
        replyHandler: @escaping @MainActor @Sendable (Any?, String?) -> Void
    ) {
        guard let body = message.body as? [String: Any], let op = body["op"] as? String else {
            replyHandler(nil, "bad message")
            return
        }
        switch op {
        case "http":
            Task { replyHandler(await self.performHTTP(body), nil) }
        case "ws.open":
            openSocket(body)
            replyHandler(nil, nil)
        case "ws.send":
            sendSocket(body)
            replyHandler(nil, nil)
        case "ws.close":
            closeSocket(body)
            replyHandler(nil, nil)
        default:
            replyHandler(nil, "unknown op \(op)")
        }
    }

    // MARK: HTTP

    private func performHTTP(_ body: [String: Any]) async -> [String: Any] {
        guard let urlString = body["url"] as? String, let url = URL(string: urlString) else {
            return ["error": "bad url"]
        }
        var request = URLRequest(url: url)
        request.httpMethod = body["method"] as? String ?? "GET"
        if let timeout = (body["timeout"] as? NSNumber)?.doubleValue, timeout > 0 {
            request.timeoutInterval = timeout / 1000
        }
        for (key, value) in body["headers"] as? [String: String] ?? [:] {
            request.setValue(value, forHTTPHeaderField: key)
        }
        if let encoded = body["body"] as? [String: Any] {
            Self.apply(encoded, to: &request)
        }

        do {
            let (data, response) = try await session.data(for: request)
            let http = response as? HTTPURLResponse
            var headers: [String: String] = [:]
            for (k, v) in http?.allHeaderFields ?? [:] {
                headers[String(describing: k).lowercased()] = String(describing: v)
            }
            let status = http?.statusCode ?? 200
            onRequestCompleted?(request.httpMethod ?? "GET", url, status)
            responseObserver?(url, status, data)
            return [
                "status": status,
                "statusText": HTTPURLResponse.localizedString(forStatusCode: status),
                "headers": headers,
                "url": response.url?.absoluteString ?? urlString,
                "body": data.base64EncodedString(),
            ]
        } catch let error as URLError {
            netLog.error("\(request.httpMethod ?? "GET", privacy: .public) \(url.host() ?? "", privacy: .public)\(url.path(), privacy: .public) failed: \(error.code.rawValue)")
            onRequestCompleted?(request.httpMethod ?? "GET", url, -error.code.rawValue)
            return ["error": error.localizedDescription, "timedOut": error.code == .timedOut]
        } catch {
            return ["error": error.localizedDescription]
        }
    }

    private static func apply(_ encoded: [String: Any], to request: inout URLRequest) {
        func setDefaultContentType(_ value: String) {
            if request.value(forHTTPHeaderField: "Content-Type") == nil {
                request.setValue(value, forHTTPHeaderField: "Content-Type")
            }
        }
        if let contentType = encoded["contentType"] as? String { setDefaultContentType(contentType) }
        switch encoded["kind"] as? String {
        case "text":
            request.httpBody = Data((encoded["text"] as? String ?? "").utf8)
            setDefaultContentType("text/plain;charset=UTF-8")
        case "base64":
            request.httpBody = Data(base64Encoded: encoded["data"] as? String ?? "")
        case "formdata":
            let boundary = "----OlazFormBoundary\(UUID().uuidString.replacingOccurrences(of: "-", with: ""))"
            var data = Data()
            for part in encoded["parts"] as? [[String: Any]] ?? [] {
                let name = part["name"] as? String ?? ""
                data.append(Data("--\(boundary)\r\n".utf8))
                if let filename = part["filename"] as? String {
                    data.append(Data("Content-Disposition: form-data; name=\"\(name)\"; filename=\"\(filename)\"\r\n".utf8))
                    data.append(Data("Content-Type: \(part["type"] as? String ?? "application/octet-stream")\r\n\r\n".utf8))
                    data.append(Data(base64Encoded: part["data"] as? String ?? "") ?? Data())
                } else {
                    data.append(Data("Content-Disposition: form-data; name=\"\(name)\"\r\n\r\n".utf8))
                    data.append(Data((part["value"] as? String ?? "").utf8))
                }
                data.append(Data("\r\n".utf8))
            }
            data.append(Data("--\(boundary)--\r\n".utf8))
            request.httpBody = data
            // multipart always overrides whatever the page guessed
            request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        default:
            break
        }
    }

    // MARK: WebSocket

    private func openSocket(_ body: [String: Any]) {
        guard let id = (body["id"] as? NSNumber)?.intValue,
              let urlString = body["url"] as? String, let url = URL(string: urlString) else { return }
        var request = URLRequest(url: url)
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        // Browsers always send Origin on the upgrade; the Electron renderer was a
        // file:// page, which Chromium serialises as "file://".
        request.setValue(Self.websocketOrigin, forHTTPHeaderField: "Origin")
        // Cookie matching is scheme-based (http/https); attach the session cookies
        // (zpw_sek …) to the wss upgrade explicitly, as Chromium does.
        var httpsComponents = URLComponents(url: url, resolvingAgainstBaseURL: false)
        httpsComponents?.scheme = url.scheme == "ws" ? "http" : "https"
        if let cookieURL = httpsComponents?.url,
           let cookies = session.configuration.httpCookieStorage?.cookies(for: cookieURL), !cookies.isEmpty {
            for (name, value) in HTTPCookie.requestHeaderFields(with: cookies) {
                request.setValue(value, forHTTPHeaderField: name)
            }
            socketTrace?("ws#\(id) attaching \(cookies.count) cookies: \(cookies.map(\.name).sorted())")
        } else {
            socketTrace?("ws#\(id) no cookies for \(url.host() ?? "")")
        }
        let protocols = body["protocols"] as? [String] ?? []
        if !protocols.isEmpty {
            request.setValue(protocols.joined(separator: ", "), forHTTPHeaderField: "Sec-WebSocket-Protocol")
        }
        let task = socketSession.webSocketTask(with: request)
        task.maximumMessageSize = 64 * 1024 * 1024
        sockets[id] = task
        let params = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.map(\.name) ?? []
        socketTrace?("ws#\(id) open \(url.host() ?? "")\(url.path()) params=\(params) open-sockets=\(sockets.count)")
        task.resume()
        // Reads queue until the handshake completes; `open` comes from the delegate.
        receive(id, task)
    }

    private func receive(_ id: Int, _ task: URLSessionWebSocketTask) {
        task.receive { [weak self] result in
            Task { @MainActor in
                guard let self, self.sockets[id] === task else { return }
                switch result {
                case .success(.string(let text)):
                    self.emit(id, "text", ["text": text])
                    self.receive(id, task)
                case .success(.data(let data)):
                    self.traceFrame(id, "in ", data)
                    self.emit(id, "binary", ["data": data.base64EncodedString()])
                    self.receive(id, task)
                case .success:
                    self.receive(id, task)
                case .failure(let error):
                    self.socketFailed(id, error)
                }
            }
        }
    }

    private func socketFailed(_ id: Int, _ error: Error) {
        guard let task = sockets.removeValue(forKey: id) else { return }
        openedSockets.remove(id)
        let code = task.closeCode == .invalid ? 1006 : task.closeCode.rawValue
        let reason = task.closeReason.flatMap { String(data: $0, encoding: .utf8) } ?? ""
        let http = task.response as? HTTPURLResponse
        socketTrace?("ws#\(id) closed code=\(code) reason=\(reason.prefix(80)) http=\(http?.statusCode ?? 0) error=\(error.localizedDescription)")
        if code == 1006 { emit(id, "error", [:]) }
        emit(id, "close", ["code": code, "reason": reason, "wasClean": code != 1006])
        netLog.info("socket \(id) closed \(code): \(error.localizedDescription, privacy: .public)")
    }

    private func sendSocket(_ body: [String: Any]) {
        guard let id = (body["id"] as? NSNumber)?.intValue, let task = sockets[id] else { return }
        let message: URLSessionWebSocketTask.Message
        if let text = body["text"] as? String {
            message = .string(text)
        } else {
            let data = Data(base64Encoded: body["data"] as? String ?? "") ?? Data()
            traceFrame(id, "out", data)
            message = .data(data)
        }
        task.send(message) { [weak self] error in
            if let error { Task { @MainActor in self?.socketFailed(id, error) } }
        }
    }

    private func closeSocket(_ body: [String: Any]) {
        guard let id = (body["id"] as? NSNumber)?.intValue, let task = sockets.removeValue(forKey: id) else { return }
        let code = (body["code"] as? NSNumber)?.intValue ?? 1000
        socketTrace?("ws#\(id) closed-by-client code=\(code)")
        task.cancel(with: URLSessionWebSocketTask.CloseCode(rawValue: code) ?? .normalClosure,
                    reason: (body["reason"] as? String).map { Data($0.utf8) })
        emit(id, "close", ["code": code, "reason": body["reason"] as? String ?? "", "wasClean": true])
    }

    func closeAllSockets() {
        for task in sockets.values { task.cancel(with: .goingAway, reason: nil) }
        sockets.removeAll()
    }

    private func emit(_ id: Int, _ type: String, _ payload: [String: Any]) {
        webView?.callAsyncJavaScript(
            "window.__zaloNet && window.__zaloNet.socketEvent(id, type, payload)",
            arguments: ["id": id, "type": type, "payload": payload],
            in: nil, in: .page, completionHandler: nil
        )
    }
}

/// Bridges URLSession WebSocket delegate callbacks (background queue) to the proxy.
private final class WebSocketEvents: NSObject, URLSessionWebSocketDelegate, @unchecked Sendable {
    var onOpen: (@Sendable (Int, String?) -> Void)?
    var onClose: (@Sendable (Int, Int, String) -> Void)?

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol protocol: String?) {
        onOpen?(webSocketTask.taskIdentifier, `protocol`)
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask,
                    didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        onClose?(webSocketTask.taskIdentifier, closeCode.rawValue, reason.flatMap { String(data: $0, encoding: .utf8) } ?? "")
    }
}
