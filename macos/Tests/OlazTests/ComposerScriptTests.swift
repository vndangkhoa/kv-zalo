import Testing
import Foundation
import JavaScriptCore
@testable import Olaz

/// Runs the real `composer-adapter.js` in JavaScriptCore against fake official modules: the reminder gate, validation and the
/// exact service calls, the libvips config pin, my contact card payload and the toast / confirm hand-over to Swift.
@MainActor
struct ComposerScriptTests {
    private func harness() -> ScriptHarness {
        let h = ScriptHarness(prelude: """
        var posted = [], calls = [], shown = [], opened = [], locked = {};
        window.webkit = { messageHandlers: { zstate: { postMessage: function (m) { posted.push(m.compose); } }, zlog: { postMessage: function () {} } } };
        var cfg = { sendToMeId: '999', enable_reminder_1_1: 1, limitTitleTextChar: 3000, group_topics: { enable: 0 },
          image_resizer: { enable_libvips_macos: 1, enable_libvips_win32: 1 }, supportPhoto: ['jpg', 'png'], maxNumberOfFile: 50 };
        var groups = {
          g42: { setting: { lockCreatePost: 0, setTopicOnly: 0 }, creatorId: '1' },
          g43: { setting: { lockCreatePost: 1, setTopicOnly: 0 }, creatorId: '7' },
          g44: { setting: { lockCreatePost: 1, setTopicOnly: 0 }, creatorId: '7', community: true }
        };
        var toastObj = { createError: function () {}, createMessagev2: function () {}, createMessageCustom: function () {}, _createDom: function (t) { shown.push(t); } };
        var confirmMgr = { openConfirm: function (o) { opened.push(o); } };
        var cc = {
          windowId: '1',
          _shareMessage_new: function (msgs, ids, done) {
            calls.push(['share', msgs, ids]);
            toastObj._createDom('Đã gửi');
            confirmMgr.openConfirm({ params: { title: 'Gửi dưới dạng file',
              message: { type: 'div', props: { children: [{ type: function () {}, props: { textKey: 'STR_A' } }, ' tiếp?',
                { type: 'div', props: { className: 'flx', children: [{ type: 'div', props: { children: 'a' } }, { type: 'div', props: { children: '.jpg' } }] } }] } },
              okText: 'Gửi', onOk: function () { calls.push(['ok']); }, onCancel: function () { calls.push(['cancel']); } } });
            done(ids, [{ toUid: ids[0] }]);
            return Promise.resolve();
          }
        };
        var core = {
          S: { cfg: cfg,
               friends: { getProfileMeSync: function () { return { userId: '1', zaloName: 'Tôi', avatar: 'https://a/me.jpg', phoneNumber: '+84901234567' }; },
                          getProfileFriendByIdSync: function (id) { return { type: id === 'oa1' ? 2 : 0 }; } },
               groups: { getGroupByIdSync: function (id) { return groups[id] || null; } } },
          safe: function (fn, d) { try { var v = fn(); return v === undefined ? d : v; } catch (e) { return d; } },
          str: function (v) { return typeof v === 'string' ? v : (v == null ? '' : String(v)); },
          scrubErr: function (e) { return String(e); }, diag: function () {},
          isGroupId: function (id) { return String(id).charAt(0) === 'g'; },
          meNow: function () { return '1'; }, myName: function () { return 'Tôi'; }, myAvatar: function () { return ''; },
          sleep: function (ms) { return new Promise(function (r) { setTimeout(r, ms); }); },
          ready: function () { return Promise.resolve(true); }, MSG: { CONTACT: 6 },
          boxFor: function (id) { return Promise.resolve({ cc: cc, info: { userId: id }, direct: false }); },
          serial: function (fn) { return Promise.resolve().then(fn); }, refresh: function () {}
        };
        window.__zaloChat = { __core: core };
        window.__zaloRequire = {};
        window.__zaloDI = function () { return [{ instance: { verifySetting: function (o) { return !!locked[o.convId]; } } }]; };
        var rmdCfg = function () {}; rmdCfg.isNewUiEnabled = function () { return false; }; rmdCfg.getMinLeadTimeMs = function () { return 60000; }; rmdCfg.isOnlyMeEnabled = function () { return false; };
        var exportsList = [
          { default: { str: function (k) { return 'S:' + k; }, trans: function (k, a) { return 'T:' + k + ':' + a.join(','); }, currentLanguage: function () {} } },
          { default: toastObj }, { ConfirmManagerV2: confirmMgr }, { ModalManagerV2: { openModal: function () {}, closeModal: function () {} } },
          { a: function () {}, b: function (codes) { return codes.InvalidPhotoSize; } },
          { b: { Photo: { SEND_FROM_SCREENSHOT: 309, CLICK_ICON_SEND_PHOTO: 301 }, File: { CLICK_ICON_SEND_FILE: 401 } } },
          { default: { isValidFileName: function () { return true; }, isEnableUpEecx: function () { return false; }, getExtension: function () {} } },
          { MSG_SUBTYPE_FILE_EEXC: 9, MSG_FILE: 19 },
          { c: function (uids) { var getQRCodeUser = 1; return Promise.resolve({ '1': 'https://qr/1' }); }, b: function () {} },
          { default: { getPhoneNumberPro: function (p) { var getCountryCode = 0; return '0901234567'; } } },
          { default: { getRememberSendWithPhoneNumber: function () { var getItemForCurrentUser = 0; return true; } } },
          { default: { isOABlocked: function () { return false; }, getOAStatus: function () {} } },
          { default: { isNoticeable: function (g, me) { return g.creatorId === me || g.setting.lockCreatePost == 0; }, isAdminOrCreator: function () {},
                       isOAType: function (p) { return p.type >= 1; }, getStringByteLength: function (s) { return unescape(encodeURIComponent(s)).length; },
                       checkAuth: function (g, me) { return { isAdmin: false, groupOwner: g.creatorId === me }; } } },
          { default: { createGroupTopicv2: function () { calls.push(['group'].concat(Array.prototype.slice.call(arguments))); return Promise.resolve({}); },
                       createOneOneGroupTopic: function (o) { calls.push(['one', o]); return Promise.resolve({}); }, getGroupInfoByLink: function () {} } },
          { REPEAT_CUSTOM: 4, REMINDER_SRC_CHATBAR: 100, CREATE_NOTICE_SRC_CHAT_BAR: 1, ON_FEATURE: { ENABLE_SENDTOME_REMINDER: true }, TEXT_TOPIC: 0 },
          { a: rmdCfg },
          { a: { get: function (k, g) { var COMMUNITY = 0; return g.community ? 'STE_COMMUNITY_ADMIN_CREATE_TOPIC' : k; } } }
        ];
        window.__zaloEachExport = function (req, visit) { exportsList.forEach(visit); };
        """)
        h.load("composer-adapter.js")
        return h
    }

    private func run(_ h: ScriptHarness, _ script: String) -> JSValue? {
        h.ctx.evaluateScript("__results.r = undefined; Promise.resolve().then(function () { return \(script); }).then(function (r) { __results.r = r; }, function (e) { __results.r = 'threw ' + e; });")
        h.ctx.evaluateScript("__pump(0)")
        return h.ctx.evaluateScript("__results.r")
    }

    private func json(_ h: ScriptHarness, _ expression: String) -> String {
        h.ctx.evaluateScript("JSON.stringify(\(expression))")?.toString() ?? ""
    }

    @Test func modulesAreFound() {
        let h = harness()
        #expect(json(h, "window.__zaloCompose.missing()") == "[]")
        #expect(h.failures.isEmpty)
    }

    @Test func reminderGate() {
        let h = harness()
        _ = run(h, "window.__zaloCompose.reminderInfo('2')")
        #expect(json(h, "__results.r.allowed") == "true")
        _ = run(h, "window.__zaloCompose.reminderInfo('oa1')")
        #expect(json(h, "[__results.r.allowed, __results.r.hidden]") == "[false,true]")
        _ = run(h, "window.__zaloCompose.reminderInfo('g42')")
        #expect(json(h, "[__results.r.allowed, __results.r.canPin, __results.r.pinToggle]") == "[true,true,true]")
        _ = run(h, "window.__zaloCompose.reminderInfo('g43')")
        #expect(json(h, "[__results.r.allowed, __results.r.text]") == #"[false,"Chỉ trưởng/phó nhóm được phép tạo ghi chú, nhắc hẹn"]"#)
        _ = run(h, "window.__zaloCompose.reminderInfo('g44')")
        #expect(json(h, "__results.r.text") == #""Chỉ trưởng/phó cộng đồng được phép tạo ghi chú, nhắc hẹn""#)
        _ = run(h, "window.__zaloCompose.reminderInfo('999')")
        #expect(json(h, "__results.r.allowed") == "true")
    }

    @Test func reminderValidationNeverCallsTheService() {
        let h = harness()
        _ = run(h, "window.__zaloCompose.createReminder('2', { title: '   ', startTime: Date.now() + 3600e3, repeat: 0 })")
        #expect(json(h, "__results.r") == #"{"status":"invalid","text":"Vui lòng nhập nội dung nhắc hẹn"}"#)
        _ = run(h, "window.__zaloCompose.createReminder('2', { title: 'x', startTime: Date.now() - 3600e3, repeat: 0 })")
        #expect(json(h, "__results.r.status") == #""invalid""#)
        _ = run(h, "window.__zaloCompose.createReminder('2', { title: 'x'.repeat(3000), startTime: Date.now() + 3600e3, repeat: 0 })")
        #expect(json(h, "__results.r.text") == #""Bạn đã nhập quá số kí tự cho phép""#)
        _ = run(h, "window.__zaloCompose.createReminder('g43', { title: 'x', startTime: Date.now() + 3600e3, repeat: 0 })")
        #expect(json(h, "__results.r.status") == #""denied""#)
        _ = run(h, "(locked['2'] = true, window.__zaloCompose.createReminder('2', { title: 'x', startTime: Date.now() + 3600e3, repeat: 0 }))")
        #expect(json(h, "__results.r.text") == #""Yêu cầu thực hiện không thành công vì bạn hoặc người này đã chặn nhau""#)
        #expect(json(h, "calls") == "[]")
    }

    @Test func reminderServiceCalls() {
        let h = harness()
        // 1.7e12 ms is 22:13:20 UTC; + 1 h + 12.345 s → 23:13:32.345, sent as 23:13:00 (the official modal drops the seconds).
        _ = run(h, "window.__zaloCompose.createReminder('2', { title: ' Họp ', startTime: 1.7e12 + 3600e3 + 12345, repeat: 2 })")
        #expect(json(h, "__results.r") == #"{"status":"done"}"#)
        #expect(json(h, "calls[0]") == #"["one",{"toUid":"2","type":0,"color":-16245706,"emoji":"⏰","startTime":1700003580000,"duration":-1,"params":{"title":"Họp"},"needPin":false,"repeat":2,"creatorUid":"1","src":100}]"#)
        _ = run(h, "window.__zaloCompose.createReminder('g42', { title: 'Nhóm', startTime: 1.7e12 + 7200e3, repeat: 0, needPin: true })")
        #expect(json(h, "calls[1]") == #"["group","42",0,-16245706,"⏰",1700007180000,-1,{"title":"Nhóm"},true,0,0,1]"#)
    }

    @Test func libvipsSwitchStaysOff() {
        let h = harness()
        _ = run(h, "window.__zaloCompose.sendMyCard('2', false)")
        #expect(json(h, "cfg.image_resizer.enable_libvips_macos") == "0")
        h.ctx.evaluateScript("cfg.image_resizer = { enable_libvips_macos: 1, other: 2 }")
        #expect(json(h, "[cfg.image_resizer.enable_libvips_macos, cfg.image_resizer.other]") == "[0,2]")
    }

    @Test func myCardPayloadAndOfficialPrompts() {
        let h = harness()
        _ = run(h, "window.__zaloCompose.sendMyCard('2', true)")
        #expect(json(h, "__results.r") == #"{"status":"done"}"#)
        #expect(json(h, "calls[0][1][0]") == #"{"message":{"action":"recommened.user","params":"1","thumb":"https://a/me.jpg","title":"Tôi","description":"{\"qrCodeUrl\":\"https://qr/1\",\"phone\":\"0901234567\"}"},"msgType":6}"#)
        #expect(json(h, "calls[0][2]") == #"["2"]"#)
        // The official toast and confirm of the run went to Swift instead of the hidden page.
        #expect(json(h, "posted[0]") == #"{"kind":"toast","convId":"2","text":"Đã gửi"}"#)
        #expect(json(h, "[posted[1].kind, posted[1].title, posted[1].message, posted[1].ok]") == #"["confirm","Gửi dưới dạng file","S:STR_A tiếp?\na.jpg","Gửi"]"#)
        #expect(json(h, "opened.length") == "0")
        #expect(json(h, "window.__zaloCompose.answer(posted[1].token, true)") == "true")
        #expect(json(h, "calls[1]") == #"["ok"]"#)
        // Answered once only.
        #expect(json(h, "window.__zaloCompose.answer(posted[1].token, false)") == "false")
        // Once the run is over (and its grace time), toasts stay in the page.
        h.ctx.evaluateScript("__pump(5000); toastObj._createDom('Khác')")
        #expect(json(h, "posted.length") == "2")
        #expect(h.failures.isEmpty)
    }
}
