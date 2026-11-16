// Capability-aware delivery selection for idle, running, queue and unsupported
// cases.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Delivery policy")
struct DeliveryPolicyTests {
    @Test("idle work offers only Send")
    func idle() {
        #expect(DeliveryPolicy.choices(for: .idle) == [.send])
        #expect(DeliveryPolicy.defaultChoice(for: .idle) == .send)
    }

    @Test("running work defaults to Steer")
    func runningDefaultsToSteer() {
        let capabilities = SessionCapabilities(running: true, supportsSteer: true, supportsQueue: true)
        #expect(DeliveryPolicy.defaultChoice(for: capabilities) == .steer)
        #expect(DeliveryPolicy.choices(for: capabilities) == [.steer, .queue])
    }

    @Test("running work without Steer defaults to Queue when supported")
    func runningWithoutSteer() {
        let capabilities = SessionCapabilities(running: true, supportsSteer: false, supportsQueue: true)
        #expect(DeliveryPolicy.choices(for: capabilities) == [.queue])
        #expect(DeliveryPolicy.defaultChoice(for: capabilities) == .queue)
    }

    @Test("running work with no supported mode offers nothing")
    func runningWithoutModes() {
        let capabilities = SessionCapabilities(running: true, supportsSteer: false, supportsQueue: false)
        #expect(DeliveryPolicy.choices(for: capabilities).isEmpty)
        #expect(DeliveryPolicy.defaultChoice(for: capabilities) == nil)
    }

    @Test("unsupported explicitly selected modes throw a localized error")
    func unsupportedMode() {
        let idleCapabilities = SessionCapabilities.idle
        #expect(throws: SessionServiceError(.unsupportedDelivery)) {
            _ = try DeliveryPolicy.resolve(.steer, for: idleCapabilities)
        }
        #expect(throws: SessionServiceError(.unsupportedDelivery)) {
            _ = try DeliveryPolicy.resolve(.queue, for: idleCapabilities)
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
