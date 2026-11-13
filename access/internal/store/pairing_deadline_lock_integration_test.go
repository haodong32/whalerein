//go:build integration

package store_test

import (
	"bytes"
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// Waiting for the device fence cannot commit after any original deadline.
func TestPairingCommitRechecksDeadlinesAfterDeviceLockWait(t *testing.T) {
	for _, deadlineKind := range []string{"invitation", "operation", "outbox"} {
		t.Run(deadlineKind, func(t *testing.T) {
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
			hash := bytes.Repeat([]byte{4}, 32)
			deadline := now.Add(2 * time.Second)
			tokenExpiry := now.Add(time.Minute)
			if deadlineKind == "invitation" {
				tokenExpiry = deadline
			}
			if err := s.CreatePairingToken(ctx, hash, "host", tokenExpiry, now); err != nil {
				t.Fatal(err)
			}
			op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
			if deadlineKind == "operation" {
				if _, err := s.Pool().Exec(ctx, `UPDATE pairing_operations SET expires_at=$2 WHERE claim_id=$1`, op.ClaimID, deadline); err != nil {
					t.Fatal(err)
				}
			}
			if deadlineKind == "outbox" {
				notice.ExpiresAtUnixMs = deadline.UnixMilli()
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(notice.Body, &claim); err != nil {
					t.Fatal(err)
				}
				claim.ExpiresAtUnixMs = notice.ExpiresAtUnixMs
				notice.Body, _ = proto.Marshal(&claim)
			}
			blocker, err := s.Pool().Begin(ctx)
			if err != nil {
				t.Fatal(err)
			}
			defer func() { _ = blocker.Rollback(context.Background()) }()
			if _, err = blocker.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, "device"); err != nil {
				t.Fatal(err)
			}
			committer := proofTestStore(t, db, "deadline-committer")
			type result struct {
				accepted bool
				err      error
			}
			finished := make(chan result, 1)
			go func() {
				accepted, err := committer.CommitPairing(ctx, hash, "host", "device", fixtureKeyPin(3), bytes.Repeat([]byte{5}, 32), now, notice, &op)
				finished <- result{accepted, err}
			}()
			waitForProofLock(t, s, "deadline-committer")
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
			got := <-finished
			want := store.ErrPairingIntentExpired
			if deadlineKind == "invitation" {
				want = store.ErrPairingInvitationExpired
			}
			if got.accepted || !errors.Is(got.err, want) {
				t.Fatal("deadline elapsed while waiting but pairing committed")
			}
			token, err := s.PairingTokenByHash(ctx, hash)
			if err != nil || token.UsedAt != nil {
				t.Fatal("expired attempt consumed invitation")
			}
			if active, err := s.HasActiveGrant(ctx, "host", "device"); err != nil || active {
				t.Fatal("expired attempt created grant")
			}
			if _, err := s.DeviceByID(ctx, "device"); !errors.Is(err, store.ErrNotFound) {
				t.Fatal("expired attempt created device")
			}
			if _, err := s.GetRequest(ctx, notice.RequestId); !errors.Is(err, store.ErrNotFound) {
				t.Fatal("expired attempt created outbox")
			}
			if _, err := s.PairingOutcome(ctx, "claim", "device"); !errors.Is(err, store.ErrNotFound) {
				t.Fatal("expired attempt created outcome")
			}
		})
	}
}

// A HELLO intent cannot be saved after its invitation or intent deadline while waiting for a fence.
func TestPairingHelloSaveRechecksDeadlinesAfterDeviceLockWait(t *testing.T) {
	for _, deadlineKind := range []string{"invitation", "intent"} {
		t.Run(deadlineKind, func(t *testing.T) {
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
			deadline := now.Add(2 * time.Second)
			tokenExpiry, intentExpiry := now.Add(time.Minute), now.Add(time.Minute)
			if deadlineKind == "invitation" {
				tokenExpiry = deadline
			} else {
				intentExpiry = deadline
			}
			hash := bytes.Repeat([]byte{4}, 32)
			if err := s.CreatePairingToken(ctx, hash, "host", tokenExpiry, now); err != nil {
				t.Fatal(err)
			}
			op := store.PairingOperation{ClaimID: "claim", HostID: "host", DeviceID: "device", TokenHash: hash, HelloHash: bytes.Repeat([]byte{6}, 32), HelloResponse: []byte("opaque"), ExpiresAt: intentExpiry}
			blocker, err := s.Pool().Begin(ctx)
			if err != nil {
				t.Fatal(err)
			}
			defer func() { _ = blocker.Rollback(context.Background()) }()
			if _, err = blocker.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended('whalerein-device:' || $1, 0))`, "device"); err != nil {
				t.Fatal(err)
			}
			writer := proofTestStore(t, db, "hello-deadline-writer")
			done := make(chan error, 1)
			go func() { done <- writer.SavePairingOperation(ctx, op) }()
			waitForProofLock(t, s, "hello-deadline-writer")
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
			want := store.ErrPairingIntentExpired
			if deadlineKind == "invitation" {
				want = store.ErrPairingInvitationExpired
			}
			if err := <-done; !errors.Is(err, want) {
				t.Fatal("expired HELLO intent was saved")
			}
			if _, err := s.PairingOperation(ctx, "claim"); !errors.Is(err, store.ErrNotFound) {
				t.Fatal("expired HELLO left an operation")
			}
			token, err := s.PairingTokenByHash(ctx, hash)
			if err != nil || token.UsedAt != nil {
				t.Fatal("expired HELLO consumed the invitation")
			}
		})
	}
}
