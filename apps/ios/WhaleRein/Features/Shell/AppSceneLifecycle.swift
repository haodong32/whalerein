// Ordered owner of process scene transitions.
//
// iOS delivers active/inactive/background notifications as an ordered stream,
// but may repeat them, and SwiftUI starts each change from its own callback.
// The shell accepts every notification synchronously here so the privacy shield
// is already up before the system takes its snapshot, then serializes the
// asynchronous work for each accepted transition. A transition carries a
// monotonic generation: an older transition that finishes after a newer
// notification must not reveal content, resume a watch or refresh a surface.
//
// Inactive and background are deliberately different states:
//
// - inactive shields and pauses visible rendering but keeps the healthy watch,
//   its cursor and the event subscription. It performs no network work.
// - background also marks the observation suspended. The ordered background
//   step persists the draft and stops the Core watch exactly once; the ordered
//   foreground step starts it once again from the Core cursor.
//
// A repeated notification for the current phase is coalesced, so duplicate
// callbacks cannot stop the watch twice or start two resumes.

import Foundation

struct AppSceneLifecycle: Equatable, Sendable {
    /// One accepted phase change. `resumesObservation` is captured when the
    /// notification arrives, so an inactive notification between a background
    /// entry and its resume cannot erase the pending resume.
    struct Transition: Equatable, Sendable {
        let phase: AppScenePhase
        let previous: AppScenePhase
        let generation: UInt64
        let resumesObservation: Bool
    }

    private(set) var phase: AppScenePhase = .active
    /// Monotonic token of the newest accepted notification.
    private(set) var generation: UInt64 = 0
    /// True from an accepted background notification until the ordered resume
    /// completes. A brief inactive notification does not clear it.
    private(set) var observationSuspended = false

    /// True only when content may be shown without a pending resume. The app
    /// may be active while a background resume is still in flight.
    var isActiveAndResumed: Bool { phase == .active && !observationSuspended }

    /// Accepts one notification. Returns nil when the phase did not change, so
    /// rapid duplicates coalesce into a single transition and one execution of
    /// its side effects.
    mutating func accept(_ next: AppScenePhase) -> Transition? {
        guard next != phase else { return nil }
        let previous = phase
        generation &+= 1
        phase = next
        if next == .background {
            observationSuspended = true
        }
        return Transition(
            phase: next,
            previous: previous,
            generation: generation,
            resumesObservation: next == .active && observationSuspended
        )
    }

    /// True while `transition` is still the newest accepted notification.
    func isCurrent(_ transition: Transition) -> Bool {
        generation == transition.generation && phase == transition.phase
    }

    /// Clears the background suspension only when the ordered resume finished
    /// while still current. A newer background notification keeps it set.
    mutating func observationDidResume() {
        observationSuspended = false
    }
}