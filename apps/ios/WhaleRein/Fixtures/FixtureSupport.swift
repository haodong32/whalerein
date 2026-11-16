// Deterministic clock and seeded RNG for synthetic data. Tests inject fixed
// values; the running app uses the system clock with the same deterministic
// content so transcripts never depend on wall time or system randomness.

import Foundation

protocol FixtureClock: Sendable {
    func now() -> Date
    func sleep(for duration: Duration) async
}

struct SystemFixtureClock: FixtureClock {
    func now() -> Date { Date() }

    func sleep(for duration: Duration) async {
        try? await Task.sleep(for: duration)
    }
}

struct TestFixtureClock: FixtureClock {
    let date: Date
    let sleepsImmediately: Bool

    init(date: Date = Date(timeIntervalSince1970: 1_800_000_000), sleepsImmediately: Bool = true) {
        self.date = date
        self.sleepsImmediately = sleepsImmediately
    }

    func now() -> Date { date }

    func sleep(for duration: Duration) async {
        if !sleepsImmediately {
            try? await Task.sleep(for: duration)
        }
    }
}

/// SplitMix64: small, deterministic and adequate for synthetic fixtures.
struct SeededRandom: RandomNumberGenerator {
    private var state: UInt64

    init(seed: UInt64) {
        state = seed == 0 ? 0x9E37_79B9_7F4A_7C15 : seed
    }

    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }

    mutating func int(in range: ClosedRange<Int>) -> Int {
        let span = UInt64(range.upperBound - range.lowerBound + 1)
        return range.lowerBound + Int(next() % span)
    }
}
