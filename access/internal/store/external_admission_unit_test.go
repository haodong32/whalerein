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

func admissionOperation() ExternalOperation {
	return ExternalOperation{
		OperationID: "op-a", AuthorizationID: "auth-a", Purpose: "host_enrollment",
		WorkspaceID: "ws-1", HostID: "host-1", PayloadDigest: []byte("digest"),
		EnrollmentRevision: "1",
	}
}

func TestAdmitOperationValidatesAndReportsTransactionFailures(t *testing.T) {
	operation := admissionOperation()

	t.Run("empty operation id", func(t *testing.T) {
		db := &fakeDB{t: t}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), ExternalOperation{})
		if admitted || err == nil || !strings.Contains(err.Error(), "operation id is required") {
			t.Fatalf("empty operation admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("begin failure", func(t *testing.T) {
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "begin admission") {
			t.Fatalf("begin admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("authorization replay read failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_operations") {
				return externalRow{err: pgx.ErrNoRows}
			}
			return externalRow{err: errStoreUnit}
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "load operation") {
			t.Fatalf("replay read admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("authorization replay commit failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_operations") {
				return externalRow{err: pgx.ErrNoRows}
			}
			return operationRow(operation)
		}, commitErr: errStoreUnit}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit admission replay") {
			t.Fatalf("replay commit admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("unique violation read failure is a conflict", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_operations") {
				return externalRow{err: &pgconn.PgError{Code: "23505"}}
			}
			return externalRow{err: errStoreUnit}
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, ErrOperationConflict) {
			t.Fatalf("unique read admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("unique violation payload mismatch is a conflict", func(t *testing.T) {
		recorded := operation
		recorded.PayloadDigest = []byte("other")
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_operations") {
				return externalRow{err: &pgconn.PgError{Code: "23505"}}
			}
			return operationRow(recorded)
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, ErrOperationConflict) || tx.committed {
			t.Fatalf("unique mismatch admitted=%v err=%v committed=%v", admitted, err, tx.committed)
		}
	})

	t.Run("unique violation replay commit failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_operations") {
				return externalRow{err: &pgconn.PgError{Code: "23505"}}
			}
			return operationRow(operation)
		}, commitErr: errStoreUnit}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit admission replay") {
			t.Fatalf("unique replay admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("record admission failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{err: errStoreUnit}
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record admission") {
			t.Fatalf("record admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("admission readback failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_operations") {
				return externalRow{values: []any{"op-a"}}
			}
			return externalRow{err: errStoreUnit}
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, errStoreUnit) {
			t.Fatalf("readback admitted=%v err=%v", admitted, err)
		}
	})

	t.Run("commit failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_operations") {
				return externalRow{values: []any{"op-a"}}
			}
			return operationRow(operation)
		}, commitErr: errStoreUnit}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, admitted, err := newStore(db).AdmitOperation(context.Background(), operation)
		if admitted || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit admission") {
			t.Fatalf("commit admitted=%v err=%v", admitted, err)
		}
	})
}

func TestMarkEnrollmentReadyReportsTransactionFailures(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	ready := ExternalEnrollment{
		HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		OperationID: "op-1", State: "enrollment_ready",
		CurrentCredentialOperationID: "op-1", CredentialGeneration: 1,
	}

	t.Run("begin failure", func(t *testing.T) {
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit }}
		if _, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", at); advanced || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "begin enrollment ready") {
			t.Fatalf("begin advanced=%v err=%v", advanced, err)
		}
	})

	t.Run("advance write failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{err: errStoreUnit}
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", at); advanced || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "advance enrollment ready") {
			t.Fatalf("advance advanced=%v err=%v", advanced, err)
		}
	})

	t.Run("existing enrollment read failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE external_enrollments") {
				return externalRow{err: pgx.ErrNoRows}
			}
			return externalRow{err: errStoreUnit}
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", at); advanced || !errors.Is(err, errStoreUnit) {
			t.Fatalf("existing read advanced=%v err=%v", advanced, err)
		}
	})

	t.Run("existing enrollment commit failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE external_enrollments") {
				return externalRow{err: pgx.ErrNoRows}
			}
			return enrollmentRow(ready)
		}, commitErr: errStoreUnit}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", at); advanced || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit enrollment ready") {
			t.Fatalf("existing commit advanced=%v err=%v", advanced, err)
		}
	})

	t.Run("outcome write failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return enrollmentRow(ready)
		}, execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if !strings.Contains(sql, "UPDATE external_operations") {
				t.Fatalf("unexpected exec %q", sql)
			}
			return pgconn.CommandTag{}, errStoreUnit
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", at); advanced || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record enrollment readiness") {
			t.Fatalf("outcome advanced=%v err=%v", advanced, err)
		}
	})

	t.Run("commit failure", func(t *testing.T) {
		tx := &externalTx{queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return enrollmentRow(ready)
		}, commitErr: errStoreUnit}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", at); advanced || !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit enrollment ready") {
			t.Fatalf("commit advanced=%v err=%v", advanced, err)
		}
	})
}
