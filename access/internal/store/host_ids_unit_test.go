package store

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
)

func TestAuthorizedHostIDsPreserveOrderAndPropagateErrors(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	rows := &fakeRowSet{rows: []pgx.Row{hostRow("h1", "", 1, at), hostRow("h2", "", 2, at)}}
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }}
	ids, err := newStore(db).ListAuthorizedHostIDs(context.Background(), "device")
	if err != nil || len(ids) != 2 || ids[0] != "h1" || ids[1] != "h2" || rows.closed != 1 {
		t.Fatalf("ids=%v err=%v closed=%d", ids, err, rows.closed)
	}

	empty := &fakeRowSet{}
	db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return empty, nil }}
	ids, err = newStore(db).ListAuthorizedHostIDs(context.Background(), "device")
	if err != nil || len(ids) != 0 {
		t.Fatalf("empty ids=%v err=%v", ids, err)
	}

	db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return nil, errStoreUnit
	}}
	if _, err := newStore(db).ListAuthorizedHostIDs(context.Background(), "device"); !errors.Is(err, errStoreUnit) {
		t.Fatalf("query error = %v", err)
	}
}
