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

func replacementOperation() ExternalOperation {
	return ExternalOperation{
		OperationID: "op-r", AuthorizationID: "auth-r", HostID: "host-1",
		WorkspaceID: "ws-1", EnrollmentRevision: "1", PayloadDigest: []byte("digest"),
	}
}

func replacementEnrollment() ExternalEnrollment {
	return ExternalEnrollment{
		HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		OperationID: "op-1", State: "enrollment_ready",
		CurrentCredentialOperationID: "op-r", CredentialGeneration: 2,
	}
}

// scriptedReplaceTx walks a successful credential replacement so a test can
// replace one statement with a storage failure.
func scriptedReplaceTx(t *testing.T, admitted ExternalOperation) *externalTx {
	t.Helper()
	tx := &externalTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "FOR UPDATE"):
			return operationRow(admitted)
		case strings.Contains(sql, "UPDATE external_enrollments"), strings.Contains(sql, "FROM external_enrollments"):
			return enrollmentRow(replacementEnrollment())
		default:
			t.Fatalf("unexpected query row %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "pg_advisory_xact_lock") && !strings.Contains(sql, "UPDATE external_operations") {
			t.Fatalf("unexpected exec %q", sql)
		}
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	return tx
}

// replacementDB admits the operation without opening an admission transaction.
func replacementDB(t *testing.T, recorded ExternalOperation, tx *externalTx) *fakeDB {
	t.Helper()
	return &fakeDB{t: t,
		queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(recorded) },
		beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
	}
}

func TestReplaceHostCredentialValidatesTheOperationAndAdmission(t *testing.T) {
	operation := replacementOperation()

	t.Run("empty operation id is refused before storage", func(t *testing.T) {
		db := &fakeDB{t: t}
		_, _, err := newStore(db).ReplaceHostCredential(context.Background(), ExternalOperation{})
		if err == nil || !strings.Contains(err.Error(), "operation id is required") {
			t.Fatalf("empty operation error = %v", err)
		}
	})

	t.Run("admission read failure", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{err: errStoreUnit}
		}}
		_, _, err := newStore(db).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) {
			t.Fatalf("admission read error = %v", err)
		}
	})

	t.Run("fallback admission begin failure", func(t *testing.T) {
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return externalRow{err: pgx.ErrNoRows} },
			beginFn:    func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit },
		}
		_, _, err := newStore(db).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "begin admission") {
			t.Fatalf("fallback admission error = %v", err)
		}
	})
}

func TestReplaceHostCredentialReportsTransactionFailures(t *testing.T) {
	operation := replacementOperation()

	t.Run("begin failure", func(t *testing.T) {
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit },
		}
		_, _, err := newStore(db).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "begin credential replacement") {
			t.Fatalf("begin error = %v", err)
		}
	})

	t.Run("host lock failure", func(t *testing.T) {
		tx := &externalTx{execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.CommandTag{}, errStoreUnit
		}}
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "lock host scope") {
			t.Fatalf("lock error = %v", err)
		}
	})

	t.Run("admitted operation read failure", func(t *testing.T) {
		tx := &externalTx{}
		tx.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		tx.queryRowFn = func(context.Context, string, ...any) pgx.Row { return externalRow{err: errStoreUnit} }
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "load operation for mutation") {
			t.Fatalf("operation read error = %v", err)
		}
	})
}

func TestReplaceHostCredentialReplaysACommittedOutcome(t *testing.T) {
	operation := replacementOperation()
	committed := operation
	committed.Outcome = OperationCommitted
	committed.CredentialGeneration = 2
	committed.CredentialOperationID = "op-r"

	t.Run("replay enrollment load failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, committed)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if !strings.Contains(sql, "FOR UPDATE") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		_, _, err := newStore(replacementDB(t, committed, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) {
			t.Fatalf("replay load error = %v", err)
		}
	})

	t.Run("replay commit failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, committed)
		tx.commitErr = errStoreUnit
		_, _, err := newStore(replacementDB(t, committed, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit credential replay") {
			t.Fatalf("replay commit error = %v", err)
		}
	})

	t.Run("replay returns the stored enrollment without a credential", func(t *testing.T) {
		tx := scriptedReplaceTx(t, committed)
		enrollment, credential, err := newStore(replacementDB(t, committed, tx)).ReplaceHostCredential(context.Background(), operation)
		if err != nil || credential != "" || enrollment.HostID != "host-1" || !tx.committed {
			t.Fatalf("replay = %+v credential=%q err=%v committed=%v", enrollment, credential, err, tx.committed)
		}
	})
}

func TestReplaceHostCredentialReportsRecordedRefusals(t *testing.T) {
	operation := replacementOperation()
	for _, tc := range []struct {
		name    string
		outcome string
		reason  string
		wantErr error
	}{
		{"expired authorization", OperationExpired, "", ErrAuthorizationExpired},
		{"fenced operation", OperationFenced, "", ErrOperationFenced},
		{"refused revision", OperationRefused, "revision_mismatch", ErrOperationRefused},
		{"refused disabled enrollment", OperationRefused, "enrollment_disabled", ErrEnrollmentDisabled},
	} {
		t.Run(tc.name, func(t *testing.T) {
			recorded := operation
			recorded.Outcome = tc.outcome
			recorded.Reason = tc.reason
			tx := scriptedReplaceTx(t, recorded)
			if _, _, err := newStore(replacementDB(t, recorded, tx)).ReplaceHostCredential(context.Background(), operation); !errors.Is(err, tc.wantErr) {
				t.Fatalf("refusal error = %v", err)
			}
			if tx.committed {
				t.Fatal("a recorded refusal must not commit")
			}
		})
	}
}

func TestReplaceHostCredentialReportsExpiredAuthorizationOutcomes(t *testing.T) {
	operation := replacementOperation()
	expires := time.Unix(1_700_000_000, 0).UTC()
	expiring := operation
	expiring.AuthorizationExpiresAt = &expires

	withExpiryCheck := func(t *testing.T, expired pgx.Row, tx *externalTx) {
		t.Helper()
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "clock_timestamp()") {
				return expired
			}
			return base(ctx, sql, args...)
		}
	}

	t.Run("deadline check failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, expiring)
		withExpiryCheck(t, externalRow{err: errStoreUnit}, tx)
		_, _, err := newStore(replacementDB(t, expiring, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "check operation admission deadline") {
			t.Fatalf("deadline error = %v", err)
		}
	})

	t.Run("expired outcome write failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, expiring)
		withExpiryCheck(t, externalRow{values: []any{true}}, tx)
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		_, _, err := newStore(replacementDB(t, expiring, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record operation outcome") {
			t.Fatalf("expired outcome error = %v", err)
		}
	})

	t.Run("expired outcome commit failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, expiring)
		withExpiryCheck(t, externalRow{values: []any{true}}, tx)
		tx.commitErr = errStoreUnit
		_, _, err := newStore(replacementDB(t, expiring, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit expired operation") {
			t.Fatalf("expired commit error = %v", err)
		}
	})

	t.Run("expired authorization", func(t *testing.T) {
		tx := scriptedReplaceTx(t, expiring)
		withExpiryCheck(t, externalRow{values: []any{true}}, tx)
		if _, _, err := newStore(replacementDB(t, expiring, tx)).ReplaceHostCredential(context.Background(), operation); !errors.Is(err, ErrAuthorizationExpired) {
			t.Fatalf("expired error = %v", err)
		}
		if !tx.committed {
			t.Fatal("the expired outcome must commit")
		}
	})
}

func TestReplaceHostCredentialFailsClosedWhenTheBindingChanged(t *testing.T) {
	operation := replacementOperation()

	t.Run("enrollment update failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, operation)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE external_enrollments") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "replace host credential") {
			t.Fatalf("enrollment update error = %v", err)
		}
	})

	t.Run("revision mismatch enrollment read failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, operation)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE external_enrollments") {
				return externalRow{err: pgx.ErrNoRows}
			}
			if strings.Contains(sql, "FROM external_enrollments") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) {
			t.Fatalf("mismatch load error = %v", err)
		}
	})

	t.Run("revision mismatch outcome write failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, operation)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE external_enrollments") {
				return externalRow{err: pgx.ErrNoRows}
			}
			return base(ctx, sql, args...)
		}
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record operation outcome") {
			t.Fatalf("mismatch outcome error = %v", err)
		}
	})

	t.Run("revision mismatch commit failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, operation)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE external_enrollments") {
				return externalRow{err: pgx.ErrNoRows}
			}
			return base(ctx, sql, args...)
		}
		tx.commitErr = errStoreUnit
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit refusal") {
			t.Fatalf("mismatch commit error = %v", err)
		}
	})
}

func TestReplaceHostCredentialReportsTheFinalWriteFailures(t *testing.T) {
	operation := replacementOperation()

	t.Run("outcome write failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, operation)
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record credential replacement outcome") {
			t.Fatalf("outcome write error = %v", err)
		}
	})

	t.Run("commit failure", func(t *testing.T) {
		tx := scriptedReplaceTx(t, operation)
		tx.commitErr = errStoreUnit
		_, _, err := newStore(replacementDB(t, operation, tx)).ReplaceHostCredential(context.Background(), operation)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit credential replacement") {
			t.Fatalf("commit error = %v", err)
		}
		if tx.committed {
			t.Fatal("a failed commit must not report success")
		}
	})
}
