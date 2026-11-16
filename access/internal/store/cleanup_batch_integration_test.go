//go:build integration

package store_test

import (
	"context"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// TestCleanupStatementsAreBoundedAndCatchUp seeds more rows than one batch so
// a long-pause backlog drains over repeated bounded passes instead of one
// unbounded statement.
func TestCleanupStatementsAreBoundedAndCatchUp(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	if _, err := s.Pool().Exec(ctx, `INSERT INTO hosts(host_id) VALUES ('cleanup-host')`); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Pool().Exec(ctx, `INSERT INTO devices(device_id, public_key_fingerprint) VALUES ('cleanup-device','')`); err != nil {
		t.Fatal(err)
	}
	// One more row than a batch, all expired with a non-empty body.
	if _, err := s.Pool().Exec(ctx, `
		INSERT INTO requests(request_id, host_id, device_id, kind, status, request_body, expires_at)
		SELECT 'cleanup-' || g, 'cleanup-host', 'cleanup-device', 1, 'received', 'body'::bytea, now() - interval '2 hours'
		FROM generate_series(1, $1) AS g`, 2001); err != nil {
		t.Fatal(err)
	}
	cutoff := time.Now().UTC().Add(time.Hour)

	cleared, err := s.ClearExpiredBodies(ctx, cutoff)
	if err != nil || cleared != 2000 {
		t.Fatalf("first body batch = %d err=%v", cleared, err)
	}
	cleared, err = s.ClearExpiredBodies(ctx, cutoff)
	if err != nil || cleared != 1 {
		t.Fatalf("second body batch = %d err=%v", cleared, err)
	}

	deleted, err := s.DeleteExpiredQueries(ctx, cutoff)
	if err != nil || deleted != 2000 {
		t.Fatalf("first query batch = %d err=%v", deleted, err)
	}
	deleted, err = s.DeleteExpiredQueries(ctx, cutoff)
	if err != nil || deleted != 1 {
		t.Fatalf("second query batch = %d err=%v", deleted, err)
	}
}
