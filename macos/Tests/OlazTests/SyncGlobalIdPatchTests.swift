import Testing
import Foundation
import JavaScriptCore
@testable import Olaz

/// `AsarSchemeHandler.patchSyncGlobalIdResolver`: sync-v2's GET_GLOBAL_NOISED_IDS resolver falls back to
/// the official global-id service for users `getminiprofiles` returns nothing for.
@MainActor
@Suite struct SyncGlobalIdPatch {
    static let resolver = """
    var ZS={};ZS.resolveWithRetry=async(e,t)=>{let s=await t(e)||{},n=e.filter((e=>!s[e]));for(let i=1;i<=2&&n.length>0;i++){const a=await t(n)||{};s=Object.assign({},s,a);n=e.filter((e=>!s[e]))}return s};
    ZS.oneToOnePCNoised2gNoised=async e=>{try{return e&&Array.isArray(e)&&0!==e.length?await ZS.resolveWithRetry(e,(e=>Xr.default.getGlobalIdsFromUserIds(e))):{}}catch(t){throw t}};
    """

    /// Runs the (patched) resolver in JavaScriptCore with a profile service that only knows `known`.
    private func run(_ patched: String, known: [String: String], fallback: String?) throws -> [String: String] {
        let ctx = try #require(JSContext())
        ctx.exceptionHandler = { _, e in Issue.record("JS exception: \(String(describing: e))") }
        ctx.evaluateScript("var window=this;var Xr={default:{getGlobalIdsFromUserIds:async(ids)=>{const o={};for(const i of ids)if(\(jsObject(known))[i])o[i]=\(jsObject(known))[i];return o}}};")
        if let fallback { ctx.evaluateScript("window.__swzGlobalIdFallback=\(fallback);") }
        ctx.evaluateScript(patched)
        ctx.evaluateScript("var __out=null;ZS.oneToOnePCNoised2gNoised(['1111111111','2222222222']).then(r=>{__out=JSON.stringify(r)});")
        // Drain the promise job queue (JSC runs microtasks when the outermost evaluate returns).
        let out = ctx.evaluateScript("__out")?.toString() ?? "null"
        return (try? JSONSerialization.jsonObject(with: Data(out.utf8)) as? [String: String]) ?? [:]
    }

    private func jsObject(_ d: [String: String]) -> String {
        "{" + d.map { "\"\($0.key)\":\"\($0.value)\"" }.joined(separator: ",") + "}"
    }

    @Test func patchedResolverMergesFallbackForUnmappedIds() throws {
        let patched = String(decoding: AsarSchemeHandler.patchSyncGlobalIdResolver(in: Data(Self.resolver.utf8)), as: UTF8.self)
        #expect(patched.contains("__swzGlobalIdFallback"))
        #expect(!patched.contains("(e=>Xr.default.getGlobalIdsFromUserIds(e))"))
        let withHook = try run(patched, known: ["1111111111": "gA"], fallback: "async (ids)=>({[ids[0]]:'gB'})")
        #expect(withHook == ["1111111111": "gA", "2222222222": "gB"])
    }

    @Test func patchedResolverIsHarmlessWithoutHookOrWhenHookFails() throws {
        let patched = String(decoding: AsarSchemeHandler.patchSyncGlobalIdResolver(in: Data(Self.resolver.utf8)), as: UTF8.self)
        #expect(try run(patched, known: ["1111111111": "gA"], fallback: nil) == ["1111111111": "gA"])
        #expect(try run(patched, known: ["1111111111": "gA"], fallback: "async()=>{throw new Error('x')}") == ["1111111111": "gA"])
    }

    @Test func ambiguousOrMissingShapeIsServedUnchanged() {
        let none = Data("var a=1;".utf8)
        #expect(AsarSchemeHandler.patchSyncGlobalIdResolver(in: none) == none)
        let twice = Data((Self.resolver + Self.resolver).utf8)
        #expect(AsarSchemeHandler.patchSyncGlobalIdResolver(in: twice) == twice)
    }

    @Test func installedBundleIsPatchedExactlyOnce() throws {
        guard FileManager.default.fileExists(atPath: OfficialZaloInstall.defaultAppURL.path) else { return }
        let info = try OfficialZaloInstall.locate()
        guard let path = info.asar.paths(withPrefix: "pc-dist/lazy/main-startup.").first(where: { $0.hasSuffix(".js") }),
              let data = info.asar.read(path) else { return }
        let patched = AsarSchemeHandler.patchSyncGlobalIdResolver(in: data)
        #expect(patched != data, "resolver shape no longer matches this Zalo version")
        let text = String(decoding: patched, as: UTF8.self)
        #expect(text.components(separatedBy: "__swzGlobalIdFallback").count == 2)   // exactly one patched call site
    }
}
