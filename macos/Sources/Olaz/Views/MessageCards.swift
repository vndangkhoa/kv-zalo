import SwiftUI
import AppKit
import MapKit

// Native renderings of the PC app's rich message types (card layouts follow the official components, see the
// contract comment in HostScripts/chat-adapter.js for the bundle offsets). Everything shown comes from the
// official message object; buttons that would act on the user's behalf (vote, accept, call back, send a sticker)
// are not offered. Images go through `RemoteImage` (MediaLoader: URL cache + DNS-over-HTTPS fallback).

// MARK: - Shared chrome

private struct CardChrome: ViewModifier {
    var radius: CGFloat = 14

    func body(content: Content) -> some View {
        content
            .background(ChatPalette.incoming)
            .clipShape(RoundedRectangle(cornerRadius: radius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).strokeBorder(Color.primary.opacity(0.06), lineWidth: 0.5))
    }
}

extension View {
    func cardChrome(radius: CGFloat = 14) -> some View { modifier(CardChrome(radius: radius)) }
}

enum CardFormat {
    static let vi = Locale(identifier: "vi_VN")

    private static func formatter(_ format: String) -> DateFormatter {
        let f = DateFormatter()
        f.locale = vi
        f.dateFormat = format
        return f
    }
    static let weekday = formatter("EEEE")
    static let day = formatter("dd")
    static let month = formatter("MMMM")
    /// Official reminder line: "WD DD/MM/YYYY" + time.
    static let dateTime = formatter("EEEE dd/MM/yyyy HH:mm")
}

// MARK: - Contact card (danh thiếp)

/// Official `name-card` (module "F1+M"): avatar, name (the local friend name wins), phone / OA link, QR, account-type label,
/// and the footer buttons: KẾT BẠN (stranger) or GỌI ĐIỆN (friend) on the left, NHẮN TIN on the right; none on my own card.
/// Tapping the card opens the profile. Buttons start official flows only when clicked (see `CardActions.swift`).
struct ContactCardView: View {
    let card: ChatMessage.ContactCard
    @Environment(ZaloEngine.self) private var engine
    @Environment(\.cardActions) private var actions
    @State private var isSelf = false
    @State private var isFriend = false
    @State private var stateKnown = false

    private var extra: String { card.phone.isEmpty ? card.oaLink : card.phone }

    private func profileRequest(addFriend: Bool) -> ProfileRequest {
        ProfileRequest(uid: card.uid, conversationId: actions.conversationId, name: card.name, avatar: card.avatar, phone: card.phone,
                       isOfficialAccount: card.isOfficialAccount, origin: addFriend ? .add : .card, startWithFriendRequest: addFriend)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                if !card.uid.isEmpty { actions.showProfile(profileRequest(addFriend: false)) }
            } label: { header }
            .buttonStyle(.plain)
            .disabled(card.uid.isEmpty)
            .help(card.uid.isEmpty ? "" : "Xem trang cá nhân")
            if !card.uid.isEmpty, !isSelf {
                Divider()
                HStack(spacing: 0) {
                    if stateKnown, !card.isOfficialAccount {
                        if isFriend { callButton } else { addFriendButton }
                        Divider().frame(height: 22)
                    }
                    footerButton("NHẮN TIN", help: "Mở cuộc trò chuyện với \(card.name)") { engine.openContactConversation(card.uid) }
                }
            }
        }
        .frame(width: 280)
        .cardChrome()
        .task(id: card.uid) {
            guard !card.uid.isEmpty else { return }
            let state = await engine.contactCardState(card.uid)
            isSelf = state.isSelf
            isFriend = state.isFriend
            stateKnown = true
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            RemoteAvatar(url: card.avatar, name: card.name, size: 52)
            VStack(alignment: .leading, spacing: 2) {
                Text(card.name.isEmpty ? "Danh thiếp" : card.name)
                    .font(.system(size: 14, weight: .semibold)).lineLimit(2)
                if !extra.isEmpty {
                    Text(extra).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(1).textSelection(.enabled)
                }
                Text(card.isOfficialAccount ? "Official Account" : "Danh thiếp Zalo")
                    .font(.system(size: 11, weight: .medium)).foregroundStyle(card.isOfficialAccount ? AnyShapeStyle(Color.orange) : AnyShapeStyle(TintShapeStyle()))
                    .padding(.top, 2)
            }
            Spacer(minLength: 0)
            if let qr = card.qr {
                RemoteImage(url: qr, contentMode: .fit, maxPixelSize: 220) { Color.clear }
                    .frame(width: 54, height: 54)
                    .background(Color.white, in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                    .accessibilityHidden(true)
            }
        }
        .padding(12)
        .foregroundStyle(.primary)
        .contentShape(Rectangle())
    }

    private var addFriendButton: some View {
        footerButton("KẾT BẠN", help: "Gửi lời mời kết bạn") { actions.showProfile(profileRequest(addFriend: true)) }
    }

    private var callButton: some View {
        footerButton("GỌI ĐIỆN", help: engine.canCall ? "Gọi thoại" : CardActionText.callUnsupported, enabled: engine.canCall) {
            engine.startCall(to: card.uid, video: false)
        }
    }

    private func footerButton(_ title: String, help: String, enabled: Bool = true, action: @escaping () -> Void) -> some View {
        Button(title, action: action)
            .buttonStyle(.plain)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(enabled ? AnyShapeStyle(TintShapeStyle()) : AnyShapeStyle(Color.secondary))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 9)
            .contentShape(Rectangle())
            .disabled(!enabled)
            .help(help)
    }
}

// MARK: - Link preview

/// Official link card (module "SJ2v"): thumbnail, title (page title or URL), description, source site. Click opens the URL.
struct LinkCard: View {
    let title: String
    let url: URL?
    let thumbnail: URL?
    var description: String = ""
    var source: String = ""
    @Environment(\.cardActions) private var actions

    var body: some View {
        Button {
            actions.openLink(url)   // a group invite link opens the group sheet (official GROUP_PREVIEW)
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                if let thumbnail {
                    RemoteImage(url: thumbnail, contentMode: .fill, maxPixelSize: 520) { Rectangle().fill(.quaternary) }
                        .frame(width: 280, height: 146)
                        .clipped()
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text(title.isEmpty ? (url?.absoluteString ?? "Liên kết") : title)
                        .font(.system(size: 13, weight: .semibold)).lineLimit(2)
                        .multilineTextAlignment(.leading)
                    if !description.isEmpty {
                        Text(description).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(2)
                            .multilineTextAlignment(.leading)
                    }
                    let site = source.isEmpty ? ChatFormat.host(url) : source
                    if !site.isEmpty {
                        Text(site).font(.system(size: 11.5)).foregroundStyle(.tertiary).lineLimit(1)
                    }
                }
                .foregroundStyle(.primary)
                .padding(10)
                .frame(width: 280, alignment: .leading)
            }
            .cardChrome()
            .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(WebLink.sanitized(url) == nil)
        .help(WebLink.sanitized(url)?.absoluteString ?? "")
    }
}

// MARK: - Location

/// Official location card: title, address and a map of the coordinates; click opens the map in the browser.
struct LocationCard: View {
    let info: ChatMessage.LocationInfo
    var mine: Bool = false

    var body: some View {
        Button {
            WebLink.open(info.mapURL)
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                if info.coordinate != nil || info.thumbnail != nil {
                    LocationMap(info: info).frame(width: 260, height: 150)
                }
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Image(systemName: "mappin.circle.fill").foregroundStyle(.red)
                        Text(info.title.isEmpty ? (info.address.isEmpty ? "Vị trí" : info.address) : info.title)
                            .font(.system(size: 13, weight: .semibold)).lineLimit(2).multilineTextAlignment(.leading)
                    }
                    if !info.title.isEmpty, !info.address.isEmpty {
                        Text(info.address).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(3)
                            .multilineTextAlignment(.leading)
                    }
                }
                .foregroundStyle(.primary)
                .padding(10)
                .frame(width: 260, alignment: .leading)
            }
            .cardChrome()
            .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(WebLink.sanitized(info.mapURL) == nil)
        .help(info.mapURL == nil ? "" : "Mở bản đồ")
    }
}

/// The message's own map thumbnail when it has one, otherwise an Apple Maps snapshot of the coordinates.
private struct LocationMap: View {
    let info: ChatMessage.LocationInfo
    @State private var snapshot: NSImage?

    var body: some View {
        ZStack {
            if let thumb = info.thumbnail {
                RemoteImage(url: thumb, contentMode: .fill, maxPixelSize: 520) { placeholder }
            } else if let snapshot {
                Image(nsImage: snapshot).resizable().aspectRatio(contentMode: .fill)
            } else {
                placeholder
            }
            if info.thumbnail == nil {
                Image(systemName: "mappin").font(.system(size: 26, weight: .bold)).foregroundStyle(.red)
                    .shadow(radius: 1.5).offset(y: -12)
            }
        }
        .clipped()
        .task(id: info.coordinate) { await loadSnapshot() }
    }

    private var placeholder: some View {
        Rectangle().fill(.quaternary).overlay { Image(systemName: "map").font(.system(size: 28)).foregroundStyle(.tertiary) }
    }

    private static let cache: NSCache<NSString, NSImage> = { let c = NSCache<NSString, NSImage>(); c.countLimit = 200; return c }()

    private func loadSnapshot() async {
        guard info.thumbnail == nil, let coordinate = info.coordinate else { return }
        let key = "\(coordinate.latitude),\(coordinate.longitude)" as NSString
        if let hit = Self.cache.object(forKey: key) { snapshot = hit; return }
        let options = MKMapSnapshotter.Options()
        options.region = MKCoordinateRegion(center: CLLocationCoordinate2D(latitude: coordinate.latitude, longitude: coordinate.longitude),
                                            span: MKCoordinateSpan(latitudeDelta: 0.012, longitudeDelta: 0.012))
        options.size = CGSize(width: 260, height: 150)
        options.showsBuildings = true
        guard let result = try? await MKMapSnapshotter(options: options).start() else { return }
        Self.cache.setObject(result.image, forKey: key)
        snapshot = result.image
    }
}

// MARK: - E-card (standalone)

/// Official e-card (module at ~247500 / ~229000): banner (background `href`, avatar `thumb`, title, description),
/// new-friend and birthday cards, and reminder cards. Rendered centered, outside the bubbles.
struct ECardView: View {
    let card: ChatMessage.ECard
    /// The card's live buttons (official action names) and whether it offers its suggested stickers; see `CardActions.swift`.
    var buttons: [CardButton] = []
    var suggestsStickers = false

    var body: some View {
        Group {
            switch card.style {
            case .reminder: reminder
            case .friend, .birthday: greeting
            case .banner: banner
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
    }

    private var backdrop: some View {
        Group {
            if let image = card.image {
                RemoteImage(url: image, contentMode: .fill, maxPixelSize: 720) { gradient }
            } else {
                gradient
            }
        }
    }

    private var gradient: some View {
        LinearGradient(colors: [ZaloTheme.blue.opacity(0.85), Color.purple.opacity(0.65)], startPoint: .topLeading, endPoint: .bottomTrailing)
    }

    private var banner: some View {
        VStack(spacing: 0) {
            ZStack(alignment: .bottom) {
                backdrop.frame(width: 300, height: 170).clipped()
                LinearGradient(colors: [.clear, .black.opacity(0.45)], startPoint: .center, endPoint: .bottom)
                VStack(spacing: 6) {
                    if let thumb = card.thumb {
                        RemoteImage(url: thumb, contentMode: .fill, maxPixelSize: 200) { Circle().fill(.quaternary) }
                            .frame(width: 67, height: 67).clipShape(Circle())
                            .overlay(Circle().strokeBorder(.white.opacity(0.9), lineWidth: 2))
                            .padding(.top, 14)
                    }
                    Spacer(minLength: 0)
                    VStack(spacing: 2) {
                        if !card.title.isEmpty { Text(card.title).font(.system(size: 14, weight: .bold)).lineLimit(2) }
                        if !card.description.isEmpty { Text(card.description).font(.system(size: 13)).lineLimit(2) }
                    }
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.white)
                    .shadow(color: .black.opacity(0.35), radius: 2)
                    .padding(.horizontal, 12).padding(.bottom, 10)
                }
                .frame(width: 300, height: 170)
            }
            if suggestsStickers { StickerSuggestionStrip().padding(.vertical, 6).frame(width: 300) }
            CardButtonBar(buttons: buttons)
        }
        .cardChrome(radius: 16)
        .accessibilityElement(children: .combine)
    }

    /// New friend / birthday: background image, overlapping avatars, one title line, then the suggested stickers / buttons.
    private var greeting: some View {
        VStack(spacing: 0) {
            greetingHeader
            if suggestsStickers || !buttons.isEmpty {
                VStack(spacing: 6) {
                    if suggestsStickers {
                        Text(card.style == .birthday ? "Gửi ngay sticker dưới đây thay lời chúc mừng" : "Chọn một sticker dưới đây để bắt đầu trò chuyện")
                            .font(.system(size: 12)).foregroundStyle(.secondary).multilineTextAlignment(.center).padding(.top, 8).padding(.horizontal, 12)
                        StickerSuggestionStrip()
                    }
                    CardButtonBar(buttons: buttons)
                }
                .frame(width: 300)
                .padding(.bottom, buttons.isEmpty ? 6 : 0)
            }
        }
        .modifier(GreetingChrome(framed: suggestsStickers || !buttons.isEmpty))
    }

    private var greetingHeader: some View {
        ZStack {
            backdrop.frame(width: 300, height: 150).clipped()
            Color.black.opacity(0.18)
            VStack(spacing: 10) {
                HStack(spacing: -14) {
                    ForEach(Array(card.avatars.enumerated()), id: \.offset) { _, url in
                        RemoteAvatar(url: url, name: "", size: 52)
                            .overlay(Circle().strokeBorder(.white, lineWidth: 2))
                    }
                }
                if !card.title.isEmpty {
                    Text(card.title).font(.system(size: 13.5, weight: .bold)).foregroundStyle(.white)
                        .shadow(color: .black.opacity(0.4), radius: 2)
                        .multilineTextAlignment(.center).lineLimit(2).padding(.horizontal, 14)
                }
            }
        }
        .frame(width: 300, height: 150)
        .clipShape(UnevenRoundedRectangle(topLeadingRadius: 16, bottomLeadingRadius: 0, bottomTrailingRadius: 0, topTrailingRadius: 16, style: .continuous))
        .accessibilityElement(children: .combine)
    }

    /// Reminder (`action.open.reminder`): the official reminder controller's card, or the static card (see `ReminderViews.swift`).
    private var reminder: some View { ReminderCardView(card: card) }
}

/// A greeting card with stickers / buttons below its image gets the card chrome around the whole stack; a bare image card keeps its rounded image.
private struct GreetingChrome: ViewModifier {
    let framed: Bool

    func body(content: Content) -> some View {
        if framed { content.cardChrome(radius: 16) } else { content.clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous)) }
    }
}

// MARK: - OA list message / article

/// Official OA message (module "9hee"): a lead card (image, title, description) and rows; click opens the page.
struct OAMessageCard: View {
    let parts: [ChatMessage.OAPart]
    @Environment(\.cardActions) private var actions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                if index > 0 { Divider() }
                Button {
                    actions.openLink(part.url)
                } label: {
                    if part.isHeader { header(part) } else { row(part) }
                }
                .buttonStyle(.plain)
                .disabled(WebLink.sanitized(part.url) == nil)
                .help(part.help.isEmpty ? (part.url?.absoluteString ?? "") : part.help)
            }
        }
        .frame(width: 300)
        .cardChrome()
    }

    private func header(_ part: ChatMessage.OAPart) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if let thumb = part.thumb {
                RemoteImage(url: thumb, contentMode: .fill, maxPixelSize: 600) { Rectangle().fill(.quaternary) }
                    .frame(width: 300, height: 158).clipped()
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(part.title).font(.system(size: 14, weight: .semibold)).lineLimit(3)
                if !part.description.isEmpty {
                    Text(part.description).font(.system(size: 12.5)).foregroundStyle(.secondary).lineLimit(3)
                }
            }
            .multilineTextAlignment(.leading)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(10)
        }
        .foregroundStyle(.primary)
        .contentShape(Rectangle())
    }

    private func row(_ part: ChatMessage.OAPart) -> some View {
        HStack(spacing: 10) {
            Text(part.title).font(.system(size: 13)).lineLimit(3).multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let thumb = part.thumb {
                RemoteImage(url: thumb, contentMode: .fill, maxPixelSize: 160) { Rectangle().fill(.quaternary) }
                    .frame(width: 52, height: 52)
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            }
        }
        .foregroundStyle(.primary)
        .padding(10)
        .contentShape(Rectangle())
    }
}

// MARK: - Poll, sticker set, ZInstant, notices

/// Poll event + card (voting, voter list, lock): `PollViews.swift`.
struct PollCard: View {
    let poll: ChatMessage.PollInfo
    let message: ChatMessage

    var body: some View { PollMessageView(poll: poll, message: message) }
}

struct StickerSetCard: View {
    let title: String
    let thumbnail: URL?

    var body: some View {
        HStack(spacing: 12) {
            RemoteImage(url: thumbnail, contentMode: .fit, maxPixelSize: 200, animated: true) {
                Image(systemName: "face.smiling").font(.system(size: 26)).foregroundStyle(.tertiary)
            }
            .frame(width: 60, height: 60)
            VStack(alignment: .leading, spacing: 2) {
                Text(title.isEmpty ? "Bộ sticker" : title).font(.system(size: 13.5, weight: .semibold)).lineLimit(2)
                Text("Bộ sticker").font(.system(size: 11.5)).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .frame(width: 260)
        .cardChrome()
        .accessibilityElement(children: .combine)
    }
}

/// ZInstant (ZNS) message: the official text form of its metadata (the live layout needs the official zinstant engine).
struct ZInstantCard: View {
    let title: String
    let text: String
    let thumbnail: URL?
    /// Links of the metadata's `customMsg.actions` (the template's own buttons need the zinstant engine, which is not available).
    var buttons: [CardButton] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let thumbnail {
                RemoteImage(url: thumbnail, contentMode: .fill, maxPixelSize: 560) { Rectangle().fill(.quaternary) }
                    .frame(width: 280, height: 150).clipped()
            }
            VStack(alignment: .leading, spacing: 4) {
                if !title.isEmpty { Text(title).font(.system(size: 13.5, weight: .semibold)) }
                if !text.isEmpty { Text(ChatFormat.linkified(text, mine: false)).font(.system(size: 13)) }
                CardLinkRow(buttons: buttons)
            }
            .multilineTextAlignment(.leading)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
        }
        .frame(width: 280)
        .cardChrome()
    }
}

/// Centered notice (info message, to-do, vanish, syncing): small secondary text with an optional symbol.
struct NoticeRow: View {
    var symbol: String?
    let text: String
    /// Links of an info message (`msginfo.actionlist` actions).
    var buttons: [CardButton] = []

    var body: some View {
        VStack(spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 5) {
                if let symbol { Image(systemName: symbol).font(.system(size: 10.5)) }
                Text(text)
            }
            CardLinkRow(buttons: buttons)
        }
        .font(.system(size: 11.5))
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 6)
        .padding(.horizontal, 40)
        .accessibilityElement(children: buttons.isEmpty ? .combine : .contain)
        .accessibilityLabel("Hệ thống: \(text)")
    }
}
