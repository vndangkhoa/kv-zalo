import Testing
@testable import Olaz

/// The inbox is "Tin nhắn" in Olaz's own UI while the official ConvListController filters stay untouched, and a label
/// filter is a view of the inbox (the sidebar has no rows for labels).
@Suite struct InboxNamingTests {
    @Test func defaultListIsAlwaysTinNhan() {
        #expect(ConversationExtras.inboxTitle == "Tin nhắn")
        for archivedEnabled in [true, false] {
            var extras = ConversationExtras()
            extras.archivedEnabled = archivedEnabled
            #expect(extras.defaultTab.title == "Tin nhắn")
        }
        #expect(ConversationTypeFilter.other.title == "Khác")
        #expect(ConversationTypeFilter.unread.title == "Chưa đọc")
    }

    @Test func officialFilterMappingIsUnchanged() {
        var extras = ConversationExtras()
        #expect(extras.defaultTab == .all)
        extras.archivedEnabled = true
        #expect(extras.defaultTab == .focused)
        let focused = SidebarDestination.inbox.officialFilter(defaultTab: .focused)
        #expect(focused?.tab == .focused && focused?.labelIds == [])
        let label = SidebarDestination.label("7").officialFilter(defaultTab: .focused)
        #expect(label?.tab == .focused && label?.labelIds == ["7"])
    }

    @Test func labelFilterBelongsToTheInboxRow() {
        #expect(SidebarDestination.label("7").sidebarRow == .inbox)
        #expect(SidebarDestination.inbox.sidebarRow == .inbox)
        #expect(SidebarDestination.other.sidebarRow == .other)
        #expect(SidebarDestination.groups.sidebarRow == .groups)
    }

    @Test func filteringLabelResolvesOnlyExistingLabels() {
        var extras = ConversationExtras()
        extras.labels = [.init(id: "7", name: "Gia đình", colorHex: "#4BC377", convIds: ["a"])]
        #expect(extras.label(filteredBy: .label("7"))?.name == "Gia đình")
        // A deleted label (the shell falls back to the inbox right after) resolves to nothing.
        #expect(extras.label(filteredBy: .label("8")) == nil)
        #expect(extras.label(filteredBy: .inbox) == nil)
    }
}
