package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
)

func TestListAuthorizedHostsPageValidatesBoundsBeforeQuerying(t *testing.T) {
	for _, tc := range []struct {
		name          string
		limit, offset int
		wantMessage   string
	}{
		{"zero limit", 0, 0, "limit must be positive"},
		{"negative limit", -3, 0, "limit must be positive"},
		{"negative offset", 5, -1, "offset must not be negative"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
				t.Fatal("an invalid page must not reach the database")
				return nil, nil
			}}
			hosts, err := newStore(db).ListAuthorizedHostsPage(context.Background(), "device", tc.limit, tc.offset)
			if err == nil || !strings.Contains(err.Error(), tc.wantMessage) {
				t.Fatalf("bounds error = %v", err)
			}
			if hosts != nil {
				t.Fatalf("invalid page returned hosts %v", hosts)
			}
		})
	}
}

func TestListAuthorizedHostsPagePreservesOrderAndForwardsTheWindow(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	rows := &fakeRowSet{rows: []pgx.Row{
		hostRow("host-a", "fingerprint-a", 2, at),
		hostRow("host-b", "fingerprint-b", 1, at.Add(time.Minute)),
	}}
	var gotLimit, gotOffset int
	db := &fakeDB{t: t, queryFn: func(_ context.Context, sql string, args ...any) (pgx.Rows, error) {
		if !strings.Contains(sql, "LIMIT $2 OFFSET $3") {
			t.Fatalf("page query lost its window: %q", sql)
		}
		if len(args) != 3 || args[0] != "device" {
			t.Fatalf("page query args = %v", args)
		}
		gotLimit, _ = args[1].(int)
		gotOffset, _ = args[2].(int)
		return rows, nil
	}}
	hosts, err := newStore(db).ListAuthorizedHostsPage(context.Background(), "device", 2, 7)
	if err != nil {
		t.Fatal(err)
	}
	if len(hosts) != 2 || hosts[0].HostID != "host-a" || hosts[1].HostID != "host-b" {
		t.Fatalf("page order = %+v", hosts)
	}
	if hosts[0].Fingerprint != "fingerprint-a" || hosts[0].ProtocolVersion != 2 ||
		hosts[1].ProtocolVersion != 1 || !hosts[1].CreatedAt.Equal(at.Add(time.Minute)) {
		t.Fatalf("page rows = %+v", hosts)
	}
	if gotLimit != 2 || gotOffset != 7 {
		t.Fatalf("forwarded window limit=%d offset=%d", gotLimit, gotOffset)
	}
	if rows.closed != 1 {
		t.Fatalf("page cursor closed %d times", rows.closed)
	}
}

func TestListAuthorizedHostsPageReturnsAnEmptyPageForNoRows(t *testing.T) {
	rows := &fakeRowSet{}
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }}
	hosts, err := newStore(db).ListAuthorizedHostsPage(context.Background(), "device", 10, 0)
	if err != nil || hosts == nil || len(hosts) != 0 {
		t.Fatalf("empty page = %v err=%v", hosts, err)
	}
	if rows.closed != 1 {
		t.Fatalf("empty page cursor closed %d times", rows.closed)
	}
}

func TestListAuthorizedHostsPageFailsClosed(t *testing.T) {
	for _, tc := range []struct {
		name       string
		queryErr   error
		rows       *fakeRowSet
		wantClosed int
	}{
		{"query failure", errStoreUnit, nil, 0},
		{"scan failure", nil, &fakeRowSet{rows: []pgx.Row{
			hostRow("host-a", "", 1, time.Now().UTC()), syntheticRow{err: errStoreUnit},
		}}, 1},
		{"cursor failure", nil, &fakeRowSet{rows: []pgx.Row{hostRow("host-a", "", 1, time.Now().UTC())}, err: errStoreUnit}, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
				return tc.rows, tc.queryErr
			}}
			if _, err := newStore(db).ListAuthorizedHostsPage(context.Background(), "device", 5, 0); !errors.Is(err, errStoreUnit) {
				t.Fatalf("page error = %v", err)
			}
			if tc.rows != nil && tc.rows.closed != tc.wantClosed {
				t.Fatalf("cursor closed %d times", tc.rows.closed)
			}
		})
	}
}

func TestListDeviceGrantsFailsClosed(t *testing.T) {
	grantRow := syntheticRow{fill: func(dest ...any) {
		*dest[0].(*string) = "host-a"
		*dest[1].(*string) = "device"
		*dest[2].(*bool) = true
		*dest[3].(*uint64) = 2
		*dest[4].(*uint64) = 3
		*dest[5].(*uint64) = 4
	}}

	t.Run("query failure", func(t *testing.T) {
		db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
			return nil, errStoreUnit
		}}
		if _, err := newStore(db).ListDeviceGrants(context.Background(), "device"); !errors.Is(err, errStoreUnit) {
			t.Fatalf("list error = %v", err)
		}
	})

	for _, tc := range []struct {
		name string
		rows *fakeRowSet
	}{
		{"scan failure", &fakeRowSet{rows: []pgx.Row{syntheticRow{err: errStoreUnit}}}},
		{"cursor failure", &fakeRowSet{rows: []pgx.Row{grantRow}, err: errStoreUnit}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return tc.rows, nil }}
			if _, err := newStore(db).ListDeviceGrants(context.Background(), "device"); !errors.Is(err, errStoreUnit) {
				t.Fatalf("list error = %v", err)
			}
			if tc.rows.closed != 1 {
				t.Fatalf("cursor closed %d times", tc.rows.closed)
			}
		})
	}
}

func TestListAuthorizedHostIDsPropagatesListingFailures(t *testing.T) {
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return nil, errStoreUnit
	}}
	if _, err := newStore(db).ListAuthorizedHostIDs(context.Background(), "device"); !errors.Is(err, errStoreUnit) {
		t.Fatalf("id listing error = %v", err)
	}

	rows := &fakeRowSet{rows: []pgx.Row{hostRow("host-a", "", 1, time.Now().UTC()), hostRow("host-b", "", 1, time.Now().UTC())}}
	db = &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }}
	ids, err := newStore(db).ListAuthorizedHostIDs(context.Background(), "device")
	if err != nil || len(ids) != 2 || ids[0] != "host-a" || ids[1] != "host-b" {
		t.Fatalf("ids = %v err=%v", ids, err)
	}
}
