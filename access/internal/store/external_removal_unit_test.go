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

// scriptedDisableTx walks the complete removal path with successful statements.
// A test overrides exactly one hook to model the failure under examination; the
// defaults assert that the remaining statements keep their expected scope.
func scriptedDisableTx(t *testing.T, deviceRows []pgx.Row) *externalTx {
	t.Helper()
	tx := &externalTx{}
	tx.queryRowFn = func(_ context.Context, sql string, args ...any) pgx.Row {
		switch {
		case strings.Contains(sql, "FOR UPDATE"):
			if len(args) != 1 || args[0] != "host-1" {
				t.Fatalf("unexpected enrollment lock scope %v", args)
			}
			return externalRow{values: []any{"op-1", "ws-1", "1", (*time.Time)(nil)}}
		case strings.Contains(sql, "UPDATE external_enrollments"):
			disabled, ok := args[1].(time.Time)
			if !ok || disabled.IsZero() {
				t.Fatalf("disable update timestamp = %v", args[1])
			}
			return externalRow{values: []any{&disabled}}
		case strings.Contains(sql, "FROM external_removals"):
			return externalRow{err: pgx.ErrNoRows}
		case strings.Contains(sql, "INSERT INTO external_removals"):
			return externalRow{values: []any{args[0]}}
		default:
			t.Fatalf("unexpected query row %q", sql)
			return externalRow{err: pgx.ErrNoRows}
		}
	}
	tx.queryFn = func(_ context.Context, sql string, _ ...any) (pgx.Rows, error) {
		if !strings.Contains(sql, "UPDATE grants") {
			t.Fatalf("unexpected query %q", sql)
		}
		return &fakeRowSet{rows: deviceRows}, nil
	}
	tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
		if !strings.Contains(sql, "pg_advisory_xact_lock") &&
			!strings.Contains(sql, "DELETE FROM device_credentials") &&
			!strings.Contains(sql, "UPDATE external_operations") {
			t.Fatalf("unexpected exec %q", sql)
		}
		return pgconn.NewCommandTag("UPDATE 1"), nil
	}
	return tx
}

func revokedDeviceRow(deviceID string, generation uint64) pgx.Row {
	return externalRow{values: []any{deviceID, generation}}
}

func TestDisableEnrollmentWithTargetReportsBeginAndLockFailures(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("begin failure", func(t *testing.T) {
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "begin disable enrollment") {
			t.Fatalf("begin error = %v", err)
		}
	})

	t.Run("host lock failure", func(t *testing.T) {
		tx := &externalTx{execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			return pgconn.CommandTag{}, errStoreUnit
		}}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "lock host scope") {
			t.Fatalf("lock error = %v", err)
		}
		if !tx.rolledBack {
			t.Fatal("a lock failure must roll back")
		}
	})
}

func TestDisableEnrollmentWithTargetReportsEnrollmentReadFailures(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	for _, tc := range []struct {
		name    string
		row     pgx.Row
		wantErr error
		want    string
	}{
		{"missing enrollment", externalRow{err: pgx.ErrNoRows}, ErrNotFound, ""},
		{"storage failure", externalRow{err: errStoreUnit}, errStoreUnit, "load enrollment for disable"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &externalTx{}
			tx.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
				return pgconn.NewCommandTag("SELECT 1"), nil
			}
			tx.queryRowFn = func(context.Context, string, ...any) pgx.Row { return tc.row }
			db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
			_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
			if !errors.Is(err, tc.wantErr) || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("enrollment read error = %v", err)
			}
			if !tx.rolledBack {
				t.Fatal("an unreadable enrollment must roll back")
			}
		})
	}
}

func TestDisableEnrollmentWithTargetReplaysAFrozenRemoval(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	stored := removalRow{
		RemovalOperationID: "rem-1", HostID: "host-1", WorkspaceID: "ws-1",
		EnrollmentOperationID: "op-1", EnrollmentRevision: "1",
		DisabledAt: at, Revocations: map[string]uint64{"device-1": 4},
	}

	t.Run("stored removal load failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, nil)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "FROM external_removals") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "rem-1", nil, at)
		if !errors.Is(err, errStoreUnit) {
			t.Fatalf("removal load error = %v", err)
		}
	})

	t.Run("stored removal for another host is a target mismatch", func(t *testing.T) {
		other := stored
		other.HostID = "host-2"
		tx := scriptedDisableTx(t, nil)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "FROM external_removals") {
				return removalRowOf(other)
			}
			return base(ctx, sql, args...)
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "rem-1", nil, at); !errors.Is(err, ErrTargetMismatch) {
			t.Fatalf("replayed removal error = %v", err)
		}
		if tx.committed {
			t.Fatal("a mismatched replayed removal must not commit")
		}
	})

	t.Run("stored removal commit failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, nil)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "FROM external_removals") {
				return removalRowOf(stored)
			}
			return base(ctx, sql, args...)
		}
		tx.commitErr = errStoreUnit
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "rem-1", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit removal replay") {
			t.Fatalf("replay commit error = %v", err)
		}
	})
}

func TestDisableEnrollmentWithTargetFailsClosedWhileRevokingGrants(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("disable update failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, nil)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "UPDATE external_enrollments") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "disable enrollment") {
			t.Fatalf("disable update error = %v", err)
		}
	})

	t.Run("grant revocation query failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, nil)
		tx.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) {
			return nil, errStoreUnit
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "revoke disabled host grants") {
			t.Fatalf("revocation query error = %v", err)
		}
	})

	for _, tc := range []struct {
		name string
		rows *fakeRowSet
	}{
		{"revoked grant scan failure", &fakeRowSet{rows: []pgx.Row{revokedDeviceRow("device-1", 4), syntheticRow{err: errStoreUnit}}}},
		{"revoked grant cursor failure", &fakeRowSet{rows: []pgx.Row{revokedDeviceRow("device-1", 4)}, err: errStoreUnit}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := scriptedDisableTx(t, nil)
			tx.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) { return tc.rows, nil }
			db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
			_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
			if !errors.Is(err, errStoreUnit) {
				t.Fatalf("grant read error = %v", err)
			}
			if tc.rows.closed != 1 {
				t.Fatalf("grant cursor closed %d times", tc.rows.closed)
			}
		})
	}

	t.Run("credential deletion failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, []pgx.Row{revokedDeviceRow("device-1", 4)})
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "DELETE FROM device_credentials") {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "delete disabled host credentials") {
			t.Fatalf("credential deletion error = %v", err)
		}
	})

	t.Run("enrollment outcome write failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, nil)
		tx.execFn = func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
			if strings.Contains(sql, "UPDATE external_operations") {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return pgconn.NewCommandTag("UPDATE 1"), nil
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record enrollment disable") {
			t.Fatalf("outcome write error = %v", err)
		}
	})
}

func TestDisableEnrollmentWithTargetHandlesRemovalInsertConflicts(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	stored := removalRow{
		RemovalOperationID: "rem-1", HostID: "host-1", WorkspaceID: "ws-1",
		EnrollmentOperationID: "op-1", EnrollmentRevision: "1", DisabledAt: at,
	}

	// conflict scripts a concurrent insert: the removal INSERT returns no row
	// while every preceding statement still succeeds.
	conflict := func(t *testing.T, removal pgx.Row) *externalTx {
		t.Helper()
		tx := scriptedDisableTx(t, nil)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			switch {
			case strings.Contains(sql, "INSERT INTO external_removals"):
				return externalRow{err: pgx.ErrNoRows}
			case strings.Contains(sql, "FROM external_removals"):
				return removal
			default:
				return base(ctx, sql, args...)
			}
		}
		return tx
	}

	t.Run("conflicting removal cannot be loaded", func(t *testing.T) {
		tx := conflict(t, externalRow{err: errStoreUnit})
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) {
			t.Fatalf("conflict load error = %v", err)
		}
	})

	t.Run("conflicting removal belongs to another host", func(t *testing.T) {
		other := stored
		other.HostID = "host-2"
		tx := conflict(t, removalRowOf(other))
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		if _, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at); !errors.Is(err, ErrTargetMismatch) {
			t.Fatalf("conflict target error = %v", err)
		}
		if tx.committed {
			t.Fatal("a conflicting removal must not commit")
		}
	})

	t.Run("conflicting removal replay commit failure", func(t *testing.T) {
		tx := conflict(t, removalRowOf(stored))
		tx.commitErr = errStoreUnit
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit removal replay") {
			t.Fatalf("conflict commit error = %v", err)
		}
	})

	t.Run("conflicting removal replays the stored revocations", func(t *testing.T) {
		replayed := stored
		replayed.Revocations = map[string]uint64{"device-1": 4, "device-2": 7}
		tx := conflict(t, removalRowOf(replayed))
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		disabled, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if err != nil {
			t.Fatal(err)
		}
		if disabled.RemovalOperationID != "rem-1" || !disabled.DisabledAt.Equal(at) ||
			disabled.Revocations["device-1"] != 4 || disabled.Revocations["device-2"] != 7 {
			t.Fatalf("replayed removal = %+v", disabled)
		}
		if !tx.committed {
			t.Fatal("a replayed conflicting removal must commit")
		}
	})
}

func TestDisableEnrollmentWithTargetReportsRemovalWriteAndCommitFailures(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("removal insert failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, nil)
		base := tx.queryRowFn
		tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
			if strings.Contains(sql, "INSERT INTO external_removals") {
				return externalRow{err: errStoreUnit}
			}
			return base(ctx, sql, args...)
		}
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "record removal") {
			t.Fatalf("removal insert error = %v", err)
		}
	})

	t.Run("main commit failure", func(t *testing.T) {
		tx := scriptedDisableTx(t, nil)
		tx.commitErr = errStoreUnit
		db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
		_, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "", nil, at)
		if !errors.Is(err, errStoreUnit) || !strings.Contains(err.Error(), "commit disable enrollment") {
			t.Fatalf("disable commit error = %v", err)
		}
		if tx.committed {
			t.Fatal("a failed commit must not report success")
		}
	})
}

func TestDisableEnrollmentWithTargetDefaultsTheDisableTimestamp(t *testing.T) {
	var applied time.Time
	tx := scriptedDisableTx(t, nil)
	base := tx.queryRowFn
	tx.queryRowFn = func(ctx context.Context, sql string, args ...any) pgx.Row {
		if strings.Contains(sql, "UPDATE external_enrollments") {
			applied, _ = args[1].(time.Time)
		}
		return base(ctx, sql, args...)
	}
	db := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, nil }}
	disabled, err := newStore(db).DisableEnrollmentWithTarget(context.Background(), "host-1", "rem-1", nil, time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	if applied.IsZero() || !disabled.DisabledAt.Equal(applied) {
		t.Fatalf("defaulted disable timestamp applied=%v result=%v", applied, disabled.DisabledAt)
	}
	if disabled.HostID != "host-1" || disabled.RemovalOperationID != "rem-1" ||
		disabled.Target.WorkspaceID != "ws-1" || disabled.Target.EnrollmentOperationID != "op-1" {
		t.Fatalf("disable result = %+v", disabled)
	}
}
