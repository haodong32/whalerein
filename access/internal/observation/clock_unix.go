//go:build unix

package observation

import (
	"fmt"

	"golang.org/x/sys/unix"
)

// monotonicClock reads the shared kernel CLOCK_MONOTONIC in nanoseconds. The
// contract requires one clock source and epoch for every participating
// producer; there is no offset or drift mapping.
type monotonicClock struct{}

func (monotonicClock) Now() (uint64, error) {
	var reading unix.Timespec
	if err := unix.ClockGettime(unix.CLOCK_MONOTONIC, &reading); err != nil {
		return 0, fmt.Errorf("%w: %v", ErrClockUnavailable, err)
	}
	if reading.Sec < 0 || reading.Nsec < 0 {
		return 0, fmt.Errorf("%w: negative reading", ErrClockUnavailable)
	}
	return uint64(reading.Sec)*1_000_000_000 + uint64(reading.Nsec), nil
}

func systemClock() Clock { return monotonicClock{} }
