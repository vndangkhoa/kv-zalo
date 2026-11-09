import SwiftUI

/// Content column for Danh bạ destinations (friends / groups / requests / add friend). Selection lives in
/// `ShellModel.contactSelection` and is dropped when the destination changes (an id of one list means nothing in another).
struct ContactsListColumn: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell
    let destination: SidebarDestination

    var body: some View {
        Group {
            switch destination {
            case .groups: GroupsList()
            case .requests: RequestsList()
            case .addFriend: AddFriendForm()
            default: FriendsList()
            }
        }
        .toolbar {
            if destination == .groups {
                ToolbarItem {
                    Button("Tạo nhóm", systemImage: "person.2.badge.plus") { shell.composeGroup() }
                        .help("Tạo nhóm trò chuyện (⇧⌘N)")
                }
            }
            if destination != .addFriend {
                ToolbarItem {
                    Button("Thêm bạn", systemImage: "person.badge.plus") { shell.destination = .addFriend }
                        .help("Thêm bạn bằng số điện thoại")
                }
            }
        }
        .task { engine.refreshContacts() }
        .onChange(of: destination) { _, _ in shell.contactSelection = nil }
    }
}

/// Detail column for Danh bạ destinations: the selected contact / group / request, or the add-friend result.
struct ContactsDetailColumn: View {
    @Environment(ZaloEngine.self) private var engine
    @Environment(ShellModel.self) private var shell
    let destination: SidebarDestination

    var body: some View {
        Group {
            switch destination {
            case .groups: groupDetail
            case .requests: RequestsDetail()
            case .addFriend: AddFriendDetail()
            default: friendDetail
            }
        }
        .contactsToast()
    }

    @ViewBuilder private var friendDetail: some View {
        if let id = shell.contactSelection, let friend = engine.visibleFriends.first(where: { $0.id == id }) {
            FriendCard(friend: friend).id(friend.id)
        } else {
            ContentUnavailableView("Chọn một người bạn", systemImage: "person.crop.circle",
                                   description: Text("Chọn bạn bè ở danh sách bên cạnh để xem thông tin và liên lạc."))
        }
    }

    @ViewBuilder private var groupDetail: some View {
        if let id = shell.contactSelection, let group = engine.visibleGroups.first(where: { $0.id == id }) {
            GroupCard(group: group).id(group.id)
        } else {
            ContentUnavailableView("Nhóm và cộng đồng", systemImage: "person.2",
                                   description: Text("Chọn một nhóm ở danh sách bên cạnh, hoặc nhấn Return để mở trò chuyện."))
        }
    }
}
