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
		case *uint64:
			value, _ := r.values[index].(uint64)
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

func TestEnrollHostReportsADuplicateHostAsConflict(t *testing.T) {
	tx := &externalTx{insertedID: "op-1", execErr: &pgconn.PgError{Code: "23505"}}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	_, _, _, err := newStore(db).EnrollHost(context.Background(), ExternalOperation{OperationID: "op-1"}, ExternalEnrollment{HostID: "host-1"})
	if !errors.Is(err, ErrConflict) {
		t.Fatalf("expected ErrConflict, got %v", err)
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

// scriptedTx is a pgx.Tx with scripted row, query and exec outcomes.
type scriptedTx struct {
	pgx.Tx
	queryRowFn func(context.Context, string, ...any) pgx.Row
	queryFn    func(context.Context, string, ...any) (pgx.Rows, error)
	execFn     func(context.Context, string, ...any) (pgconn.CommandTag, error)
	committed  bool
	rolledBack bool
}

func (tx *scriptedTx) QueryRow(ctx context.Context, sql string, args ...any) pgx.Row {
	return tx.queryRowFn(ctx, sql, args...)
}
func (tx *scriptedTx) Query(ctx context.Context, sql string, args ...any) (pgx.Rows, error) {
	return tx.queryFn(ctx, sql, args...)
}
func (tx *scriptedTx) Exec(ctx context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	return tx.execFn(ctx, sql, args...)
}
func (tx *scriptedTx) Commit(context.Context) error   { tx.committed = true; return nil }
func (tx *scriptedTx) Rollback(context.Context) error { tx.rolledBack = true; return nil }

func TestMarkEnrollmentReadyAdvancesAndRecordsTheOutcomeAtomically(t *testing.T) {
	tx := &scriptedTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		if strings.Contains(sql, "UPDATE external_enrollments") {
			return externalRow{values: []any{"host-1", "ws-1", "1", []byte("sign"), []byte("noise"), "op-1", "enrollment_ready", nil}}
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
	existing := &scriptedTx{}
	existing.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		if strings.Contains(sql, "UPDATE external_enrollments") {
			return externalRow{err: pgx.ErrNoRows}
		}
		return externalRow{values: []any{"host-1", "ws-1", "1", []byte("sign"), []byte("noise"), "op-1", "enrollment_ready", nil}}
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

	missing := &scriptedTx{}
	missing.queryRowFn = func(context.Context, string, ...any) pgx.Row { return externalRow{err: pgx.ErrNoRows} }
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
	tx := &scriptedTx{}
	disabledAt := time.Now().UTC()
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "SELECT operation_id, disabled_at"):
			return externalRow{values: []any{"op-1", (*time.Time)(nil)}}
		case strings.Contains(sql, "UPDATE external_enrollments"):
			updated := disabledAt
			return externalRow{values: []any{&updated}}
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
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	disabled, err := newStore(db).DisableEnrollment(context.Background(), "host-1", disabledAt)
	if err != nil {
		t.Fatal(err)
	}
	if !disabled.DisabledAt.Equal(disabledAt) || len(disabled.Revocations) != 2 ||
		disabled.Revocations["device-1"] != 4 || disabled.Revocations["device-2"] != 7 {
		t.Fatalf("unexpected disable result %+v", disabled)
	}
	if execs != 2 || !tx.committed {
		t.Fatalf("execs=%d committed=%v", execs, tx.committed)
	}
}

func TestDisableEnrollmentIsIdempotentAndReportsMissingHosts(t *testing.T) {
	disabledAt := time.Now().UTC()
	tx := &scriptedTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		if !strings.Contains(sql, "SELECT operation_id, disabled_at") {
			t.Fatalf("a repeated disable must not write the enrollment again: %q", sql)
		}
		return externalRow{values: []any{"op-1", &disabledAt}}
	}
	tx.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) {
		return &fakeRowSet{}, nil
	}
	tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "pg_advisory_xact_lock") && !strings.Contains(sql, "UPDATE external_operations") {
			t.Fatalf("unexpected exec %q", sql)
		}
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	disabled, err := newStore(db).DisableEnrollment(context.Background(), "host-1", time.Now().UTC())
	if err != nil || !disabled.DisabledAt.Equal(disabledAt) || len(disabled.Revocations) != 0 {
		t.Fatalf("repeat disable = %+v, %v", disabled, err)
	}

	missing := &scriptedTx{
		queryRowFn: func(context.Context, string, ...any) pgx.Row { return externalRow{err: pgx.ErrNoRows} },
		execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.NewCommandTag("SELECT 1"), nil
		},
	}
	db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return missing, nil }}
	if _, err := newStore(db).DisableEnrollment(context.Background(), "missing", time.Now().UTC()); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expected ErrNotFound, got %v", err)
	}
}

func TestReplaceHostCredentialCommitsOneOperationAndCredential(t *testing.T) {
	tx := &scriptedTx{}
	tx.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "INSERT INTO external_operations"):
			return externalRow{values: []any{"op-r"}}
		case strings.Contains(sql, "UPDATE external_enrollments"):
			return externalRow{values: []any{"host-1", "ws-1", "1", []byte("sign"), []byte("noise"), "op-1", "enrollment_ready", nil}}
		default:
			t.Fatalf("unexpected query row %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	var updated string
	tx.execFn = func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
		if strings.Contains(sql, "outcome = 'credential_replaced'") {
			updated, _ = args[0].(string)
		}
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	enrollment, credential, err := newStore(db).ReplaceHostCredential(context.Background(), ExternalOperation{
		OperationID: "op-r", AuthorizationID: "auth-r", HostID: "host-1",
		WorkspaceID: "ws-1", EnrollmentRevision: "1",
	})
	if err != nil {
		t.Fatal(err)
	}
	if credential == "" || enrollment.HostID != "host-1" || !tx.committed {
		t.Fatalf("unexpected replacement %+v credential=%q committed=%v", enrollment, credential, tx.committed)
	}
	if updated != "op-r" {
		t.Fatalf("updated operation = %q", updated)
	}
}

func TestReplaceHostCredentialRejectsReplayAndChangedState(t *testing.T) {
	replay := &scriptedTx{queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return externalRow{err: pgx.ErrNoRows}
	}}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return replay, nil }}
	if _, _, err := newStore(db).ReplaceHostCredential(context.Background(), ExternalOperation{
		OperationID: "op-r", AuthorizationID: "auth-r", HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
	}); !errors.Is(err, ErrOperationConflict) {
		t.Fatalf("expected a replay conflict, got %v", err)
	}

	changed := &scriptedTx{}
	changed.queryRowFn = func(_ context.Context, sql string, _ ...any) pgx.Row {
		if strings.Contains(sql, "INSERT INTO external_operations") {
			return externalRow{values: []any{"op-r"}}
		}
		if strings.Contains(sql, "UPDATE external_enrollments") {
			return externalRow{err: pgx.ErrNoRows}
		}
		return externalRow{values: []any{"host-1", "ws-1", "2", []byte("sign"), []byte("noise"), "op-1", "enrollment_pending", nil}}
	}
	changed.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		t.Fatal("a refused replacement must not record success")
		return pgconn.CommandTag{}, nil
	}
	db = &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return changed, nil }}
	if _, _, err := newStore(db).ReplaceHostCredential(context.Background(), ExternalOperation{
		OperationID: "op-r", AuthorizationID: "auth-r", HostID: "host-1", WorkspaceID: "ws-1", EnrollmentRevision: "1",
	}); !errors.Is(err, ErrConflict) {
		t.Fatalf("expected a conflict, got %v", err)
	}
}
