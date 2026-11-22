//go:build integration

package store_test

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// controlledClockEpoch is the fixed wall-clock origin for the lease race
// tests. Every deadline below is derived from this value, so the tests do not
// depend on when the suite runs.
var controlledClockEpoch = time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

// leaseClock controls the value returned by clock_timestamp() for one
// isolated test schema. The store SQL keeps using clock_timestamp() verbatim;
// only the schema search_path resolves that name to this test-owned function.
// Production stores never see it, so the database-clock predicate under test
// is unchanged.
type leaseClock struct {
	pool   *pgxpool.Pool
	schema string
	mu     sync.Mutex
}

func (c *leaseClock) set(t *testing.T, at time.Time) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	at = at.UTC().Truncate(time.Microsecond)
	c.mu.Lock()
	defer c.mu.Unlock()
	if _, err := c.pool.Exec(ctx, `UPDATE `+testQuoteIdent(c.schema)+`.test_clock SET at=$1`, at); err != nil {
		t.Fatalf("set controlled clock to %v: %v", at, err)
	}
}

func testQuoteIdent(value string) string {
	return `"` + strings.ReplaceAll(value, `"`, `""`) + `"`
}

// newControlledClockStore creates an isolated schema plus a store whose
// clock_timestamp() reads a row this test can move. The real built-in clock is
// restored for every other test by keeping the shadow function inside the
// per-test schema.
func newControlledClockStore(t *testing.T) (*store.Store, *leaseClock) {
	t.Helper()
	db := storetest.NewSchema(t)
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	cfg, err := pgxpool.ParseConfig(db.DSN)
	if err != nil {
		t.Fatalf("parse controlled-clock DSN: %v", err)
	}
	// Put the test schema before pg_catalog so the unqualified
	// clock_timestamp() in the store SQL resolves to the test function. The
	// default storetest search_path omits pg_catalog, which keeps the built-in
	// first; naming it explicitly after the schema is what makes the shadow
	// deterministic.
	cfg.ConnConfig.RuntimeParams["search_path"] = db.Schema + ", pg_catalog"
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		t.Fatalf("open controlled-clock pool: %v", err)
	}
	t.Cleanup(pool.Close)

	schema := testQuoteIdent(db.Schema)
	if _, err := pool.Exec(ctx, `CREATE TABLE `+schema+`.test_clock (
		singleton boolean PRIMARY KEY DEFAULT true,
		at timestamptz NOT NULL)`); err != nil {
		t.Fatalf("create controlled clock table: %v", err)
	}
	if _, err := pool.Exec(ctx, `INSERT INTO `+schema+`.test_clock(singleton,at) VALUES (true,$1)`, controlledClockEpoch); err != nil {
		t.Fatalf("seed controlled clock: %v", err)
	}
	if _, err := pool.Exec(ctx, `CREATE FUNCTION `+schema+`.clock_timestamp()
		RETURNS timestamptz LANGUAGE sql STABLE
		AS $clock$ SELECT at FROM `+schema+`.test_clock WHERE singleton $clock$`); err != nil {
		t.Fatalf("create controlled clock function: %v", err)
	}

	s := store.NewWithPool(pool)
	if err := s.Migrate(ctx); err != nil {
		t.Fatalf("migrate controlled-clock schema: %v", err)
	}
	return s, &leaseClock{pool: pool, schema: db.Schema}
}

// controlledLeaseRequest inserts one exact live request. All timestamps are
// explicit so the row never depends on the real wall clock.
func controlledLeaseRequest(t *testing.T, ctx context.Context, s *store.Store, requestID, deviceID string, createdAt, expiresAt, leaseUntil time.Time) store.Request {
	t.Helper()
	row := newRequest(requestID, "h1", deviceID, "command-"+requestID, []byte("digest-"+requestID),
		whalereinv1.OnlineStatus_received, store.ForwardPending, createdAt)
	row.ExpiresAt = expiresAt
	deadline := leaseUntil.UTC().Truncate(time.Microsecond)
	row.ClientLeaseUntil = &deadline
	row.ClientLeaseState = store.ClientLeaseLive
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatalf("create controlled lease request %s: %v", requestID, err)
	}
	return row
}

func controlledContext(t *testing.T) (context.Context, context.CancelFunc) {
	t.Helper()
	return context.WithTimeout(context.Background(), 30*time.Second)
}

func controlledDevice(t *testing.T, s *store.Store, deviceID string) {
	t.Helper()
	ctx, cancel := controlledContext(t)
	defer cancel()
	if _, err := s.EnsureHost(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "h1", deviceID, "fp-"+deviceID, []byte("auth-"+deviceID), controlledClockEpoch); err != nil {
		t.Fatal(err)
	}
}

// TestSessionObservationAC29ControlledClockRenewWins covers
// lease/AC-29.renew-wins: a renewal committed before the elapsed boundary
// keeps the claim and preserves the original hard deadline, and the later
// sweep does not close it.
func TestSessionObservationAC29ControlledClockRenewWins(t *testing.T) {
	ctx, cancel := controlledContext(t)
	defer cancel()
	s, clock := newControlledClockStore(t)
	controlledDevice(t, s, "d1")

	expiresAt := controlledClockEpoch.Add(time.Hour)
	leaseUntil := controlledClockEpoch.Add(10 * time.Second)
	controlledLeaseRequest(t, ctx, s, "r1", "d1", controlledClockEpoch, expiresAt, leaseUntil)

	// Exactly one microsecond before the lease boundary is still live.
	justBefore := leaseUntil.Add(-time.Microsecond)
	clock.set(t, justBefore)
	renewed, err := s.RenewClientLease(ctx, "d1", "r1", 30*time.Second)
	if err != nil || !renewed {
		t.Fatalf("renewal before expiry = %v %v", renewed, err)
	}
	stored := mustRequest(t, ctx, s, "r1")
	if !stored.ExpiresAt.Equal(expiresAt) {
		t.Fatalf("renewal changed the hard deadline: %v want %v", stored.ExpiresAt, expiresAt)
	}
	wantLease := justBefore.Add(30 * time.Second)
	if stored.ClientLeaseUntil == nil || !stored.ClientLeaseUntil.Equal(wantLease) {
		t.Fatalf("renewed lease = %v want %v", stored.ClientLeaseUntil, wantLease)
	}

	// Moving to the old boundary must not close the renewed claim.
	clock.set(t, leaseUntil)
	expired, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(expired) != 0 {
		t.Fatalf("expiry after winning renewal = %+v err=%v", expired, err)
	}
	stored = mustRequest(t, ctx, s, "r1")
	if stored.ClientLeaseState != store.ClientLeaseLive {
		t.Fatalf("winning renewal was closed: %q", stored.ClientLeaseState)
	}
	if stored.CancelPending {
		t.Fatal("winning renewal left cancellation pending")
	}
}

// TestSessionObservationAC29ControlledClockExpiryWins covers
// lease/AC-29.expiry-wins: an expiry committed first closes admission and a
// later renewal cannot revive the exact claim.
func TestSessionObservationAC29ControlledClockExpiryWins(t *testing.T) {
	ctx, cancel := controlledContext(t)
	defer cancel()
	s, clock := newControlledClockStore(t)
	controlledDevice(t, s, "d1")

	expiresAt := controlledClockEpoch.Add(time.Hour)
	leaseUntil := controlledClockEpoch.Add(10 * time.Second)
	controlledLeaseRequest(t, ctx, s, "r1", "d1", controlledClockEpoch, expiresAt, leaseUntil)

	// At the exact boundary the renewal predicate is false (strict >), so the
	// expiry owns this row.
	clock.set(t, leaseUntil)
	expired, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(expired) != 1 || expired[0].RequestID != "r1" {
		t.Fatalf("boundary expiry = %+v err=%v", expired, err)
	}
	if expired[0].ClientLeaseState != store.ClientLeaseExpired || !expired[0].CancelPending {
		t.Fatalf("expiry facts = %+v", expired[0])
	}

	renewed, err := s.RenewClientLease(ctx, "d1", "r1", 30*time.Second)
	if err != nil || renewed {
		t.Fatalf("renewal revived an expired claim: %v %v", renewed, err)
	}
	heartbeat, err := s.HeartbeatRequest(ctx, "d1", "", "r1", 30*time.Second)
	if err != nil || heartbeat.Renewed || heartbeat.Failure != store.HeartbeatLeaseElapsed {
		t.Fatalf("heartbeat after expiry = %+v err=%v", heartbeat, err)
	}
	stored := mustRequest(t, ctx, s, "r1")
	if stored.ClientLeaseState != store.ClientLeaseExpired {
		t.Fatalf("expiry did not stay monotonic: %q", stored.ClientLeaseState)
	}
	if !stored.ExpiresAt.Equal(expiresAt) {
		t.Fatalf("late renewal reset the deadline: %v want %v", stored.ExpiresAt, expiresAt)
	}
	// Expiry is not duplicated and the exact row is the only one affected.
	again, err := s.ExpireClientLeases(ctx, 10)
	if err != nil || len(again) != 0 {
		t.Fatalf("expiry repeated = %+v err=%v", again, err)
	}
	if count, err := s.CountRequests(ctx); err != nil || count != 1 {
		t.Fatalf("request count = %d err=%v", count, err)
	}
}

// TestSessionObservationAC29ControlledClockLate covers lease/AC-29.late: a
// renewal after the hard deadline is refused even when the lease row still
// looks live and unexpired, and scoped attempts cannot renew it.
func TestSessionObservationAC29ControlledClockLate(t *testing.T) {
	ctx, cancel := controlledContext(t)
	defer cancel()
	s, clock := newControlledClockStore(t)
	controlledDevice(t, s, "d1")
	controlledDevice(t, s, "d2")

	hardDeadline := controlledClockEpoch.Add(time.Minute)
	// The row still looks active: live state and a lease far beyond the hard
	// deadline.
	leaseUntil := controlledClockEpoch.Add(time.Hour)
	controlledLeaseRequest(t, ctx, s, "r1", "d1", controlledClockEpoch, hardDeadline, leaseUntil)
	controlledLeaseRequest(t, ctx, s, "sibling", "d1", controlledClockEpoch, controlledClockEpoch.Add(2*time.Hour), controlledClockEpoch.Add(time.Hour))

	clock.set(t, hardDeadline.Add(time.Microsecond))
	renewed, err := s.RenewClientLease(ctx, "d1", "r1", 30*time.Second)
	if err != nil || renewed {
		t.Fatalf("renewal after hard deadline = %v %v", renewed, err)
	}
	heartbeat, err := s.HeartbeatRequest(ctx, "d1", "", "r1", 30*time.Second)
	if err != nil || heartbeat.Renewed || heartbeat.Failure != store.HeartbeatHardDeadline {
		t.Fatalf("heartbeat after hard deadline = %+v err=%v", heartbeat, err)
	}
	// Exact scope: a foreign device and a missing request cannot touch R.
	if renewed, err := s.RenewClientLease(ctx, "d2", "r1", 30*time.Second); err != nil || renewed {
		t.Fatalf("foreign renewal = %v %v", renewed, err)
	}
	if renewed, err := s.RenewClientLease(ctx, "d1", "missing", 30*time.Second); err != nil || renewed {
		t.Fatalf("missing-request renewal = %v %v", renewed, err)
	}
	stored := mustRequest(t, ctx, s, "r1")
	if stored.ClientLeaseState != store.ClientLeaseLive || stored.ClientLeaseUntil == nil || !stored.ClientLeaseUntil.Equal(leaseUntil) {
		t.Fatalf("late renewal changed R: %+v", stored)
	}
	if !stored.ExpiresAt.Equal(hardDeadline) || stored.CancelPending {
		t.Fatalf("late renewal reset R's deadline or cancellation: %+v", stored)
	}
	if sibling := mustRequest(t, ctx, s, "sibling"); sibling.ClientLeaseState != store.ClientLeaseLive || !sibling.ClientLeaseUntil.Equal(controlledClockEpoch.Add(time.Hour)) {
		t.Fatalf("late renewal touched a sibling: %+v", sibling)
	}
}

// TestSessionObservationAC29ControlledClockHardDeadline covers
// lease/AC-29.hard-deadline: repeated renewal is capped at the configured hard
// deadline and only the exact request can move its own lease.
func TestSessionObservationAC29ControlledClockHardDeadline(t *testing.T) {
	ctx, cancel := controlledContext(t)
	defer cancel()
	s, clock := newControlledClockStore(t)
	controlledDevice(t, s, "d1")

	expiresAt := controlledClockEpoch.Add(time.Hour)
	leaseUntil := controlledClockEpoch.Add(time.Second)
	controlledLeaseRequest(t, ctx, s, "r1", "d1", controlledClockEpoch, expiresAt, leaseUntil)
	// The sibling has a later hard deadline, so it stays renewable while R is
	// already past its own deadline.
	controlledLeaseRequest(t, ctx, s, "sibling", "d1", controlledClockEpoch, controlledClockEpoch.Add(2*time.Hour), controlledClockEpoch.Add(90*time.Minute))

	// Repeated renewal at increasing times never crosses expires_at.
	for _, step := range []time.Duration{
		0,
		10 * time.Minute,
		30 * time.Minute,
		time.Hour - time.Minute,
		time.Hour - time.Microsecond,
	} {
		clock.set(t, controlledClockEpoch.Add(step))
		renewed, err := s.RenewClientLease(ctx, "d1", "r1", 24*time.Hour)
		if err != nil || !renewed {
			t.Fatalf("renewal at +%v = %v %v", step, renewed, err)
		}
		stored := mustRequest(t, ctx, s, "r1")
		if !stored.ExpiresAt.Equal(expiresAt) {
			t.Fatalf("renewal at +%v moved the hard deadline: %v want %v", step, stored.ExpiresAt, expiresAt)
		}
		if stored.ClientLeaseUntil == nil || stored.ClientLeaseUntil.After(expiresAt) {
			t.Fatalf("renewal at +%v crossed the hard deadline: %v", step, stored.ClientLeaseUntil)
		}
	}

	// At and after the hard deadline renewal is refused even though the lease
	// is still live.
	for _, at := range []time.Time{expiresAt, expiresAt.Add(time.Microsecond)} {
		clock.set(t, at)
		if renewed, err := s.RenewClientLease(ctx, "d1", "r1", 24*time.Hour); err != nil || renewed {
			t.Fatalf("renewal at hard deadline %v = %v %v", at, renewed, err)
		}
		if heartbeat, err := s.HeartbeatRequest(ctx, "d1", "", "r1", 24*time.Hour); err != nil || heartbeat.Renewed || heartbeat.Failure != store.HeartbeatHardDeadline {
			t.Fatalf("heartbeat at hard deadline %v = %+v err=%v", at, heartbeat, err)
		}
	}

	// A different request and a wrong device never renew R. A valid renewal of
	// the sibling is the only operation allowed to move the sibling lease.
	if renewed, err := s.RenewClientLease(ctx, "d1", "sibling", time.Minute); err != nil || !renewed {
		t.Fatalf("sibling renewal = %v %v", renewed, err)
	}
	if renewed, err := s.RenewClientLease(ctx, "d2", "r1", time.Minute); err != nil || renewed {
		t.Fatalf("wrong-device renewal = %v %v", renewed, err)
	}
	if renewed, err := s.RenewClientLease(ctx, "d1", "other", time.Minute); err != nil || renewed {
		t.Fatalf("wrong-request renewal = %v %v", renewed, err)
	}
	if stored := mustRequest(t, ctx, s, "r1"); !stored.ExpiresAt.Equal(expiresAt) {
		t.Fatalf("scoped renewal moved R's deadline: %v", stored.ExpiresAt)
	}
}

// TestSessionObservationAC29ControlledClockMonotonicBoundary walks one fresh
// row across just-before, at, and after the exact lease boundary. The winner
// is fixed by the clock, not by scheduling.
func TestSessionObservationAC29ControlledClockMonotonicBoundary(t *testing.T) {
	for _, tc := range []struct {
		name       string
		offset     time.Duration
		wantRenew  bool
		wantExpire bool
	}{
		{"just-before", -time.Microsecond, true, false},
		{"at", 0, false, true},
		{"after", time.Microsecond, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := controlledContext(t)
			defer cancel()
			s, clock := newControlledClockStore(t)
			controlledDevice(t, s, "d1")

			leaseUntil := controlledClockEpoch.Add(10 * time.Second)
			controlledLeaseRequest(t, ctx, s, "r1", "d1", controlledClockEpoch, controlledClockEpoch.Add(time.Hour), leaseUntil)
			clock.set(t, leaseUntil.Add(tc.offset))

			renewed, err := s.RenewClientLease(ctx, "d1", "r1", time.Minute)
			if err != nil {
				t.Fatal(err)
			}
			expired, err := s.ExpireClientLeases(ctx, 10)
			if err != nil {
				t.Fatal(err)
			}
			expiredR1 := false
			for _, row := range expired {
				if row.RequestID == "r1" {
					expiredR1 = true
				}
			}
			if renewed != tc.wantRenew || expiredR1 != tc.wantExpire {
				t.Fatalf("renewed=%v expired=%v want renewal=%v expiry=%v", renewed, expiredR1, tc.wantRenew, tc.wantExpire)
			}
			stored := mustRequest(t, ctx, s, "r1")
			if tc.wantRenew {
				if stored.ClientLeaseState != store.ClientLeaseLive {
					t.Fatalf("renewal winner state = %q", stored.ClientLeaseState)
				}
			} else if stored.ClientLeaseState != store.ClientLeaseExpired {
				t.Fatalf("expiry winner state = %q", stored.ClientLeaseState)
			}
			if count, err := s.CountRequests(ctx); err != nil || count != 1 {
				t.Fatalf("request count = %d err=%v", count, err)
			}
		})
	}
}

// TestSessionObservationAC29ControlledClockInterleavedRace runs renewal and
// expiry as concurrent transactions at the exact boundary and asserts one
// winner, one stored row, and no duplicate closure. Concurrent scheduling
// cannot change the winner because the clock, not lock timing, decides it.
func TestSessionObservationAC29ControlledClockInterleavedRace(t *testing.T) {
	for _, tc := range []struct {
		name       string
		offset     time.Duration
		wantRenew  bool
		wantExpire bool
	}{
		{"renew-wins-before-boundary", -time.Microsecond, true, false},
		{"expiry-wins-at-boundary", 0, false, true},
		{"expiry-wins-after-boundary", time.Microsecond, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			for iteration := 0; iteration < 4; iteration++ {
				ctx, cancel := controlledContext(t)
				s, clock := newControlledClockStore(t)
				controlledDevice(t, s, "d1")
				requestID := "race-" + tc.name
				leaseUntil := controlledClockEpoch.Add(10 * time.Second)
				controlledLeaseRequest(t, ctx, s, requestID, "d1", controlledClockEpoch, controlledClockEpoch.Add(time.Hour), leaseUntil)
				clock.set(t, leaseUntil.Add(tc.offset))

				var (
					renewed   bool
					renewErr  error
					expired   bool
					expireErr error
					wg        sync.WaitGroup
				)
				start := make(chan struct{})
				wg.Add(2)
				go func() {
					defer wg.Done()
					<-start
					renewed, renewErr = s.RenewClientLease(ctx, "d1", requestID, time.Minute)
				}()
				go func() {
					defer wg.Done()
					<-start
					rows, err := s.ExpireClientLeases(ctx, 100)
					expireErr = err
					for _, row := range rows {
						if row.RequestID == requestID {
							expired = true
						}
					}
				}()
				close(start)
				wg.Wait()
				cancel()

				if renewErr != nil || expireErr != nil {
					t.Fatalf("iteration %d errors: renew=%v expire=%v", iteration, renewErr, expireErr)
				}
				if renewed != tc.wantRenew || expired != tc.wantExpire {
					t.Fatalf("iteration %d renewed=%v expired=%v want renewal=%v expiry=%v",
						iteration, renewed, expired, tc.wantRenew, tc.wantExpire)
				}
				if renewed == expired {
					t.Fatalf("iteration %d did not have exactly one winner", iteration)
				}

				// Re-read with a short-lived context because ctx may have been
				// cancelled after the race.
				readCtx, readCancel := controlledContext(t)
				stored, err := s.GetRequest(readCtx, requestID)
				readCancel()
				if err != nil {
					t.Fatal(err)
				}
				if renewed {
					if stored.ClientLeaseState != store.ClientLeaseLive || stored.CancelPending {
						t.Fatalf("iteration %d renewal winner state = %+v", iteration, stored)
					}
				} else {
					if stored.ClientLeaseState != store.ClientLeaseExpired || !stored.CancelPending {
						t.Fatalf("iteration %d expiry winner state = %+v", iteration, stored)
					}
				}
				// A second sweep must not duplicate the closure or create a
				// second claim.
				againCtx, againCancel := controlledContext(t)
				again, err := s.ExpireClientLeases(againCtx, 100)
				if err != nil || len(again) != 0 {
					againCancel()
					t.Fatalf("iteration %d repeated expiry = %+v err=%v", iteration, again, err)
				}
				count, err := s.CountRequests(againCtx)
				againCancel()
				if err != nil || count != 1 {
					t.Fatalf("iteration %d request count = %d err=%v", iteration, count, err)
				}
			}
		})
	}
}

// TestSessionObservationAC29ControlledClockLockedInterleave forces an actual
// interleave: a test transaction holds R's row lock while expiry runs, so the
// expiry statement observes the row in flight and skips it. The pending
// heartbeat then commits a valid renewal. There is one winner and no second
// claim.
func TestSessionObservationAC29ControlledClockLockedInterleave(t *testing.T) {
	ctx, cancel := controlledContext(t)
	defer cancel()
	s, clock := newControlledClockStore(t)
	controlledDevice(t, s, "d1")
	leaseUntil := controlledClockEpoch.Add(10 * time.Second)
	controlledLeaseRequest(t, ctx, s, "r1", "d1", controlledClockEpoch, controlledClockEpoch.Add(time.Hour), leaseUntil)
	clock.set(t, leaseUntil.Add(-time.Microsecond))

	tx, err := s.Pool().Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err := tx.Exec(ctx, `SELECT 1 FROM requests WHERE request_id=$1 FOR UPDATE`, "r1"); err != nil {
		t.Fatalf("lock request row: %v", err)
	}

	var (
		outcome store.HeartbeatOutcome
		hbErr   error
	)
	hbDone := make(chan struct{})
	go func() {
		defer close(hbDone)
		outcome, hbErr = s.HeartbeatRequest(ctx, "d1", "", "r1", time.Minute)
	}()

	// Expiry runs while the heartbeat transaction is pending on the same row.
	expired, expireErr := s.ExpireClientLeases(ctx, 100)
	if expireErr != nil || len(expired) != 0 {
		t.Fatalf("expiry during pending renewal = %+v err=%v", expired, expireErr)
	}
	if err := tx.Commit(ctx); err != nil {
		t.Fatalf("release request row: %v", err)
	}
	<-hbDone

	if hbErr != nil || !outcome.Renewed {
		t.Fatalf("pending heartbeat = %+v err=%v", outcome, hbErr)
	}
	stored := mustRequest(t, ctx, s, "r1")
	if stored.ClientLeaseState != store.ClientLeaseLive || stored.CancelPending {
		t.Fatalf("interleaved renewal winner state = %+v", stored)
	}
	if count, err := s.CountRequests(ctx); err != nil || count != 1 {
		t.Fatalf("request count = %d err=%v", count, err)
	}
	if again, err := s.ExpireClientLeases(ctx, 100); err != nil || len(again) != 0 {
		t.Fatalf("interleaved renewal left a duplicate claim: %+v err=%v", again, err)
	}
}

// TestSessionObservationAC29ControlledClockHeartbeatRace races the explicit
// transactional heartbeat against expiry. At the boundary the heartbeat cannot
// renew; before it the renewal and expiry still have exactly one winner.
func TestSessionObservationAC29ControlledClockHeartbeatRace(t *testing.T) {
	for _, tc := range []struct {
		name       string
		offset     time.Duration
		wantRenew  bool
		wantExpire bool
	}{
		{"heartbeat-wins-before-boundary", -time.Microsecond, true, false},
		{"expiry-wins-at-boundary", 0, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := controlledContext(t)
			defer cancel()
			s, clock := newControlledClockStore(t)
			controlledDevice(t, s, "d1")
			leaseUntil := controlledClockEpoch.Add(10 * time.Second)
			controlledLeaseRequest(t, ctx, s, "r1", "d1", controlledClockEpoch, controlledClockEpoch.Add(time.Hour), leaseUntil)
			clock.set(t, leaseUntil.Add(tc.offset))

			var (
				outcome   store.HeartbeatOutcome
				hbErr     error
				expired   bool
				expireErr error
				wg        sync.WaitGroup
			)
			start := make(chan struct{})
			wg.Add(2)
			go func() {
				defer wg.Done()
				<-start
				outcome, hbErr = s.HeartbeatRequest(ctx, "d1", "", "r1", time.Minute)
			}()
			go func() {
				defer wg.Done()
				<-start
				rows, err := s.ExpireClientLeases(ctx, 100)
				expireErr = err
				for _, row := range rows {
					if row.RequestID == "r1" {
						expired = true
					}
				}
			}()
			close(start)
			wg.Wait()

			if hbErr != nil || expireErr != nil {
				t.Fatalf("race errors: heartbeat=%v expiry=%v", hbErr, expireErr)
			}
			if outcome.Renewed != tc.wantRenew || expired != tc.wantExpire {
				t.Fatalf("heartbeat renewed=%v expired=%v want renewal=%v expiry=%v",
					outcome.Renewed, expired, tc.wantRenew, tc.wantExpire)
			}
			if outcome.Renewed == expired {
				t.Fatal("heartbeat race did not have exactly one winner")
			}
			stored := mustRequest(t, ctx, s, "r1")
			if tc.wantRenew {
				if stored.ClientLeaseState != store.ClientLeaseLive {
					t.Fatalf("heartbeat winner state = %q", stored.ClientLeaseState)
				}
				wantLease := leaseUntil.Add(tc.offset).Add(time.Minute)
				if stored.ClientLeaseUntil == nil || !stored.ClientLeaseUntil.Equal(wantLease) {
					t.Fatalf("heartbeat lease = %v want %v", stored.ClientLeaseUntil, wantLease)
				}
			} else {
				if stored.ClientLeaseState != store.ClientLeaseExpired {
					t.Fatalf("expiry winner state = %q", stored.ClientLeaseState)
				}
				if outcome.Failure != store.HeartbeatLeaseElapsed {
					t.Fatalf("boundary heartbeat failure = %q", outcome.Failure)
				}
			}
		})
	}
}
