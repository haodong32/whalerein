package store

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
)

// The proof nonce guard validates before it touches the database, then records
// one durable single-use row. These cases run through the storage seam so they
// do not need PostgreSQL.
func TestConsumeHostProofNonceValidatesAndRecordsOnce(t *testing.T) {
	ctx := context.Background()
	hash := make([]byte, 32)

	if _, err := (&Store{}).ConsumeHostProofNonce(ctx, hash, "enrollment_status", time.Time{}); err == nil {
		t.Fatal("a store without a pool must fail closed")
	}
	if _, err := (&Store{}).ConsumeHostProofNonce(ctx, make([]byte, 31), "enrollment_status", time.Time{}); err == nil {
		t.Fatal("a short nonce hash must be refused")
	}
	if _, err := (&Store{}).ConsumeHostProofNonce(ctx, hash, "", time.Time{}); err == nil {
		t.Fatal("an empty purpose must be refused")
	}

	calls := 0
	db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		calls++
		if calls == 1 {
			return deleteTag(0), nil
		}
		return pgconn.NewCommandTag("INSERT 0 1"), nil
	}}
	consumed, err := newStore(db).ConsumeHostProofNonce(ctx, hash, "enrollment_status", time.Time{})
	if err != nil || !consumed || calls != 2 {
		t.Fatalf("first consume = %v, %v, calls %d", consumed, err, calls)
	}

	failing := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errors.New("prune failed")
	}}
	if _, err := newStore(failing).ConsumeHostProofNonce(ctx, hash, "enrollment_status", time.Time{}); err == nil {
		t.Fatal("a prune failure must be reported")
	}
}
