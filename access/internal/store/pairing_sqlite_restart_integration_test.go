//go:build integration

package store_test

import (
	"bytes"
	"context"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// AC-08 on the runtime backend: a committed pairing survives a process restart
// on SQLite and a replay cannot create a second credential or grant.
func TestSQLitePairingCommitSurvivesRestartAndReplay(t *testing.T) {
	s, db := storetest.NewSQLite(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "host"); err != nil {
		t.Fatal(err)
	}
	hash := bytes.Repeat([]byte{9}, 32)
	if err := s.CreatePairingToken(ctx, hash, "host", now.Add(time.Minute), now); err != nil {
		t.Fatal(err)
	}
	op, notice := fixturePairingOperation(t, s, hash, "host", "device", "claim", now, nil)
	accepted, err := s.CommitPairing(ctx, hash, "host", "device", fixtureKeyPin(3), bytes.Repeat([]byte{4}, 32), now, notice, &op)
	if err != nil || !accepted {
		t.Fatalf("first commit = %v %v", accepted, err)
	}
	s.Close()
	reopened := db.Open(t)
	defer reopened.Close()
	token, err := reopened.PairingTokenByHash(ctx, hash)
	if err != nil || token.UsedAt == nil {
		t.Fatalf("invitation after restart = %#v %v", token, err)
	}
	if _, err := reopened.PairingOutcome(ctx, "claim", "device"); err != nil {
		t.Fatalf("outcome after restart: %v", err)
	}
	if active, err := reopened.HasActiveGrant(ctx, "host", "device"); err != nil || !active {
		t.Fatalf("grant after restart = %v %v", active, err)
	}
	// A replay must not mint a second credential or a new outcome.
	if accepted, err := reopened.CommitPairing(ctx, hash, "host", "device", fixtureKeyPin(3), bytes.Repeat([]byte{4}, 32), now, notice, &op); accepted {
		t.Fatalf("replay was accepted: %v", err)
	}
}
