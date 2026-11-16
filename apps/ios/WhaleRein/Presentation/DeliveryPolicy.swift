// Capability-aware delivery selection. Idle work starts with Send; running work
// defaults to Steer and offers Queue only when the Source supports it.

import Foundation

enum DeliveryPolicy {
    static func choices(for capabilities: SessionCapabilities) -> [DeliveryChoice] {
        guard capabilities.running else { return [.send] }
        var result: [DeliveryChoice] = []
        if capabilities.supportsSteer { result.append(.steer) }
        if capabilities.supportsQueue { result.append(.queue) }
        return result
    }

    static func defaultChoice(for capabilities: SessionCapabilities) -> DeliveryChoice? {
        choices(for: capabilities).first
    }

    /// Returns the choice to submit, or throws a localized unsupported-mode error
    /// so the composer can keep the draft.
    static func resolve(_ requested: DeliveryChoice, for capabilities: SessionCapabilities) throws -> DeliveryChoice {
        let allowed = choices(for: capabilities)
        guard allowed.contains(requested) else {
            throw SessionServiceError(.unsupportedDelivery)
        }
        return requested
    }

    static func isSupported(_ choice: DeliveryChoice, for capabilities: SessionCapabilities) -> Bool {
        choices(for: capabilities).contains(choice)
    }
}
