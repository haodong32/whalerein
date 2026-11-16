package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

// TestDeleteExpiredPairingTokensReportsAFullOperationsBatch proves the cleanup
// drain sees a saturated pairing_operations batch even when the token batch is
// short, so a long-pause backlog is not silently left behind.
func TestDeleteExpiredPairingTokensReportsAFullOperationsBatch(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	cases := []struct {
		name       string
		operations int64
		tokens     int64
		want       int64
	}{
		{"full operations batch with no tokens", CleanupBatchSize, 0, CleanupBatchSize},
		{"short operations batch with no tokens", 7, 0, 7},
		{"both batches partial", 2, 4, 6},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, queryFn: emptyOutcomeRows, execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
				if strings.Contains(sql, "DELETE FROM pairing_operations") {
					return deleteTag(tc.operations), nil
				}
				return deleteTag(tc.tokens), nil
			}}
			deleted, err := newStore(db).DeleteExpiredPairingTokens(context.Background(), at)
			if err != nil || deleted != tc.want {
				t.Fatalf("deleted=%d want=%d err=%v", deleted, tc.want, err)
			}
		})
	}
}

func TestDeleteExpiredPairingTokensReportsAnOperationsFailure(t *testing.T) {
	db := &fakeDB{t: t, queryFn: emptyOutcomeRows, execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if strings.Contains(sql, "DELETE FROM pairing_operations") {
			return pgconn.CommandTag{}, errors.New("synthetic operations failure")
		}
		t.Fatal("token delete ran after an operations failure")
		return pgconn.CommandTag{}, nil
	}}
	if _, err := newStore(db).DeleteExpiredPairingTokens(context.Background(), time.Now()); err == nil {
		t.Fatal("operations failure was swallowed")
	}
}

func emptyOutcomeRows(context.Context, string, ...any) (pgx.Rows, error) {
	return &fakeRowSet{}, nil
}
