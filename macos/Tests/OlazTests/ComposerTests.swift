import Testing
import Foundation
@testable import Olaz

/// The ZaloCap stdout framing (main.js "F58p"), composer outcome decoding and the reminder sheet's time chips.
@MainActor
struct ComposerTests {
    @Test func framesAreCutOutOfLogNoise() {
        var buffer = "\" 2026/10/27 - 08:15:09:198 | MainWindow::connectEvents\"\nSUCCESS_INIT\n"
            + #"@cap_resp@{"id":7,"data":"{\"cmd\":\"cap:recvReq\",\"data\":\"zalo:capture\"}"}@end_cap_resp@"#
            + "log line\n"
            + #"@cap_resp@{"id":8,"data":"{\"cmd\":\"cap:forwardScrs\",\"data\":\"/tmp/shot.png\"}"}@end_"#
        let first = ScreenCapture.frames(in: &buffer)
        #expect(first.count == 1)
        #expect(first.first?.command == "cap:recvReq")
        #expect(first.first?.data as? String == "zalo:capture")
        #expect((first.first?.id as? NSNumber)?.intValue == 7)
        // The second frame is incomplete: kept until its end marker arrives.
        #expect(buffer.hasPrefix("@cap_resp@"))
        buffer += "cap_resp@"
        let second = ScreenCapture.frames(in: &buffer)
        #expect(second.map(\.command) == ["cap:forwardScrs"])
        #expect(second.first?.data as? String == "/tmp/shot.png")
        #expect(buffer.isEmpty)
    }

    @Test func frameSplitAcrossChunksAndLineBreaks() {
        var buffer = "noise @cap_"
        #expect(ScreenCapture.frames(in: &buffer).isEmpty)
        buffer += "resp@{\"id\":1,\r\n\"data\":\"{\\\"cmd\\\":\\\"cap:acceptScrs\\\"}\"}@end_cap_resp@"
        let frames = ScreenCapture.frames(in: &buffer)
        #expect(frames.map(\.command) == ["cap:acceptScrs"])
        #expect(frames.first?.data == nil)
    }

    @Test func garbageWithoutFrameStaysBounded() {
        var buffer = String(repeating: "x", count: 5000)
        #expect(ScreenCapture.frames(in: &buffer).isEmpty)
        #expect(buffer.count <= "@cap_resp@".count)
    }

    @Test func outcomes() {
        #expect(ComposerOutcome(json: ["status": "done"]) == .done)
        #expect(ComposerOutcome(json: ["status": "cancelled"]) == .cancelled)
        // The official toast already said why (too many files).
        #expect(ComposerOutcome(json: ["status": "invalid"]) == .cancelled)
        #expect(ComposerOutcome(json: ["status": "invalid", "text": "Vui lòng nhập nội dung nhắc hẹn"]) == .failed("Vui lòng nhập nội dung nhắc hẹn"))
        #expect(ComposerOutcome(json: ["status": "locked", "text": "x"]) == .failed("x"))
        if case .pending = ComposerOutcome(json: ["status": "pending"]) {} else { Issue.record("pending") }
        #expect(ComposerOutcome(json: nil) == .failed(ComposerOutcome.genericFailure))
    }

    @Test func reminderTitleLimits() throws {
        let info = try #require(ReminderComposeInfo(json: ["allowed": true, "titleMaxChars": 10, "titleMaxBytes": 12]))
        #expect(!info.titleTooLong("abc"))
        #expect(info.titleTooLong(String(repeating: "a", count: 10)))
        // 5 characters, 15 UTF-8 bytes.
        #expect(info.titleTooLong("ệệệệệ"))
    }

    @Test func reminderChips() throws {
        var calendar = Calendar.current
        calendar.timeZone = .current
        let now = try #require(calendar.date(from: DateComponents(year: 2026, month: 10, day: 27, hour: 22, minute: 50, second: 41)))
        let in30 = ReminderComposeSheet.time(for: .in30, from: now)
        #expect(calendar.component(.second, from: in30) == 0)
        #expect(calendar.component(.hour, from: in30) == 23 && calendar.component(.minute, from: in30) == 20)
        let tomorrow = ReminderComposeSheet.time(for: .tomorrow9, from: now)
        #expect(calendar.component(.day, from: tomorrow) == 28 && calendar.component(.hour, from: tomorrow) == 9 && calendar.component(.minute, from: tomorrow) == 0)
    }
}
