package store

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
)

// Host signature replay uses the same storage seam: inputs are validated before
// the first database call, and a repeated signature hash is reported as used.
func TestConsumeHostSignatureValidatesAndReportsReuse(t *testing.T) {
	ctx := context.Background()
	hash := make([]byte, 32)

	if _, err := (&Store{}).ConsumeHostSignature(ctx, hash, "host-1", "pairing", "", 0, time.Time{}); err == nil {
		t.Fatal("a store without a pool must fail closed")
	}
	if _, err := (&Store{}).ConsumeHostSignature(ctx, make([]byte, 31), "host-1", "pairing", "", 0, time.Time{}); err == nil {
		t.Fatal("a short signature hash must be refused")
	}
	if _, err := (&Store{}).ConsumeHostSignature(ctx, hash, "", "", "", 0, time.Time{}); err == nil {
		t.Fatal("a missing Host or purpose must be refused")
	}

	inserted := 0
	db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		inserted++
		if inserted == 1 {
			return deleteTag(0), nil
		}
		return pgconn.NewCommandTag("INSERT 0 0"), nil
	}}
	consumed, err := newStore(db).ConsumeHostSignature(ctx, hash, "host-1", "pairing", "", 0, time.Time{})
	if err != nil || consumed {
		t.Fatalf("a duplicate signature must report reuse: %v, %v", consumed, err)
	}

	failing := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errors.New("insert failed")
	}}
	if _, err := newStore(failing).ConsumeHostSignature(ctx, hash, "host-1", "pairing", "", 0, time.Time{}); err == nil {
		t.Fatal("a database failure must be reported")
	}
}
