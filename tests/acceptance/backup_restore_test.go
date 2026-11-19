//go:build acceptance

package acceptance

import (
	"bytes"
	"context"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

// The documented historical-restore invalidation from deploy/README.md. It
// runs only against the restored scratch database.
const restoreInvalidationSQL = `
BEGIN;
UPDATE grants SET revoked_at = COALESCE(revoked_at, now()),
                  replayed_at = NULL, updated_at = now();
DELETE FROM device_credentials;
DELETE FROM pairing_operations;
UPDATE pairing_tokens SET used_at = COALESCE(used_at, now()), updated_at = now();
UPDATE requests SET forward_state = 'none',
    status = CASE WHEN status IN ('received', 'host_accepted') THEN 'unknown' ELSE status END,
    updated_at = now();
COMMIT;`

// TestBackupRestoreCannotResurrectGrantOrDuplicateCommand restores a dump of
// the live installation into an isolated database and applies the documented
// post-restore invalidation. It proves that:
//
//   - an unmodified historical restore really does contain an active old grant
//     and the completed command record (the hazard is real);
//   - after invalidation no grant or credential can authorize the old device;
//   - the completed command deduplication record is preserved, so restoring the
//     backup cannot make the same command execute twice.
func TestBackupRestoreCannotResurrectGrantOrDuplicateCommand(t *testing.T) {
	s := newStack(t)
	commandID := uniqueID("accept-restore-command")
	commandRequest := uniqueID("accept-restore-command-row")
	inFlightRequest := uniqueID("accept-restore-inflight")
	insertRequest(t, s.db, s.hostID, s.devices[0], commandRequest, commandID, 3, "succeeded", time.Now().Add(time.Hour), "none", []byte("command-body"), []byte("result-body"))
	insertRequest(t, s.db, s.hostID, s.devices[0], inFlightRequest, "", 3, "host_accepted", time.Now().Add(time.Hour), "published", []byte("in-flight-body"), nil)
	t.Cleanup(func() {
		_, _ = s.db.Exec(context.Background(), "DELETE FROM requests WHERE request_id IN ($1,$2)", commandRequest, inFlightRequest)
	})

	restored, _ := scratchRestore(t, s)
	ctx := context.Background()

	// The hazard: an untouched restore still authorizes the old device and
	// still contains the completed command record.
	var activeGrants, credentials, commands int
	if err := restored.QueryRow(ctx, "SELECT count(*) FROM grants WHERE host_id=$1 AND device_id=$2 AND revoked_at IS NULL", s.hostID, s.devices[0]).Scan(&activeGrants); err != nil {
		t.Fatalf("count restored grants: %v", err)
	}
	if activeGrants != 1 {
		t.Fatalf("untouched restore active grants = %d, want 1 (proving the hazard)", activeGrants)
	}
	if err := restored.QueryRow(ctx, "SELECT count(*) FROM device_credentials WHERE device_id=$1", s.devices[0]).Scan(&credentials); err != nil {
		t.Fatalf("count restored credentials: %v", err)
	}
	if credentials == 0 {
		t.Fatal("untouched restore lost the old credential; the test would not prove anything")
	}
	if err := restored.QueryRow(ctx, "SELECT count(*) FROM requests WHERE command_id=$1", commandID).Scan(&commands); err != nil {
		t.Fatalf("count restored commands: %v", err)
	}
	if commands != 1 {
		t.Fatalf("untouched restore command rows = %d, want 1", commands)
	}

	if _, err := restored.Exec(ctx, restoreInvalidationSQL); err != nil {
		t.Fatalf("apply documented restore invalidation: %v", err)
	}

	if err := restored.QueryRow(ctx, "SELECT count(*) FROM grants WHERE host_id=$1 AND device_id=$2 AND revoked_at IS NULL", s.hostID, s.devices[0]).Scan(&activeGrants); err != nil {
		t.Fatalf("count invalidated grants: %v", err)
	}
	if activeGrants != 0 {
		t.Fatalf("invalidated restore still has %d active grants", activeGrants)
	}
	if err := restored.QueryRow(ctx, "SELECT count(*) FROM device_credentials").Scan(&credentials); err != nil {
		t.Fatalf("count invalidated credentials: %v", err)
	}
	if credentials != 0 {
		t.Fatalf("invalidated restore kept %d device credentials", credentials)
	}
	var pairingOperations int
	if err := restored.QueryRow(ctx, "SELECT count(*) FROM pairing_operations").Scan(&pairingOperations); err != nil {
		t.Fatalf("count pairing operations: %v", err)
	}
	if pairingOperations != 0 {
		t.Fatalf("invalidated restore kept %d pairing operations", pairingOperations)
	}
	var status, forwardState string
	if err := restored.QueryRow(ctx, "SELECT status, forward_state FROM requests WHERE request_id=$1", inFlightRequest).Scan(&status, &forwardState); err != nil {
		t.Fatalf("read restored in-flight request: %v", err)
	}
	if status != "unknown" || forwardState != "none" {
		t.Fatalf("restored in-flight request = %s/%s, want unknown/none (never falsely settled or forwarded)", status, forwardState)
	}

	// The completed command dedupe record survives invalidation and still
	// rejects a duplicate command id, so a restore cannot re-execute it.
	if err := restored.QueryRow(ctx, "SELECT status FROM requests WHERE command_id=$1", commandID).Scan(&status); err != nil {
		t.Fatalf("read restored command: %v", err)
	}
	if status != "succeeded" {
		t.Fatalf("restored command status = %q, want succeeded", status)
	}
	if _, err := restored.Exec(ctx, `
		INSERT INTO requests (request_id, host_id, device_id, command_id, kind, status, expires_at)
		VALUES ($1,$2,$3,$4,3,'received',now() + interval '1 hour')`,
		uniqueID("accept-restore-duplicate"), s.hostID, s.devices[0], commandID); err == nil {
		t.Fatal("restored command record allowed a duplicate command id")
	}
}

func scratchRestore(t *testing.T, s *stack) (*pgxpool.Pool, string) {
	t.Helper()
	scratch := strings.ReplaceAll(uniqueID("whalerein_restore"), "-", "_")
	create := exec.Command("docker", "exec", s.postgresContainer, "createdb", "-U", "postgres", scratch)
	if output, err := create.CombinedOutput(); err != nil {
		t.Fatalf("create scratch database: %v (%s)", err, output)
	}
	t.Cleanup(func() {
		_ = exec.Command("docker", "exec", s.postgresContainer, "dropdb", "-U", "postgres", "--force", scratch).Run()
	})

	dump, err := exec.Command("docker", "exec", s.postgresContainer, "pg_dump", "-U", "postgres", "-Fc", s.db.Config().ConnConfig.Database).Output()
	if err != nil {
		t.Fatalf("pg_dump: %v", err)
	}
	if len(dump) == 0 {
		t.Fatal("pg_dump produced no bytes")
	}
	restore := exec.Command("docker", "exec", "-i", s.postgresContainer, "pg_restore", "-U", "postgres", "-d", scratch, "--no-owner", "--no-privileges")
	restore.Stdin = bytes.NewReader(dump)
	if output, err := restore.CombinedOutput(); err != nil {
		t.Fatalf("pg_restore: %v (%s)", err, output)
	}

	ctx := context.Background()
	restored, err := pgxpool.New(ctx, databaseURLFor(scratch))
	if err != nil {
		t.Fatalf("open restored database: %v", err)
	}
	t.Cleanup(restored.Close)
	return restored, scratch
}
