import Testing
import Foundation
import JavaScriptCore
@testable import Olaz

/// Runs the real `poll-adapter.js` / `card-actions-adapter.js` in JavaScriptCore against fake official modules, with a fake clock
/// (`__pump` advances it and fires due timers) so the waits in the adapters run instantly. Nothing here touches a network or the app.
final class ScriptHarness {
    let ctx: JSContext
    var failures: [String] = []

    init(prelude: String) {
        ctx = JSContext()!
        ctx.exceptionHandler = { [unowned self] _, e in failures.append(String(describing: e)) }
        ctx.evaluateScript("""
        var window = this; var location = { pathname: '/index.html' };
        var __now = 1.7e12, __timers = [];
        Date.now = function () { return __now; };
        var setTimeout = function (f, ms) { __timers.push({ f: f, at: __now + (ms || 0) }); return __timers.length; };
        var clearTimeout = function () {}; var setInterval = function () { return 0; }; var clearInterval = function () {};
        function __pump(step) {
          __now += step;
          var due = __timers.filter(function (t) { return t.at <= __now; });
          __timers = __timers.filter(function (t) { return t.at > __now; });
          due.sort(function (a, b) { return a.at - b.at; }).forEach(function (t) { t.f(); });
        }
        var __results = {};
        """)
        ctx.evaluateScript(prelude)
    }

    func load(_ name: String, file: StaticString = #filePath) {
        let url = URL(fileURLWithPath: "\(file)").deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/Olaz/HostScripts/\(name)")
        guard let source = try? String(contentsOf: url, encoding: .utf8) else { failures.append("missing \(name)"); return }
        ctx.evaluateScript(source)
    }

    /// Starts `expression` (a promise) under `slot` and runs the fake clock until it settles or `steps` pumps passed.
    @discardableResult
    func start(_ slot: String, _ expression: String) -> Self {
        ctx.evaluateScript("__results['\(slot)'] = undefined; Promise.resolve(\(expression)).then(function (r) { __results['\(slot)'] = JSON.stringify(r === undefined ? null : r); }, function (e) { __results['\(slot)'] = 'REJECTED ' + e; });")
        return self
    }

    func pump(steps: Int = 80, step: Int = 400) {
        for _ in 0..<steps { ctx.evaluateScript("__pump(\(step))") }
    }

    func result(_ slot: String) -> [String: Any]? {
        guard let text = ctx.evaluateScript("__results['\(slot)']")?.toString(), text != "undefined", text != "null" else { return nil }
        return (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any]
    }

    func isSettled(_ slot: String) -> Bool { ctx.evaluateScript("__results['\(slot)'] !== undefined")?.toBool() ?? false }

    func run(_ slot: String, _ expression: String, steps: Int = 80, step: Int = 400) -> [String: Any]? {
        start(slot, expression).pump(steps: steps, step: step)
        return result(slot)
    }

    /// Like `run`, but stops pumping the moment the promise settles (so the fake clock does not run past what the test is about).
    func runUntilSettled(_ slot: String, _ expression: String, step: Int = 400, limit: Int = 400) -> [String: Any]? {
        start(slot, expression)
        for _ in 0..<limit where !isSettled(slot) { ctx.evaluateScript("__pump(\(step))") }
        return result(slot)
    }

    func number(_ expression: String) -> Int { Int(ctx.evaluateScript(expression)?.toInt32() ?? -1) }
    func text(_ expression: String) -> String { ctx.evaluateScript(expression)?.toString() ?? "" }
}

// MARK: - Poll pin / unpin

@Suite struct PollPinScriptTests {
    /// Fake official modules: the poll manager, the profile service, constants, config and one board-topic manager per conversation.
    /// `loadHangs`: the board has no data and its `loadTopics` never settles. `multi`: how `unpinMultiTopics` behaves ("ok", "hang" = never settles
    /// and nothing changes, "hangGone" = never settles but the board later drops the topic, as the official background retry would).
    private func harness(canPin: Bool = true, topicsEnabled: Bool = true, limitOn: Bool = true, pinLimit: Int = 3, topics: String = "[]",
                         loadHangs: Bool = false, multi: String = "ok") -> ScriptHarness {
        let h = ScriptHarness(prelude: """
        window.__zaloPollTestName = '__zaloPollT'; window.__zaloPollTestSink = function () {};
        window.__zaloStore = { getState: function () { return { user: { userId: '1' } }; } };
        window.__zaloDI = function () { return []; };
        window.__zaloEachExport = function () {};
        var TOKEN = { name: 'poll-manager' }, calls = [];
        var cfg = { groupPoll: {}, group_topics: { enable: \(topicsEnabled ? 1 : 0), enable_pin_limit: \(limitOn ? 1 : 0) } };
        var mgr = {};
        ['getPollDetail', 'getPollDetailFromServer', 'votePoll', 'lockPoll', 'createPoll', 'sharePollInGroup', 'isEnableClose', 'updatePollCache', 'getPollDetailSync', 'isAdmin']
          .forEach(function (k) { mgr[k] = function () { return k === 'getPollDetailSync' ? { poll_id: 77, question: 'Ăn gì?', options: [] } : null; }; });
        mgr.isEnablePin = function () { return \(canPin); };
        mgr.isAdmin = function () { return \(canPin); };
        mgr.createPoll = async function (o) { calls.push(['create', o.pollData.pinAct]); };
        var board = {
          pinLimit: \(pinLimit), loads: 0, list: \(topics),
          topicStore: { hasGroup: function () { return !\(loadHangs); }, getTopics: function () { return board.list.slice(); } },
          loadTopics: function () { board.loads++; return \(loadHangs) ? new Promise(function () {}) : Promise.resolve(); },
          callCreateOrPinTopic: function (conv, topic, pre, ok, fail) { calls.push(['pin', conv, topic.topicId, topic.type, topic.pollData.question]); board.list.unshift({ id: String(topic.topicId), type: topic.type, params: {} }); ok({}); },
          callUnpinTopic: async function (conv, t) { calls.push(['unpin', conv, t.id, t.type]); board.list = board.list.filter(function (x) { return !(x.id === t.id && x.type === t.type); }); },
          unpinMultiTopics: function (conv, ts) {
            calls.push(['multi', conv, ts.map(function (x) { return x.id; }).join(',')]);
            var drop = function () { board.list = board.list.filter(function (x) { return ts.indexOf(x) < 0; }); };
            if ('\(multi)' === 'ok') { drop(); return Promise.resolve(); }
            if ('\(multi)' === 'hangGone') setTimeout(drop, 5000);
            return new Promise(function () {});
          }
        };
        var mods = {
          C8zZ: { GroupPollManager: TOKEN },
          jDHv: { ModuleContainer: { resolve: function (t) { return t === TOKEN ? mgr : null; } } },
          UiPd: { default: { getMiniInfo: function () { return null; }, getProfileFriendByIds: async function () {} } },
          '1pet': { SRC_GET_PROFILE: { FETCH_MINI_INFO: 5 }, POLL_TOPIC: 9, REMINDER_TOPIC: 7, MSG_TOPIC: 8, TEXT_TOPIC: 6 },
          Gm1y: { default: { getGroupByIdSync: function () { return { setting: { setTopicOnly: \(canPin ? 0 : 1), lockCreatePoll: 0 } }; } } },
          NDmK: { default: cfg },
          jnJr: { a: { getInstance: function () { return board; } } }
        };
        window.__zaloRequire = { c: {} };
        Object.keys(mods).forEach(function (k) { window.__zaloRequire.c[k] = { exports: mods[k] }; });
        """)
        h.load("poll-adapter.js")
        return h
    }

    private func calls(_ h: ScriptHarness) -> String { h.text("JSON.stringify(calls)") }

    @Test func stateShowsPinnedAndPermissionAndNeverGuessesTheBoard() {
        let pinned = harness(topics: #"[{"id":"77","type":9,"params":{}}]"#)
        let state = pinned.run("s", "window.__zaloPollT.pinState('g5', '77')")
        #expect(state?["ready"] as? Bool == true && state?["pinned"] as? Bool == true && state?["canPin"] as? Bool == true && state?["limit"] as? Int == 3)
        #expect(pinned.number("board.loads") == 0, "a board that is already loaded is not read again")
        // A different topic type with the same id is not this poll.
        let other = harness(topics: #"[{"id":"77","type":8,"params":{}}]"#)
        #expect(other.run("s", "window.__zaloPollT.pinState('g5', '77')")?["pinned"] as? Bool == false)
        // Not offered: the legacy single-notice board, a private chat, a member without the right.
        #expect(harness(topicsEnabled: false).run("s", "window.__zaloPollT.pinState('g5', '77')")?["ready"] as? Bool == false)
        #expect(harness().run("s", "window.__zaloPollT.pinState('5', '77')")?["ready"] as? Bool == false)
        let member = harness(canPin: false).run("s", "window.__zaloPollT.pinState('g5', '77')")
        #expect(member?["ready"] as? Bool == true && member?["canPin"] as? Bool == false)
    }

    @Test func pinWithRoomSendsTheOfficialCallOnly() {
        let h = harness(pinLimit: 3, topics: #"[{"id":"1","type":8,"params":{}}]"#)
        let result = h.run("p", "window.__zaloPollT.pin('g5', '77')")
        #expect(result?["ok"] as? Bool == true)
        #expect(calls(h) == #"[["pin","g5",77,9,"Ăn gì?"]]"#, "raw numeric poll id, POLL_TOPIC, nothing unpinned")
        // Already pinned: nothing is sent again.
        #expect(h.run("again", "window.__zaloPollT.pin('g5', '77')")?["ok"] as? Bool == true)
        #expect(h.number("calls.length") == 1)
    }

    @Test func fullBoardAsksBeforeReplacingTheOldestAndOnlyForThatTopic() {
        let board = #"[{"id":"30","type":8,"params":{"title":"mới"}},{"id":"20","type":9,"params":{"question":"Họp?"}},{"id":"10","type":8,"params":{"title":"cũ nhất"}}]"#
        let h = harness(pinLimit: 3, topics: board)
        let ask = h.run("a", "window.__zaloPollT.pin('g5', '77')")
        #expect(ask?["error"] as? String == "confirm" && ask?["ref"] as? String == "8:10" && ask?["limit"] as? Int == 3)
        #expect((ask?["replace"] as? [String: Any])?["title"] as? String == "cũ nhất")
        #expect(h.number("calls.length") == 0, "asking sends nothing")
        // A confirmation for some other topic is not accepted.
        #expect(h.run("w", "window.__zaloPollT.pin('g5', '77', '8:30')")?["error"] as? String == "confirm")
        #expect(h.number("calls.length") == 0)
        let done = h.run("c", "window.__zaloPollT.pin('g5', '77', '8:10')")
        #expect(done?["ok"] as? Bool == true)
        #expect(calls(h) == #"[["multi","g5","10"],["pin","g5",77,9,"Ăn gì?"]]"#, "the oldest goes first, then the new pin")
    }

    @Test func overflowByMoreThanOneAndMissingRightAreRefused() {
        let many = #"[{"id":"4","type":8},{"id":"3","type":8},{"id":"2","type":8},{"id":"1","type":8}]"#
        let h = harness(pinLimit: 2, topics: many)
        #expect(h.run("l", "window.__zaloPollT.pin('g5', '77', '8:1')")?["error"] as? String == "limit")
        let member = harness(canPin: false)
        #expect(member.run("f", "window.__zaloPollT.pin('g5', '77')")?["error"] as? String == "forbidden")
        #expect(member.run("g", "window.__zaloPollT.unpin('g5', '77')")?["error"] as? String == "forbidden")
        #expect(member.number("calls.length") == 0)
        #expect(harness(limitOn: false, pinLimit: 1, topics: #"[{"id":"1","type":8}]"#).run("n", "window.__zaloPollT.pin('g5', '77')")?["ok"] as? Bool == true,
                "no replace step when the pin limit is off")
    }

    @Test func aBoardThatNeverLoadsDoesNotHangAndIsRetriedLater() {
        let h = harness(loadHangs: true)
        // Every call settles (bounded) and offers nothing; the stuck read is not repeated at once.
        #expect(h.runUntilSettled("a", "window.__zaloPollT.pinState('g5', '77')")?["ready"] as? Bool == false)
        #expect(h.runUntilSettled("b", "window.__zaloPollT.pin('g5', '77')")?["error"] as? String == "unavailable")
        #expect(h.runUntilSettled("c", "window.__zaloPollT.unpin('g5', '77')")?["error"] as? String == "unavailable")
        #expect(h.number("board.loads") == 1)
        h.pump(steps: 60)   // past the retry delay
        #expect(h.runUntilSettled("d", "window.__zaloPollT.pinState('g5', '77')")?["ready"] as? Bool == false)
        #expect(h.number("board.loads") == 2, "retried after the delay")
        // A new session forgets the failed read.
        h.ctx.evaluateScript("window.__zaloPollT.reset();")
        _ = h.runUntilSettled("e", "window.__zaloPollT.pinState('g5', '77')")
        #expect(h.number("board.loads") == 3)
    }

    @Test func pinNeedsTheOfficialAllowanceNotOnlyIsEnablePin() {
        // `isEnablePin` says yes (setTopicOnly is not set) but `_checkUserAllowance` wants an admin or `setTopicOnly === 0`.
        let h = harness()
        h.ctx.evaluateScript("mods.Gm1y.default.getGroupByIdSync = function () { return { setting: {} }; }; mgr.isAdmin = function () { return false; };")
        let state = h.run("s", "window.__zaloPollT.pinState('g5', '77')")
        #expect(state?["ready"] as? Bool == true && state?["canPin"] as? Bool == false)
        #expect(h.run("p", "window.__zaloPollT.pin('g5', '77')")?["error"] as? String == "forbidden")
        #expect(h.number("calls.length") == 0)
    }

    @Test func replaceStepNeverReportsFailureWhenTheOutcomeIsUncertain() {
        let board = #"[{"id":"30","type":8},{"id":"20","type":8},{"id":"10","type":8}]"#
        // The unpin never answers and the topic stays: unknown, and the new pin is not sent.
        let stuck = harness(pinLimit: 3, topics: board, multi: "hang")
        #expect(stuck.run("a", "window.__zaloPollT.pin('g5', '77', '8:10')", steps: 160)?["error"] as? String == "unknown")
        #expect(stuck.text("JSON.stringify(calls.map(function (c) { return c[0]; }))") == #"["multi"]"#)
        // The unpin never answers but the board drops the topic shortly after: the pin continues.
        let late = harness(pinLimit: 3, topics: board, multi: "hangGone")
        #expect(late.run("b", "window.__zaloPollT.pin('g5', '77', '8:10')", steps: 160)?["ok"] as? Bool == true)
        #expect(late.text("JSON.stringify(calls.map(function (c) { return c[0]; }))") == #"["multi","pin"]"#)
    }

    @Test func createWithPinOnlyAsksForThePinWhenTheBoardHasRoom() {
        let spec = "{question:'Q', options:['a','b'], pin:true}"
        let roomy = harness(pinLimit: 3, topics: #"[{"id":"1","type":8}]"#)
        let ok = roomy.run("a", "window.__zaloPollT.create('g5', \(spec))")
        #expect(ok?["ok"] as? Bool == true && ok?["pinSkipped"] == nil)
        #expect(roomy.text("JSON.stringify(calls)") == #"[["create",true]]"#)
        // Full board: the official pin step would open the invisible replace dialog, so the poll is created unpinned.
        let full = harness(pinLimit: 1, topics: #"[{"id":"1","type":8}]"#)
        let skipped = full.run("b", "window.__zaloPollT.create('g5', \(spec))")
        #expect(skipped?["ok"] as? Bool == true && skipped?["pinSkipped"] as? Bool == true)
        #expect(full.text("JSON.stringify(calls)") == #"[["create",false]]"#)
        // Board unreadable: not guessed either.
        let unknown = harness(loadHangs: true)
        #expect(unknown.run("c", "window.__zaloPollT.create('g5', \(spec))", steps: 160)?["pinSkipped"] as? Bool == true)
        #expect(unknown.text("JSON.stringify(calls)") == #"[["create",false]]"#)
    }

    @Test func unpinUsesTheStoredTopicIdentity() {
        let h = harness(topics: #"[{"id":"77","type":9,"params":{}}]"#)
        #expect(h.run("u", "window.__zaloPollT.unpin('g5', '77')")?["ok"] as? Bool == true)
        #expect(calls(h) == #"[["unpin","g5","77",9]]"#)
        // Not pinned: nothing to do, nothing sent.
        #expect(h.run("v", "window.__zaloPollT.unpin('g5', '77')")?["ok"] as? Bool == true)
        #expect(h.number("calls.length") == 1)
    }
}

// MARK: - Reminders

@Suite struct ReminderScriptTests {
    /// Fake chat core + a reminder controller class shaped like the official one (module "eD/0").
    /// `gate`: what the official `legacy-bridge` `f()` answers; nil = the module is not there.
    private func harness(gate: String? = "true") -> ScriptHarness {
        let h = ScriptHarness(prelude: """
        window.__zaloStore = {};
        var created = 0, disposed = [], responds = [], rsvpResolvers = [], started = [], ctls = {};
        var EVENT = { NONE: 0, ACCEPT: 1, REJECT: 2 };
        function Ctl(m, conv) {
          this.m = m; this.listeners = [];
          this.state = { variant: 'invite', ecardStartTime: 1, pendingRsvp: null, deleted: false, permission: { canRsvp: true, canView: true },
            reminder: { id: m.rid, creatorId: '9', scope: 'group', audience: 'all', title: 'Họp', startTime: 1, rsvp: { accept: 0, reject: 0, mine: 0 } } };
          created++; ctls[m.msgId] = this;
        }
        Ctl.prototype.getState = function () { return this.state; };
        Ctl.prototype.subscribe = function (f) { this.listeners.push(f); var s = this; return function () { s.listeners = s.listeners.filter(function (x) { return x !== f; }); }; };
        Ctl.prototype.start = async function () { started.push(this.m.msgId); };
        Ctl.prototype.dispose = function () { disposed.push(this.m.msgId); };
        Ctl.prototype.loadStored = function () {}; Ctl.prototype.derive = function () {};
        Ctl.prototype.set = function (patch) { Object.assign(this.state, patch); var st = this.state; this.listeners.forEach(function (f) { f(st); }); };
        Ctl.prototype.respond = function (e) {
          var self = this;
          if (this.state.pendingRsvp !== null) return Promise.resolve();
          responds.push([this.m.msgId, e]);
          this.set({ pendingRsvp: e });
          return new Promise(function (resolve) {
            rsvpResolvers.push(function () {
              var r = Object.assign({}, self.state.reminder); r.rsvp = Object.assign({}, r.rsvp, { mine: e });
              self.set({ reminder: r, pendingRsvp: null }); resolve();
            });
          });
        };
        var messages = {};
        function msg(id, rid) {
          messages[id] = { msgType: 19, msgId: id, rid: rid, message: { params: JSON.stringify({ actions: [{ actionId: 'action.open.reminder', data: JSON.stringify({ topicId: rid }) }] }) } };
        }
        var core = {
          S: {}, safe: function (f, d) { try { var v = f(); return v === undefined ? d : v; } catch (e) { return d; } },
          str: function (v) { return typeof v === 'string' ? v : ''; }, own: function (o, k) { return Object.prototype.hasOwnProperty.call(o, k); },
          parseJSON: function (v) { if (typeof v !== 'string') return v; try { return JSON.parse(v); } catch (e) { return null; } },
          diag: function () {}, scrubErr: function () { return ''; }, isGroupId: function (s) { return /^g/.test(s); },
          isMeUid: function () { return false; }, meNow: function () { return '1'; }, nameOf: function (u) { return 'Tên ' + u; }, avatarOf: function () { return ''; },
          MSG: { ECARD: 19 }, findMessage: function (conv, id) { return messages[id] || null; }, ready: async function () { return true; },
          sleep: function (ms) { return new Promise(function (r) { setTimeout(r, ms); }); }, controller: function () { return null; }
        };
        var listed = [], fetched = [], known = { '11': { dName: 'An', avatar: 'a' } }, lists = { accept: ['11', '12', 'x', '11', '99'], reject: ['13'] }, listFails = false;
        var SVC_TOKEN = { name: 'reminder-detail-service' };
        var service = { listResponders: async function (r) { listed.push(r.id); if (listFails) throw new Error('net'); return lists; } };
        core.S.friends = { getMiniInfo: function (u) { return known[u] || null; },
          getProfileFriendByIds: async function (ids) { fetched.push(ids.join(',')); ids.forEach(function (u) { if (u !== '99') known[u] = { dName: 'N' + u, avatar: '' }; }); } };
        core.S.codes = { FETCH_MINI_INFO: 5 };
        window.__zaloChat = { __core: core };
        var mods = \(gate.map { "{ '2Jz/': { f: function () { return \($0); } } }" } ?? "{}");
        mods['jDHv'] = { ModuleContainer: { resolve: function (t) { return t === SVC_TOKEN ? service : null; } } };
        mods['GHUl'] = { a: SVC_TOKEN };
        window.__zaloRequire = { c: {} };
        Object.keys(mods).forEach(function (k) { window.__zaloRequire.c[k] = { exports: mods[k] }; });
        window.__zaloEachExport = function (req, visit) {
          visit({ a: Ctl });
          visit({ FRIEND_REQUEST_SRC_ECARD: 1, MAX_CHAR_SAY_HI: 150, EVENT_STATUS: EVENT });
        };
        window.__zaloDI = function () { return []; };
        """)
        h.load("card-actions-adapter.js")
        return h
    }

    private let api = "window.__zaloChat"

    @Test func installsAndFollowsAController() {
        let h = harness()
        #expect(h.failures.isEmpty, "\(h.failures)")
        h.ctx.evaluateScript("msg('m1', '55');")
        let snap = h.run("w", "\(api).reminderWatch('g1', 'm1', '')")
        #expect(snap?["variant"] as? String == "invite" && snap?["rid"] as? String == "55" && snap?["isGroup"] as? Bool == true)
        #expect(snap?["accepted"] as? Int == 0 && snap?["pending"] as? String == "")
    }

    @Test func officialGateAndBrokenDataKeepTheControllerOut() {
        let closed = harness(gate: "false")
        closed.ctx.evaluateScript("msg('m1', '55');")
        #expect(closed.run("w", "\(api).reminderWatch('g1', 'm1', '')") == nil)
        #expect(closed.number("created") == 0, "gate closed: the official client draws its legacy card, no controller is built")
        let unknown = harness(gate: nil)   // the module cannot be found → unknown → not blocked
        unknown.ctx.evaluateScript("msg('m1', '55');")
        #expect(unknown.run("w", "\(api).reminderWatch('g1', 'm1', '')")?["rid"] as? String == "55")
        let bad = harness()
        bad.ctx.evaluateScript("""
        messages.b1 = { msgType: 19, msgId: 'b1', rid: '1', message: { params: JSON.stringify({ actions: [{ actionId: 'action.open.reminder', data: 'not json' }] }) } };
        messages.b2 = { msgType: 19, msgId: 'b2', rid: '1', message: { params: JSON.stringify({ actions: [{ actionId: 'action.open.reminder', data: JSON.stringify({ title: 'x' }) }] }) } };
        """)
        #expect(bad.run("b1", "\(api).reminderWatch('g1', 'b1', '')") == nil && bad.run("b2", "\(api).reminderWatch('g1', 'b2', '')") == nil)
        #expect(bad.number("created") == 0)
    }

    @Test func evictionIsLeastRecentlyUsedAndSparesWatchedControllers() {
        let h = harness()
        // Five cards on screen (each holds a waiting watch), then many more that nobody waits on.
        for i in 0..<5 { h.ctx.evaluateScript("msg('w\(i)', 'r\(i)');") }
        for i in 0..<5 {
            let first = h.run("s\(i)", "\(api).reminderWatch('g1', 'w\(i)', '')", steps: 1, step: 10)
            let sig = first?["sig"] as? String ?? ""
            h.start("hold\(i)", "\(api).reminderWatch('g1', 'w\(i)', \(jsString(sig)))")
        }
        for i in 0..<45 {
            h.ctx.evaluateScript("msg('u\(i)', 'q\(i)');")
            _ = h.run("x\(i)", "\(api).reminderWatch('g1', 'u\(i)', '')", steps: 1, step: 10)
        }
        let disposed = h.text("disposed.join(',')").split(separator: ",").map(String.init)
        #expect(disposed.count == 10, "50 controllers, 40 kept")
        #expect(disposed.allSatisfy { $0.hasPrefix("u") }, "watched controllers are never evicted: \(disposed)")
        #expect(disposed == (0..<10).map { "u\($0)" }, "oldest unused first")
        // The watched cards are still served by their original controllers (no re-creation).
        #expect(h.number("created") == 50)
    }

    @Test func oneAnswerPerReminderAcrossItsCards() {
        let h = harness()
        h.ctx.evaluateScript("msg('alert', '55'); msg('invite', '55');")
        _ = h.run("a", "\(api).reminderWatch('g1', 'alert', '')", steps: 1, step: 10)
        _ = h.run("b", "\(api).reminderWatch('g1', 'invite', '')", steps: 1, step: 10)
        h.start("r1", "\(api).reminderRespond('g1', 'invite', 'accept')").pump(steps: 2, step: 10)
        // The second card (another message of the same reminder) is refused while the first answer is in flight.
        let second = h.run("r2", "\(api).reminderRespond('g1', 'alert', 'reject')", steps: 2, step: 10)
        #expect(second?["ok"] as? Bool == false && second?["pending"] as? Bool == true)
        #expect(h.number("responds.length") == 1, "only one official respond call")
        // Both cards show it as pending.
        #expect(h.run("pa", "\(api).reminderWatch('g1', 'alert', '')", steps: 1, step: 10)?["pending"] as? String == "accept")
        #expect(h.run("pi", "\(api).reminderWatch('g1', 'invite', '')", steps: 1, step: 10)?["pending"] as? String == "accept")
        h.ctx.evaluateScript("rsvpResolvers.shift()();")
        h.pump(steps: 3, step: 10)
        let first = h.result("r1")
        #expect(first?["ok"] as? Bool == true && (first?["snapshot"] as? [String: Any])?["pending"] as? String == "")
        // After it settled the reminder can be answered again.
        h.start("r3", "\(api).reminderRespond('g1', 'alert', 'reject')").pump(steps: 2, step: 10)
        #expect(h.number("responds.length") == 2)
    }

    @Test func respondersAreReadOnlyWhenAskedAndOnlyForGroupReminders() {
        let h = harness()
        h.ctx.evaluateScript("msg('m1', '55');")
        // Nothing is read just by showing the card.
        _ = h.run("w", "\(api).reminderWatch('g1', 'm1', '')", steps: 1, step: 10)
        #expect(h.number("listed.length") == 0)
        let list = h.run("l", "\(api).reminderResponders('g1', 'm1')", steps: 5, step: 100)
        #expect(list?["ok"] as? Bool == true && list?["acceptTotal"] as? Int == 3 && list?["rejectTotal"] as? Int == 1, "ids de-duplicated, junk dropped, counts include people without a profile")
        let accept = list?["accept"] as? [[String: Any]] ?? []
        #expect(accept.map { $0["uid"] as? String } == ["11", "12"] && accept.first?["name"] as? String == "Tên 11")
        #expect(accept.count == 2, "a user without mini info is not drawn (official row)")
        #expect(h.text("fetched.join('|')") == "12,99,13", "only profiles the service lacks are fetched")
        #expect(h.text("listed.join(',')") == "55")
        // A failed read degrades to "not ok" (the page offers a retry); the cached lists of the reminder are used when it has some.
        h.ctx.evaluateScript("listFails = true;")
        #expect(h.run("f", "\(api).reminderResponders('g1', 'm1')", steps: 5, step: 100)?["ok"] as? Bool == false)
        // A deleted reminder is not read.
        h.ctx.evaluateScript("ctls.m1.set({ deleted: true });")
        #expect(h.run("d", "\(api).reminderResponders('g1', 'm1')", steps: 5, step: 100) == nil)
        #expect(h.number("listed.length") == 2, "nothing read for a deleted reminder")
        h.ctx.evaluateScript("ctls.m1.set({ deleted: false });")
        // "Only me" reminders have no such page.
        h.ctx.evaluateScript("msg('m2', '56'); listFails = false;")
        _ = h.run("w2", "\(api).reminderWatch('g1', 'm2', '')", steps: 1, step: 10)
        h.ctx.evaluateScript("ctls.m2.set({ reminder: Object.assign({}, ctls.m2.state.reminder, { audience: 'onlyMe' }) });")
        #expect(h.run("o", "\(api).reminderResponders('g1', 'm2')", steps: 5, step: 100) == nil)
        h.ctx.evaluateScript("ctls.m2.set({ reminder: Object.assign({}, ctls.m2.state.reminder, { audience: 'all', scope: 'oneOne' }) });")
        #expect(h.run("p", "\(api).reminderResponders('g1', 'm2')", steps: 5, step: 100) == nil)
        #expect(h.number("listed.length") == 2, "no read for reminders without the page")
        // The controller's own stored-data read finishes before the list is read; a new session drops every controller and answer slot.
        h.ctx.evaluateScript("started.length = 0; disposed.length = 0;")
        _ = h.run("rs", "\(api).cardReset()", steps: 1, step: 10)
        #expect(h.text("disposed.slice().sort().join(',')") == "m1,m2")
    }

    @Test func aTimeoutIsPendingNotAnErrorAndKeepsThePendingStateUntilItSettles() {
        let h = harness()
        h.ctx.evaluateScript("msg('invite', '55');")
        let late = h.run("r1", "\(api).reminderRespond('g1', 'invite', 'accept')", steps: 90, step: 400)   // 36 s of fake time, never answered
        #expect(late?["ok"] as? Bool == false && late?["pending"] as? Bool == true)
        #expect((late?["text"] as? String)?.contains("Đang xử lý") == true)
        #expect((late?["snapshot"] as? [String: Any])?["pending"] as? String == "accept", "the card keeps its buttons disabled")
        // A retry does not start a second answer.
        let retry = h.run("r2", "\(api).reminderRespond('g1', 'invite', 'reject')", steps: 2, step: 10)
        #expect(retry?["pending"] as? Bool == true && h.number("responds.length") == 1)
        // The late answer lands: the state clears and the new value is shown.
        h.ctx.evaluateScript("rsvpResolvers.shift()();")
        h.pump(steps: 3, step: 10)
        let after = h.run("w", "\(api).reminderWatch('g1', 'invite', '')", steps: 1, step: 10)
        #expect(after?["pending"] as? String == "" && after?["mine"] as? String == "accept")
    }
}

private func jsString(_ s: String) -> String {
    let data = try? JSONSerialization.data(withJSONObject: [s], options: [])
    let text = data.flatMap { String(data: $0, encoding: .utf8) } ?? "[\"\"]"
    return String(text.dropFirst().dropLast())
}

// MARK: - Swift models

@Suite struct ReminderPinModelTests {
    private func json(_ text: String) throws -> Any {
        try JSONSerialization.jsonObject(with: Data(text.utf8), options: [.fragmentsAllowed])
    }

    @Test func snapshotCarriesTheReminderIdCountsAndPending() throws {
        let s = try #require(ReminderSnapshot(json: try json(#"{"variant":"invite","rid":"55","isGroup":true,"onlyMe":false,"canRsvp":true,"accepted":3,"rejected":1,"pending":"accept","mine":"accept","sig":"x"}"#)))
        #expect(s.id == "55" && s.acceptedCount == 3 && s.rejectedCount == 1 && s.pending == .accept)
        #expect(s.hasResponders && s.responderSummary == "3 người tham gia, 1 người từ chối")
        var none = s
        none.acceptedCount = 0; none.rejectedCount = 0
        #expect(none.responderSummary == nil)
        none.acceptedCount = 0; none.rejectedCount = 2
        #expect(none.responderSummary == "2 người từ chối")
        // Not for "only me" or one-to-one reminders; hostile numbers degrade.
        #expect(try !#require(ReminderSnapshot(json: try json(#"{"variant":"invite","isGroup":true,"onlyMe":true}"#))).hasResponders)
        #expect(try !#require(ReminderSnapshot(json: try json(#"{"variant":"invite","isGroup":false}"#))).hasResponders)
        let odd = try #require(ReminderSnapshot(json: try json(#"{"variant":"invite","accepted":-4,"rejected":1e300}"#)))
        #expect(odd.acceptedCount == 0 && odd.rejectedCount >= 0)
    }

    @Test func respondersParsing() throws {
        let list = try #require(ReminderResponders(json: try json(#"{"ok":true,"accept":[{"uid":"1","name":"An","avatar":"https://x/a.jpg","isMe":true},{"uid":"1","name":"dup"},{"uid":"","name":"x"},{"uid":"2","name":"Bình","avatar":""}],"reject":[],"acceptTotal":9,"rejectTotal":0,"acceptMore":5}"#)))
        #expect(list.accept.map(\.uid) == ["1", "2"] && list.accept[0].isMe && list.accept[0].avatar != nil && list.accept[1].avatar == nil)
        #expect(list.acceptTotal == 9 && list.rejectTotal == 0 && list.reject.isEmpty && list.acceptMore == 5 && list.rejectMore == 0)
        // A total smaller than what is listed never hides people.
        #expect(try #require(ReminderResponders(json: try json(#"{"ok":true,"accept":[{"uid":"1"},{"uid":"2"}],"reject":[],"acceptTotal":1}"#))).acceptTotal == 2)
        #expect(ReminderResponders(json: try json(#"{"ok":false}"#)) == nil)
        #expect(ReminderResponders(json: nil) == nil)
    }

    @MainActor @Test func rsvpBusyIsPerReminderNotPerMessage() async throws {
        let center = CardActionCenter()
        let engine = ZaloEngine()
        // The invite card (message m1) answers; the alert card of the same reminder (message m2) sees it busy and its click is ignored.
        center.respond(engine: engine, conversation: "g1", message: "m1", reminder: "55", accept: true)
        let scope = CardActionCenter.reminderScope("55")
        #expect(center.isBusy(conversation: "g1", message: scope, ref: nil))
        #expect(center.isBusy(conversation: "g1", message: scope, ref: "rsvp:accept") && !center.isBusy(conversation: "g1", message: scope, ref: "rsvp:reject"))
        #expect(!center.isBusy(conversation: "g1", message: "m1", ref: nil), "not keyed by the message")
        center.respond(engine: engine, conversation: "g1", message: "m2", reminder: "55", accept: false)
        #expect(center.busy.count == 1)
        // Another reminder is independent.
        center.respond(engine: engine, conversation: "g1", message: "m3", reminder: "56", accept: true)
        #expect(center.busy.count == 2)
        // The environment accessor sees the shared state.
        let actions = CardActions(conversationId: "g1", messageId: "m2", center: center, engine: engine)
        #expect(actions.isRsvpBusy("55") && actions.isRsvpBusy("56") && !actions.isRsvpBusy("57"))
        // Without an engine answer the call ends as "đang xử lý", never as an error, and releases the key.
        for _ in 0..<200 where !center.busy.isEmpty { try await Task.sleep(for: .milliseconds(20)) }
        #expect(center.busy.isEmpty)
        #expect(center.notice?.text == "Đang xử lý, hãy kiểm tra lại sau.")
    }

    @Test func pinStateAndConfirmation() throws {
        let state = PollPinState(json: try json(#"{"ready":true,"canPin":true,"pinned":false,"limit":5}"#))
        #expect(state.ready && state.offersPin && !state.pinned && state.limit == 5)
        #expect(!PollPinState(json: try json(#"{"ready":true,"canPin":false}"#)).offersPin, "official: only with isEnablePin")
        #expect(!PollPinState(json: try json(#"{"ready":false,"canPin":true}"#)).offersPin, "unknown board: nothing is offered")
        #expect(PollPinState(json: nil) == PollPinState())
        let confirm = try #require(PollPinConfirm(json: try json(#"{"ok":false,"error":"confirm","limit":3,"ref":"8:10","replace":{"kind":"message","title":"Họp lúc 9h"}}"#)))
        #expect(confirm.ref == "8:10")
        #expect(confirm.message == "Đã đạt giới hạn 3 ghim. Ghim cũ dưới đây sẽ được bỏ để cập nhật nội dung mới.\nTin nhắn: Họp lúc 9h")
        #expect(PollPinConfirm(json: try json(#"{"error":"confirm"}"#)) == nil, "a confirmation without the topic it would drop is not asked")
        #expect(PollActionResult(ok: false, error: "forbidden").message(for: .pin) == "Bạn không có quyền thực hiện thao tác này.")
        #expect(PollActionResult(ok: false, error: "limit").message(for: .pin).contains("Bảng ghim đã đầy"))
        #expect(PollActionResult(ok: false, error: "failed").message(for: .unpin).contains("Bỏ ghim"))
        #expect(PollActionResult(ok: false, error: "forbidden").message(for: .lock) == "Bạn không có quyền khóa bình chọn này")
    }
}
