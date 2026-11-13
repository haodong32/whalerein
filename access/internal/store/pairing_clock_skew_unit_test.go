package store

import (
	"bytes"
	"context"
	"errors"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
)

// A lagging database clock and stale pre-lock application time must not
// extend an already expired pairing deadline according to the current host.
func TestPairingDeadlinesFailClosedWhenDatabaseClockLags(t *testing.T) {
	for _, tc := range []struct {
		name, stage, deadline string
		want                  error
	}{
		{"HELLO invitation", "hello", "invitation", ErrPairingInvitationExpired},
		{"HELLO intent", "hello", "intent", ErrPairingIntentExpired},
		{"FINISH invitation", "finish", "invitation", ErrPairingInvitationExpired},
		{"FINISH intent", "finish", "intent", ErrPairingIntentExpired},
		{"FINISH outbox", "finish", "outbox", ErrPairingIntentExpired},
	} {
		t.Run(tc.name, func(t *testing.T) {
			current := time.Now().UTC()
			stale := current.Add(-2 * time.Second)
			expired := current.Add(-time.Second)
			databaseNow := current.Add(-time.Hour)
			if tc.stage == "hello" {
				tx := &saveFenceTx{t: t, device: 2, host: 3, databaseNow: databaseNow, tokenExpiry: current.Add(time.Minute)}
				op := PairingOperation{ClaimID: "claim", HostID: "host", DeviceID: "device", TokenHash: bytes.Repeat([]byte{4}, 32), HelloHash: bytes.Repeat([]byte{6}, 32), HelloResponse: []byte("opaque"), DeviceGeneration: 2, HostGeneration: 3, ExpiresAt: current.Add(time.Minute)}
				if tc.deadline == "invitation" {
					tx.tokenExpiry = expired
				} else {
					op.ExpiresAt = expired
				}
				err := savePairingOperation(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, op)
				if !errors.Is(err, tc.want) {
					t.Fatal("expired HELLO deadline was not rejected")
				}
				for _, step := range tx.steps {
					if step == "commit" {
						t.Fatal("expired HELLO committed")
					}
				}
				return
			}
			f := newBoundCommitFixture(t)
			f.now = stale
			f.op.ExpiresAt = current.Add(time.Minute)
			f.response.ExpiresAtUnixMs = current.Add(time.Minute).UnixMilli()
			f.notice.ExpiresAtUnixMs = current.Add(time.Minute).UnixMilli()
			f.claim.ExpiresAtUnixMs = f.notice.ExpiresAtUnixMs
			tx := &commitPairingTx{t: t, device: 2, host: 3, databaseNow: databaseNow, tokenExpiry: current.Add(time.Minute), operationExpiry: current.Add(time.Minute)}
			switch tc.deadline {
			case "invitation":
				tx.tokenExpiry = expired
			case "intent":
				tx.operationExpiry = expired
			case "outbox":
				f.notice.ExpiresAtUnixMs = expired.UnixMilli()
				f.claim.ExpiresAtUnixMs = f.notice.ExpiresAtUnixMs
			}
			f.encode(t)
			accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { return tx, nil })
			if accepted || !errors.Is(err, tc.want) {
				t.Fatal("expired FINISH deadline was not rejected")
			}
			for _, step := range tx.steps {
				if step == "commit" {
					t.Fatal("expired FINISH committed")
				}
			}
		})
	}
}

func TestPairingDeadlinesFailClosedWhenDatabaseClockLeads(t *testing.T) {
	current := time.Now().UTC()
	databaseNow := current.Add(2 * time.Minute)
	expiry := current.Add(time.Minute)
	for _, stage := range []string{"hello", "finish"} {
		t.Run(stage, func(t *testing.T) {
			if stage == "hello" {
				tx := &saveFenceTx{t: t, device: 2, host: 3, databaseNow: databaseNow, tokenExpiry: expiry}
				op := PairingOperation{ClaimID: "claim", HostID: "host", DeviceID: "device", TokenHash: bytes.Repeat([]byte{4}, 32), HelloHash: bytes.Repeat([]byte{6}, 32), HelloResponse: []byte("opaque"), DeviceGeneration: 2, HostGeneration: 3, ExpiresAt: expiry}
				if err := savePairingOperation(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil }, op); !errors.Is(err, ErrPairingInvitationExpired) {
					t.Fatal("database-expired HELLO was not rejected")
				}
				for _, step := range tx.steps {
					if step == "commit" {
						t.Fatal("expired HELLO committed")
					}
				}
				return
			}
			f := newBoundCommitFixture(t)
			f.now = current
			f.op.ExpiresAt = expiry
			f.response.ExpiresAtUnixMs = expiry.UnixMilli()
			f.notice.ExpiresAtUnixMs = expiry.UnixMilli()
			f.claim.ExpiresAtUnixMs = f.notice.ExpiresAtUnixMs
			f.encode(t)
			tx := &commitPairingTx{t: t, device: 2, host: 3, databaseNow: databaseNow, tokenExpiry: expiry, operationExpiry: expiry}
			if accepted, err := f.commit(func(context.Context) (pgx.Tx, error) { return tx, nil }); accepted || !errors.Is(err, ErrPairingInvitationExpired) {
				t.Fatal("database-expired FINISH was not rejected")
			}
			for _, step := range tx.steps {
				if step == "commit" {
					t.Fatal("expired FINISH committed")
				}
			}
		})
	}
}
