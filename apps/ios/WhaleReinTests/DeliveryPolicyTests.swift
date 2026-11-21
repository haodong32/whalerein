// Capability-aware delivery selection for idle, running, queue and unsupported
// cases. Ordinary Send uses the native queue; steer and start-if-idle are
// explicit alternatives.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Delivery policy")
struct DeliveryPolicyTests {
    @Test("idle work defaults to the native queue and offers explicit alternatives")
    func idle() {
        #expect(DeliveryPolicy.choices(for: .idle) == [.queue, .send, .steer])
        #expect(DeliveryPolicy.defaultChoice(for: .idle) == .queue)
    }

    @Test("running work defaults to the native queue")
    func runningDefaultsToQueue() {
        let capabilities = SessionCapabilities(running: true, supportsSteer: true, supportsQueue: true)
        #expect(DeliveryPolicy.defaultChoice(for: capabilities) == .queue)
        #expect(DeliveryPolicy.choices(for: capabilities) == [.queue, .steer])
    }

    @Test("a running Source without Queue falls back to Steer")
    func runningWithoutQueue() {
        let capabilities = SessionCapabilities(running: true, supportsSteer: true, supportsQueue: false)
        #expect(DeliveryPolicy.choices(for: capabilities) == [.steer])
        #expect(DeliveryPolicy.defaultChoice(for: capabilities) == .steer)
    }

    @Test("an idle Source without Queue falls back to start-if-idle")
    func idleWithoutQueue() {
        let capabilities = SessionCapabilities(running: false, supportsSteer: false, supportsQueue: false)
        #expect(DeliveryPolicy.choices(for: capabilities) == [.send])
        #expect(DeliveryPolicy.defaultChoice(for: capabilities) == .send)
    }

    @Test("running work with no supported mode offers nothing")
    func runningWithoutModes() {
        let capabilities = SessionCapabilities(running: true, supportsSteer: false, supportsQueue: false)
        #expect(DeliveryPolicy.choices(for: capabilities).isEmpty)
        #expect(DeliveryPolicy.defaultChoice(for: capabilities) == nil)
    }

    @Test("unsupported explicitly selected modes throw a localized error")
    func unsupportedMode() {
        let noModes = SessionCapabilities(running: false, supportsSteer: false, supportsQueue: false)
        #expect(throws: SessionServiceError(.unsupportedDelivery)) {
            _ = try DeliveryPolicy.resolve(.steer, for: noModes)
        }
        #expect(throws: SessionServiceError(.unsupportedDelivery)) {
            _ = try DeliveryPolicy.resolve(.queue, for: noModes)
        }
        let running = SessionCapabilities(running: true, supportsSteer: true, supportsQueue: true)
        #expect(throws: SessionServiceError(.unsupportedDelivery)) {
            _ = try DeliveryPolicy.resolve(.send, for: running)
        }
    }

    @Test("supported modes are returned unchanged")
    func supportedModes() throws {
        let capabilities = SessionCapabilities(running: true, supportsSteer: true, supportsQueue: true)
        #expect(try DeliveryPolicy.resolve(.steer, for: capabilities) == .steer)
        #expect(try DeliveryPolicy.resolve(.queue, for: capabilities) == .queue)
        #expect(try DeliveryPolicy.resolve(.send, for: .idle) == .send)
    }

    @Test("localized delivery labels exist for every choice")
    func deliveryLabels() {
        for choice in DeliveryChoice.allCases {
            let key = L10nKey.deliveryKey(for: choice)
            #expect(!key.string.isEmpty)
        }
    }
}
