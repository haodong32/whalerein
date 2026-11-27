package observation

import "errors"

// Clock is the injectable measurement clock. Production reads the shared
// kernel CLOCK_MONOTONIC in nanoseconds through golang.org/x/sys/unix; tests
// inject a deterministic source. Open verifies the source once and rejects a
// clock that cannot be read, so a capture never mixes unverified time.
type Clock interface {
	Now() (uint64, error)
}

// ErrClockUnavailable marks a measurement clock that cannot be verified on
// this platform or topology.
var ErrClockUnavailable = errors.New("observation: CLOCK_MONOTONIC is unavailable")
