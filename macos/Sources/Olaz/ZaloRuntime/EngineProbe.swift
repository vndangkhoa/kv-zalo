import AppKit
import WebKit

/// `Olaz --engine-probe [login|main] [seconds]`
///
/// Development harness: boots the official renderer with the Swift bridge, shows
/// its DOM in a debug window, and prints console output plus every bridge call,
/// so missing bridge members are easy to spot.
@MainActor
enum EngineProbe {
    static func run(arguments: [String]) {
        let page: OfficialClientHost.Page = arguments.contains("main") ? .main : .login
        let seconds = arguments.compactMap(Double.init).first ?? 30
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)

        let install: OfficialZaloInstall.Info
        do {
            install = try OfficialZaloInstall.locate()
        } catch {
            print("[probe] cannot load official Zalo (launch Olaz once to install it): \(error.localizedDescription)")
            exit(1)
        }
        print("[probe] Zalo \(install.version) from \(install.appURL.path)")
        OfficialClientHost.diagnostics = { print($0) }

        let platform = ZaloPlatform(appVersion: install.version)
        let mainProcess = ZaloMainProcess(platform: platform)
        let ua = OfficialClientHost.userAgent(clientType: platform.identity.clientType, clientVersion: platform.identity.clientVersion)
        let host = OfficialClientHost(install: install, userAgent: ua) { bridge in
            BridgeImplementation.install(on: bridge, platform: platform, mainProcess: mainProcess)
        }
        mainProcess.host = host
        host.onPageEvent = { event in
            switch event {
            case .startedLoading: mainProcess.pageDidStartLoading()
            case .finishedLoading(let page): print("[probe] loaded \(page.rawValue)"); mainProcess.pageDidFinishLoading(page)
            case .failed(let error): print("[probe] navigation failed: \(error.localizedDescription)")
            }
        }
        host.onState = { state in
            var s = state
            if let qr = s["qr"] as? String {
                if let out = ProcessInfo.processInfo.environment["OLAZ_QR_OUT"],
                   let comma = qr.firstIndex(of: ","), let png = Data(base64Encoded: String(qr[qr.index(after: comma)...])) {
                    try? png.write(to: URL(fileURLWithPath: out))
                }
                s["qr"] = "<\(qr.count) chars \(qr.prefix(30))…>"
            }
            print("[state] \(s)")
        }
        var seen: [String: Int] = [:]
        host.bridge.trace = { path, args in
            seen[path, default: 0] += 1
            if seen[path]! <= 3 {
                let rendered = args.map { "\($0)".prefix(120) }.joined(separator: ", ")
                print("[bridge] \(path)(\(rendered))")
            }
        }
        host.onConsole = { print($0.prefix(600)) }
        host.network.onRequestCompleted = { method, url, status in
            print("[net] \(status) \(method) \(url.host() ?? "")\(url.path())")
        }
        host.showDebugWindow()
        if arguments.contains("main") || arguments.contains("login") {
            host.load(page, query: mainProcess.pageQuery())
        } else {
            mainProcess.start()
        }
        app.activate()

        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
            Task { @MainActor in
                let fibers = await host.evaluate("""
                    const el = document.getElementById('app');
                    const key = Object.keys(el).find(k => k.startsWith('__reactContainer') || k.startsWith('_reactRootContainer'));
                    let root = el._reactRootContainer ? el._reactRootContainer._internalRoot : (key ? el[key] : null);
                    let fiber = root && root.current ? root.current : root;
                    const lines = [];
                    const name = (f) => (f.type && (f.type.displayName || f.type.name)) || (typeof f.type === 'string' ? f.type : 'tag' + f.tag);
                    const walk = (f, d) => { let n = 0; while (f && lines.length < 160) { const portal = f.tag === 4 ? (() => { const c = f.stateNode && f.stateNode.containerInfo; return c ? ' [portal → ' + c.nodeName + (c.id ? '#' + c.id : '') + '.' + String(c.className || '').slice(0, 40) + ' connected=' + c.isConnected + ' kids=' + c.childElementCount + ' parent=' + (c.parentNode ? c.parentNode.nodeName + '.' + String(c.parentNode.className || '').slice(0,30) : 'none') + ']' : ''; })() : '';
                        const susp = portal + (f.tag === 13 ? (f.memoizedState ? ' [SUSPENDED]' : ' [suspense ok]') : ''); const off = f.tag === 22 || f.tag === 23 ? ' [offscreen ' + JSON.stringify(f.memoizedProps && f.memoizedProps.mode) + ']' : '';
                        if (d < 40) lines.push('  '.repeat(Math.min(d, 20)) + name(f) + susp + off + (f.lanes ? ' lanes=' + f.lanes : ''));
                        if (f.child) walk(f.child, d + 1); f = f.sibling; if (++n > 30) break; } };
                    walk(fiber, 0);
                    return 'ROOT pendingLanes=' + (root && root.pendingLanes) + ' callbackNode=' + !!(root && root.callbackNode) + '\\n' + lines.join('\\n');
                    """)
                print("[probe] fibers\n\(fibers ?? "nil")")
                let apis = await host.evaluate("return JSON.stringify(Object.fromEntries(['requestIdleCallback','cancelIdleCallback','BroadcastChannel','SharedWorker','OffscreenCanvas','createImageBitmap','showOpenFilePicker','CompressionStream','structuredClone'].map(k => [k, typeof window[k]]).concat([['performance.memory', typeof performance.memory], ['navigator.userAgentData', typeof navigator.userAgentData], ['navigator.locks', typeof navigator.locks], ['window.chrome', typeof window.chrome], ['navigator.connection', typeof navigator.connection]])))")
                print("[probe] apis \(apis ?? "nil")")
                let summary = await host.evaluate("""
                    const walk = (el, d) => { if (d > 7) return ''; const cs = getComputedStyle(el); const tag = el.tagName.toLowerCase() + (el.id ? '#' + el.id : '') + (el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\\s+/).slice(0,3).join('.') : '');
                      const flags = (cs.display === 'none' ? ' [none]' : '') + (cs.visibility === 'hidden' ? ' [hidden]' : '') + (cs.opacity === '0' ? ' [op0]' : '');
                      return '  '.repeat(d) + tag + flags + '\\n' + Array.from(el.children).slice(0, 12).map(c => walk(c, d + 1)).join(''); };
                    const qrEls = Array.from(document.querySelectorAll('[class*=qr], [class*=login]')).slice(0, 25).map(e => e.tagName + '.' + String(e.className).slice(0, 60) + ' ' + Math.round(e.getBoundingClientRect().width) + 'x' + Math.round(e.getBoundingClientRect().height));
                    return 'ELEMENTS ' + document.querySelectorAll('*').length + ' rootKids ' + document.getElementById('app').children.length + '\\n' + qrEls.join('\\n') + '\\n' + JSON.stringify({title: document.title, text: document.body ? document.body.innerText.slice(0, 300) : '', imgs: Array.from(document.images).map(i => i.src.slice(0, 60)), canvas: document.querySelectorAll('canvas').length}) + '\\n' + walk(document.body, 0).slice(0, 6000);
                    """)
                print("[probe] final \(summary ?? "nil")")
                exit(0)
            }
        }
        app.run()
    }
}
