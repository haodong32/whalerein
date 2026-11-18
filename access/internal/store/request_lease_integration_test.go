//go:build integration

package store_test

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func leaseRequest(t *testing.T, ctx context.Context, s *store.Store, id, host, device, command string, lease time.Duration, now time.Time) store.Request {
	t.Helper()
	row := newRequest(id, host, device, command, []byte("digest-"+id), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	deadline := now.Add(lease)
	row.ClientLeaseUntil = &deadline
	row.ClientLeaseState = store.ClientLeaseLive
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatalf("create lease request %s: %v", id, err)
	}
	return row
}

func elapsedLease(t *testing.T, ctx context.Context, s *store.Store, requestID string) {
	t.Helper()
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET client_lease_until = clock_timestamp() - interval '1 second' WHERE request_id = $1`, requestID); err != nil {
		t.Fatalf("make lease elapsed: %v", err)
	}
}

func mustRequest(t *testing.T, ctx context.Context, s *store.Store, requestID string) store.Request {
	t.Helper()
	row, err := s.GetRequest(ctx, requestID)
	if err != nil {
		t.Fatalf("read request %s: %v", requestID, err)
	}
	return row
}

func soon() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), 30*time.Second)
}

// TestSessionObservationAC29ExpiryWins proves a renewal after the elapsed
// deadline is rejected and the row is expired exactly once.
func TestSessionObservationAC29ExpiryWins(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	leaseRequest(t, ctx, s, "r1", "h1", "d1", "c1", 10*time.Second, now)
	elapsedLease(t, ctx, s, "r1")

	expired, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(expired) != 1 || expired[0].RequestID != "r1" {
		t.Fatalf("expiry = %+v err=%v", expired, err)
	}
	if expired[0].ClientLeaseState != store.ClientLeaseExpired || !expired[0].CancelPending || expired[0].CancelPublishedAt != nil {
		t.Fatalf("expiry facts = %+v", expired[0])
	}
	renewed, err := s.RenewClientLease(ctx, "d1", "r1", 10*time.Second)
	if err != nil || renewed {
		t.Fatalf("late renewal = %v %v", renewed, err)
	}
	again, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(again) != 0 {
		t.Fatalf("expiry ran twice: %+v %v", again, err)
	}
}

// TestSessionObservationAC29RenewWins proves a valid renewal extends the lease
// so the later sweep does not expire the row, and the original deadline stays
// unchanged.
func TestSessionObservationAC29RenewWins(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	row := leaseRequest(t, ctx, s, "r1", "h1", "d1", "c1", time.Second, now)

	renewed, err := s.RenewClientLease(ctx, "d1", "r1", 10*time.Second)
	if err != nil || !renewed {
		t.Fatalf("valid renewal = %v %v", renewed, err)
	}
	stored := mustRequest(t, ctx, s, "r1")
	if stored.ExpiresAt.UnixMicro() != row.ExpiresAt.UnixMicro() {
		t.Fatalf("renewal changed the hard deadline: %v -> %v", row.ExpiresAt, stored.ExpiresAt)
	}
	if stored.ClientLeaseUntil == nil || !stored.ClientLeaseUntil.After(now.Add(time.Second)) {
		t.Fatalf("renewal did not extend the lease: %+v", stored.ClientLeaseUntil)
	}

	// Pass the deadline the row had before renewal. The renewed lease must
	// keep it live, so expiry has nothing to close.
	time.Sleep(1100 * time.Millisecond)
	expired, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(expired) != 0 {
		t.Fatalf("renewal lost to an earlier deadline: %+v err=%v", expired, err)
	}
	if got := mustRequest(t, ctx, s, "r1"); got.ClientLeaseState != store.ClientLeaseLive {
		t.Fatalf("state after renewal window = %q", got.ClientLeaseState)
	}
}

// TestSessionObservationAC29RenewalAndExpiryHaveSingleWinner races the two
// transactions and requires exactly one winner, never both.
func TestSessionObservationAC29RenewalAndExpiryHaveSingleWinner(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	for i := 0; i < 20; i++ {
		id := fmt.Sprintf("race-%d", i)
		leaseRequest(t, ctx, s, id, "h1", "d1", "c-"+id, 10*time.Second, now)
		elapsedLease(t, ctx, s, id)

		start := make(chan struct{})
		var renewed, expired bool
		var renewErr, expireErr error
		var wg sync.WaitGroup
		wg.Add(2)
		go func() {
			defer wg.Done()
			<-start
			renewed, renewErr = s.RenewClientLease(ctx, "d1", id, 10*time.Second)
		}()
		go func() {
			defer wg.Done()
			<-start
			rows, err := s.ExpireClientLeases(ctx, 100)
			expireErr = err
			for _, row := range rows {
				if row.RequestID == id {
					expired = true
				}
			}
		}()
		close(start)
		wg.Wait()
		if renewErr != nil || expireErr != nil {
			t.Fatalf("race %s errors: renew=%v expire=%v", id, renewErr, expireErr)
		}
		if renewed && expired {
			t.Fatalf("race %s had two winners", id)
		}
		if !renewed && !expired {
			t.Fatalf("race %s had no winner", id)
		}
		if renewed {
			if got := mustRequest(t, ctx, s, id); got.ClientLeaseState != store.ClientLeaseLive {
				t.Fatalf("renewal winner state = %q", got.ClientLeaseState)
			}
		} else if got := mustRequest(t, ctx, s, id); got.ClientLeaseState != store.ClientLeaseExpired {
			t.Fatalf("expiry winner state = %q", got.ClientLeaseState)
		}
	}
}

// TestSessionObservationAC29LateRenewCannotRevive covers late-renew and
// read-only-recovery: the recorded outcome stays readable, execution does not
// reopen and the original deadline is not reset.
func TestSessionObservationAC29LateRenewCannotRevive(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	row := newRequest("r1", "h1", "d1", "c1", []byte("digest"), whalereinv1.OnlineStatus_host_accepted, store.ForwardPending, now)
	row.ResultBody = []byte("accepted-result")
	row.ExpiresAt = now.Add(time.Hour)
	deadline := now.Add(10 * time.Second)
	row.ClientLeaseUntil = &deadline
	row.ClientLeaseState = store.ClientLeaseLive
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	elapsedLease(t, ctx, s, "r1")
	if _, err := s.ExpireClientLeases(ctx, 10); err != nil {
		t.Fatal(err)
	}

	renewed, err := s.RenewClientLease(ctx, "d1", "r1", 10*time.Second)
	if err != nil || renewed {
		t.Fatalf("late renewal revived R: %v %v", renewed, err)
	}
	recovered := mustRequest(t, ctx, s, "r1")
	if recovered.ClientLeaseState != store.ClientLeaseExpired {
		t.Fatalf("state = %q", recovered.ClientLeaseState)
	}
	if recovered.Status != whalereinv1.OnlineStatus_host_accepted || string(recovered.ResultBody) != "accepted-result" {
		t.Fatalf("recorded outcome lost: %+v", recovered)
	}
	if recovered.ExpiresAt.UnixMicro() != row.ExpiresAt.UnixMicro() {
		t.Fatalf("late renewal reset the deadline: %v -> %v", row.ExpiresAt, recovered.ExpiresAt)
	}
	if !recovered.CancelPending {
		t.Fatal("late renewal cleared pending cancellation metadata")
	}
}

// TestSessionObservationAC29LeaseNeverExceedsDeadline bounds a long renewal by
// the original request deadline.
func TestSessionObservationAC29LeaseNeverExceedsDeadline(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	row := newRequest("r1", "h1", "d1", "c1", []byte("digest"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	row.ExpiresAt = now.Add(2 * time.Second)
	deadline := now.Add(time.Second)
	row.ClientLeaseUntil = &deadline
	row.ClientLeaseState = store.ClientLeaseLive
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	renewed, err := s.RenewClientLease(ctx, "d1", "r1", time.Hour)
	if err != nil || !renewed {
		t.Fatalf("renew = %v %v", renewed, err)
	}
	stored := mustRequest(t, ctx, s, "r1")
	if stored.ClientLeaseUntil == nil || !stored.ClientLeaseUntil.Equal(stored.ExpiresAt) {
		t.Fatalf("lease %v not bounded by deadline %v", stored.ClientLeaseUntil, stored.ExpiresAt)
	}
}

// TestSessionObservationAC26ExactRenewalScope proves unrelated requests and a
// wrong device never renew R, while its own renewal succeeds.
func TestSessionObservationAC26ExactRenewalScope(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	seedDevice(t, s, "h1", "d2")
	now := time.Now().UTC()
	leaseRequest(t, ctx, s, "r1", "h1", "d1", "c1", 10*time.Second, now)
	leaseRequest(t, ctx, s, "r2", "h1", "d1", "c2", 10*time.Second, now)
	before1 := mustRequest(t, ctx, s, "r1")
	before2 := mustRequest(t, ctx, s, "r2")

	if renewed, err := s.RenewClientLease(ctx, "d2", "r1", 10*time.Second); err != nil || renewed {
		t.Fatalf("wrong-device renewal = %v %v", renewed, err)
	}
	if after := mustRequest(t, ctx, s, "r1"); after.ClientLeaseUntil == nil || !after.ClientLeaseUntil.Equal(*before1.ClientLeaseUntil) {
		t.Fatal("wrong-device renewal changed R")
	}
	if renewed, err := s.RenewClientLease(ctx, "d1", "r1", 10*time.Second); err != nil || !renewed {
		t.Fatalf("own renewal = %v %v", renewed, err)
	}
	if after := mustRequest(t, ctx, s, "r1"); !after.ClientLeaseUntil.After(*before1.ClientLeaseUntil) {
		t.Fatal("own renewal did not extend R")
	}
	if after := mustRequest(t, ctx, s, "r2"); after.ClientLeaseUntil == nil || !after.ClientLeaseUntil.Equal(*before2.ClientLeaseUntil) {
		t.Fatal("renewal touched an unrelated request")
	}
}

// TestSessionObservationAC27TerminalStatusStopsRenewal proves terminal rows
// never renew or expire.
func TestSessionObservationAC27TerminalStatusStopsRenewal(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	for _, status := range []whalereinv1.OnlineStatus{
		whalereinv1.OnlineStatus_succeeded,
		whalereinv1.OnlineStatus_failed,
		whalereinv1.OnlineStatus_rejected,
		whalereinv1.OnlineStatus_host_offline,
		whalereinv1.OnlineStatus_unknown,
	} {
		id := "r-" + store.StatusName(status)
		row := newRequest(id, "h1", "d1", "c-"+id, []byte("digest"), status, store.ForwardNone, now)
		deadline := now.Add(time.Second)
		row.ClientLeaseUntil = &deadline
		row.ClientLeaseState = store.ClientLeaseLive
		if _, err := s.CreateRequest(ctx, row); err != nil {
			t.Fatal(err)
		}
		elapsedLease(t, ctx, s, id)
		if renewed, err := s.RenewClientLease(ctx, "d1", id, 10*time.Second); err != nil || renewed {
			t.Fatalf("terminal %s renewed: %v %v", status, renewed, err)
		}
	}
	expired, err := s.ExpireClientLeases(ctx, 100)
	if err != nil || len(expired) != 0 {
		t.Fatalf("terminal rows expired: %+v %v", expired, err)
	}
}

// TestSessionObservationAC30RestartPreservesExpiryFacts proves the expired
// state and pending cancellation survive an Access restart against the same
// database.
func TestSessionObservationAC30RestartPreservesExpiryFacts(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, db := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	leaseRequest(t, ctx, s, "r1", "h1", "d1", "c1", 10*time.Second, now)
	elapsedLease(t, ctx, s, "r1")
	if _, err := s.ExpireClientLeases(ctx, 10); err != nil {
		t.Fatal(err)
	}

	restarted := db.Open(t)
	after := mustRequest(t, ctx, restarted, "r1")
	if after.ClientLeaseState != store.ClientLeaseExpired || !after.CancelPending {
		t.Fatalf("restart lost expiry facts: %+v", after)
	}
	if renewed, err := restarted.RenewClientLease(ctx, "d1", "r1", 10*time.Second); err != nil || renewed {
		t.Fatalf("restart revived R: %v %v", renewed, err)
	}
	pending, err := restarted.PendingCancelRequests(ctx, 10)
	if err != nil || len(pending) != 1 || pending[0].RequestID != "r1" {
		t.Fatalf("restart lost pending cancellation: %+v %v", pending, err)
	}
	if again, err := restarted.ExpireClientLeases(ctx, 10); err != nil || len(again) != 0 {
		t.Fatalf("expiry repeated after restart: %+v %v", again, err)
	}
}

// TestSessionObservationAC30DuplicateNoticeCannotCancelFreshRequest proves a
// replayed exact notice cannot affect a fresh request R2.
func TestSessionObservationAC30DuplicateNoticeCannotCancelFreshRequest(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	leaseRequest(t, ctx, s, "r1", "h1", "d1", "c1", 10*time.Second, now)
	elapsedLease(t, ctx, s, "r1")
	if _, err := s.ExpireClientLeases(ctx, 10); err != nil {
		t.Fatal(err)
	}
	leaseRequest(t, ctx, s, "r2", "h1", "d1", "c2", 10*time.Second, now)
	before := mustRequest(t, ctx, s, "r2")

	for i := 0; i < 2; i++ {
		if err := s.MarkCancelPublished(ctx, "r1"); err != nil {
			t.Fatalf("mark cancellation %d: %v", i, err)
		}
	}
	if renewed, err := s.RenewClientLease(ctx, "d1", "r2", 10*time.Second); err != nil || !renewed {
		t.Fatalf("R2 renewal after duplicate notice = %v %v", renewed, err)
	}
	after := mustRequest(t, ctx, s, "r2")
	if after.ClientLeaseState != store.ClientLeaseLive || !after.ClientLeaseUntil.After(*before.ClientLeaseUntil) {
		t.Fatalf("R2 changed by duplicate notice: %+v", after)
	}
	expired, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(expired) != 0 {
		t.Fatalf("fresh R2 expired: %+v %v", expired, err)
	}
	if r1 := mustRequest(t, ctx, s, "r1"); r1.Status != whalereinv1.OnlineStatus_received || r1.CancelPending {
		t.Fatalf("R1 facts changed: %+v", r1)
	}
}

// TestSessionObservationAC15HistoricalRowsStayConservative proves an upgrade
// leaves unrecorded liveness unknown: it is never expired, and the schema
// rejects an invalid state.
func TestSessionObservationAC15HistoricalRowsStayConservative(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	row := newRequest("legacy", "h1", "d1", "c-legacy", []byte("digest"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET client_lease_until = NULL WHERE request_id = 'legacy'`); err != nil {
		t.Fatal(err)
	}
	expired, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(expired) != 0 {
		t.Fatalf("historical unknown lease expired: %+v %v", expired, err)
	}
	legacy := mustRequest(t, ctx, s, "legacy")
	if legacy.ClientLeaseUntil != nil || legacy.ClientLeaseState != store.ClientLeaseLive {
		t.Fatalf("historical row rewritten: %+v", legacy)
	}
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET client_lease_state = 'bogus' WHERE request_id = 'legacy'`); err == nil {
		t.Fatal("invalid lease state was accepted by the schema")
	}
}

// TestSessionObservationAC30PendingCancellationSurvivesQueryRetention proves
// bounded retention never deletes an identity that still needs exact replay.
func TestSessionObservationAC30PendingCancellationSurvivesQueryRetention(t *testing.T) {
	ctx, cancel := soon()
	defer cancel()
	s, _ := storetest.New(t)
	seedDevice(t, s, "h1", "d1")
	now := time.Now().UTC()
	row := newRequest("query-lease", "h1", "d1", "", []byte("digest"), whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	row.Kind = whalereinv1.RequestKind_REQUEST_KIND_QUERY
	row.ExpiresAt = now.Add(-time.Second)
	deadline := now.Add(-2 * time.Second)
	row.ClientLeaseUntil = &deadline
	row.ClientLeaseState = store.ClientLeaseLive
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	if _, err := s.ExpireClientLeases(ctx, 10); err != nil {
		t.Fatal(err)
	}
	if _, err := s.DeleteExpiredQueries(ctx, now.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	if got := mustRequest(t, ctx, s, "query-lease"); !got.CancelPending {
		t.Fatal("retention deleted a pending exact cancellation")
	}
	if err := s.MarkCancelPublished(ctx, "query-lease"); err != nil {
		t.Fatal(err)
	}
	if _, err := s.DeleteExpiredQueries(ctx, now.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	if _, err := s.GetRequest(ctx, "query-lease"); err != store.ErrNotFound {
		t.Fatalf("published read-only identity was retained: %v", err)
	}
}
