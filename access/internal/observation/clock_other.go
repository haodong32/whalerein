//go:build !unix

package observation

// systemClock reports that the shared CLOCK_MONOTONIC source is unavailable on
// this platform. Production capture is Linux-only, so tracing fails explicitly
// instead of substituting another clock.
func systemClock() Clock { return unavailableClock{} }

type unavailableClock struct{}

func (unavailableClock) Now() (uint64, error) { return 0, ErrClockUnavailable }
