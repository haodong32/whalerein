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

// externalRow is a pgx.Row that yields fixed values, or an error.
type externalRow struct {
	values []any
	err    error
}

func (r externalRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	if len(dest) != len(r.values) {
		return errors.New("column count mismatch")
	}
	for index, target := range dest {
		switch typed := target.(type) {
		case *string:
			*typed, _ = r.values[index].(string)
		case *[]byte:
			value, _ := r.values[index].([]byte)
			*typed = value
		case *time.Time:
			value, _ := r.values[index].(time.Time)
			*typed = value
		case **time.Time:
			value, _ := r.values[index].(*time.Time)
			*typed = value
		case *int:
			value, _ := r.values[index].(int)
			*typed = value
		}
	}
	return nil
}

// externalTx records whether it committed and serves its own rows.
type externalTx struct {
	pgx.Tx
	row        pgx.Row
	insertedID string
	execErr    error
	committed  bool
	rolledBack bool
}

func (tx *externalTx) QueryRow(context.Context, string, ...any) pgx.Row {
	if tx.insertedID == "" {
		return externalRow{err: pgx.ErrNoRows}
	}
	// The insert returns the operation id only.
	return externalRow{values: []any{tx.insertedID}}
}

func (tx *externalTx) Exec(context.Context, string, ...any) (pgconn.CommandTag, error) {
	return pgconn.NewCommandTag("INSERT 0 1"), tx.execErr
}
func (tx *externalTx) Commit(context.Context) error   { tx.committed = true; return nil }
func (tx *externalTx) Rollback(context.Context) error { tx.rolledBack = true; return nil }

func TestEnrollHostCommitsOperationEnrollmentAndCredential(t *testing.T) {
	tx := &externalTx{insertedID: "op-1"}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	enrollment := ExternalEnrollment{HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1"}

	recorded, credential, committed, err := newStore(db).EnrollHost(context.Background(), ExternalOperation{
		OperationID: "op-1", AuthorizationID: "auth-1", PayloadDigest: []byte("digest"),
	}, enrollment)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !committed || credential == "" {
		t.Fatal("the committing call must report a commit and one credential")
	}
	if !tx.committed {
		t.Fatal("the transaction must commit")
	}
	if recorded.OperationID != "op-1" || recorded.State != "enrollment_pending" {
		t.Fatalf("unexpected recorded enrollment %+v", recorded)
	}
}

func TestEnrollHostReportsACommitFailure(t *testing.T) {
	tx := &externalTx{insertedID: "op-1", execErr: errors.New("insert failed")}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	_, _, _, err := newStore(db).EnrollHost(context.Background(), ExternalOperation{OperationID: "op-1"}, ExternalEnrollment{HostID: "host-1"})
	if err == nil || !strings.Contains(err.Error(), "record enrollment") {
		t.Fatalf("expected a record failure, got %v", err)
	}
}

func TestEnrollHostReportsABeginFailure(t *testing.T) {
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errors.New("no connection") }}
	if _, _, _, err := newStore(db).EnrollHost(context.Background(), ExternalOperation{}, ExternalEnrollment{}); err == nil {
		t.Fatal("a begin failure must be surfaced")
	}
}

func TestEnrollmentReturnsNotFoundAndErrors(t *testing.T) {
	for name, row := range map[string]pgx.Row{
		"not found": externalRow{err: pgx.ErrNoRows},
		"error":     externalRow{err: errors.New("boom")},
	} {
		t.Run(name, func(t *testing.T) {
			db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return row }}
			_, err := newStore(db).Enrollment(context.Background(), "host-1")
			if err == nil {
				t.Fatal("expected an error")
			}
			if name == "not found" && !errors.Is(err, ErrNotFound) {
				t.Fatalf("expected ErrNotFound, got %v", err)
			}
		})
	}
}

func TestEnrollmentReturnsAStoredRow(t *testing.T) {
	disabled := time.Now().UTC()
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return externalRow{values: []any{"host-1", "ws-1", "3", []byte("sign"), []byte("noise"), "op-1", "enrollment_disabled", &disabled}}
	}}
	enrollment, err := newStore(db).Enrollment(context.Background(), "host-1")
	if err != nil {
		t.Fatal(err)
	}
	if enrollment.HostID != "host-1" || enrollment.EnrollmentRevision != "3" || enrollment.DisabledAt == nil {
		t.Fatalf("unexpected enrollment %+v", enrollment)
	}
}

func TestOperationLookupDistinguishesMissingRows(t *testing.T) {
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return externalRow{err: pgx.ErrNoRows}
	}}
	if _, err := newStore(db).Operation(context.Background(), "op-1"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expected ErrNotFound, got %v", err)
	}
}

func TestNextEnrollmentRevisionCountsExistingRows(t *testing.T) {
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return externalRow{values: []any{4}}
	}}
	revision, err := newStore(db).NextEnrollmentRevision(context.Background(), "ws-1")
	if err != nil {
		t.Fatal(err)
	}
	if revision != "5" {
		t.Fatalf("expected the next revision, got %q", revision)
	}
}

func TestNewOperationIDIsUniqueAndPrefixed(t *testing.T) {
	first := NewOperationID()
	second := NewOperationID()
	if first == second || !strings.HasPrefix(first, "op-") {
		t.Fatalf("unexpected operation ids %q and %q", first, second)
	}
}
