package serverready

import (
	"context"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

type session bool

func (s session) Ready() bool { return bool(s) }

type presence bool

func (p presence) Available() bool { return bool(p) }

type storage struct {
	enabled bool
	err     error
}

func (s storage) Enabled() bool               { return s.enabled }
func (s storage) Ready(context.Context) error { return s.err }

func TestAddChecksReportsBrokerAndStorage(t *testing.T) {
	registry := diagnostics.NewReadiness()
	AddChecks(registry, session(true), presence(true), storage{})
	if failures := registry.Check(context.Background()); len(failures) != 0 {
		t.Fatalf("ready checks: %v", failures)
	}
	registry = diagnostics.NewReadiness()
	AddChecks(registry, session(false), presence(true), storage{})
	if failures := registry.Check(context.Background()); len(failures) == 0 {
		t.Fatal("unready session accepted")
	}
	registry = diagnostics.NewReadiness()
	AddChecks(registry, session(true), presence(false), storage{})
	if failures := registry.Check(context.Background()); len(failures) == 0 {
		t.Fatal("unreconciled presence accepted")
	}
	registry = diagnostics.NewReadiness()
	AddChecks(registry, session(true), presence(true), storage{enabled: true, err: context.DeadlineExceeded})
	if failures := registry.Check(context.Background()); len(failures) == 0 {
		t.Fatal("unready storage accepted")
	}
	// A nil storage seam never registers the check.
	registry = diagnostics.NewReadiness()
	AddChecks(registry, session(true), presence(true), nil)
	if failures := registry.Check(context.Background()); len(failures) != 0 {
		t.Fatalf("nil storage: %v", failures)
	}
}

func TestHTTPTimeoutsBoundsStorageTransfer(t *testing.T) {
	read, write := time.Second, time.Second
	if gotRead, gotWrite := HTTPTimeouts(read, write, false, time.Hour, time.Hour); gotRead != read || gotWrite != write {
		t.Fatalf("disabled storage changed timeouts: %v %v", gotRead, gotWrite)
	}
	if gotRead, gotWrite := HTTPTimeouts(read, write, true, 2*time.Second, 3*time.Second); gotRead != 7*time.Second || gotWrite != 8*time.Second {
		t.Fatalf("bounded timeouts: %v %v", gotRead, gotWrite)
	}
	if gotRead, gotWrite := HTTPTimeouts(10*time.Second, 10*time.Second, true, time.Second, time.Second); gotRead != 10*time.Second || gotWrite != 10*time.Second {
		t.Fatalf("larger configured timeouts were lowered: %v %v", gotRead, gotWrite)
	}
}
