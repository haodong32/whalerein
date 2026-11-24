package store

import (
	"context"
	"encoding/json"
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
		case *uint64:
			value, _ := r.values[index].(uint64)
			*typed = value
		case *bool:
			value, _ := r.values[index].(bool)
			*typed = value
		}
	}
	return nil
}

// operationRow builds the 20-column external_operations row.
func operationRow(operation ExternalOperation) pgx.Row {
	if operation.State == "" {
		operation.State = "operation_admitted"
	}
	if operation.Outcome == "" {
		operation.Outcome = OperationAdmitted
	}
	return externalRow{values: []any{
		operation.OperationID, operation.AuthorizationID, operation.Purpose,
		operation.WorkspaceID, operation.RegionID, operation.HostID,
		operation.PayloadDigest, operation.State, operation.EnrollmentRevision,
		operation.CommittedAt, operation.AuthorizationExpiresAt,
		operation.ExpectedWorkspaceID, operation.ExpectedEnrollmentRevision,
		operation.AdmittedAt, operation.Outcome, operation.Reason,
		operation.UpdatedAt, operation.CredentialIssued,
		operation.CredentialGeneration, operation.CredentialOperationID,
	}}
}

// enrollmentRow builds the 10-column external_enrollments row.
func enrollmentRow(enrollment ExternalEnrollment) pgx.Row {
	if enrollment.State == "" {
		enrollment.State = "enrollment_pending"
	}
	return externalRow{values: []any{
		enrollment.HostID, enrollment.WorkspaceID, enrollment.EnrollmentRevision,
		enrollment.HostSigningPublicKey, enrollment.HostNoisePublicKey,
		enrollment.OperationID, enrollment.State, enrollment.DisabledAt,
		enrollment.CurrentCredentialOperationID, enrollment.CredentialGeneration,
	}}
}

// removalRow builds the 8-column external_removals row.
func removalRowOf(removal removalRow) pgx.Row {
	revocations, _ := json.Marshal(removal.Revocations)
	return externalRow{values: []any{
		removal.RemovalOperationID, removal.HostID, removal.WorkspaceID,
		removal.EnrollmentOperationID, removal.EnrollmentRevision,
		removal.DisabledAt, removal.EnforcedAt, revocations,
	}}
}

// externalTx scripts the transaction outcomes used by the external mutations.
type externalTx struct {
	pgx.Tx
	queryRowFn func(context.Context, string, ...any) pgx.Row
	queryFn    func(context.Context, string, ...any) (pgx.Rows, error)
	execFn     func(context.Context, string, ...any) (pgconn.CommandTag, error)
	commitErr  error
	committed  bool
	rolledBack bool
}

func (tx *externalTx) QueryRow(ctx context.Context, sql string, args ...any) pgx.Row {
	if tx.queryRowFn == nil {
		return externalRow{err: pgx.ErrNoRows}
	}
	return tx.queryRowFn(ctx, sql, args...)
}

func (tx *externalTx) Query(ctx context.Context, sql string, args ...any) (pgx.Rows, error) {
	if tx.queryFn == nil {
		return nil, errors.New("unexpected query")
	}
	return tx.queryFn(ctx, sql, args...)
}

func (tx *externalTx) Exec(ctx context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	if tx.execFn == nil {
		return pgconn.NewCommandTag("UPDATE 0"), nil
	}
	return tx.execFn(ctx, sql, args...)
}

func (tx *externalTx) Commit(context.Context) error {
	if tx.commitErr != nil {
		return tx.commitErr
	}
	tx.committed = true
	return nil
}

func (tx *externalTx) Rollback(context.Context) error { tx.rolledBack = true; return nil }

// scriptedTx keeps the older scripted transaction name available to the other
// unit tests in this package.
type scriptedTx = externalTx

func TestEnrollHostCommitsOperationEnrollmentAndCredential(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	operation := ExternalOperation{
		OperationID: "op-1", AuthorizationID: "auth-1", Purpose: "host_enrollment",
		WorkspaceID: "ws-1", RegionID: "region-1", HostID: "host-1",
		PayloadDigest: []byte("digest"), State: "operation_admitted",
		EnrollmentRevision: "1", AdmittedAt: &at,
	}
	tx := &externalTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "FOR UPDATE"):
			return operationRow(operation)
		case strings.Contains(sql, "INSERT INTO external_enrollments"):
			return enrollmentRow(ExternalEnrollment{
				HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
				OperationID: "op-1", State: "enrollment_pending",
				CurrentCredentialOperationID: "op-1", CredentialGeneration: 1,
			})
		default:
			t.Fatalf("unexpected query row %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	var outcomeOperation string
	tx.execFn = func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		switch {
		case strings.Contains(sql, "pg_advisory_xact_lock"):
			return pgconn.NewCommandTag("SELECT 1"), nil
		case strings.Contains(sql, "UPDATE external_operations"):
			outcomeOperation, _ = args[0].(string)
			return pgconn.NewCommandTag("UPDATE 1"), nil
		default:
			t.Fatalf("unexpected exec %q", sql)
			return pgconn.CommandTag{}, nil
		}
	}
	db := &fakeDB{t: t,
		queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
		beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
	}
	enrollment := ExternalEnrollment{
		HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		HostSigningPublicKey: []byte("sign"), HostNoisePublicKey: []byte("noise"),
	}
	recorded, credential, committed, err := newStore(db).EnrollHost(context.Background(), operation, enrollment)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !committed || credential == "" {
		t.Fatal("the committing call must report a commit and one credential")
	}
	if !tx.committed {
		t.Fatal("the transaction must commit")
	}
	if recorded.OperationID != "op-1" || recorded.State != "enrollment_pending" ||
		recorded.CurrentCredentialOperationID != "op-1" || recorded.CredentialGeneration != 1 {
		t.Fatalf("unexpected recorded enrollment %+v", recorded)
	}
	if outcomeOperation != "op-1" {
		t.Fatalf("the mutation must record operation op-1, got %q", outcomeOperation)
	}
}

func TestEnrollHostReportsACommitFailure(t *testing.T) {
	operation := ExternalOperation{OperationID: "op-1", PayloadDigest: []byte("digest")}
	tx := &externalTx{commitErr: errors.New("commit failed")}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "FOR UPDATE"):
			return operationRow(operation)
		case strings.Contains(sql, "INSERT INTO external_enrollments"):
			return enrollmentRow(ExternalEnrollment{HostID: "host-1", OperationID: "op-1"})
		default:
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	tx.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	db := &fakeDB{t: t,
		queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
		beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
	}
	_, _, _, err := newStore(db).EnrollHost(context.Background(), operation, ExternalEnrollment{HostID: "host-1"})
	if err == nil || !strings.Contains(err.Error(), "commit") {
		t.Fatalf("expected a commit failure, got %v", err)
	}
	if !tx.rolledBack {
		t.Fatal("a failed commit must still roll back")
	}
}

func TestEnrollHostReportsADuplicateHostAsConflict(t *testing.T) {
	operation := ExternalOperation{OperationID: "op-1", PayloadDigest: []byte("digest")}
	tx := &externalTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "FOR UPDATE"):
			return operationRow(operation)
		case strings.Contains(sql, "INSERT INTO external_enrollments"):
			return externalRow{err: pgx.ErrNoRows}
		default:
			return enrollmentRow(ExternalEnrollment{
				HostID: "host-1", OperationID: "op-existing", State: "enrollment_ready",
			})
		}
	}
	tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if strings.Contains(sql, "pg_advisory_xact_lock") {
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		if !strings.Contains(sql, "UPDATE external_operations") {
			t.Fatalf("unexpected exec %q", sql)
		}
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	db := &fakeDB{t: t,
		queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
		beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
	}
	_, _, _, err := newStore(db).EnrollHost(context.Background(), operation, ExternalEnrollment{HostID: "host-1"})
	if !errors.Is(err, ErrDuplicateHost) {
		t.Fatalf("expected ErrDuplicateHost, got %v", err)
	}
	if !tx.committed {
		t.Fatal("the refusal journal row must commit")
	}
}

func TestEnrollHostRefusesADisabledHostAndAnExpiredAuthorization(t *testing.T) {
	disabled := time.Unix(1_700_000_100, 0).UTC()

	t.Run("disabled host", func(t *testing.T) {
		operation := ExternalOperation{OperationID: "op-1", PayloadDigest: []byte("digest")}
		tx := &externalTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			switch {
			case strings.Contains(sql, "FOR UPDATE"):
				return operationRow(operation)
			case strings.Contains(sql, "INSERT INTO external_enrollments"):
				return externalRow{err: pgx.ErrNoRows}
			default:
				return enrollmentRow(ExternalEnrollment{
					HostID: "host-1", OperationID: "op-old", State: "enrollment_disabled", DisabledAt: &disabled,
				})
			}
		}
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "pg_advisory_xact_lock") || strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.NewCommandTag("UPDATE 1"), nil
			}
			t.Fatalf("unexpected exec %q", sql)
			return pgconn.CommandTag{}, nil
		}
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
		}
		if _, _, _, err := newStore(db).EnrollHost(context.Background(), operation, ExternalEnrollment{HostID: "host-1"}); !errors.Is(err, ErrEnrollmentDisabled) {
			t.Fatalf("expected ErrEnrollmentDisabled, got %v", err)
		}
		if !tx.committed {
			t.Fatal("the refusal journal row must commit")
		}
	})

	t.Run("expired authorization", func(t *testing.T) {
		expires := time.Unix(1_700_000_000, 0).UTC()
		operation := ExternalOperation{
			OperationID: "op-1", PayloadDigest: []byte("digest"), AuthorizationExpiresAt: &expires,
		}
		tx := &externalTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			switch {
			case strings.Contains(sql, "FOR UPDATE"):
				return operationRow(operation)
			case strings.Contains(sql, "clock_timestamp()"):
				return externalRow{values: []any{true}}
			default:
				return externalRow{err: pgx.ErrNoRows}
			}
		}
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "pg_advisory_xact_lock") || strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.NewCommandTag("UPDATE 1"), nil
			}
			t.Fatalf("unexpected exec %q", sql)
			return pgconn.CommandTag{}, nil
		}
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
		}
		if _, _, _, err := newStore(db).EnrollHost(context.Background(), operation, ExternalEnrollment{HostID: "host-1"}); !errors.Is(err, ErrAuthorizationExpired) {
			t.Fatalf("expected ErrAuthorizationExpired, got %v", err)
		}
		if !tx.committed {
			t.Fatal("the expired outcome must commit")
		}
	})

	t.Run("changed admission payload", func(t *testing.T) {
		operation := ExternalOperation{OperationID: "op-1", PayloadDigest: []byte("digest")}
		recorded := operation
		recorded.PayloadDigest = []byte("other")
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "FOR UPDATE") {
				return operationRow(recorded)
			}
			return externalRow{err: pgx.ErrNoRows}
		}}
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
		}
		if _, _, _, err := newStore(db).EnrollHost(context.Background(), operation, ExternalEnrollment{HostID: "host-1"}); !errors.Is(err, ErrOperationConflict) {
			t.Fatalf("expected ErrOperationConflict, got %v", err)
		}
		if tx.committed {
			t.Fatal("a changed admission must not commit")
		}
	})
}

func TestEnrollHostReportsABeginFailure(t *testing.T) {
	db := &fakeDB{t: t,
		queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return operationRow(ExternalOperation{OperationID: "op-1"})
		},
		beginFn: func(context.Context) (pgx.Tx, error) { return nil, errors.New("no connection") },
	}
	if _, _, _, err := newStore(db).EnrollHost(context.Background(), ExternalOperation{OperationID: "op-1"}, ExternalEnrollment{}); err == nil || !strings.Contains(err.Error(), "begin") {
		t.Fatalf("a begin failure must be surfaced, got %v", err)
	}
	if _, _, _, err := newStore(db).EnrollHost(context.Background(), ExternalOperation{}, ExternalEnrollment{}); err == nil || !strings.Contains(err.Error(), "operation id") {
		t.Fatalf("an empty operation id must be refused, got %v", err)
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

func TestEnrollmentReturnsAStoredRowWithItsRemovalState(t *testing.T) {
	disabled := time.Unix(1_700_000_000, 0).UTC()
	enforced := disabled.Add(time.Minute)
	db := &fakeDB{t: t, queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
		if strings.Contains(sql, "FROM external_removals") {
			return removalRowOf(removalRow{
				RemovalOperationID: "rem-1", HostID: "host-1", WorkspaceID: "ws-1",
				EnrollmentOperationID: "op-1", EnrollmentRevision: "3",
				DisabledAt: disabled, EnforcedAt: &enforced,
				Revocations: map[string]uint64{"device-1": 4},
			})
		}
		return enrollmentRow(ExternalEnrollment{
			HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "3",
			HostSigningPublicKey: []byte("sign"), HostNoisePublicKey: []byte("noise"),
			OperationID: "op-1", State: "enrollment_disabled", DisabledAt: &disabled,
			CurrentCredentialOperationID: "op-1", CredentialGeneration: 2,
		})
	}}
	enrollment, err := newStore(db).Enrollment(context.Background(), "host-1")
	if err != nil {
		t.Fatal(err)
	}
	if enrollment.HostID != "host-1" || enrollment.EnrollmentRevision != "3" ||
		enrollment.DisabledAt == nil || enrollment.CredentialGeneration != 2 ||
		enrollment.RemovalState != "enforced" || enrollment.RemovalOperationID != "rem-1" ||
		enrollment.RemovalEnforcedAt == nil {
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

func TestNewOperationIDIsUniqueAndPrefixed(t *testing.T) {
	first := NewOperationID()
	second := NewOperationID()
	if first == second || !strings.HasPrefix(first, "op-") {
		t.Fatalf("unexpected operation ids %q and %q", first, second)
	}
}

func TestHostBrokerCredentialLoadsOnlyTheHashAndLifecycle(t *testing.T) {
	disabled := time.Now().UTC()
	db := &fakeDB{t: t, queryRowFn: func(_ context.Context, sql string, args ...any) pgx.Row {
		if !strings.Contains(sql, "FROM external_enrollments") || args[0] != "host-1" {
			t.Fatalf("unexpected query %q args %v", sql, args)
		}
		return externalRow{values: []any{"host-1", []byte("hash"), "enrollment_disabled", &disabled}}
	}}
	credential, err := newStore(db).HostBrokerCredential(context.Background(), "host-1")
	if err != nil {
		t.Fatal(err)
	}
	if credential.HostID != "host-1" || string(credential.BrokerCredentialHash) != "hash" ||
		credential.State != "enrollment_disabled" || credential.DisabledAt == nil {
		t.Fatalf("unexpected credential %+v", credential)
	}
	if _, err := newStore(&fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return externalRow{err: pgx.ErrNoRows}
	}}).HostBrokerCredential(context.Background(), "missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expected ErrNotFound, got %v", err)
	}
}

func TestConsumeHostProofNonceIsFirstWriterWins(t *testing.T) {
	var inserts int
	db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if strings.Contains(sql, "DELETE FROM host_proof_nonces") {
			return pgconn.NewCommandTag("DELETE 0"), nil
		}
		inserts++
		if inserts == 1 {
			return pgconn.NewCommandTag("INSERT 0 1"), nil
		}
		return pgconn.NewCommandTag("INSERT 0 0"), nil
	}}
	store := newStore(db)
	nonce := make([]byte, 32)
	consumed, err := store.ConsumeHostProofNonce(context.Background(), nonce, "enrollment_status", time.Now().UTC())
	if err != nil || !consumed {
		t.Fatalf("first consume = %v, %v", consumed, err)
	}
	consumed, err = store.ConsumeHostProofNonce(context.Background(), nonce, "enrollment_status", time.Now().UTC())
	if err != nil || consumed {
		t.Fatalf("replayed consume = %v, %v", consumed, err)
	}
	if _, err := store.ConsumeHostProofNonce(context.Background(), []byte("short"), "enrollment_status", time.Now().UTC()); err == nil {
		t.Fatal("a malformed nonce hash must be refused")
	}
}

func TestMarkEnrollmentReadyAdvancesAndRecordsTheOutcomeAtomically(t *testing.T) {
	tx := &externalTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		if strings.Contains(sql, "UPDATE external_enrollments") {
			return enrollmentRow(ExternalEnrollment{
				HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
				OperationID: "op-1", State: "enrollment_ready",
				CurrentCredentialOperationID: "op-1", CredentialGeneration: 1,
			})
		}
		return externalRow{err: pgx.ErrNoRows}
	}
	tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "external_operations") ||
			!strings.Contains(sql, "outcome = 'enrollment_ready'") {
			t.Fatalf("unexpected exec %q", sql)
		}
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	enrollment, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", time.Now().UTC())
	if err != nil {
		t.Fatal(err)
	}
	if !advanced || enrollment.State != "enrollment_ready" || !tx.committed {
		t.Fatalf("unexpected ready result %+v advanced=%v committed=%v", enrollment, advanced, tx.committed)
	}
}

func TestMarkEnrollmentReadyIsIdempotentForKnownAndUnknownHosts(t *testing.T) {
	existing := &externalTx{}
	existing.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		if strings.Contains(sql, "UPDATE external_enrollments") {
			return externalRow{err: pgx.ErrNoRows}
		}
		return enrollmentRow(ExternalEnrollment{
			HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
			OperationID: "op-1", State: "enrollment_ready",
			CurrentCredentialOperationID: "op-1", CredentialGeneration: 1,
		})
	}
	existing.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		t.Fatal("an already-ready enrollment must not update the operation")
		return pgconn.CommandTag{}, nil
	}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return existing, nil }}
	enrollment, advanced, err := newStore(db).MarkEnrollmentReady(context.Background(), "host-1", time.Now().UTC())
	if err != nil || advanced || enrollment.State != "enrollment_ready" || !existing.committed {
		t.Fatalf("existing ready = %+v advanced=%v err=%v", enrollment, advanced, err)
	}

	missing := &externalTx{queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return externalRow{err: pgx.ErrNoRows}
	}}
	missing.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		t.Fatal("an unknown Host must not write")
		return pgconn.CommandTag{}, nil
	}
	db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return missing, nil }}
	if _, _, err := newStore(db).MarkEnrollmentReady(context.Background(), "missing", time.Now().UTC()); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expected ErrNotFound, got %v", err)
	}
}

func TestDisableEnrollmentRevokesGrantsAndDeletesOrphanedCredentials(t *testing.T) {
	tx := &externalTx{}
	disabledAt := time.Now().UTC()
	var insertedRemoval string
	tx.queryRowFn = func(_ context.Context, sql string, args ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "SELECT operation_id, workspace_id, enrollment_revision, disabled_at"):
			return externalRow{values: []any{"op-1", "ws-1", "1", (*time.Time)(nil)}}
		case strings.Contains(sql, "UPDATE external_enrollments"):
			updated := disabledAt
			return externalRow{values: []any{&updated}}
		case strings.Contains(sql, "INSERT INTO external_removals"):
			insertedRemoval, _ = args[0].(string)
			return externalRow{values: []any{insertedRemoval}}
		default:
			t.Fatalf("unexpected query row %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	tx.queryFn = func(_ context.Context, sql string, _ ...any) (pgx.Rows, error) {
		if !strings.Contains(sql, "UPDATE grants") {
			t.Fatalf("unexpected query %q", sql)
		}
		return &fakeRowSet{rows: []pgx.Row{
			externalRow{values: []any{"device-1", uint64(4)}},
			externalRow{values: []any{"device-2", uint64(7)}},
		}}, nil
	}
	execs := 0
	tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if strings.Contains(sql, "pg_advisory_xact_lock") {
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		execs++
		if !strings.Contains(sql, "DELETE FROM device_credentials") && !strings.Contains(sql, "UPDATE external_operations") {
			t.Fatalf("unexpected exec %q", sql)
		}
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	expected := &RemovalTarget{WorkspaceID: "ws-1", EnrollmentOperationID: "op-1", EnrollmentRevision: "1"}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	disabled, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", expected, disabledAt)
	if err != nil {
		t.Fatal(err)
	}
	if !disabled.DisabledAt.Equal(disabledAt) || len(disabled.Revocations) != 2 ||
		disabled.Revocations["device-1"] != 4 || disabled.Revocations["device-2"] != 7 {
		t.Fatalf("unexpected disable result %+v", disabled)
	}
	if disabled.RemovalOperationID != insertedRemoval || disabled.Target != *expected {
		t.Fatalf("unexpected removal identity %+v", disabled)
	}
	if execs != 2 || !tx.committed {
		t.Fatalf("execs=%d committed=%v", execs, tx.committed)
	}
}

func TestDisableEnrollmentIsIdempotentAndReportsMissingHosts(t *testing.T) {
	disabledAt := time.Now().UTC()
	tx := &externalTx{}
	var insertedRemoval string
	tx.queryRowFn = func(_ context.Context, sql string, args ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "SELECT operation_id, workspace_id, enrollment_revision, disabled_at"):
			return externalRow{values: []any{"op-1", "ws-1", "1", &disabledAt}}
		case strings.Contains(sql, "INSERT INTO external_removals"):
			insertedRemoval, _ = args[0].(string)
			return externalRow{values: []any{insertedRemoval}}
		default:
			t.Fatalf("a repeated disable must not write the enrollment again: %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	tx.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) {
		t.Fatal("a repeated disable must not revoke again")
		return nil, nil
	}
	execs := 0
	tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "pg_advisory_xact_lock") {
			t.Fatalf("unexpected exec %q", sql)
		}
		execs++
		return pgconn.NewCommandTag("SELECT 1"), nil
	}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	disabled, err := newStore(db).DisableEnrollment(context.Background(), "host-1", time.Now().UTC())
	if err != nil || !disabled.DisabledAt.Equal(disabledAt) || len(disabled.Revocations) != 0 {
		t.Fatalf("repeat disable = %+v, %v", disabled, err)
	}
	if disabled.RemovalOperationID != insertedRemoval || execs != 1 || !tx.committed {
		t.Fatalf("removal=%q execs=%d committed=%v", disabled.RemovalOperationID, execs, tx.committed)
	}

	missing := &externalTx{
		queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "SELECT operation_id, workspace_id, enrollment_revision, disabled_at") {
				return externalRow{err: pgx.ErrNoRows}
			}
			t.Fatalf("a missing Host must not write %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		},
		execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			return pgconn.NewCommandTag("SELECT 1"), nil
		},
	}
	db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return missing, nil }}
	if _, err := newStore(db).DisableEnrollment(context.Background(), "missing", time.Now().UTC()); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expected ErrNotFound, got %v", err)
	}
	if !missing.rolledBack {
		t.Fatal("a missing Host must roll back")
	}
}

func TestDisableEnrollmentRejectsAMismatchedRemovalTargetAndReplaysStoredRevocations(t *testing.T) {
	disabledAt := time.Now().UTC()
	effective := removalRow{
		HostID: "host-1", WorkspaceID: "ws-1", EnrollmentOperationID: "op-1",
		EnrollmentRevision: "1", DisabledAt: disabledAt,
	}

	t.Run("target mismatch", func(t *testing.T) {
		tx := &externalTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "SELECT operation_id, workspace_id, enrollment_revision, disabled_at") {
				return externalRow{values: []any{"op-1", "ws-1", "1", (*time.Time)(nil)}}
			}
			t.Fatalf("a mismatched target must not write: %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if !strings.Contains(sql, "pg_advisory_xact_lock") {
				t.Fatalf("unexpected exec %q", sql)
			}
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		expected := &RemovalTarget{WorkspaceID: "ws-1", EnrollmentOperationID: "op-other", EnrollmentRevision: "1"}
		if _, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "rem-2", expected, disabledAt); !errors.Is(err, ErrTargetMismatch) {
			t.Fatalf("expected ErrTargetMismatch, got %v", err)
		}
		if !tx.rolledBack {
			t.Fatal("a target mismatch must roll back")
		}
	})

	t.Run("stored revocation replay", func(t *testing.T) {
		expected := &RemovalTarget{WorkspaceID: "ws-1", EnrollmentOperationID: "op-1", EnrollmentRevision: "1"}
		stored := effective
		stored.RemovalOperationID = "rem-2"
		stored.Revocations = map[string]uint64{"device-1": 4, "device-2": 7}
		tx := &externalTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			switch {
			case strings.Contains(sql, "SELECT operation_id, workspace_id, enrollment_revision, disabled_at"):
				return externalRow{values: []any{"op-1", "ws-1", "1", (*time.Time)(nil)}}
			case strings.Contains(sql, "FROM external_removals"):
				return removalRowOf(stored)
			default:
				t.Fatalf("a stored removal replay must not write: %q", sql)
				return externalRow{err: pgx.ErrNoRows}
			}
		}
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if !strings.Contains(sql, "pg_advisory_xact_lock") {
				t.Fatalf("unexpected exec %q", sql)
			}
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		disabled, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "rem-2", expected, disabledAt)
		if err != nil {
			t.Fatal(err)
		}
		if disabled.RemovalOperationID != "rem-2" || !disabled.DisabledAt.Equal(disabledAt) ||
			disabled.Revocations["device-1"] != 4 || disabled.Revocations["device-2"] != 7 {
			t.Fatalf("stored removal = %+v", disabled)
		}
		if !tx.committed {
			t.Fatal("a stored removal replay must commit its read transaction")
		}
	})
}

func TestMarkRemovalEnforcedRecordsOnlyTheFirstAcknowledgement(t *testing.T) {
	var calls int
	db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		calls++
		if !strings.Contains(sql, "UPDATE external_removals") || args[0] != "rem-1" {
			t.Fatalf("unexpected exec %q args %v", sql, args)
		}
		if calls == 1 {
			return updateTag(1), nil
		}
		return updateTag(0), nil
	}}
	if err := newStore(db).MarkRemovalEnforced(context.Background(), "rem-1", time.Now().UTC()); err != nil {
		t.Fatalf("first acknowledgement = %v", err)
	}
	if err := newStore(db).MarkRemovalEnforced(context.Background(), "rem-1", time.Now().UTC()); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing removal = %v", err)
	}
	if err := newStore(&fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, errStoreUnit
	}}).MarkRemovalEnforced(context.Background(), "rem-1", time.Now().UTC()); !errors.Is(err, errStoreUnit) {
		t.Fatalf("storage failure = %v", err)
	}
	if err := newStore(&fakeDB{t: t}).MarkRemovalEnforced(context.Background(), "", time.Now().UTC()); err == nil || !strings.Contains(err.Error(), "removal operation id") {
		t.Fatalf("empty removal id = %v", err)
	}
}

func TestReplaceHostCredentialCommitsOneOperationAndCredential(t *testing.T) {
	operation := ExternalOperation{
		OperationID: "op-r", AuthorizationID: "auth-r", HostID: "host-1",
		WorkspaceID: "ws-1", EnrollmentRevision: "1", PayloadDigest: []byte("digest"),
	}
	tx := &externalTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "FOR UPDATE"):
			return operationRow(operation)
		case strings.Contains(sql, "UPDATE external_enrollments"):
			return enrollmentRow(ExternalEnrollment{
				HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
				OperationID: "op-1", State: "enrollment_ready",
				CurrentCredentialOperationID: "op-r", CredentialGeneration: 2,
			})
		default:
			t.Fatalf("unexpected query row %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	var updatedOperation string
	var updatedGeneration uint64
	tx.execFn = func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		switch {
		case strings.Contains(sql, "pg_advisory_xact_lock"):
			return pgconn.NewCommandTag("SELECT 1"), nil
		case strings.Contains(sql, "UPDATE external_operations"):
			updatedOperation, _ = args[0].(string)
			updatedGeneration, _ = args[1].(uint64)
			return pgconn.NewCommandTag("UPDATE 1"), nil
		default:
			t.Fatalf("unexpected exec %q", sql)
			return pgconn.CommandTag{}, nil
		}
	}
	db := &fakeDB{t: t,
		queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
		beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
	}
	enrollment, credential, err := newStore(db).ReplaceHostCredential(context.Background(), operation)
	if err != nil {
		t.Fatal(err)
	}
	if credential == "" || enrollment.HostID != "host-1" || !tx.committed {
		t.Fatalf("unexpected replacement %+v credential=%q committed=%v", enrollment, credential, tx.committed)
	}
	if updatedOperation != "op-r" || updatedGeneration != 2 {
		t.Fatalf("updated operation = %q generation=%d", updatedOperation, updatedGeneration)
	}
}

func TestReplaceHostCredentialRejectsReplayAndChangedState(t *testing.T) {
	operation := ExternalOperation{
		OperationID: "op-r", AuthorizationID: "auth-r", HostID: "host-1",
		WorkspaceID: "ws-1", EnrollmentRevision: "1", PayloadDigest: []byte("digest"),
	}
	enrollment := ExternalEnrollment{
		HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
		OperationID: "op-1", State: "enrollment_ready",
		CurrentCredentialOperationID: "op-r", CredentialGeneration: 2,
	}

	t.Run("committed replay returns the stored enrollment without a new credential", func(t *testing.T) {
		committed := operation
		committed.Outcome = OperationCommitted
		committed.CredentialGeneration = 2
		committed.CredentialOperationID = "op-r"
		tx := &externalTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			switch {
			case strings.Contains(sql, "FOR UPDATE"):
				return operationRow(committed)
			case strings.Contains(sql, "FROM external_enrollments"):
				return enrollmentRow(enrollment)
			default:
				t.Fatalf("unexpected query row %q", sql)
				return externalRow{err: pgx.ErrNoRows}
			}
		}
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if !strings.Contains(sql, "pg_advisory_xact_lock") {
				t.Fatalf("a replay must not write %q", sql)
			}
			return pgconn.NewCommandTag("SELECT 1"), nil
		}
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(committed) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
		}
		got, credential, err := newStore(db).ReplaceHostCredential(context.Background(), operation)
		if err != nil || credential != "" || got.HostID != "host-1" || !tx.committed {
			t.Fatalf("replay = %+v credential=%q err=%v committed=%v", got, credential, err, tx.committed)
		}
	})

	t.Run("changed payload is an admission conflict", func(t *testing.T) {
		changed := operation
		changed.PayloadDigest = []byte("other")
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "FOR UPDATE") {
				return operationRow(changed)
			}
			return externalRow{err: pgx.ErrNoRows}
		}}
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
		}
		if _, _, err := newStore(db).ReplaceHostCredential(context.Background(), operation); !errors.Is(err, ErrOperationConflict) {
			t.Fatalf("expected ErrOperationConflict, got %v", err)
		}
		if tx.committed {
			t.Fatal("a changed payload must not commit")
		}
	})

	t.Run("revision mismatch is refused and journaled", func(t *testing.T) {
		tx := &externalTx{}
		tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
			switch {
			case strings.Contains(sql, "FOR UPDATE"):
				return operationRow(operation)
			case strings.Contains(sql, "UPDATE external_enrollments"):
				return externalRow{err: pgx.ErrNoRows}
			case strings.Contains(sql, "FROM external_enrollments"):
				return enrollmentRow(ExternalEnrollment{
					HostID: "host-1", OperationID: "op-1", State: "enrollment_pending",
				})
			default:
				return externalRow{err: pgx.ErrNoRows}
			}
		}
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "pg_advisory_xact_lock") || strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.NewCommandTag("UPDATE 1"), nil
			}
			t.Fatalf("unexpected exec %q", sql)
			return pgconn.CommandTag{}, nil
		}
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(operation) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
		}
		if _, _, err := newStore(db).ReplaceHostCredential(context.Background(), operation); !errors.Is(err, ErrConflict) {
			t.Fatalf("expected a conflict, got %v", err)
		}
		if !tx.committed {
			t.Fatal("the refusal journal row must commit")
		}
	})

	t.Run("recorded refusal is not retried", func(t *testing.T) {
		refused := operation
		refused.Outcome = OperationRefused
		refused.Reason = "duplicate_host"
		tx := &externalTx{queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			if strings.Contains(sql, "FOR UPDATE") {
				return operationRow(refused)
			}
			return externalRow{err: pgx.ErrNoRows}
		}}
		db := &fakeDB{t: t,
			queryRowFn: func(context.Context, string, ...any) pgx.Row { return operationRow(refused) },
			beginFn:    func(context.Context) (pgx.Tx, error) { return tx, nil },
		}
		if _, _, err := newStore(db).ReplaceHostCredential(context.Background(), operation); !errors.Is(err, ErrDuplicateHost) {
			t.Fatalf("expected the recorded refusal, got %v", err)
		}
		if tx.committed {
			t.Fatal("a recorded refusal must not commit a new mutation")
		}
	})
}

func TestHigherRevisionUsesNumericOrdering(t *testing.T) {
	for _, tc := range []struct {
		candidate, current string
		want               bool
	}{
		{"10", "9", true},
		{"9", "10", false},
		{"2", "1", true},
		{"1", "1", false},
		{"", "1", false},
		{"2", "", false},
	} {
		if got := higherRevision(tc.candidate, tc.current); got != tc.want {
			t.Fatalf("higherRevision(%q, %q) = %v, want %v", tc.candidate, tc.current, got, tc.want)
		}
	}
}

func TestRevisionGenerationIsPositive(t *testing.T) {
	for _, tc := range []struct {
		revision string
		want     int64
	}{{"1", 1}, {"9", 9}, {"10", 10}, {"", 1}, {"not-a-number", 1}, {"0", 1}} {
		if got := revisionGeneration(tc.revision); got != tc.want {
			t.Fatalf("revisionGeneration(%q) = %d, want %d", tc.revision, got, tc.want)
		}
	}
}
