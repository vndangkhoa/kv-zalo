import SwiftUI

/// Voice / video call buttons for the chat header (1-1 conversations: friends only; groups: video only, by the
/// member thresholds of the official header). Disabled with a tooltip while calls are unavailable.
/// Meant for a `ToolbarItemGroup`: the buttons are plain icon buttons there and share one glass capsule.
struct ChatCallButtons: View {
    @Environment(ZaloEngine.self) private var engine
    let conversation: ZaloEngine.ConversationSummary

    var body: some View {
        let audio = engine.callAvailability(for: conversation, video: false)
        let video = engine.callAvailability(for: conversation, video: true)
        if audio != .hidden { button("Gọi thoại", symbol: "phone", availability: audio, isVideo: false) }
        if video != .hidden { button("Gọi video", symbol: "video", availability: video, isVideo: true) }
    }

    @ViewBuilder
    private func button(_ title: String, symbol: String, availability: CallAvailability, isVideo: Bool) -> some View {
        let tooltip: String = {
            if case .disabled(let reason) = availability { return reason }
            return conversation.isGroup ? "Gọi video nhóm \(conversation.displayName)" : "\(title) với \(conversation.displayName)"
        }()
        Button {
            engine.startCall(to: conversation.id, video: isVideo)
        } label: {
            Label(title, systemImage: symbol)
        }
        .labelStyle(.iconOnly)
        .disabled(availability != .available)
        .help(tooltip)
        .accessibilityLabel(title)
    }
}

/// Banner for an incoming call plus the alert for call errors. The native helper (ZaloCall) draws the
/// ringing window with Accept / Decline; the banner tells the user where to look when the app is in front.
struct CallOverlay: ViewModifier {
    @Environment(ZaloEngine.self) private var engine

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .top) {
                if let incoming = engine.call.incoming {
                    IncomingCallBanner(incoming: incoming)
                        .padding(.top, 10)
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
            .animation(.smooth(duration: 0.25), value: engine.call.incoming)
            .sheet(item: Binding(get: { engine.call.picker }, set: { _ in })) { picker in
                GroupCallPickerSheet(picker: picker)
            }
            .alert(engine.call.notice?.title ?? "Cuộc gọi",
                   isPresented: Binding(get: { engine.call.notice != nil }, set: { if !$0 { engine.call.notice = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(engine.call.notice?.message ?? "")
            }
    }
}

extension View {
    func callOverlay() -> some View { modifier(CallOverlay()) }
}

private struct IncomingCallBanner: View {
    @Environment(ZaloEngine.self) private var engine
    let incoming: CallState.Incoming

    var body: some View {
        HStack(spacing: 12) {
            RemoteAvatar(url: incoming.avatar, name: incoming.name, size: 38)
            VStack(alignment: .leading, spacing: 1) {
                Text(incoming.group ? "Cuộc gọi nhóm đến" : "Cuộc gọi đến")
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
                Text(incoming.name)
                    .font(.system(size: 14, weight: .semibold))
                    .lineLimit(1)
                if let caller = incoming.caller {
                    Text("\(caller) đang gọi")
                        .font(.system(size: 11.5))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 12)
            Button("Mở cửa sổ cuộc gọi") { engine.showCallWindow() }
                .buttonStyle(.glassProminent)
                .help("Trả lời hoặc từ chối trong cửa sổ cuộc gọi của Zalo")
            Button {
                engine.call.incoming = nil
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.borderless)
            .help("Ẩn")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(maxWidth: 460)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .combine)
    }
}
