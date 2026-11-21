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

func enrollmentOperation() ExternalOperation {
	at := time.Unix(1_700_000_000, 0).UTC()
	return ExternalOperation{
		OperationID: "op-1", AuthorizationID: "auth-1", Purpose: "host_enrollment",
		WorkspaceID: "ws-1", HostID: "host-1", PayloadDigest: []byte("digest"),
		EnrollmentRevision: "1", AdmittedAt: &at,
	}
}

// scriptedEnrollTx walks a successful enrollment so a test can replace one
// statement with the failure under examination.
func scriptedEnrollTx(t *testing.T, admitted ExternalOperation) *externalTx {
	t.Helper()
	tx := &externalTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "FOR UPDATE"):
			return operationRow(admitted)
		case strings.Contains(sql, "INSERT INTO external_enrollments"):
			return enrollmentRow(ExternalEnrollment{
				HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
				OperationID: "op-1", State: "enrollment_pending",
				CurrentCredentialOperationID: "op-1", CredentialGeneration: 1,
			})
		case strings.Contains(sql, "FROM external_enrollments"):
			return enrollmentRow(ExternalEnrollment{
				HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
				OperationID: "op-1", State: "enrollment_ready",
				CurrentCredentialOperationID: "op-1", CredentialGeneration: 1,
			})
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

func enrollDB(t *testing.T, recorded ExternalOperation, tx *externalTx) *fakeDB {
	t.Helper()
	return &fakeDB{t: t,
		queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(recorded) },
		beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
	}
}

func enrollRequest() ExternalEnrollment {
	return ExternalEnrollment{
		HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		HostSigningPublicKey: []byte("sign"), HostNoisePublicKey: []byte("noise"),
	}
}

func TestEnrollHostValidatesAdmissionAndLocks(t *testing.T) {
	operation := enrollmentOperation()

	t.Run("admission read failure", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return externalRow{err: errStoreUnit}
		}}
		if _, _, _, err := newStore(db).EnrollHost(context.Background(), operation, enrollRequest()); !errors.Is(err, errStoreUnit) {
			t.Fatalf("admission read error = %v", err)
		}
	})

	t.Run("fallback admission begin failure", func(t *testing.T) {
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return externalRow{err: pgx.ErrNoRows} },
			beginFn:    func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit },
		}
		_, _, _, err := newStore(db).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "begin admission") {
			t.Fatalf("fallback admission error = %v", err)
		}
	})

	t.Run("host lock failure", func(t *testing.T) {
		tx := &externalTx{execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.CommandTag{}, errStoreUnit
		}}
		_, _, _, err := newStore(enrollDB(t, operation, tx)).EnrollHost(context.Background(), operation, enrollRequest())
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
		_, _, _, err := newStore(enrollDB(t, operation, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "load operation for mutation") {
			t.Fatalf("operation read error = %v", err)
		}
	})
}

func TestEnrollHostReplaysACommittedEnrollment(t *testing.T) {
	operation := enrollmentOperation()
	committed := operation
	committed.Outcome = OperationCommitted

	t.Run("replay enrollment load failure", func(t *testing.T) {
		tx := scriptedEnrollTx(t, committed)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if !strings.Contains(sql, "FOR UPDATE") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		_, _, _, err := newStore(enrollDB(t, committed, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) {
			t.Fatalf("replay load error = %v", err)
		}
	})

	t.Run("replay commit failure", func(t *testing.T) {
		tx := scriptedEnrollTx(t, committed)
		tx.commitErr = errStoreUnit
		_, _, _, err := newStore(enrollDB(t, committed, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit") {
			t.Fatalf("replay commit error = %v", err)
		}
	})

	t.Run("replay reports the stored enrollment without a credential", func(t *testing.T) {
		tx := scriptedEnrollTx(t, committed)
		enrollment, credential, committedNow, err := newStore(enrollDB(t, committed, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if err != nil || credential != "" || committedNow || enrollment.State != "enrollment_ready" || !tx.committed {
			t.Fatalf("replay = %+v credential=%q committed=%v err=%v", enrollment, credential, committedNow, err)
		}
	})
}

func TestEnrollHostReportsRecordedRefusals(t *testing.T) {
	operation := enrollmentOperation()
	for _, tc := range []struct {
		name    string
		outcome string
		reason  string
		wantErr error
	}{
		{"expired authorization", OperationExpired, "", ErrAuthorizationExpired},
		{"fenced operation", OperationFenced, "", ErrOperationFenced},
		{"refused duplicate host", OperationRefused, "duplicate_host", ErrDuplicateHost},
		{"refused disabled enrollment", OperationRefused, "enrollment_disabled", ErrEnrollmentDisabled},
		{"refused operation", OperationRefused, "revision_mismatch", ErrOperationRefused},
	} {
		t.Run(tc.name, func(t *testing.T) {
			recorded := operation
			recorded.Outcome = tc.outcome
			recorded.Reason = tc.reason
			tx := scriptedEnrollTx(t, recorded)
			_, _, _, err := newStore(enrollDB(t, recorded, tx)).EnrollHost(context.Background(), operation, enrollRequest())
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("refusal error = %v", err)
			}
			if tx.committed {
				t.Fatal("a recorded refusal must not commit")
			}
		})
	}
}

func TestEnrollHostReportsExpiredAuthorizationOutcomes(t *testing.T) {
	operation := enrollmentOperation()
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
		tx := scriptedEnrollTx(t, expiring)
		withExpiryCheck(t, externalRow{err: errStoreUnit}, tx)
		_, _, _, err := newStore(enrollDB(t, expiring, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "check operation admission deadline") {
			t.Fatalf("deadline error = %v", err)
		}
	})

	t.Run("expired outcome write failure", func(t *testing.T) {
		tx := scriptedEnrollTx(t, expiring)
		withExpiryCheck(t, externalRow{values: []any{true}}, tx)
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		_, _, _, err := newStore(enrollDB(t, expiring, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record operation outcome") {
			t.Fatalf("expired outcome error = %v", err)
		}
	})

	t.Run("expired outcome commit failure", func(t *testing.T) {
		tx := scriptedEnrollTx(t, expiring)
		withExpiryCheck(t, externalRow{values: []any{true}}, tx)
		tx.commitErr = errStoreUnit
		_, _, _, err := newStore(enrollDB(t, expiring, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit expired operation") {
			t.Fatalf("expired commit error = %v", err)
		}
	})

	t.Run("expired authorization", func(t *testing.T) {
		tx := scriptedEnrollTx(t, expiring)
		withExpiryCheck(t, externalRow{values: []any{true}}, tx)
		_, _, _, err := newStore(enrollDB(t, expiring, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, ErrAuthorizationExpired) {
			t.Fatalf("expired error = %v", err)
		}
		if !tx.committed {
			t.Fatal("the expired outcome must commit")
		}
	})
}

func TestEnrollHostJournalsRefusalsForExistingEnrollments(t *testing.T) {
	operation := enrollmentOperation()
	disabledAt := time.Unix(1_700_000_100, 0).UTC()

	existingEnrollment := func(disabled bool) ExternalEnrollment {
		enrollment := ExternalEnrollment{
			HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
			OperationID: "op-old", State: "enrollment_ready",
		}
		if disabled {
			enrollment.State = "enrollment_disabled"
			enrollment.DisabledAt = &disabledAt
		}
		return enrollment
	}

	for _, tc := range []struct {
		name     string
		disabled bool
		wantErr  error
	}{
		{"duplicate host", false, ErrDuplicateHost},
		{"disabled enrollment", true, ErrEnrollmentDisabled},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := scriptedEnrollTx(t, operation)
			base := tx.queryRowFn
			tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
				switch {
				case strings.Contains(sql, "INSERT INTO external_enrollments"):
					return externalRow{err: pgx.ErrNoRows}
				case strings.Contains(sql, "FROM external_enrollments"):
					return enrollmentRow(existingEnrollment(tc.disabled))
				default:
					return base(ctx, sql, args...)
				}
			}
			_, _, _, err := newStore(enrollDB(t, operation, tx)).EnrollHost(context.Background(), operation, enrollRequest())
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("refusal error = %v", err)
			}
			if !tx.committed {
				t.Fatal("the refusal journal row must commit")
			}
		})
	}

	for _, tc := range []struct {
		name     string
		disabled bool
		fail     string
	}{
		{"duplicate outcome write failure", false, "outcome"},
		{"duplicate refusal commit failure", false, "commit"},
		{"disabled outcome write failure", true, "outcome"},
		{"disabled refusal commit failure", true, "commit"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := scriptedEnrollTx(t, operation)
			base := tx.queryRowFn
			tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
				switch {
				case strings.Contains(sql, "INSERT INTO external_enrollments"):
					return externalRow{err: pgx.ErrNoRows}
				case strings.Contains(sql, "FROM external_enrollments"):
					return enrollmentRow(existingEnrollment(tc.disabled))
				default:
					return base(ctx, sql, args...)
				}
			}
			if tc.fail == "outcome" {
				tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
					if strings.Contains(sql, "UPDATE external_operations") {
						return pgconn.CommandTag{}, errStoreUnit
					}
					return pgconn.NewCommandTag("UPDATE 1"), nil
				}
			} else {
				tx.commitErr = errStoreUnit
			}
			_, _, _, err := newStore(enrollDB(t, operation, tx)).EnrollHost(context.Background(), operation, enrollRequest())
			if !errors.Is(err, errStoreUnit) {
				t.Fatalf("journal failure = %v", err)
			}
		})
	}
}

func TestEnrollHostReportsEnrollmentWriteFailures(t *testing.T) {
	operation := enrollmentOperation()

	t.Run("enrollment insert failure", func(t *testing.T) {
		tx := scriptedEnrollTx(t, operation)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_enrollments") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		_, _, _, err := newStore(enrollDB(t, operation, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record enrollment") {
			t.Fatalf("enrollment insert error = %v", err)
		}
	})

	t.Run("outcome write failure", func(t *testing.T) {
		tx := scriptedEnrollTx(t, operation)
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		_, _, _, err := newStore(enrollDB(t, operation, tx)).EnrollHost(context.Background(), operation, enrollRequest())
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record enrollment outcome") {
			t.Fatalf("outcome write error = %v", err)
		}
	})
}
