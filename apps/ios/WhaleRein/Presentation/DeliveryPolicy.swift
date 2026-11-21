// Capability-aware delivery selection.
//
// Ordinary Send uses the native queue, matching the CLI default
// (`enqueue-after-run`). Steering and start-if-idle remain explicit choices
// when the Source advertises them; a Source without queue support keeps the
// best supported fallback rather than inventing a mode.

import Foundation

enum DeliveryPolicy {
    static func choices(for capabilities: SessionCapabilities) -> [DeliveryChoice] {
        var result: [DeliveryChoice] = []
        if capabilities.supportsQueue {
            result.append(.queue)
        }
        // Start-if-idle is meaningful only while no turn is running.
        if !capabilities.running {
            result.append(.send)
        }
        if capabilities.supportsSteer {
            result.append(.steer)
        }
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
