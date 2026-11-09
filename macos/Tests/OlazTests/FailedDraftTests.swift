import Testing
import AppKit
@testable import Olaz

@Suite struct FailedDraftTests {
    @Test func appendingShiftsMentionsAndKeepsOrder() {
        let a = ComposerDraft(text: "hi @An", mentions: [Mention(uid: "1", pos: 3, len: 3)])
        let b = ComposerDraft(text: "😀 @Bo", mentions: [Mention(uid: "2", pos: 3, len: 3)])
        let merged = a.appending(b)
        #expect(merged.text == "hi @An\n😀 @Bo")
        // "hi @An" is 6 UTF-16 units + newline.
        #expect(merged.mentions == [Mention(uid: "1", pos: 3, len: 3), Mention(uid: "2", pos: 10, len: 3)])
        #expect((merged.text as NSString).substring(with: NSRange(location: 10, length: 3)) == "@Bo")
    }

    @Test func appendingToEmptyTakesTheOther() {
        let failed = ComposerDraft(text: "x", mentions: [Mention(uid: "1", pos: 0, len: 2)])
        #expect(ComposerDraft(text: "").appending(failed) == failed)
        #expect(failed.appending(ComposerDraft(text: "")) == failed)
    }

    @Test func storeKeepsPerConversationAndMergesFailures() {
        var store = FailedDraftStore()
        store.stash(ComposerDraft(text: "one"), for: "c1")
        store.stash(ComposerDraft(text: "two"), for: "c1")
        store.stash(ComposerDraft(text: "other"), for: "c2")
        store.stash(ComposerDraft(text: ""), for: "c3")
        #expect(store.revision == 3)
        #expect(!store.has("c3"))
        #expect(store.take(for: "c1")?.text == "one\ntwo")
        #expect(store.take(for: "c1") == nil)
        #expect(store.take(for: "c2")?.text == "other")
    }

    @Test func drainEmptiesTheStore() {
        var store = FailedDraftStore()
        store.stash(ComposerDraft(text: "a"), for: "c1")
        store.stash(ComposerDraft(text: "b"), for: "c2")
        #expect(Set(store.drain().map(\.0)) == ["c1", "c2"])
        #expect(!store.has("c1") && !store.has("c2"))
    }

    @MainActor @Test func setContentRebuildsMentionTokens() {
        let tv = NSTextView()
        let text = "chào @An Bình ok"
        ComposerTextView.setContent(tv, text: text, mentions: [
            Mention(uid: "7", pos: 5, len: 8),
            Mention(uid: "bad", pos: 0, len: 3),        // not an "@": skipped
            Mention(uid: "oob", pos: 14, len: 9)])       // out of range: skipped
        var tokens: [(NSRange, MentionToken)] = []
        tv.textStorage!.enumerateAttribute(.zaloMention, in: NSRange(location: 0, length: tv.textStorage!.length)) { v, r, _ in
            if let t = v as? MentionToken { tokens.append((r, t)) }
        }
        #expect(tokens.count == 1)
        #expect(tokens[0].0 == NSRange(location: 5, length: 8))
        #expect(tokens[0].1.uid == "7" && tokens[0].1.label == "@An Bình")
    }
}
