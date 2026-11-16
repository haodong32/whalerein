package store

import (
	"strings"
	"testing"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// TestStatusTransitionMatrixRejectsBackwardAndTerminalOverwrites locks the
// allowed HostResult transitions. The matrix deliberately excludes
// host_accepted -> rejected/host_offline: a Host that already accepted the
// command may fail or become unknown, but a client must never observe the
// request moving backward to rejected or offline.
func TestStatusTransitionMatrixRejectsBackwardAndTerminalOverwrites(t *testing.T) {
	all := []whalereinv1.OnlineStatus{
		whalereinv1.OnlineStatus_received,
		whalereinv1.OnlineStatus_host_offline,
		whalereinv1.OnlineStatus_rejected,
		whalereinv1.OnlineStatus_host_accepted,
		whalereinv1.OnlineStatus_succeeded,
		whalereinv1.OnlineStatus_failed,
		whalereinv1.OnlineStatus_unknown,
	}
	allowed := map[whalereinv1.OnlineStatus][]whalereinv1.OnlineStatus{
		whalereinv1.OnlineStatus_received: {
			whalereinv1.OnlineStatus_host_offline,
			whalereinv1.OnlineStatus_rejected,
			whalereinv1.OnlineStatus_host_accepted,
			whalereinv1.OnlineStatus_succeeded,
			whalereinv1.OnlineStatus_failed,
			whalereinv1.OnlineStatus_unknown,
		},
		whalereinv1.OnlineStatus_host_accepted: {
			whalereinv1.OnlineStatus_succeeded,
			whalereinv1.OnlineStatus_failed,
			whalereinv1.OnlineStatus_unknown,
		},
		whalereinv1.OnlineStatus_unknown: {
			whalereinv1.OnlineStatus_succeeded,
			whalereinv1.OnlineStatus_failed,
		},
	}
	for _, from := range all {
		for _, to := range all {
			want := false
			for _, candidate := range allowed[from] {
				if candidate == to {
					want = true
				}
			}
			if got := statusTransitionAllowed(from, to); got != want {
				t.Errorf("transition %s -> %s = %v, want %v", from, to, got, want)
			}
		}
	}
	// Named regressions from the audit.
	for _, tc := range []struct{ from, to whalereinv1.OnlineStatus }{
		{whalereinv1.OnlineStatus_host_accepted, whalereinv1.OnlineStatus_rejected},
		{whalereinv1.OnlineStatus_host_accepted, whalereinv1.OnlineStatus_host_offline},
		{whalereinv1.OnlineStatus_succeeded, whalereinv1.OnlineStatus_unknown},
		{whalereinv1.OnlineStatus_failed, whalereinv1.OnlineStatus_succeeded},
	} {
		if statusTransitionAllowed(tc.from, tc.to) {
			t.Fatalf("%s -> %s must stay rejected", tc.from, tc.to)
		}
	}
}

// TestHostResultPredicateIsGeneratedFromTheStatusMatrix locks the exact SQL
// fragment that applyHostResult embeds. Because production embeds this string,
// editing the matrix (or the generator) cannot silently widen the durable
// transition guard.
func TestHostResultPredicateIsGeneratedFromTheStatusMatrix(t *testing.T) {
	want := "(status = 'received' AND $4 IN ('host_offline','rejected','host_accepted','succeeded','failed','unknown'))" +
		" OR (status = 'host_accepted' AND $4 IN ('succeeded','failed','unknown'))" +
		" OR (status = 'unknown' AND $4 IN ('succeeded','failed'))"
	if hostResultStatusPredicate != want {
		t.Fatalf("predicate = %q, want %q", hostResultStatusPredicate, want)
	}
	for _, forbidden := range []string{
		"status = 'host_accepted' AND $4 IN ('host_offline'",
		"status = 'host_accepted' AND $4 IN ('rejected'",
		"'succeeded','host_offline'",
		"'succeeded','rejected'",
	} {
		if strings.Contains(hostResultStatusPredicate, forbidden) {
			t.Fatalf("predicate permits a forbidden transition: %q", forbidden)
		}
	}
	// Every allowed pair must be present, so a shrinking matrix also fails.
	for from, targets := range statusTransitions {
		for _, to := range targets {
			if !statusTransitionAllowed(from, to) {
				t.Fatalf("matrix and predicate disagree on %s -> %s", from, to)
			}
		}
	}
}
