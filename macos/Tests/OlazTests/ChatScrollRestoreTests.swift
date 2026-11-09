import Foundation
import Testing
@testable import Olaz

/// After an older page is prepended, the chat scrolls the row that was at the top back there. When that row was a day
/// separator replaced by older messages of the same day, its message is used instead of leaving the view at the top
/// (which used to load page after page on its own).
@Suite struct ChatScrollRestoreTests {
    private func rows(_ ids: [String]) -> [ChatRow] {
        ids.map { ChatRow(id: $0, kind: .day(Date(timeIntervalSince1970: 0))) }
    }

    @Test func keepsTheSavedRowWhenItStillExists() {
        let after = rows(["day-a1", "a1", "a2", "day-b1", "b1"])
        #expect(ChatRow.restoreTarget("day-b1", fallback: "b1", in: after) == "day-b1")
        #expect(ChatRow.restoreTarget("a2", fallback: "a1", in: after) == "a2")
    }

    @Test func daySeparatorReplacedBySameDayHistoryFallsBackToItsMessage() {
        // Before: [day-b1, b1, b2]; older messages of the same day (b0) arrive, so the separator becomes day-b0.
        let after = rows(["day-b0", "b0", "b1", "b2"])
        #expect(ChatRow.restoreTarget("day-b1", fallback: "b1", in: after) == "b1")
    }

    @Test func unknownOrMissingRowUsesTheFallback() {
        let after = rows(["day-x0", "x0", "x1"])
        #expect(ChatRow.restoreTarget(nil, fallback: "x1", in: after) == "x1")
        #expect(ChatRow.restoreTarget("gone", fallback: "x1", in: after) == "x1")
        #expect(ChatRow.restoreTarget("day-gone", fallback: "x1", in: after) == "x1")
    }
}
