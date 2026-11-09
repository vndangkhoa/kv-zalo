import SwiftUI

// Reminder cards (e-card `action.open.reminder`). The state — which variant the official client draws, who may answer, my
// answer — comes from the official reminder e-card controller (`ZaloEngine.reminderWatch`, card-actions-adapter.js). RSVP goes
// through the same controller (`respond`) and only on a click. While the controller has nothing to draw (official variants
// hidden / pending, or it is not available) the static card built from the message itself stays.

struct ReminderCardView: View {
    let card: ChatMessage.ECard
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.cardActions) private var actions
    @State private var snapshot: ReminderSnapshot?

    var body: some View {
        Group {
            if let snapshot, snapshot.isDrawn { official(snapshot) } else { staticCard }
        }
        .task(id: actions.messageId) { await watch() }
    }

    /// Follows the controller until the row goes away; gives up (static card) when it is not available.
    private func watch() async {
        guard !actions.messageId.isEmpty else { return }
        var signature = ""
        var misses = 0
        while !Task.isCancelled {
            if let next = await engine.reminderWatch(in: actions.conversationId, message: actions.messageId, signature: signature) {
                snapshot = next
                signature = next.sig
                misses = 0
            } else {
                misses += 1
                if misses >= 3 { return }
                try? await Task.sleep(for: .seconds(4))
            }
        }
    }

    // MARK: Static card (from the message)

    private var staticCard: some View {
        HStack(spacing: 14) {
            ReminderCalendar(start: card.reminderStart)
            VStack(alignment: .leading, spacing: 4) {
                Text(card.reminderTitle.isEmpty ? "Nhắc hẹn" : card.reminderTitle)
                    .font(.system(size: 14, weight: .semibold)).lineLimit(3)
                ReminderMeta(start: card.reminderStart, repeatLabel: card.reminderRepeat)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(width: 300)
        .cardChrome(radius: 16)
        .accessibilityElement(children: .combine)
    }

    // MARK: Official variants

    @ViewBuilder private func official(_ s: ReminderSnapshot) -> some View {
        switch s.variant {
        case .invite: invite(s)
        case .alert: alert(s)
        case .demotedCreate, .demotedAlert: demoted(s)
        case .hidden, .pending: staticCard
        }
    }

    /// `MessageInviteCard`: creator line, summary, time / repeat, then "Tham gia" / "Từ chối" or my answer with "Thay đổi".
    private func invite(_ s: ReminderSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { actions.showReminder() } label: {
                VStack(alignment: .leading, spacing: 10) {
                    if s.onlyMe {
                        Text("(Chỉ bạn nhìn thấy)").font(.system(size: 11.5)).foregroundStyle(.secondary)
                    }
                    if !s.creatorName.isEmpty {
                        Text(s.creatorName + " tạo nhắc hẹn").font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                    HStack(spacing: 14) {
                        ReminderCalendar(start: s.start)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(s.title.isEmpty ? "Nhắc hẹn" : s.title).font(.system(size: 14, weight: .semibold)).lineLimit(3)
                                .strikethrough(s.deleted)
                            ReminderMeta(start: s.start, repeatLabel: s.repeatLabel)
                        }
                        Spacer(minLength: 0)
                    }
                    if s.deleted { Text("Nhắc hẹn đã bị xoá").font(.system(size: 12)).foregroundStyle(.orange) }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Xem chi tiết")
            if s.hasResponders {
                // Official `attendeeRowProps` (STR_RMD_RSVP_RESPONDERS_LINK): a link unless the reminder was deleted.
                Button { actions.showReminder(openList: true) } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "person.2").font(.system(size: 11)).frame(width: 14)
                        Text("Danh sách tham gia")
                        Spacer(minLength: 0)
                        if !s.deleted { Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold)) }
                    }
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(s.deleted ? AnyShapeStyle(Color.secondary) : AnyShapeStyle(.tint))
                    .padding(.horizontal, 12).padding(.bottom, 10)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(s.deleted)
            }
            if s.showsRsvpButtons {
                Divider()
                ReminderRsvpButtons(snapshot: s)
            } else if s.showsRsvpStatus {
                Divider()
                ReminderRsvpStatus(snapshot: s).padding(.horizontal, 12).padding(.vertical, 8)
            }
        }
        .frame(width: 300)
        .cardChrome(radius: 16)
    }

    /// `AlertCard`: the reminder is due.
    private func alert(_ s: ReminderSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 12) {
                Image(systemName: "alarm.fill").font(.system(size: 20)).foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 3) {
                    if s.onlyMe { Text("(Chỉ bạn nhìn thấy)").font(.system(size: 11.5)).foregroundStyle(.secondary) }
                    Text("Nhắc hẹn: \(s.title)").font(.system(size: 14, weight: .semibold)).lineLimit(3).strikethrough(s.deleted)
                    if let start = s.start {
                        Text(CardFormat.dateTime.string(from: start).capitalized).font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(12)
            Divider()
            Button { actions.showReminder() } label: {
                Text("Xem chi tiết").font(.system(size: 12.5, weight: .semibold)).foregroundStyle(.tint)
                    .frame(maxWidth: .infinity).padding(.vertical, 10).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
        .frame(width: 300)
        .cardChrome(radius: 16)
    }

    /// One line (`demotedCreate` / `demotedAlert`) with "Xem".
    private func demoted(_ s: ReminderSnapshot) -> some View {
        let when = s.start.map { CardFormat.dateTime.string(from: $0).capitalized } ?? ""
        let text: String
        if s.variant == .demotedAlert {
            text = "Nhắc hẹn: \(s.title) - \(when)"
        } else if s.onlyMe {
            text = "(Chỉ bạn nhìn thấy) Bạn đã tạo nhắc hẹn mới \(s.title) - \(when)."
        } else {
            text = "\(s.creatorName) tạo nhắc hẹn mới \(s.title) - \(when)."
        }
        return HStack(spacing: 6) {
            Text(text).font(.system(size: 11.5)).foregroundStyle(.secondary).multilineTextAlignment(.center)
            Button("Xem") { actions.showReminder() }
                .buttonStyle(.plain).font(.system(size: 11.5, weight: .medium)).foregroundStyle(.tint)
        }
        .frame(maxWidth: 360)
        .padding(.horizontal, 20)
    }
}

// MARK: - Pieces

/// The calendar block of the reminder cards.
struct ReminderCalendar: View {
    let start: Date?

    var body: some View {
        VStack(spacing: 0) {
            Text(start.map { CardFormat.weekday.string(from: $0).capitalized } ?? "")
                .font(.system(size: 10.5, weight: .semibold)).foregroundStyle(.white)
                .frame(maxWidth: .infinity).padding(.vertical, 3).background(Color.red)
            Text(start.map { CardFormat.day.string(from: $0) } ?? "--")
                .font(.system(size: 22, weight: .bold, design: .rounded)).padding(.top, 2)
            Text(start.map { CardFormat.month.string(from: $0).capitalized } ?? "")
                .font(.system(size: 10.5)).foregroundStyle(.secondary).padding(.bottom, 4)
        }
        .frame(width: 64)
        .background(Color(nsColor: .controlBackgroundColor))
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Color.primary.opacity(0.12), lineWidth: 0.5))
    }
}

struct ReminderMeta: View {
    let start: Date?
    let repeatLabel: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let start {
                Label(CardFormat.dateTime.string(from: start).capitalized, systemImage: "clock")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            }
            if !repeatLabel.isEmpty {
                Label(repeatLabel, systemImage: "repeat").font(.system(size: 12)).foregroundStyle(.secondary)
            }
        }
    }
}

/// "Từ chối" | "Tham gia" (official `RsvpButtons`).
struct ReminderRsvpButtons: View {
    let snapshot: ReminderSnapshot
    @Environment(\.cardActions) private var actions

    var body: some View {
        HStack(spacing: 0) {
            button("Từ chối", accept: false)
            Divider().frame(height: 24)
            button("Tham gia", accept: true)
        }
    }

    /// Busy while any card of this reminder is sending an answer, or the official controller still has one pending.
    private var locked: Bool { actions.isRsvpBusy(snapshot.id) || snapshot.pending != nil }

    private func button(_ title: String, accept: Bool) -> some View {
        let ref = accept ? "rsvp:accept" : "rsvp:reject"
        let sending = actions.isRsvpBusy(snapshot.id, ref: ref) || snapshot.pending == (accept ? .accept : .reject)
        return Button { actions.respond(accept: accept, reminder: snapshot.id) } label: {
            ZStack {
                Text(title).opacity(sending ? 0 : 1)
                if sending { ProgressView().controlSize(.small) }
            }
            .font(.system(size: 12.5, weight: .semibold))
            .foregroundStyle(accept ? AnyShapeStyle(.tint) : AnyShapeStyle(Color.secondary))
            .frame(maxWidth: .infinity).padding(.vertical, 10).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(locked)
    }
}

/// "Bạn xác nhận: Tham gia · Thay đổi" (official `RsvpStatusBar` + `RsvpStatusMenu`).
struct ReminderRsvpStatus: View {
    let snapshot: ReminderSnapshot
    @Environment(\.cardActions) private var actions

    var body: some View {
        HStack(spacing: 8) {
            Text(snapshot.mine == .accept ? "Bạn xác nhận: Tham gia" : "Bạn xác nhận: Từ chối")
                .font(.system(size: 12)).foregroundStyle(.secondary)
            Spacer(minLength: 0)
            if actions.isRsvpBusy(snapshot.id) || snapshot.pending != nil {
                ProgressView().controlSize(.small)
            } else if snapshot.canRsvp, !snapshot.deleted {
                Menu("Thay đổi") {
                    Button("Tham gia") { actions.respond(accept: true, reminder: snapshot.id) }.disabled(snapshot.mine == .accept)
                    Button("Từ chối") { actions.respond(accept: false, reminder: snapshot.id) }.disabled(snapshot.mine == .reject)
                }
                .menuStyle(.borderlessButton).fixedSize()
                .font(.system(size: 12, weight: .medium))
            }
        }
    }
}

// MARK: - Detail

/// Reminder detail (the official "Xem chi tiết" / "Xem"): what the controller holds, with the RSVP actions, and for group
/// reminders the "Danh sách tham gia" page (who accepted / declined), as the official detail modal has.
struct ReminderDetailSheet: View {
    let request: ReminderRequest
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.dismiss) private var dismiss
    @State private var snapshot: ReminderSnapshot?
    @State private var unavailable = false
    @State private var showingList: Bool
    @State private var responders: RespondersState = .idle

    enum RespondersState: Equatable { case idle, loading, failed, ready(ReminderResponders) }

    init(request: ReminderRequest) {
        self.request = request
        _showingList = State(initialValue: request.openList)
    }

    var body: some View {
        Group {
            if showingList, snapshot?.hasResponders != false { respondersPage } else { detailPage }
        }
        .padding(22)
        .frame(width: 360)
        .environment(\.cardActions, CardActions(conversationId: request.conversationId, messageId: request.messageId,
                                                center: CardActionCenter.shared, engine: engine))
        .task { await watch() }
        .task(id: showingList) { if showingList { await loadResponders() } }
    }

    private var detailPage: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 14) {
                ReminderCalendar(start: snapshot?.start)
                VStack(alignment: .leading, spacing: 5) {
                    Text(snapshot?.title.isEmpty == false ? snapshot?.title ?? "" : "Nhắc hẹn")
                        .font(.system(size: 16, weight: .bold)).strikethrough(snapshot?.deleted == true)
                    ReminderMeta(start: snapshot?.start, repeatLabel: snapshot?.repeatLabel ?? "")
                }
                Spacer(minLength: 0)
            }
            if let snapshot {
                if snapshot.onlyMe {
                    Text("Chỉ bạn nhìn thấy").font(.system(size: 12)).foregroundStyle(.secondary)
                } else if !snapshot.creatorName.isEmpty {
                    Label(snapshot.creatorName + " tạo nhắc hẹn", systemImage: "person.fill").font(.system(size: 12.5)).foregroundStyle(.secondary)
                }
                if snapshot.hasResponders, let summary = snapshot.responderSummary {
                    // Official: the counts line opens the responders page (a link unless the reminder was deleted).
                    Button { showingList = true } label: {
                        HStack(spacing: 6) {
                            Label(summary, systemImage: "person.2").font(.system(size: 12.5, weight: .medium))
                            if !snapshot.deleted { Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold)) }
                        }
                        .foregroundStyle(snapshot.deleted ? AnyShapeStyle(Color.secondary) : AnyShapeStyle(.tint))
                    }
                    .buttonStyle(.plain)
                    .disabled(snapshot.deleted)
                }
                if snapshot.deleted {
                    Text("Nhắc hẹn đã bị xoá").font(.system(size: 12.5)).foregroundStyle(.orange)
                } else if snapshot.showsRsvpButtons {
                    ReminderRsvpButtons(snapshot: snapshot)
                        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(.quaternary.opacity(0.5)))
                } else if snapshot.showsRsvpStatus {
                    ReminderRsvpStatus(snapshot: snapshot)
                }
            } else if unavailable {
                Text("Không tải được chi tiết nhắc hẹn lúc này.").font(.system(size: 12.5)).foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
            HStack { Spacer(); Button("Đóng") { dismiss() }.keyboardShortcut(.cancelAction) }
        }
    }

    // MARK: Responders page

    private enum Tab: Hashable { case accept, reject }
    @State private var tab: Tab = .accept

    private var respondersPage: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Button("Quay lại", systemImage: "chevron.left") { showingList = false }.labelStyle(.iconOnly).buttonStyle(.plain)
                Text("Danh sách tham gia").font(.system(size: 15, weight: .semibold))
                Spacer()
            }
            Picker("", selection: $tab) {
                Text(tabTitle("Tham gia", count: readyList?.acceptTotal)).tag(Tab.accept)
                Text(tabTitle("Từ chối", count: readyList?.rejectTotal)).tag(Tab.reject)
            }
            .pickerStyle(.segmented).labelsHidden()
            Group {
                switch responders {
                case .idle, .loading:
                    ProgressView().controlSize(.small).frame(maxWidth: .infinity, minHeight: 80)
                case .failed:
                    VStack(spacing: 8) {
                        Text("Có lỗi xảy ra").font(.system(size: 12.5)).foregroundStyle(.secondary)
                        Button("Thử lại") { Task { await loadResponders() } }.controlSize(.small)
                    }
                    .frame(maxWidth: .infinity, minHeight: 80)
                case .ready(let list):
                    peopleList(tab == .accept ? list.accept : list.reject, more: tab == .accept ? list.acceptMore : list.rejectMore)
                }
            }
            .frame(height: 280)
            HStack { Spacer(); Button("Đóng") { dismiss() }.keyboardShortcut(.cancelAction) }
        }
    }

    private var readyList: ReminderResponders? {
        if case .ready(let list) = responders { return list }
        return nil
    }

    private func tabTitle(_ title: String, count: Int?) -> String { count.map { "\(title) \($0)" } ?? title }

    private func peopleList(_ people: [ReminderResponders.Person], more: Int) -> some View {
        Group {
            if people.isEmpty {
                Text(tab == .accept ? "Chưa có ai tham gia." : "Chưa có ai từ chối.")
                    .font(.system(size: 12.5)).foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 80)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        ForEach(people) { person in
                            HStack(spacing: 10) {
                                RemoteAvatar(url: person.avatar, name: person.name, size: 30)
                                Text(person.name).font(.system(size: 13)).lineLimit(1)
                                if person.isMe { Text("Bạn").font(.system(size: 11)).foregroundStyle(.secondary) }
                                Spacer(minLength: 0)
                            }
                            .accessibilityElement(children: .combine)
                        }
                        if more > 0 {
                            Text("và \(more) người khác").font(.system(size: 11.5)).foregroundStyle(.secondary)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    /// Read-only; runs when the page is opened (and on "Thử lại").
    private func loadResponders() async {
        responders = .loading
        if let list = await engine.reminderResponders(in: request.conversationId, message: request.messageId) {
            responders = .ready(list)
        } else if !Task.isCancelled {
            responders = .failed
        }
    }

    private func watch() async {
        var signature = ""
        var misses = 0
        while !Task.isCancelled {
            if let next = await engine.reminderWatch(in: request.conversationId, message: request.messageId, signature: signature) {
                snapshot = next
                signature = next.sig
                misses = 0
            } else {
                misses += 1
                if misses >= 3 { unavailable = true; return }
                try? await Task.sleep(for: .seconds(4))
            }
        }
    }
}
