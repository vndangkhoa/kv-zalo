import Foundation

/// Finds directories that SwiftPM copies into the target's resource bundle
/// (`resources: [.copy("JSModules"), .copy("Resources")]`).
///
/// `Bundle.module` alone breaks in a hand-assembled `.app` (its generated accessor
/// looks next to the bundle URL and otherwise at an absolute `.build` path), so the
/// app bundle's `Contents/Resources` is searched first; `scripts/make-app.sh` copies
/// `Olaz_Olaz.bundle` there.
enum ResourceLocator {
    static let resourceBundleName = "Olaz_Olaz.bundle"

    /// `<resource bundle>/<subdirectory>` (e.g. `"JSModules"`, `"Resources"`), or nil.
    static func url(forSubdirectory subdirectory: String) -> URL? {
        let fm = FileManager.default
        // A resource bundle's files sit at its root (flat) or under Contents/Resources
        // (what the swiftbuild backend emits); `Bundle.resourceURL` resolves both.
        func resources(ofBundleAt url: URL) -> URL? {
            fm.fileExists(atPath: url.path) ? (Bundle(url: url)?.resourceURL ?? url) : nil
        }
        var candidates: [URL] = []
        if let res = Bundle.main.resourceURL {
            if let inner = resources(ofBundleAt: res.appendingPathComponent(resourceBundleName)) {
                candidates.append(inner)
            }
            candidates.append(res)
        }
        if let beside = resources(ofBundleAt: Bundle.main.bundleURL.appendingPathComponent(resourceBundleName)) {
            candidates.append(beside)
        }
        for base in candidates {
            let url = base.appendingPathComponent(subdirectory, isDirectory: true)
            var isDir: ObjCBool = false
            if fm.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue {
                return url
            }
        }
        // Development / `swift test` fallback.
        if let res = Bundle.module.resourceURL {
            let url = res.appendingPathComponent(subdirectory, isDirectory: true)
            if fm.fileExists(atPath: url.path) { return url }
        }
        return nil
    }

    /// A file inside a resource subdirectory, e.g. `url(for: "vendor/libsignal-protocol.js", in: "JSModules")`.
    static func url(for relativePath: String, in subdirectory: String) -> URL? {
        guard let dir = url(forSubdirectory: subdirectory) else { return nil }
        let url = dir.appendingPathComponent(relativePath)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }
}
