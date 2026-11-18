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

// TestSessionObservationAC29RenewIsAnExactConditionalUpdate locks the SQL
// predicates that make renewal exact: one device/request pair, live and
// unexpired state, never past the original deadline.
func TestSessionObservationAC29RenewIsAnExactConditionalUpdate(t *testing.T) {
	for _, tc := range []struct {
		name    string
		rows    int64
		want    bool
		execErr error
		wantErr bool
	}{
		{"extended", 1, true, nil, false},
		{"rejected", 0, false, nil, false},
		{"database failure", 0, false, errStoreUnit, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			calls := 0
			db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
				calls++
				for _, fragment := range []string{
					"request_id = $1 AND device_id = $2",
					"client_lease_state = 'live'",
					"status IN ('received','host_accepted')",
					"expires_at > clock_timestamp()",
					"client_lease_until IS NULL OR client_lease_until > clock_timestamp()",
					"LEAST(expires_at",
				} {
					if !strings.Contains(sql, fragment) {
						t.Fatalf("renewal SQL lost %q: %s", fragment, sql)
					}
				}
				if len(args) != 4 || args[0] != "r1" || args[1] != "d1" || args[2] != 10*time.Second || args[3] != "" {
					t.Fatalf("renewal args = %#v", args)
				}
				return updateTag(tc.rows), tc.execErr
			}}
			renewed, err := newStore(db).RenewClientLease(context.Background(), "d1", "r1", 10*time.Second)
			if (err != nil) != tc.wantErr || renewed != tc.want || calls != 1 {
				t.Fatalf("renewed=%v err=%v calls=%d", renewed, err, calls)
			}
			if tc.wantErr && !errors.Is(err, errStoreUnit) {
				t.Fatalf("renewal error = %v", err)
			}
		})
	}
}

func TestSessionObservationAC29RenewRejectsInvalidScopeBeforeDatabase(t *testing.T) {
	calls := 0
	db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		calls++
		return updateTag(1), nil
	}}
	s := newStore(db)
	for _, tc := range []struct {
		device, request string
		lease           time.Duration
	}{
		{"", "r1", time.Second},
		{"d1", "", time.Second},
		{"d1", "r1", 0},
		{"d1", "r1", -time.Second},
	} {
		if renewed, err := s.RenewClientLease(context.Background(), tc.device, tc.request, tc.lease); err == nil || renewed {
			t.Fatalf("invalid scope accepted: %+v", tc)
		}
	}
	if calls != 0 {
		t.Fatal("invalid scope reached the database")
	}
}

func TestSessionObservationAC30ExpiryAndReplayAreBoundedQueries(t *testing.T) {
	at := time.Unix(123, 0).UTC()
	rows := &fakeRowSet{rows: []pgx.Row{requestFixtureRow([]byte{1}), requestFixtureRow([]byte{2})}}
	db := &fakeDB{t: t, queryFn: func(_ context.Context, sql string, args ...any) (pgx.Rows, error) {
		if !strings.Contains(sql, "FOR UPDATE SKIP LOCKED") ||
			!strings.Contains(sql, "client_lease_until <= clock_timestamp()") ||
			!strings.Contains(sql, "cancel_pending = true") {
			t.Fatalf("expiry SQL = %s", sql)
		}
		if len(args) != 2 || args[0] != 7 || args[1] != "" {
			t.Fatalf("expiry limit args = %#v", args)
		}
		return rows, nil
	}}
	expired, err := newStore(db).ExpireClientLeases(context.Background(), 7)
	if err != nil || len(expired) != 2 || rows.closed != 1 || !expired[0].ExpiresAt.Equal(at) {
		t.Fatalf("expired=%d err=%v closed=%d", len(expired), err, rows.closed)
	}
	if expired[0].ClientLeaseState != ClientLeaseLive {
		t.Fatalf("row fixture state = %q", expired[0].ClientLeaseState)
	}

	if got, err := newStore(&fakeDB{t: t}).ExpireClientLeases(context.Background(), 0); err != nil || got != nil {
		t.Fatal("zero limit should not query")
	}
	if got, err := newStore(&fakeDB{t: t}).PendingCancelRequests(context.Background(), 0); err != nil || got != nil {
		t.Fatal("zero limit should not query")
	}
}

func TestSessionObservationAC30ExpiryAndReplayQueriesReportErrors(t *testing.T) {
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return nil, errStoreUnit
	}}
	s := newStore(db)
	if _, err := s.ExpireClientLeases(context.Background(), 1); !errors.Is(err, errStoreUnit) {
		t.Fatalf("expiry error = %v", err)
	}
	if _, err := s.PendingCancelRequests(context.Background(), 1); !errors.Is(err, errStoreUnit) {
		t.Fatalf("pending error = %v", err)
	}
}

func TestSessionObservationAC35CancelBookkeepingIsMonotonic(t *testing.T) {
	for _, tc := range []struct {
		name string
		run  func(*Store) error
		sql  string
	}{
		{"published", func(s *Store) error { return s.MarkCancelPublished(context.Background(), "r1") }, "cancel_published_at = clock_timestamp()"},
		{"attempt", func(s *Store) error { return s.MarkCancelAttempt(context.Background(), "r1") }, "cancel_attempts = cancel_attempts + 1"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			calls := 0
			db := &fakeDB{t: t, execFn: func(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
				calls++
				if !strings.Contains(sql, tc.sql) || !strings.Contains(sql, "client_lease_state = 'expired'") || !strings.Contains(sql, "cancel_pending") {
					t.Fatalf("bookkeeping SQL = %s", sql)
				}
				if len(args) != 1 || args[0] != "r1" {
					t.Fatalf("bookkeeping args = %#v", args)
				}
				return updateTag(1), nil
			}}
			if err := tc.run(newStore(db)); err != nil || calls != 1 {
				t.Fatalf("err=%v calls=%d", err, calls)
			}
		})
	}
}

func TestSessionObservationAC30PendingCancellationQueryReturnsBoundedRows(t *testing.T) {
	rows := &fakeRowSet{rows: []pgx.Row{requestFixtureRow([]byte{1})}}
	db := &fakeDB{t: t, queryFn: func(_ context.Context, sql string, args ...any) (pgx.Rows, error) {
		if !strings.Contains(sql, "cancel_pending AND client_lease_state = 'expired'") || !strings.Contains(sql, "LIMIT $1") {
			t.Fatalf("pending SQL = %s", sql)
		}
		if len(args) != 1 || args[0] != 4 {
			t.Fatalf("pending args = %#v", args)
		}
		return rows, nil
	}}
	pending, err := newStore(db).PendingCancelRequests(context.Background(), 4)
	if err != nil || len(pending) != 1 || pending[0].RequestID != "request" || rows.closed != 1 {
		t.Fatalf("pending=%+v err=%v closed=%d", pending, err, rows.closed)
	}
}

func TestSessionObservationAC30ExpiryRejectsInvalidRows(t *testing.T) {
	for _, tc := range []struct {
		name string
		rows *fakeRowSet
	}{
		{"scan failure", &fakeRowSet{rows: []pgx.Row{syntheticRow{err: errStoreUnit}}}},
		{"cursor failure", &fakeRowSet{rows: []pgx.Row{requestFixtureRow([]byte{1})}, err: errStoreUnit}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return tc.rows, nil }}
			if _, err := newStore(db).ExpireClientLeases(context.Background(), 1); !errors.Is(err, errStoreUnit) {
				t.Fatalf("expiry error = %v", err)
			}
		})
	}
	for _, tc := range []struct {
		name string
		rows *fakeRowSet
	}{
		{"scan failure", &fakeRowSet{rows: []pgx.Row{syntheticRow{err: errStoreUnit}}}},
		{"cursor failure", &fakeRowSet{rows: []pgx.Row{requestFixtureRow([]byte{1})}, err: errStoreUnit}},
	} {
		t.Run("pending "+tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return tc.rows, nil }}
			if _, err := newStore(db).PendingCancelRequests(context.Background(), 1); !errors.Is(err, errStoreUnit) {
				t.Fatalf("pending error = %v", err)
			}
		})
	}
}

func TestSessionObservationAC35CancelBookkeepingErrorsAreReported(t *testing.T) {
	for _, tc := range []struct {
		name string
		run  func(*Store) error
	}{
		{"published", func(s *Store) error { return s.MarkCancelPublished(context.Background(), "r1") }},
		{"attempt", func(s *Store) error { return s.MarkCancelAttempt(context.Background(), "r1") }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
				return pgconn.CommandTag{}, errStoreUnit
			}}
			if err := tc.run(newStore(db)); !errors.Is(err, errStoreUnit) {
				t.Fatalf("bookkeeping error = %v", err)
			}
		})
	}
}
