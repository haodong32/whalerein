//go:build acceptance

package acceptance

import (
	"context"
	"testing"
	"time"
)

// TestRequestMetadataRetentionPolicy pins the database-capacity policy:
//
//   - disposable query/handshake records (`command_id = ”`, kind 1/2) are
//     deleted in bounded batches after expiry, and expired bodies are cleared;
//   - command deduplication records (`command_id <> ”`) are deliberately
//     retained even after expiry. Deleting them would allow a restored,
//     retried or replayed command to execute twice, so unbounded growth of
//     those small metadata rows is the safe trade-off. See docs/deploy.md.
func TestRequestMetadataRetentionPolicy(t *testing.T) {
	s := newStack(t)
	queryID := uniqueID("accept-retention-query")
	commandRequest := uniqueID("accept-retention-command")
	commandID := uniqueID("accept-retention-command-id")
	// Query metadata is pruned only outside the 24-hour retention window, so
	// this row must be older than that cutoff.
	expired := time.Now().Add(-25 * time.Hour)
	insertRequest(t, s.db, s.hostID, s.devices[0], queryID, "", 2, "succeeded", expired, "none", []byte("query-body"), []byte("query-result"))
	insertRequest(t, s.db, s.hostID, s.devices[0], commandRequest, commandID, 3, "succeeded", expired, "none", []byte("command-body"), []byte("command-result"))
	t.Cleanup(func() {
		_, _ = s.db.Exec(context.Background(), "DELETE FROM requests WHERE request_id IN ($1,$2)", queryID, commandRequest)
	})

	// Access runs one cleanup pass at startup, so a restart avoids waiting for
	// the configured one-minute interval.
	restart(t, s.restartAccess)
	waitReady(t, s.accessURL)

	var queryRows int
	waitFor(t, 30*time.Second, "expired query metadata cleanup", func() bool {
		if err := s.db.QueryRow(context.Background(), "SELECT count(*) FROM requests WHERE request_id=$1", queryID).Scan(&queryRows); err != nil {
			return false
		}
		return queryRows == 0
	})

	var bodiesCleared bool
	var requestBody, resultBody string
	if err := s.db.QueryRow(context.Background(), `
		SELECT bodies_cleared, encode(request_body,'escape'), encode(result_body,'escape')
		FROM requests WHERE request_id=$1`, commandRequest).Scan(&bodiesCleared, &requestBody, &resultBody); err != nil {
		t.Fatalf("read retained command dedupe row: %v", err)
	}
	if !bodiesCleared || requestBody != "" || resultBody != "" {
		t.Fatalf("expired command bodies were not cleared: cleared=%v body=%q result=%q", bodiesCleared, requestBody, resultBody)
	}
	var commandRows int
	if err := s.db.QueryRow(context.Background(), "SELECT count(*) FROM requests WHERE command_id=$1", commandID).Scan(&commandRows); err != nil {
		t.Fatalf("count retained command records: %v", err)
	}
	if commandRows != 1 {
		t.Fatalf("command dedupe rows = %d, want the row retained exactly once", commandRows)
	}
}
