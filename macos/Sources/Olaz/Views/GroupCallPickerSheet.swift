import SwiftUI

/// Participant picker for group calls: the renderer's GROUP_CALL modal ("Tạo cuộc gọi" / "Thêm người vào cuộc gọi")
/// as a native sheet. Members, limit and rules come from the renderer (`GroupCallPicker`); there is no "select all",
/// like the official modal. Confirming runs the official `onConfirm` (see ZaloEngine+GroupCall.swift).
struct GroupCallPickerSheet: View {
    @Environment(ZaloEngine.self) private var engine
    let picker: GroupCallPicker

    @State private var query = ""
    @State private var selection: [String] = []
    @State private var limitNotice = false
    @FocusState private var searchFocused: Bool

    var body: some View {
        let rows = picker.members(matching: query)
        VStack(spacing: 0) {
            header
            searchField
            if let note = picker.note {
                Label(note, systemImage: "lock")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20)
                    .padding(.bottom, 6)
            }
            if rows.isEmpty {
                ContentUnavailableView(picker.members.isEmpty ? "Không có thành viên để gọi" : "Không tìm thấy",
                                       systemImage: picker.members.isEmpty ? "person.2.slash" : "magnifyingglass")
                    .frame(maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 2) {
                        ForEach(rows) { member in row(member) }
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                }
            }
            footer
        }
        .frame(width: 400, height: 560)
        .onAppear { searchFocused = true }
        .onExitCommand { engine.cancelGroupPicker(picker) }
        // Dismissed by the user (click outside, system gesture): decline this very request. A replaced or answered
        // picker is no longer the engine's current one, so nothing is sent for it.
        .onDisappear { engine.cancelGroupPicker(picker) }
    }

    private var header: some View {
        VStack(spacing: 8) {
            Text(picker.title)
                .font(.system(size: 16, weight: .semibold))
            HStack(spacing: 8) {
                RemoteAvatar(url: picker.groupAvatar, name: picker.groupName, size: 26)
                Text(picker.groupName)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                if let video = picker.video {
                    Image(systemName: video ? "video.fill" : "phone.fill")
                        .font(.system(size: 11))
                        .foregroundStyle(.tint)
                }
            }
        }
        .padding(.top, 18)
        .padding(.bottom, 12)
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField(picker.searchPlaceholder, text: $query)
                .textFieldStyle(.plain)
                .focused($searchFocused)
            if !query.isEmpty {
                Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .glassEffect(.regular, in: Capsule())
        .padding(.horizontal, 16)
        .padding(.bottom, 8)
    }

    private func row(_ member: GroupCallPicker.Member) -> some View {
        let joined = picker.existing.contains(member.id)
        let checked = joined || selection.contains(member.id)
        return Button {
            let result = picker.toggled(member.id, in: selection)
            selection = result.selection
            if result.overLimit { flashLimit() } else { query = "" }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: checked ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 18))
                    .foregroundStyle(checked ? AnyShapeStyle(.tint) : AnyShapeStyle(Color.secondary.opacity(0.6)))
                RemoteAvatar(url: member.avatar, name: member.name, size: 34)
                VStack(alignment: .leading, spacing: 0) {
                    Text(member.name)
                        .font(.system(size: 14))
                        .lineLimit(1)
                    if joined {
                        Text("Đã tham gia")
                            .font(.system(size: 11.5))
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(joined)
        .opacity(joined ? 0.6 : 1)
        .accessibilityLabel(member.name)
        .accessibilityAddTraits(checked ? .isSelected : [])
    }

    private var footer: some View {
        VStack(spacing: 8) {
            if limitNotice {
                Text(picker.limitMessage)
                    .font(.system(size: 12))
                    .foregroundStyle(.orange)
                    .transition(.opacity)
            }
            HStack {
                Text("Đã chọn \(selection.count)/\(picker.maxSelectable)")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                Spacer()
                Button("Hủy") { engine.cancelGroupPicker(picker) }
                    .buttonStyle(.glass)
                    .keyboardShortcut(.cancelAction)
                Button(picker.confirmTitle) { engine.confirmGroupPicker(picker, selected: selection) }
                    .buttonStyle(.glassProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(selection.isEmpty)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .animation(.smooth(duration: 0.2), value: limitNotice)
    }

    private func flashLimit() {
        limitNotice = true
        Task {
            try? await Task.sleep(for: .seconds(2.5))
            limitNotice = false
        }
    }
}
