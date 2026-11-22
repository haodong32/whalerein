//go:build unix

package payload

import (
	"context"
	"errors"
	"testing"
	"time"
)

// The free-space reserve is measured against the real filesystem of the quota
// directory. Raising the reserve above actual free space must close admission
// before any body is consumed; this test runs natively and does not emulate an
// operating system.
func TestFilesystemReserveClosesAdmissionNatively(t *testing.T) {
	root := t.TempDir()
	now := time.Unix(1_700_000_000, 0).UTC()
	free, err := freeBytes(root)
	if err != nil {
		t.Fatalf("statfs: %v", err)
	}
	if free <= 0 {
		t.Skip("temporary filesystem reports no free space")
	}
	config := testConfig(root)
	// Leave a wide margin: coverage runs may let another process change free
	// space between the two statfs calls.
	config.FreeSpaceReserveBytes = free + (1 << 30)
	service, err := New(Dependencies{Store: &fakeStore{t: t}, Config: config})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if err := service.Ready(context.Background()); !errors.Is(err, ErrHeadroom) {
		t.Fatalf("readiness = %v, want ErrHeadroom", err)
	}
	_, err = service.Reserve(context.Background(), ReserveInput{
		HostID: "host-1", RequestID: "request-1", Purpose: "result",
		Length: 1, Digest: digestOf([]byte("x")),
	})
	if !errors.Is(err, ErrHeadroom) {
		t.Fatalf("reserve = %v, want ErrHeadroom", err)
	}
	if stats, err := service.Stats(context.Background()); err != nil || stats.HeadroomBytes != 0 {
		t.Fatalf("stats = %+v err=%v", stats, err)
	}
	_ = now
}
