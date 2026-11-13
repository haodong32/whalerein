//go:build integration

package store_test

import (
	"context"
	"errors"
	"net/url"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func proofTestStore(t *testing.T, db *storetest.DB, name string) *store.Store {
	t.Helper()
	u, err := url.Parse(db.DSN)
	if err != nil {
		t.Fatal(err)
	}
	query := u.Query()
	query.Set("application_name", name)
	u.RawQuery = query.Encode()
	s, err := store.Open(context.Background(), u.String())
	if err != nil {
		t.Fatal("open isolated proof test connection")
	}
	t.Cleanup(s.Close)
	return s
}

func waitForProofLock(t *testing.T, s *store.Store, name string) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		var waiting bool
		err := s.Pool().QueryRow(context.Background(), `SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock')`, name).Scan(&waiting)
		if err != nil {
			t.Fatal("observe isolated lock state")
		}
		if waiting {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("expected operation did not reach lock barrier")
}

// Verified outcomes and Host acknowledgements serialize with grant revocation.
func TestPairingProofAndRevocationUseSameDeviceFence(t *testing.T) {
	for _, tc := range []struct {
		name                         string
		revokeFirst, acknowledgement bool
	}{
		{"proof before revoke", false, false},
		{"revoke before proof", true, false},
		{"revoke before Host acknowledgement", true, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, db := storetest.New(t)
			ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
			defer cancel()
			now := time.Now().UTC()
			if _, err := s.EnsureHost(ctx, "host"); err != nil {
				t.Fatal(err)
			}
			hash := make([]byte, 32)
			hash[0] = 4
			if err := s.CreatePairingToken(ctx, hash, "host", now.Add(time.Minute), now); err != nil {
				t.Fatal(err)
			}
			op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
			if accepted, err := s.CommitPairing(ctx, hash, "host", "device", fixtureKeyPin(3), make([]byte, 32), now, notice, &op); err != nil || !accepted {
				t.Fatal("fixture commit failed")
			}
			if !tc.acknowledgement {
				ack, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
				if _, err := s.Pool().Exec(ctx, `UPDATE requests SET result_body=$2,status='succeeded',updated_at=$3 WHERE request_id=$1`, notice.RequestId, ack, now); err != nil {
					t.Fatal(err)
				}
				if _, err := s.Pool().Exec(ctx, `UPDATE pairing_outcomes SET host_confirmed=true,commit_binding_verified=false WHERE claim_id=$1`, "claim"); err != nil {
					t.Fatal(err)
				}
			}
			observer := proofTestStore(t, db, "proof-observer")
			revoker := proofTestStore(t, db, "proof-revoker")
			blocker, err := s.Pool().Begin(ctx)
			if err != nil {
				t.Fatal(err)
			}
			defer func() { _ = blocker.Rollback(context.Background()) }()
			if tc.revokeFirst {
				if _, err = blocker.Exec(ctx, `SELECT 1 FROM grants WHERE host_id='host' AND device_id='device' FOR UPDATE`); err != nil {
					t.Fatal(err)
				}
			} else {
				if _, err = blocker.Exec(ctx, `SELECT 1 FROM pairing_outcomes WHERE claim_id='claim' FOR UPDATE`); err != nil {
					t.Fatal(err)
				}
			}
			type result struct {
				outcome store.PairingOutcome
				err     error
			}
			observed := make(chan result, 1)
			revoked := make(chan error, 1)
			acknowledged := make(chan error, 1)
			observe := func() { out, err := observer.PairingOutcome(ctx, "claim", "device"); observed <- result{out, err} }
			revoke := func() { revoked <- revoker.RevokeGrant(ctx, "host", "device", now) }
			acknowledge := func() {
				body, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
				_, err := observer.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: "host", DeviceID: "device", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, At: now})
				acknowledged <- err
			}
			if tc.revokeFirst {
				go revoke()
				waitForProofLock(t, s, "proof-revoker")
				if tc.acknowledgement {
					go acknowledge()
				} else {
					go observe()
				}
				waitForProofLock(t, s, "proof-observer")
			} else {
				go observe()
				waitForProofLock(t, s, "proof-observer")
				go revoke()
				waitForProofLock(t, s, "proof-revoker")
			}
			if err := blocker.Commit(ctx); err != nil {
				t.Fatal(err)
			}
			if err := <-revoked; err != nil {
				t.Fatal("revocation failed")
			}
			if tc.acknowledgement {
				if err := <-acknowledged; err != nil {
					t.Fatal("acknowledgement processing failed")
				}
			} else {
				got := <-observed
				if got.err != nil {
					t.Fatal("proof observation failed")
				}
				if got.outcome.Active == tc.revokeFirst {
					t.Fatal("proof observed wrong side of revocation fence")
				}
			}
			if active, err := s.HasActiveGrant(ctx, "host", "device"); err != nil || active {
				t.Fatal("revoked grant became active")
			}
			final, err := s.PairingOutcome(ctx, "claim", "device")
			if err != nil || final.Active {
				t.Fatal("final outcome revived revoked grant")
			}
			if tc.acknowledgement && final.HostConfirmed {
				t.Fatal("late ACK confirmed a revoked pairing")
			}
		})
	}
}

// An ACK received before expiry can confirm a proven COMMIT after a lock wait,
// but retention-cleared request and result bodies must never be repopulated.
func TestTimelyAckAfterExpiryCleanupDoesNotRestoreBody(t *testing.T) {
	s, db := storetest.New(t)
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	var now time.Time
	if err := s.Pool().QueryRow(ctx, `SELECT clock_timestamp()`).Scan(&now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.EnsureHost(ctx, "host"); err != nil {
		t.Fatal(err)
	}
	hash := make([]byte, 32)
	hash[0] = 4
	if err := s.CreatePairingToken(ctx, hash, "host", now.Add(time.Minute), now); err != nil {
		t.Fatal(err)
	}
	op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
	if accepted, err := s.CommitPairing(ctx, hash, "host", "device", fixtureKeyPin(3), make([]byte, 32), now, notice, &op); err != nil || !accepted {
		t.Fatal("fixture commit failed")
	}
	deadline := now.Add(2 * time.Second)
	if _, err := s.Pool().Exec(ctx, `UPDATE requests SET expires_at=$2 WHERE request_id=$1`, notice.RequestId, deadline); err != nil {
		t.Fatal(err)
	}
	blocker, err := s.Pool().Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = blocker.Rollback(context.Background()) }()
	if _, err = blocker.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, "device"); err != nil {
		t.Fatal(err)
	}
	writer := proofTestStore(t, db, "timely-ack-writer")
	done := make(chan error, 1)
	ack, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: "device"})
	go func() {
		applied, err := writer.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: "host", DeviceID: "device", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack, At: now})
		if err == nil && !applied {
			err = errors.New("timely acknowledgement not applied")
		}
		done <- err
	}()
	waitForProofLock(t, s, "timely-ack-writer")
	var cleanupAt time.Time
	for {
		if err := s.Pool().QueryRow(ctx, `SELECT clock_timestamp()`).Scan(&cleanupAt); err != nil {
			t.Fatal(err)
		}
		if !cleanupAt.Before(deadline) {
			break
		}
		if ctx.Err() != nil {
			t.Fatal("deadline did not arrive")
		}
		time.Sleep(20 * time.Millisecond)
	}
	if _, err := s.ClearExpiredBodies(ctx, cleanupAt); err != nil {
		t.Fatal("expiry cleanup failed")
	}
	if err := blocker.Commit(ctx); err != nil {
		t.Fatal(err)
	}
	if err := <-done; err != nil {
		t.Fatal("timely acknowledgement could not be recorded")
	}
	request, err := s.GetRequest(ctx, notice.RequestId)
	if err != nil || len(request.RequestBody) != 0 || len(request.ResultBody) != 0 {
		t.Fatal("expired request or result body was restored")
	}
	outcome, err := s.PairingOutcome(ctx, "claim", "device")
	if err != nil || !outcome.Active || !outcome.HostConfirmed || outcome.CommitBindingUnproven {
		t.Fatal("timely ACK did not preserve verified confirmation")
	}
}

// Result retention uses the time after acquiring a contended request row.
func TestHostResultAfterRequestRowLockExpiresDoesNotStoreBody(t *testing.T) {
	s, db := storetest.New(t)
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	var now time.Time
	if err := s.Pool().QueryRow(ctx, `SELECT clock_timestamp()`).Scan(&now); err != nil {
		t.Fatal(err)
	}
	seedDevice(t, s, "host", "device")
	row := newRequest("request", "host", "device", "command", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	deadline := now.Add(2 * time.Second)
	row.ExpiresAt = deadline
	if _, err := s.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	blocker, err := s.Pool().Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = blocker.Rollback(context.Background()) }()
	if _, err = blocker.Exec(ctx, `SELECT 1 FROM requests WHERE request_id=$1 FOR UPDATE`, row.RequestID); err != nil {
		t.Fatal(err)
	}
	writer := proofTestStore(t, db, "row-lock-result")
	done := make(chan error, 1)
	go func() {
		applied, err := writer.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: row.RequestID, HostID: "host", DeviceID: "device", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte{1}, At: now})
		if err == nil && !applied {
			err = errors.New("result not applied")
		}
		done <- err
	}()
	waitForProofLock(t, s, "row-lock-result")
	for {
		var expired bool
		if err := s.Pool().QueryRow(ctx, `SELECT clock_timestamp()>=$1`, deadline).Scan(&expired); err != nil {
			t.Fatal(err)
		}
		if expired {
			break
		}
		if ctx.Err() != nil {
			t.Fatal("deadline did not arrive")
		}
		time.Sleep(20 * time.Millisecond)
	}
	if err := blocker.Commit(ctx); err != nil {
		t.Fatal(err)
	}
	if err := <-done; err != nil {
		t.Fatal("terminal state was not stored")
	}
	stored, err := s.GetRequest(ctx, row.RequestID)
	if err != nil || stored.Status != whalereinv1.OnlineStatus_succeeded || len(stored.ResultBody) != 0 || stored.UpdatedAt.Before(deadline) {
		t.Fatal("expired row stored a body or lost its terminal status")
	}
}

// Once cleanup clears bodies, a later clock rollback cannot restore them.
func TestCleanedPairingBodiesRemainEmptyAfterClockRollback(t *testing.T) {
	s, op, notice, now := committedPairingFixture(t)
	ctx := context.Background()
	// A future cutoff followed by the current clocks models a rollback without
	// changing the machine or database clock.
	if _, err := s.ClearExpiredBodies(ctx, now.Add(2*time.Hour)); err != nil {
		t.Fatal(err)
	}
	ack, _ := proto.Marshal(&whalereinv1.PairingClaimResult{Authorized: true, DeviceId: op.DeviceID})
	applied, err := s.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: notice.RequestId, HostID: op.HostID, DeviceID: op.DeviceID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: ack, At: now})
	if err != nil || !applied {
		t.Fatal("timely acknowledgement could not advance its status")
	}
	request, err := s.GetRequest(ctx, notice.RequestId)
	if err != nil || len(request.RequestBody) != 0 || len(request.ResultBody) != 0 {
		t.Fatal("clock rollback restored a cleaned body")
	}
	outcome, err := s.PairingOutcome(ctx, op.ClaimID, op.DeviceID)
	if err != nil || !outcome.Active || !outcome.HostConfirmed {
		t.Fatal("verified timely acknowledgement lost its proof")
	}
}
