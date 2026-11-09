import Foundation

/// Append-only diagnostics file: `~/Library/Application Support/Olaz/logs/engine.log`.
///
/// Records *shapes*, never secrets: IPC channel names with argument types,
/// request method/host/path/status (no query strings or bodies), renderer
/// console errors, and trusted-device step outcomes.
@MainActor
final class EngineLog {
    static let shared = EngineLog()

    let url: URL
    private var handle: FileHandle?
    private let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f
    }()

    /// `swift test` must not rotate the running app's log out from under it.
    private static var isTestProcess: Bool {
        let process = ProcessInfo.processInfo
        return process.environment["XCTestConfigurationFilePath"] != nil
            || ["xctest", "swiftpm-testing-helper"].contains(process.processName)
            || Bundle.main.bundlePath.hasSuffix(".xctest")
            || NSClassFromString("XCTestCase") != nil
    }

    private init() {
        let dir = Self.isTestProcess
            ? FileManager.default.temporaryDirectory.appendingPathComponent("OlazTests-logs", isDirectory: true)
            : ZaloPlatform.defaultUserData.deletingLastPathComponent().appendingPathComponent("logs", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        url = dir.appendingPathComponent("engine.log")
        // Keep the previous run for comparison; cap size.
        let fm = FileManager.default
        if let size = (try? fm.attributesOfItem(atPath: url.path))?[.size] as? Int, size > 0 {
            let previous = dir.appendingPathComponent("engine.previous.log")
            try? fm.removeItem(at: previous)
            try? fm.moveItem(at: url, to: previous)
        }
        fm.createFile(atPath: url.path, contents: nil)
        handle = try? FileHandle(forWritingTo: url)
    }

    func write(_ line: String) {
        let text = "\(formatter.string(from: .now)) \(line.prefix(1500))\n"
        handle?.write(Data(text.utf8))
    }

    /// Argument summary without values: `(string, object{3}, bytes[512])`.
    static func shape(_ args: [Any]) -> String {
        "(" + args.map { arg -> String in
            switch arg {
            case is String: "string"
            case let n as NSNumber: CFGetTypeID(n) == CFBooleanGetTypeID() ? "bool" : "number"
            case let d as Data: "bytes[\(d.count)]"
            case let a as [Any]: "array[\(a.count)]"
            case let o as [String: Any]: "object{\(o.keys.sorted().prefix(6).joined(separator: ","))}"
            case is NSNull: "null"
            default: "\(type(of: arg))"
            }
        }.joined(separator: ", ") + ")"
    }
}
