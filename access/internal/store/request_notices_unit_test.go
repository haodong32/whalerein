package store

import (
	"context"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestHeartbeatRequestRejectsInvalidScope(t *testing.T) {
	s := newStore(&fakeDB{t: t})
	for _, tc := range []struct {
		device, request string
		lease           time.Duration
	}{
		{"", "r", time.Second},
		{"d", "", time.Second},
		{"d", "r", 0},
	} {
		if _, err := s.HeartbeatRequest(context.Background(), tc.device, "", tc.request, tc.lease); err == nil {
			t.Fatalf("accepted invalid heartbeat scope %+v", tc)
		}
	}
}

func TestNoticePublicationBookkeeping(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return updateTag(1), nil
	}}
	s := newStore(db)
	published, err := s.MarkNoticePublished(context.Background(), "r1", whalereinv1.OnlineStatus_succeeded, now, now)
	if err != nil || !published {
		t.Fatalf("published=%v err=%v", published, err)
	}
	if err := s.MarkNoticeFailure(context.Background(), "r1", now, now.Add(time.Second)); err != nil {
		t.Fatalf("mark failure: %v", err)
	}
	db.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) { return updateTag(0), nil }
	if published, err := s.MarkNoticePublished(context.Background(), "r1", whalereinv1.OnlineStatus_succeeded, now, now); err != nil || published {
		t.Fatalf("stale publication published=%v err=%v", published, err)
	}
	db.execFn = func(context.Context, string, ...any) (pgconn.CommandTag, error) {
		return pgconn.CommandTag{}, context.Canceled
	}
	if _, err := s.MarkNoticePublished(context.Background(), "r1", whalereinv1.OnlineStatus_succeeded, now, now); err == nil {
		t.Fatal("notice publication error not reported")
	}
	if err := s.MarkNoticeFailure(context.Background(), "r1", now, now); err == nil {
		t.Fatal("notice failure error not reported")
	}
}

func TestNoticeStatsReadsBoundedSnapshot(t *testing.T) {
	db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
		return syntheticRow{fill: func(dest ...any) {
			*dest[0].(*int) = 3
			*dest[1].(*int64) = 7
			*dest[2].(*float64) = 12.5
			*dest[3].(*int64) = 4
		}}
	}}
	stats, err := newStore(db).NoticeStats(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if stats.Pending != 3 || stats.Attempts != 7 || stats.OldestAge != 12500*time.Millisecond || stats.PublishedTotal != 4 {
		t.Fatalf("stats=%+v", stats)
	}
	db.queryRowFn = func(context.Context, string, ...any) pgx.Row { return scanErrorRow{} }
	if _, err := newStore(db).NoticeStats(context.Background()); err == nil {
		t.Fatal("notice stats error not reported")
	}
}

func TestClaimNoticeIntentsZeroAndQueryError(t *testing.T) {
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return &fakeRowSet{}, nil
	}}
	s := newStore(db)
	rows, err := s.ClaimNoticeIntents(context.Background(), time.Now(), 0, time.Second)
	if err != nil || rows != nil {
		t.Fatalf("zero limit rows=%v err=%v", rows, err)
	}
	if rows, err := s.ClaimNoticeIntents(context.Background(), time.Now(), 2, time.Second); err != nil || len(rows) != 0 {
		t.Fatalf("empty rows=%v err=%v", rows, err)
	}
	db.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) { return nil, context.Canceled }
	if _, err := s.ClaimNoticeIntents(context.Background(), time.Now(), 2, time.Second); err == nil {
		t.Fatal("claim error not reported")
	}
}

func TestStalePayloadScopesZeroLimit(t *testing.T) {
	s := newStore(&fakeDB{t: t})
	if objects, err := s.StalePayloadScopes(context.Background(), 0); err != nil || objects != nil {
		t.Fatalf("zero limit objects=%v err=%v", objects, err)
	}
}

type scanErrorRow struct{}

func (scanErrorRow) Scan(...any) error { return context.DeadlineExceeded }

func TestDeviceSessionContextHelpers(t *testing.T) {
	ctx := WithRequestDeviceSession(context.Background(), "session")
	_ = WithRequestDeviceSession(ctx, "")
	ctx = WithDeviceSessionAuthority(ctx, "authority")
	if servingDeviceSessionAuthority(ctx) != "authority" {
		t.Fatal("device session authority not stored")
	}
}

// genericRow fills a full Request scan with zero-like values. It lets the
// notice-claim cursor be exercised without a database or column-by-column fake.
type genericRow struct{ now time.Time }

func (r genericRow) Scan(dest ...any) error {
	for index, field := range dest {
		switch value := field.(type) {
		case *string:
			if index == 0 {
				*value = "r1"
			} else {
				*value = "value"
			}
		case *int32:
			*value = 0
		case *int:
			*value = 0
		case *int64:
			*value = 0
		case *uint64:
			*value = 0
		case *bool:
			*value = false
		case *[]byte:
			*value = nil
		case *time.Time:
			*value = r.now
		case **time.Time:
			*value = nil
		case *float64:
			*value = 0
		default:
			return context.Canceled
		}
	}
	return nil
}

func TestListAuthorizedHostsPageValidatesAndPages(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
		return &fakeRowSet{rows: []pgx.Row{hostRow("host-1", "fp", 1, now)}}, nil
	}}
	s := newStore(db)
	if _, err := s.ListAuthorizedHostsPage(context.Background(), "d", 0, 0); err == nil {
		t.Fatal("zero limit accepted")
	}
	if _, err := s.ListAuthorizedHostsPage(context.Background(), "d", 1, -1); err == nil {
		t.Fatal("negative offset accepted")
	}
	hosts, err := s.ListAuthorizedHostsPage(context.Background(), "d", 1, 0)
	if err != nil || len(hosts) != 1 || hosts[0].HostID != "host-1" {
		t.Fatalf("hosts=%+v err=%v", hosts, err)
	}
	db.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) { return nil, context.Canceled }
	if _, err := newStore(db).ListAuthorizedHostsPage(context.Background(), "d", 1, 0); err == nil {
		t.Fatal("query error not reported")
	}
	db.queryFn = func(context.Context, string, ...any) (pgx.Rows, error) {
		return &fakeRowSet{rows: []pgx.Row{hostRow("host-1", "fp", 1, now)}, err: context.DeadlineExceeded}, nil
	}
	if _, err := newStore(db).ListAuthorizedHostsPage(context.Background(), "d", 1, 0); err == nil {
		t.Fatal("cursor error not reported")
	}
}
