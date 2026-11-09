import Testing
import Foundation
@testable import Olaz

/// The Zalo runtime Olaz installs into its own folder instead of needing Zalo PC (ZaloRuntimeInstaller.swift).
@Suite struct ZaloRuntimeTests {
    private func temporaryRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("olaz-runtime-\(UUID())", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    /// The files `ZaloRuntime.isComplete` looks for.
    private func makeRuntime(at dir: URL, version: String) throws {
        let fm = FileManager.default
        try fm.createDirectory(at: dir.appendingPathComponent("Contents/Resources"), withIntermediateDirectories: true)
        try (["CFBundleShortVersionString": version] as NSDictionary).write(to: dir.appendingPathComponent("Contents/Info.plist"))
        try Data("asar".utf8).write(to: dir.appendingPathComponent("Contents/Resources/app.asar"))
        let helper = ZaloRuntime.helper("ZaloCall", in: dir)
        try fm.createDirectory(at: helper.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("#!/bin/sh\n".utf8).write(to: helper)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: helper.path)
    }

    @Test func readsVersionFromInstallerName() {
        #expect(ZaloRuntime.version(fromInstallerName: "ZaloSetup-universal-26.10.20.dmg") == "26.10.20")
        #expect(ZaloRuntime.version(fromInstallerName: "ZaloSetup-universal-26.10.20-RC4.zip") == nil)
        #expect(ZaloRuntime.version(fromInstallerName: "Zalo-3.2.64.dmg") == nil)
        #expect(ZaloRuntime.version(fromInstallerName: ZaloRuntime.pinned.url.lastPathComponent) == ZaloRuntime.pinned.version)
    }

    @Test func comparesVersionsNumerically() {
        #expect(ZaloRuntime.isNewer("26.10.20", than: "26.9.10"))
        #expect(ZaloRuntime.isNewer("27.1.5", than: "26.12.31"))
        #expect(!ZaloRuntime.isNewer("26.10.20", than: "26.10.20"))
        #expect(!ZaloRuntime.isNewer("26.10", than: "26.10.0"))
        #expect(!ZaloRuntime.isNewer("26.9.10", than: "26.10.20"))
    }

    @Test func launchSwapsInPreparedUpgrade() throws {
        let root = try temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        try makeRuntime(at: root.appendingPathComponent("current"), version: "26.9.10")
        try makeRuntime(at: root.appendingPathComponent("pending"), version: "26.10.20")
        try FileManager.default.createDirectory(at: root.appendingPathComponent("work/staging-x"), withIntermediateDirectories: true)

        ZaloRuntime.prepareForLaunch(root: root)

        #expect(ZaloRuntime.version(of: root.appendingPathComponent("current")) == "26.10.20")
        #expect(ZaloRuntime.isComplete(root.appendingPathComponent("current")))
        #expect(!FileManager.default.fileExists(atPath: root.appendingPathComponent("pending").path))
        #expect((try? FileManager.default.contentsOfDirectory(atPath: root.appendingPathComponent("work").path)) ?? [] == [])
    }

    @Test func launchDropsIncompleteUpgrade() throws {
        let root = try temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        try makeRuntime(at: root.appendingPathComponent("current"), version: "26.9.10")
        try FileManager.default.createDirectory(at: root.appendingPathComponent("pending/Contents"), withIntermediateDirectories: true)

        ZaloRuntime.prepareForLaunch(root: root)

        #expect(ZaloRuntime.version(of: root.appendingPathComponent("current")) == "26.9.10")
        #expect(!FileManager.default.fileExists(atPath: root.appendingPathComponent("pending").path))
    }

    @Test func refusesAppsNotSignedByVNG() {
        #expect(throws: ZaloRuntimeInstaller.Failure.self) {
            try ZaloRuntimeInstaller.verifySignature(of: URL(fileURLWithPath: "/System/Applications/Calculator.app"))
        }
    }

    @Test func installedZaloPassesSignatureWhenPresent() throws {
        let app = URL(fileURLWithPath: "/Applications/Zalo.app")
        guard FileManager.default.fileExists(atPath: app.path) else { return }
        try ZaloRuntimeInstaller.verifySignature(of: app)
    }

    /// The whole DMG path (download, checksum, mount, signature, copy) against the official DMG, a downloaded copy or the
    /// pinned link: `OLAZ_ZALO_DMG=/path/to/ZaloSetup-universal-<pinned>.dmg swift test --filter ZaloRuntimeTests`
    /// (or `OLAZ_ZALO_DMG=pinned`).
    @Test func installsFromOfficialDiskImage() async throws {
        guard let path = ProcessInfo.processInfo.environment["OLAZ_ZALO_DMG"] else { return }
        let root = try temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        var release = ZaloRuntime.pinned
        release.version = "0-test"                       // no installed Zalo.app matches: the DMG is used
        if path != "pinned" { release.url = URL(fileURLWithPath: path) }
        let steps = StepLog()
        let installer = ZaloRuntimeInstaller(release: release, allowsLatest: false,
                                             work: root.appendingPathComponent("work"), progress: { steps.append($0) })
        let destination = root.appendingPathComponent("current")

        let version = try await installer.install(into: destination)

        #expect(version == ZaloRuntime.pinned.version)
        #expect(ZaloRuntime.isComplete(destination))
        #expect(try OfficialZaloInstall.locate(appURL: destination).asar.contains("pc-dist/index.html"))
        try CallHelperSignature.verify(executable: ZaloRuntime.helper("ZaloCall", in: destination))
        #expect(FileManager.default.isExecutableFile(atPath: ZaloRuntime.helper("ZaloCap", in: destination).path))
        #expect(steps.all.contains(.verifying) && steps.all.contains(.installing))
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: root.appendingPathComponent("work").path)
        #expect(leftovers.isEmpty)
    }

    /// `OLAZ_NETWORK_TESTS=1`: zalo.me still redirects a Mac to a DMG whose name carries the version.
    @Test func zaloMeOffersMacInstaller() async throws {
        guard ProcessInfo.processInfo.environment["OLAZ_NETWORK_TESTS"] != nil else { return }
        let latest = try await ZaloRuntimeInstaller.latestRelease()
        #expect(latest.url.pathExtension == "dmg")
        #expect(!ZaloRuntime.isNewer(ZaloRuntime.pinned.version, than: latest.version))
    }
}

private final class StepLog: @unchecked Sendable {
    private let lock = NSLock()
    private var steps: [ZaloRuntimeInstaller.Step] = []
    func append(_ step: ZaloRuntimeInstaller.Step) { lock.lock(); steps.append(step); lock.unlock() }
    var all: [ZaloRuntimeInstaller.Step] { lock.lock(); defer { lock.unlock() }; return steps }
}
