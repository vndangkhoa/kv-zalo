import Testing
import CoreImage.CIFilterBuiltins
import Foundation
@testable import SwiftZalo

func isQrWaiting(_ s: ServiceState) -> Bool { if case .qrWaiting = s { true } else { false } }
func isQrScanned(_ s: ServiceState) -> Bool { if case .qrScanned = s { true } else { false } }

@MainActor
@Suite struct LoginFlow {
    @Test func qrFlowReachesLogin() async throws {
        let store = AppStore()
        #expect(!store.isLoggedIn)
        await store.beginQrLogin()

        #expect(!store.isLoggedIn)
        await store.reload()
        #expect(store.users.isEmpty)
        #expect(store.conversations.isEmpty)
        #expect(isQrWaiting(store.serviceState))

        store.simulateScan(deviceName: "iPhone của Bạn")
        #expect(isQrScanned(store.serviceState))
        #expect(!store.isLoggedIn)

        await store.simulateConfirm()
        #expect(store.isLoggedIn)
        #expect(!store.conversations.isEmpty)
        #expect(store.conversations.allSatisfy { !store.messages(in: $0.id).isEmpty })
    }

    @Test func sendMessageAppendsAndSimulatesReply() async throws {
        let store = AppStore()
        await store.beginQrLogin()
        store.simulateScan(deviceName: "iPhone")
        await store.simulateConfirm()
        let conv = try #require(store.conversations.first)
        let before = store.messages(in: conv.id).count

        store.send(.text("Xin chào"), to: conv.id)
        #expect(store.messages(in: conv.id).count == before + 1)
        #expect(store.messages(in: conv.id).last?.isMine == true)

        // typing indicator then reply arrive via simulated service
        try await Task.sleep(for: .seconds(1.2))
        #expect(store.typingIn[conv.id] != nil)
        try await Task.sleep(for: .seconds(1.5))
        #expect(store.messages(in: conv.id).count >= before + 2)
    }

    @Test func logoutClearsSessionAndAllowsLoginAgain() async throws {
        let store = AppStore()
        await store.beginQrLogin()
        store.simulateScan(deviceName: "iPhone")
        await store.simulateConfirm()
        let conversation = try #require(store.conversations.first)
        store.searchText = "old search"
        store.tab = .settings
        store.send(.text("Pending reply"), to: conversation.id)
        try await Task.sleep(for: .seconds(1.2))
        #expect(!store.typingIn.isEmpty)

        store.logOut()
        #expect(store.isLoggedOut)
        #expect(!store.isLoggedIn)
        #expect(store.conversations.isEmpty)
        #expect(store.messages.isEmpty)
        #expect(store.users.isEmpty)
        #expect(store.labels.isEmpty)
        #expect(store.typingIn.isEmpty)
        #expect(store.selectedConversationId == nil)
        #expect(store.searchText.isEmpty)
        #expect(store.tab == .messages)
        #expect(store.service.sendMessage(.text("Invalid"), to: conversation.id) == nil)

        // LoginView starts a new QR flow after logout; it must stay on login.
        await store.beginQrLogin()
        #expect(!store.isLoggedIn)
        let waitingState = store.serviceState
        await store.beginQrLogin()
        #expect(store.serviceState == waitingState)
        store.simulateScan(deviceName: "iPhone")
        await store.simulateConfirm()
        #expect(store.isLoggedIn)
        try await Task.sleep(for: .seconds(1.8))
        // Replies from the previous session must not populate the new session.
        #expect(store.messages[conversation.id] == nil)
        #expect(store.typingIn[conversation.id] == nil)
    }

    @Test func qrImageGenerates() {
        let f = CIFilter.qrCodeGenerator()
        f.message = Data("zalo://login?demo".utf8)
        #expect(f.outputImage != nil)
        #expect(f.outputImage!.extent.width > 0)
    }
}
