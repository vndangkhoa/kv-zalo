import CoreGraphics
import Testing
@testable import Olaz

/// The window is widened before the inspector opens, so the chat keeps its width instead of being squeezed to a sliver.
@MainActor @Suite struct InspectorRoomTests {
    let screen = CGRect(x: 0, y: 0, width: 1800, height: 1100)

    @Test func minimumWindowGrowsToTheRightByTheInspectorWidth() {
        let window = CGRect(x: 420, y: 243, width: 960, height: 672)
        let grown = InspectorRoom.frame(for: window, in: screen)
        #expect(grown == CGRect(x: 420, y: 243, width: 1260, height: 672))
    }

    @Test func aWideEnoughWindowIsLeftAlone() {
        let window = CGRect(x: 264, y: 151, width: 1368, height: 832)
        #expect(InspectorRoom.frame(for: window, in: screen) == window)
    }

    @Test func growingPastTheScreenEdgeMovesTheWindowLeft() {
        let window = CGRect(x: 800, y: 100, width: 960, height: 600)
        let grown = InspectorRoom.frame(for: window, in: screen)
        #expect(grown.width == 1260 && grown.maxX == screen.maxX)
    }

    @Test func neverWiderThanTheScreen() {
        let small = CGRect(x: 0, y: 0, width: 1100, height: 800)
        let grown = InspectorRoom.frame(for: CGRect(x: 50, y: 0, width: 960, height: 600), in: small)
        #expect(grown.width == 1100 && grown.minX == 0)
    }
}
