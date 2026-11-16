//go:build acceptance

package acceptance

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"
)

// TestPostgresRestartAndReadOnlyFence proves two operational properties:
//
//   - a PostgreSQL restart does not settle an in-flight request or clear its
//     durable body; Access resumes reading the same state after reconnect;
//   - when database writes fail (read-only storage), a revoke is reported as a
//     failure rather than accepted, and after writes recover the revocation
//     fence is persisted and survives an Access restart.
//
// The read-only database is a deterministic stand-in for a full disk or a
// failed write path. A real disk-full run remains an environment-specific
// limitation because PostgreSQL has no portable per-test disk quota.
func TestPostgresRestartAndReadOnlyFence(t *testing.T) {
	s := newStack(t)
	s.restartPostgres = requireConfig(t, "WHALEREIN_ACCEPTANCE_RESTART_POSTGRES", s.restartPostgres)
	device := s.devices[0]
	token := s.tokens[0]
	requestID := uniqueID("accept-inflight")
	inFlightBody := []byte("ciphertext-in-flight")
	insertRequest(t, s.db, s.hostID, device, requestID, "", 3, "host_accepted", time.Now().Add(time.Hour), "published", inFlightBody, nil)
	t.Cleanup(func() {
		_, _ = s.db.Exec(context.Background(), "DELETE FROM requests WHERE request_id=$1", requestID)
	})

	var initialStatus string
	if err := s.db.QueryRow(context.Background(), "SELECT status FROM requests WHERE request_id=$1", requestID).Scan(&initialStatus); err != nil {
		t.Fatalf("read request status: %v", err)
	}
	if initialStatus != "host_accepted" {
		t.Fatalf("request status before restart = %q", initialStatus)
	}

	t.Run("PostgreSQL restart keeps an in-flight request durable", func(t *testing.T) {
		restart(t, s.restartPostgres)
		waitDatabase(t, s.db)
		waitReady(t, s.accessURL)

		if state, status := requestState(t, s, token, requestID); status != http.StatusOK {
			t.Fatalf("request read after restart = %d", status)
		} else if state.GetStatus().String() != "host_accepted" {
			t.Fatalf("request status after restart = %s, want host_accepted (not falsely settled)", state.GetStatus())
		}
		var status, body string
		if err := s.db.QueryRow(context.Background(), "SELECT status, encode(request_body, 'escape') FROM requests WHERE request_id=$1", requestID).Scan(&status, &body); err != nil {
			t.Fatalf("read request after restart: %v", err)
		}
		if status != "host_accepted" {
			t.Fatalf("request status after restart = %q, want host_accepted (not falsely settled)", status)
		}
		if body != string(inFlightBody) {
			t.Fatalf("request body after restart = %q, want durable body", body)
		}
	})

	t.Run("read-only database rejects the revoke and keeps the fence", func(t *testing.T) {
		database := ""
		if err := s.db.QueryRow(context.Background(), "SELECT current_database()").Scan(&database); err != nil {
			t.Fatalf("current database: %v", err)
		}
		alter := func(statement string) {
			if _, err := s.db.Exec(context.Background(), statement); err != nil {
				t.Fatalf("database statement %q: %v", statement, err)
			}
		}
		alter(`ALTER DATABASE "` + strings.ReplaceAll(database, `"`, `""`) + `" SET default_transaction_read_only = on`)
		t.Cleanup(func() {
			// Best-effort recovery even when the test body failed.
			execBestEffort(s.db, `ALTER DATABASE "`+strings.ReplaceAll(database, `"`, `""`)+`" RESET default_transaction_read_only`)
			execBestEffort(s.db, `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid()`)
			runCommandBestEffort(s.restartAccess)
		})
		// Terminate pooled sessions so Access reconnects under the read-only
		// default; existing sessions keep their original mode. Do not restart
		// Access: a read-only database would also block startup migrations, so
		// that path would test startup failure instead of a failing write.
		terminateDatabaseSessions(t, s.db)
		waitReady(t, s.accessURL)

		status, body := s.request(t, http.MethodDelete, "/v1/devices/"+device, s.adminToken)
		if status == http.StatusNoContent {
			t.Fatal("revoke was accepted while the database rejected writes")
		}
		if status < 400 {
			t.Fatalf("revoke status %d with body %s, want an error", status, body)
		}
		var revokedAt *time.Time
		if err := s.db.QueryRow(context.Background(), "SELECT revoked_at FROM grants WHERE host_id=$1 AND device_id=$2", s.hostID, device).Scan(&revokedAt); err != nil {
			t.Fatalf("read grant after failed revoke: %v", err)
		}
		if revokedAt != nil {
			t.Fatal("failed revoke still recorded a revocation fence")
		}

		alter(`ALTER DATABASE "` + strings.ReplaceAll(database, `"`, `""`) + `" RESET default_transaction_read_only`)
		terminateDatabaseSessions(t, s.db)
		waitReady(t, s.accessURL)

		if status, body := s.request(t, http.MethodDelete, "/v1/devices/"+device, s.adminToken); status != http.StatusNoContent {
			t.Fatalf("revoke after recovery status %d: %s", status, body)
		}
		var generation int64
		var revoked bool
		if err := s.db.QueryRow(context.Background(), "SELECT revoked_at IS NOT NULL FROM grants WHERE host_id=$1 AND device_id=$2", s.hostID, device).Scan(&revoked); err != nil {
			t.Fatalf("read durable grant revocation: %v", err)
		}
		if !revoked {
			t.Fatal("successful revoke did not persist revoked_at")
		}
		if err := s.db.QueryRow(context.Background(), "SELECT revocation_generation FROM devices WHERE device_id=$1", device).Scan(&generation); err != nil {
			t.Fatalf("read device revocation generation: %v", err)
		}
		if generation < 1 {
			t.Fatalf("device revocation generation = %d, want a durable fence", generation)
		}

		// The fence survives an Access restart and still blocks the credential.
		// A repeated DELETE is idempotent; the durable state and the rejected
		// credential are the properties under test.
		restart(t, s.restartAccess)
		waitReady(t, s.accessURL)
		if err := tryDeviceConnect(s.mqttAddr, device, token); err == nil {
			t.Fatal("revoked device credential was still accepted after restart")
		}
		if err := s.db.QueryRow(context.Background(), "SELECT revocation_generation FROM devices WHERE device_id=$1", device).Scan(&generation); err != nil {
			t.Fatalf("read generation after restart: %v", err)
		}
		if generation < 1 {
			t.Fatalf("revocation generation after restart = %d, want a durable fence", generation)
		}
	})
}

// TestGracefulShutdownAndLogRotation proves the deployment stop contract:
// Access receives SIGTERM, shuts down inside the documented 10 s budget (well
// before Docker's SIGKILL), reports a clean exit, leaves in-flight durable state
// untouched, and never writes credentials to its container log. The container
// log configuration is bounded so logs cannot grow without limit.
func TestGracefulShutdownAndLogRotation(t *testing.T) {
	s := newStack(t)
	s.stopAccess = requireConfig(t, "WHALEREIN_ACCEPTANCE_STOP_ACCESS", s.stopAccess)
	s.startAccess = requireConfig(t, "WHALEREIN_ACCEPTANCE_START_ACCESS", s.startAccess)

	config := logConfig(t, s.accessContainer)
	if config["Type"] != "json-file" {
		t.Fatalf("log driver = %q, want json-file", config["Type"])
	}
	if config["max-size"] != "10m" || config["max-file"] != "3" {
		t.Fatalf("log rotation = max-size %q, max-file %q; want 10m/3", config["max-size"], config["max-file"])
	}

	requestID := uniqueID("accept-shutdown")
	insertRequest(t, s.db, s.hostID, s.devices[0], requestID, "", 3, "host_accepted", time.Now().Add(time.Hour), "published", []byte("durable-body"), nil)
	t.Cleanup(func() {
		_, _ = s.db.Exec(context.Background(), "DELETE FROM requests WHERE request_id=$1", requestID)
	})

	// Keep a client polling the in-flight request while SIGTERM arrives.
	polling := make(chan struct{})
	stopPolling := make(chan struct{})
	go func() {
		defer close(polling)
		client := &http.Client{Timeout: 2 * time.Second}
		for {
			select {
			case <-stopPolling:
				return
			default:
			}
			request, err := http.NewRequest(http.MethodGet, s.accessURL+"/v1/requests/"+requestID, nil)
			if err == nil {
				request.Header.Set("Authorization", "Bearer "+s.tokens[0])
				if response, err := client.Do(request); err == nil {
					response.Body.Close()
				}
			}
			time.Sleep(50 * time.Millisecond)
		}
	}()

	elapsed, err := runCommand(t, s.stopAccess)
	close(stopPolling)
	if err != nil {
		t.Fatalf("SIGTERM shutdown: %v", err)
	}
	<-polling
	if elapsed > 15*time.Second {
		t.Fatalf("SIGTERM shutdown took %s; the 10 s application budget was not honored", elapsed)
	}
	if exit := inspectContainer(t, s.accessContainer, "{{.State.ExitCode}}"); exit != "0" {
		t.Fatalf("Access exit code = %s, want 0 (137 means SIGKILL won the grace race)", exit)
	}
	logs := containerLogs(t, s.accessContainer)
	if !strings.Contains(logs, "access stopped") {
		t.Fatalf("shutdown log is missing the graceful stop record:\n%s", logs)
	}
	for _, secret := range []string{s.adminToken, s.hostToken, s.tokens[0], s.tokens[1], "dev-broker-password", "compose-internal-token"} {
		if secret != "" && strings.Contains(logs, secret) {
			t.Fatal("container log contains a credential")
		}
	}

	t.Cleanup(func() {
		runCommandBestEffort(s.startAccess)
	})
	restart(t, s.startAccess)
	waitReady(t, s.accessURL)

	var status, body string
	if err := s.db.QueryRow(context.Background(), "SELECT status, encode(request_body, 'escape') FROM requests WHERE request_id=$1", requestID).Scan(&status, &body); err != nil {
		t.Fatalf("read durable request: %v", err)
	}
	if status != "host_accepted" || body != "durable-body" {
		t.Fatalf("shutdown changed in-flight state: status=%q body=%q", status, body)
	}
}
