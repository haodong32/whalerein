package store

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

func TestCloudDeviceGrantStatusDistinguishesAnEmptyAuthorizedScope(t *testing.T) {
	ready := "ready"
	for _, tc := range []struct {
		name     string
		state    *string
		database error
		expected error
	}{
		{"enforced grant", &ready, nil, nil},
		{"scoped absence", nil, nil, ErrCloudGrantMissing},
		{"unknown host scope", nil, pgx.ErrNoRows, ErrNotFound},
		{"database unavailable", nil, errStoreUnit, errStoreUnit},
	} {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, queryRowFn: func(_ context.Context, _ string, args ...any) pgx.Row {
				if !slices.Equal(args, []any{"workspace", "host", "device"}) {
					t.Fatal("query lost scope")
				}
				return syntheticRow{err: tc.database, fill: func(dest ...any) {
					*dest[0].(**string) = tc.state
				}}
			}}
			state, err := newStore(db).CloudDeviceGrantStatus(context.Background(), "workspace", "host", "device")
			if !errors.Is(err, tc.expected) || (tc.state != nil && state != *tc.state) {
				t.Fatalf("status = %q, error = %v", state, err)
			}
		})
	}
}

func TestHostClientIDsDistinguishesNoGrantsFromUnknownEnrollment(t *testing.T) {
	deviceRow := func(id *string) pgx.Row {
		return syntheticRow{fill: func(dest ...any) { *dest[0].(**string) = id }}
	}
	first, second := "device-a", "device-b"
	for _, tc := range []struct {
		name       string
		rows       []pgx.Row
		wanted     []string
		query, row error
		err        error
	}{
		{name: "two grants", rows: []pgx.Row{deviceRow(&first), deviceRow(&second)}, wanted: []string{first, second}},
		{name: "no grants", rows: []pgx.Row{deviceRow(nil)}},
		{name: "foreign or stale enrollment", err: ErrNotFound},
		{name: "scan error", rows: []pgx.Row{syntheticRow{err: errStoreUnit}}, err: errStoreUnit},
		{name: "iteration error", rows: []pgx.Row{deviceRow(&first)}, row: errStoreUnit, err: errStoreUnit},
		{name: "query error", query: errStoreUnit, err: errStoreUnit},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rows := &fakeRowSet{rows: tc.rows, err: tc.row}
			database := &fakeDB{t: t, queryFn: func(_ context.Context, _ string, args ...any) (pgx.Rows, error) {
				if !slices.Equal(args, []any{"workspace", "host", "3"}) {
					t.Fatalf("client list lost enrollment scope: %v", args)
				}
				return rows, tc.query
			}}
			ids, err := newStore(database).HostClientIDs(context.Background(), "workspace", "host", "3")
			if !slices.Equal(ids, tc.wanted) || !errors.Is(err, tc.err) {
				t.Fatalf("devices=%v error=%v", ids, err)
			}
			if tc.query == nil && rows.closed != 1 {
				t.Fatal("client rows stayed open")
			}
		})
	}
}

func TestCloudDeviceRevocationRecordsFenceUnderHostLock(t *testing.T) {
	now := time.Now().UTC()
	for _, tc := range []struct {
		name               string
		grant              bool
		begin, lock, scope error
		record, expected   error
		committed          bool
	}{
		{name: "no grant yet", committed: true},
		{name: "existing grant", grant: true, committed: true},
		{name: "unknown scope", scope: pgx.ErrNoRows, expected: ErrNotFound},
		{name: "database unavailable", begin: errStoreUnit, expected: errStoreUnit},
		{name: "lock unavailable", lock: errStoreUnit, expected: errStoreUnit},
		{name: "scope unavailable", scope: errStoreUnit, expected: errStoreUnit},
		{name: "write unavailable", record: errStoreUnit, expected: errStoreUnit},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var locked, checked, recorded bool
			tx := &scriptedTx{
				queryRowFn: func(_ context.Context, statement string, _ ...any) pgx.Row {
					switch {
					case strings.Contains(statement, "SELECT generation FROM external_enrollments"):
						if !locked {
							t.Fatal("enrollment scope was read without its Host lock")
						}
						checked = true
						return syntheticRow{err: tc.scope, fill: func(dest ...any) {
							*dest[0].(*int64) = 3
						}}
					case strings.Contains(statement, "FROM external_device_revocations"):
						if !recorded {
							t.Fatal("frozen revocation was read before it was recorded")
						}
						return syntheticRow{fill: func(dest ...any) {
							*dest[0].(*string) = "host"
							*dest[1].(*string) = "device"
							*dest[2].(*int64) = 3
							*dest[3].(*uint64) = 5
							*dest[4].(*uint64) = 2
							*dest[5].(*uint64) = 1
							*dest[9].(*bool) = tc.grant
						}}
					default:
						t.Fatalf("unexpected query %q", statement)
						return syntheticRow{err: errStoreUnit}
					}
				},
				execFn: func(_ context.Context, statement string, args ...any) (pgconn.CommandTag, error) {
					if strings.Contains(statement, "pg_advisory_xact_lock") {
						locked = true
						return pgconn.CommandTag{}, tc.lock
					}
					if !checked || !slices.Equal(args, []any{"host", "device", int64(3), now}) {
						t.Fatalf("revocation record lost its frozen scope: %v", args)
					}
					recorded = true
					return pgconn.CommandTag{}, tc.record
				},
			}
			database := &fakeDB{t: t, beginFn: func(context.Context) (pgx.Tx, error) { return tx, tc.begin }}
			grant, err := newStore(database).BeginCloudDeviceRevocation(context.Background(), "workspace", "host", "device", now)
			if !errors.Is(err, tc.expected) || tx.committed != tc.committed {
				t.Fatalf("grant = %+v, error = %v, committed = %v", grant, err, tx.committed)
			}
			if tc.committed && (grant.EnrollmentGeneration != 3 || grant.GrantGeneration != 5 || grant.Exists != tc.grant) {
				t.Fatalf("frozen receipt = %+v", grant)
			}
		})
	}
	database := &fakeDB{t: t, execFn: func(_ context.Context, _ string, args ...any) (pgconn.CommandTag, error) {
		if !slices.Equal(args, []any{"host", "device", now}) {
			t.Fatal("enforcement write lost its exact scope")
		}
		return updateTag(1), nil
	}}
	if err := newStore(database).CompleteCloudDeviceRevocation(context.Background(), "host", "device", now); err != nil {
		t.Fatal(err)
	}
}

func TestCloudPairingCommitScopeRejectsRevokedOrSupersededIntent(t *testing.T) {
	for _, tc := range []struct {
		name     string
		eligible bool
		database error
		expected error
	}{
		{"current enrollment", true, nil, nil},
		{"inactive scope", false, nil, ErrPairingFenced},
		{"database unavailable", false, errStoreUnit, errStoreUnit},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &scriptedTx{
				queryRowFn: func(_ context.Context, _ string, args ...any) pgx.Row {
					if !slices.Equal(args, []any{"host", "device", "workspace", "revision"}) {
						t.Fatal("commit query lost scope")
					}
					return syntheticRow{err: tc.database, fill: func(dest ...any) {
						*dest[0].(*bool) = tc.eligible
					}}
				},
			}
			op := &PairingOperation{CloudWorkspaceID: "workspace", CloudEnrollmentRevision: "revision"}
			if err := verifyCloudPairing(context.Background(), tx, op, "host", "device"); !errors.Is(err, tc.expected) {
				t.Fatalf("pairing eligibility error %v, want %v", err, tc.expected)
			}
		})
	}
}
