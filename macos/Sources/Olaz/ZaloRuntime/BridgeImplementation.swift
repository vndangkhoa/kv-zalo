import AppKit

/// Assembles the bridge the official renderer sees:
/// - table-driven members → IPC channels answered by `ZaloMainProcess`;
/// - preload-local members (Node `fs`/`path`/`os`/`crypto`, electron-store,
///   `$zFileManager` file helpers) → Swift handlers here and `local-modules.js`.
/// Semantics: `reanalysis/bridge-spec.md`.
@MainActor
enum BridgeImplementation {
    static let globals = [
        "$zcommon", "$znode", "$zwindow", "$zresource", "$zlogger", "$zupdater", "$zapp", "$zdownload",
        "$zdb", "$zsessionManager", "$zmulti", "$zcall", "$zsharedWorker", "$zelectron", "$zelectronNative",
        "$zuri", "$zscreencap", "$zlogin", "$zperf", "$zconfig", "$zMetric", "$zFileManager", "$zpc",
        "$zFeatures", "$zcloud", "$zMsgChannel", "$zscript", "$zInAppPayment", "$capp", "$zbackup",
        "$zpreload", "$zsqlitebw", "$zsafeStorage", "$zMigrateDrive", "$zTFModels", "$znetwork",
        "$zprotocol", "$znoti", "$zsqlite", "__SENTRY_IPC__", "MetriczGlobal",
    ]

    static func install(on bridge: ZaloBridge, platform: ZaloPlatform, mainProcess: ZaloMainProcess) {
        globals.forEach(bridge.declareGlobal)
        bridge.value("$zenv.isPreloaded", true)
        bridge.value("$zenv.isRenderer", true)

        mainProcess.install(on: bridge)

        // electron-store backing for local-modules.js `getElectronStore`
        bridge.sync("$zelectron.storePath") { args in platform.storeURL(args.string(0) ?? "config").path }
        bridge.sync("$zelectron.storeRead") { args in platform.readStore(args.string(0) ?? "config") }
        bridge.sync("$zelectron.storeWrite") { args in
            try platform.writeStore(args.string(0) ?? "config", args.dict(1) ?? [:])
            return nil
        }

        // $znode.os (values consumed by local-modules.js)
        let process = ProcessInfo.processInfo
        var uts = utsname()
        uname(&uts)
        func field<T>(_ value: T) -> String {
            withUnsafeBytes(of: value) { String(decoding: $0.prefix { $0 != 0 }, as: UTF8.self) }
        }
        bridge.value("$znode.os.__info", [
            "release": field(uts.release),
            "version": field(uts.version),
            "arch": field(uts.machine) == "x86_64" ? "x64" : "arm64",
            "hostname": process.hostName,
            "homedir": NSHomeDirectory(),
            "tmpdir": NSTemporaryDirectory(),
            "totalmem": process.physicalMemory,
            "cpus": process.activeProcessorCount,
            "uptime": Int(process.systemUptime),
            "username": NSUserName(),
        ] as [String: Any])
        bridge.value("$znode.fs.constants", ["F_OK": 0, "R_OK": 4, "W_OK": 2, "X_OK": 1])
        bridge.value("$znode.fs.constanst", ["F_OK": 0, "R_OK": 4, "W_OK": 2, "X_OK": 1])

        // $zelectronNative clipboard (Electron `clipboard`)
        bridge.sync("$zelectronNative.clipboardText") { _ in NSPasteboard.general.string(forType: .string) ?? "" }
        bridge.sync("$zelectronNative.clipboardWriteText") { args in
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(args.string(0) ?? "", forType: .string)
            return nil
        }
        bridge.sync("$zelectronNative.clipboardFormats") { _ in
            (NSPasteboard.general.types ?? []).map { type -> String in
                switch type {
                case .string: "text/plain"
                case .html: "text/html"
                case .png: "image/png"
                case .tiff: "image/tiff"
                case .fileURL: "text/uri-list"
                default: type.rawValue
                }
            }
        }

        FileSystemBridge.install(on: bridge, platform: platform)
        CryptoBridge.install(on: bridge)
        FileManagerBridge.install(on: bridge, platform: platform)
    }
}

/// `$zFileManager` — preload-local file helpers (spec §1.3).
@MainActor
enum FileManagerBridge {
    static func install(on bridge: ZaloBridge, platform: ZaloPlatform) {
        let fm = platform.fileManager
        func path(_ dir: String?, _ name: String) -> String {
            URL(fileURLWithPath: dir ?? platform.userData.path).appendingPathComponent(name).path
        }

        bridge.sync("$zFileManager.getSentryConfig") { _ in nil }
        bridge.async("$zFileManager.saveSentryConfig") { _ in nil }
        // Viewer key: <userData>/z_u.txt; must reject when missing.
        bridge.async("$zFileManager.getViewerkey") { args in
            let file = path(args.string(0), "z_u.txt")
            guard let data = fm.contents(atPath: file) else {
                throw ZaloBridge.BridgeError(message: "z_u.txt not found")
            }
            return String(decoding: data, as: UTF8.self)
        }
        bridge.async("$zFileManager.writeRecentId") { args in
            try Data((args.string(0) ?? "").utf8).write(to: platform.userData.appendingPathComponent("z_u.txt"), options: .atomic)
            return nil
        }

        // Client-side log files (zlog / QoS / action log). Kept local; never uploaded by us.
        let logDir = platform.userData.appendingPathComponent("cal")
        func logURL(_ name: String) -> URL { logDir.appendingPathComponent((name as NSString).lastPathComponent) }
        bridge.async("$zFileManager.writeZLog") { args in
            try fm.createDirectory(at: logDir, withIntermediateDirectories: true)
            let url = logURL(args.string(0) ?? "z.log")
            let data = args.data(1) ?? Data()
            let overwrite = (args.dict(2)?["overwrite"] as? NSNumber)?.boolValue ?? false
            if overwrite || !fm.fileExists(atPath: url.path) {
                try data.write(to: url, options: .atomic)
            } else {
                let handle = try FileHandle(forWritingTo: url)
                defer { try? handle.close() }
                try handle.seekToEnd()
                try handle.write(contentsOf: data)
            }
            return nil
        }
        bridge.async("$zFileManager.writeZLogBetween") { args in
            try fm.createDirectory(at: logDir, withIntermediateDirectories: true)
            let url = logURL(args.string(0) ?? "z.log")
            if !fm.fileExists(atPath: url.path) { fm.createFile(atPath: url.path, contents: nil) }
            let handle = try FileHandle(forWritingTo: url)
            defer { try? handle.close() }
            try handle.seekToEnd()
            try handle.write(contentsOf: args.data(1) ?? Data())
            return nil
        }
        bridge.async("$zFileManager.readZLog") { args in
            fm.contents(atPath: logURL(args.string(0) ?? "").path).map { String(decoding: $0, as: UTF8.self) }
        }
        bridge.async("$zFileManager.scandirLog") { _ in (try? fm.contentsOfDirectory(atPath: logDir.path)) ?? [] }
        bridge.async("$zFileManager.getExtraLogFiles") { _ in [String]() }
        for name in ["getQosLog", "getActionLog", "readUptimeLog"] {
            bridge.async("$zFileManager.\(name)") { _ in "" }
        }
        for name in ["removeQosLog", "removeActionLog", "writeActionLog", "writeUptimeLog", "truncateFileUpto",
                     "cleanupLogFileDescriptors", "initBinaryLogFile", "sealCalfLog", "mergeCalfLogGroup",
                     "writeLogRange", "trustBlob"] {
            bridge.async("$zFileManager.\(name)") { _ in nil }
        }
        bridge.async("$zFileManager.getInstallSource") { _ in
            throw ZaloBridge.BridgeError(message: "ENOENT: install source")
        }

        // File access for attachments the user picked (paths come from open panels).
        bridge.async("$zFileManager.getFileArrayBuffer") { args in
            guard let data = fm.contents(atPath: args.string(0) ?? "") else {
                throw FileSystemBridge.FSError(code: "ENOENT", path: args.string(0) ?? "")
            }
            return CryptoBridge.ArrayBufferValue(data: data)
        }
        bridge.async("$zFileManager.readFileAsync") { args in
            guard let data = fm.contents(atPath: args.string(0) ?? "") else {
                throw FileSystemBridge.FSError(code: "ENOENT", path: args.string(0) ?? "")
            }
            return args.string(1) == nil ? data : String(decoding: data, as: UTF8.self)
        }
        bridge.async("$zFileManager.svcStatAsync") { args in
            let attrs = try fm.attributesOfItem(atPath: args.string(0) ?? "")
            return ["size": (attrs[.size] as? NSNumber)?.intValue ?? 0]
        }
        bridge.async("$zFileManager.writeBufferToPath") { args in
            let target = args.string(0) ?? ""
            guard platform.isWritable(target) else { throw FileSystemBridge.FSError(code: "EACCES", path: target) }
            try fm.createDirectory(atPath: (target as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
            try (args.data(1) ?? Data()).write(to: URL(fileURLWithPath: target), options: .atomic)
            return true
        }
    }
}
