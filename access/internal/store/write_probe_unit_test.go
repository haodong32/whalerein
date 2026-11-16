package store

import (
	"context"
	"errors"
	"testing"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

type probeTx struct {
	pgx.Tx
	execErr  error
	rollback int
	execs    int
}

func (tx *probeTx) Exec(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
	tx.execs++
	return pgconn.NewCommandTag("UPDATE 0"), tx.execErr
}
func (tx *probeTx) Rollback(context.Context) error {
	tx.rollback++
	return nil
}

func TestWriteProbeRequiresAWriteTransactionAndRollsBack(t *testing.T) {
	tx := &probeTx{}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	if err := newStore(db).WriteProbe(context.Background()); err != nil {
		t.Fatal(err)
	}
	if tx.execs != 1 || tx.rollback != 1 {
		t.Fatalf("execs=%d rollbacks=%d", tx.execs, tx.rollback)
	}

	readOnly := &probeTx{execErr: errors.New("cannot execute UPDATE in a read-only transaction")}
	db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return readOnly, nil }}
	if err := newStore(db).WriteProbe(context.Background()); err == nil {
		t.Fatal("read-only database was reported writable")
	}
	if readOnly.rollback != 1 {
		t.Fatal("probe transaction was not rolled back")
	}

	db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errors.New("pool exhausted") }}
	if err := newStore(db).WriteProbe(context.Background()); err == nil {
		t.Fatal("connection failure was reported writable")
	}
	var zero *Store
	if err := zero.WriteProbe(context.Background()); err == nil {
		t.Fatal("nil store probed successfully")
	}
}
