import SwiftUI

extension View {
    /// Title of a content column (mailbox, Danh bạ list, zCloud). In the unified window toolbar macOS shows it like
    /// Mail's mailbox title, at the leading edge above the list; chat views set no title of their own so this one wins.
    func columnTitle(_ title: String, subtitle: String = "") -> some View {
        navigationTitle(title)
            .navigationSubtitle(subtitle)
    }

    /// Lets a column's content (list, empty or loading state) fill the whole column. Under a top `.safeAreaBar` (the search
    /// field) a state that only sizes itself would be centered in the column with the bar hugging it; filling keeps the
    /// bar at the top and centers the state in the space below.
    func fillColumn() -> some View {
        frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
