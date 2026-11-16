package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
)

func TestConsumeHostSignatureReportsWhetherThisReplicaWasFirst(t *testing.T) {
	at := time.UnixMilli(1_700_000_000_000)
	digest := make([]byte, 32)
	seen := map[string]bool{}
	db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "INSERT INTO host_signature_replays") {
			// Retention prune.
			return pgconn.NewCommandTag("DELETE 0"), nil
		}
		key := string(args[0].([]byte))
		if seen[key] {
			return pgconn.NewCommandTag("INSERT 0 0"), nil
		}
		seen[key] = true
		return pgconn.NewCommandTag("INSERT 0 1"), nil
	}}
	store := newStore(db)

	consumed, err := store.ConsumeHostSignature(context.Background(), digest, "h1", "invitation", "", at.UnixMilli(), at)
	if err != nil || !consumed {
		t.Fatalf("first use consumed=%v err=%v", consumed, err)
	}
	consumed, err = store.ConsumeHostSignature(context.Background(), digest, "h1", "invitation", "", at.UnixMilli(), at)
	if err != nil || consumed {
		t.Fatalf("replayed use consumed=%v err=%v", consumed, err)
	}
}

func TestConsumeHostSignatureRejectsInvalidInputsAndFailsClosed(t *testing.T) {
	at := time.UnixMilli(1_700_000_000_000)
	var zero *Store
	if _, err := zero.ConsumeHostSignature(context.Background(), make([]byte, 32), "h1", "invitation", "", at.UnixMilli(), at); err == nil {
		t.Fatal("nil store accepted a signature")
	}
	db := &fakeDB{t: t}
	store := newStore(db)
	if _, err := store.ConsumeHostSignature(context.Background(), []byte("short"), "h1", "invitation", "", at.UnixMilli(), at); err == nil {
		t.Fatal("short hash accepted")
	}
	if _, err := store.ConsumeHostSignature(context.Background(), make([]byte, 32), "", "invitation", "", at.UnixMilli(), at); err == nil {
		t.Fatal("empty host accepted")
	}
	// A prune failure is a guard failure: the caller must not treat the
	// signature as consumed.
	failing := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errors.New("database unavailable")
	}}
	if _, err := newStore(failing).ConsumeHostSignature(context.Background(), make([]byte, 32), "h1", "invitation", "", at.UnixMilli(), at); err == nil {
		t.Fatal("prune failure was swallowed")
	}
}
