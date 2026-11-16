package store

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// applyResultTx scripts the two SELECTs and the UPDATE of applyHostResult so a
// unit test can assert the exact SQL the production path executes.
type applyResultTx struct {
	pgx.Tx
	t         *testing.T
	now       time.Time
	affected  int64
	updateSQL string
	committed bool
}

func (tx *applyResultTx) QueryRow(_ context.Context, sql string, _ ...any) pgx.Row {
	switch {
	case strings.Contains(sql, "FROM requests"):
		return syntheticRow{fill: func(dest ...any) {
			*dest[0].(*time.Time) = tx.now.Add(time.Hour)
			*dest[1].(*bool) = false
			*dest[2].(*bool) = false
			*dest[3].(*int64) = 8
		}}
	case strings.Contains(sql, "clock_timestamp"):
		return syntheticRow{fill: func(dest ...any) {
			*dest[0].(*time.Time) = tx.now
		}}
	default:
		tx.t.Fatalf("unexpected query: %s", sql)
		return nil
	}
}

func (tx *applyResultTx) Exec(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
	if !strings.Contains(sql, "UPDATE requests") {
		tx.t.Fatalf("unexpected exec: %s", sql)
	}
	tx.updateSQL = sql
	return pgconn.NewCommandTag(fmt.Sprintf("UPDATE %d", tx.affected)), nil
}

func (tx *applyResultTx) Rollback(context.Context) error { return nil }
func (tx *applyResultTx) Commit(context.Context) error   { tx.committed = true; return nil }

// TestApplyHostResultEmbedsTheMatrixPredicate proves the real store path uses
// the generated transition predicate rather than a second hand-written matrix.
func TestApplyHostResultEmbedsTheMatrixPredicate(t *testing.T) {
	now := time.Now().UTC()
	tx := &applyResultTx{t: t, now: now, affected: 1}
	applied, err := applyHostResult(context.Background(),
		func(context.Context) (pgx.Tx, error) { return tx, nil },
		HostResultUpdate{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_rejected, At: now})
	if err != nil || !applied || !tx.committed {
		t.Fatalf("applied=%v committed=%v err=%v", applied, tx.committed, err)
	}
	if !strings.Contains(tx.updateSQL, hostResultStatusPredicate) {
		t.Fatalf("UPDATE does not embed the generated predicate:\n%s", tx.updateSQL)
	}
	if strings.Contains(tx.updateSQL, "status = 'host_accepted' AND $4 IN ('host_offline'") ||
		strings.Contains(tx.updateSQL, "status = 'host_accepted' AND $4 IN ('rejected'") {
		t.Fatalf("UPDATE contains a forbidden backward transition:\n%s", tx.updateSQL)
	}

	// A conditional no-op (for example a race that already moved the row) must
	// report applied=false without calling it an error.
	tx = &applyResultTx{t: t, now: now, affected: 0}
	applied, err = applyHostResult(context.Background(),
		func(context.Context) (pgx.Tx, error) { return tx, nil },
		HostResultUpdate{RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_host_offline, At: now})
	if err != nil || applied {
		t.Fatalf("no-op applied=%v err=%v", applied, err)
	}
}
