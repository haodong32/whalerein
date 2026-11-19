//go:build acceptance

package acceptance

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// cutoverSourceURL is the URL the disposable PostgreSQL container uses to
// reach the installation whose dump represents the stopped old stack. Docker
// Desktop routes host.docker.internal to the host, so a loopback acceptance URL
// is reachable from inside the container.
func cutoverSourceURL(t *testing.T) string {
	t.Helper()
	raw := os.Getenv("WHALEREIN_ACCEPTANCE_DATABASE_URL")
	if raw == "" {
		raw = "postgres://postgres@127.0.0.1:55432/whalerein_dev?sslmode=disable"
	}
	parsed, err := url.Parse(raw)
	if err != nil {
		t.Fatalf("parse acceptance database URL: %v", err)
	}
	switch parsed.Hostname() {
	case "127.0.0.1", "localhost", "::1":
		parsed.Host = net.JoinHostPort("host.docker.internal", parsed.Port())
	}
	return parsed.String()
}

// startCutoverPostgres starts one disposable PostgreSQL container and restores
// a dump of the configured installation into it. The dump is a consistent
// snapshot; the old stack is not asked to serve this test again. Returning a
// stable database URL lets the current Access build apply its missing
// migrations to retained state exactly as the documented stopped-client
// cutover does.
func startCutoverPostgres(t *testing.T, s *stack) string {
	t.Helper()
	image := strings.TrimSpace(os.Getenv("WHALEREIN_ACCEPTANCE_POSTGRES_IMAGE"))
	if image == "" {
		// Prefer the image the Compose stack already pulled; the generic
		// tag is the offline-development fallback.
		for _, candidate := range []string{"postgres:18.7-alpine3.24", "postgres:18-alpine"} {
			if err := exec.Command("docker", "image", "inspect", candidate).Run(); err == nil {
				image = candidate
				break
			}
		}
	}
	if image == "" {
		t.Fatal("a PostgreSQL 18 image is required for the cutover fixture; set WHALEREIN_ACCEPTANCE_POSTGRES_IMAGE")
	}
	if output, err := exec.Command("docker", "image", "inspect", image).CombinedOutput(); err != nil {
		t.Fatalf("cutover PostgreSQL image %q is required (set WHALEREIN_ACCEPTANCE_POSTGRES_IMAGE): %v (%s)", image, err, output)
	}

	port := freeLoopbackPort(t)
	container := uniqueID("whalerein-cutover-postgres")
	scratch := strings.ReplaceAll(uniqueID("whalerein_cutover"), "-", "_")
	run := exec.Command("docker", "run", "-d",
		"--name", container,
		"--add-host=host.docker.internal:host-gateway",
		"-p", fmt.Sprintf("127.0.0.1:%d:5432", port),
		"-e", "POSTGRES_HOST_AUTH_METHOD=trust",
		image)
	if output, err := run.CombinedOutput(); err != nil {
		t.Fatalf("start cutover PostgreSQL: %v (%s)", err, output)
	}
	t.Cleanup(func() { _ = exec.Command("docker", "rm", "-f", container).Run() })

	execIn := func(args ...string) ([]byte, error) {
		command := exec.Command("docker", append([]string{"exec", container}, args...)...)
		return command.CombinedOutput()
	}
	waitFor(t, 60*time.Second, "cutover PostgreSQL listener", func() bool {
		conn, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", port), time.Second)
		if err != nil {
			return false
		}
		_ = conn.Close()
		return true
	})
	deadline := time.Now().Add(60 * time.Second)
	for {
		if output, err := execIn("pg_isready", "-U", "postgres"); err == nil {
			break
		} else if time.Now().After(deadline) {
			t.Fatalf("cutover PostgreSQL never became ready: %v (%s)", err, output)
		}
		time.Sleep(200 * time.Millisecond)
	}

	if output, err := execIn("createdb", "-U", "postgres", scratch); err != nil {
		t.Fatalf("create cutover database: %v (%s)", err, output)
	}
	// Prefer the deployment PostgreSQL container when it exists: its client
	// version matches the server and it needs no host-gateway routing, which
	// keeps the case correct on a native Linux Docker host. The fallback runs
	// pg_dump inside the disposable container against the configured URL (the
	// Docker Desktop path used by local acceptance).
	running := exec.Command("docker", "inspect", "--format", "{{.State.Running}}", s.postgresContainer)
	state, stateErr := running.Output()
	if stateErr == nil && strings.TrimSpace(string(state)) == "true" {
		dump := exec.Command("docker", "exec", s.postgresContainer, "pg_dump", "-U", "postgres", "-Fc", s.db.Config().ConnConfig.Database)
		payload, dumpErr := dump.Output()
		if dumpErr != nil {
			t.Fatalf("dump retained installation from %s: %v", s.postgresContainer, dumpErr)
		}
		restore := exec.Command("docker", "exec", "-i", container, "pg_restore", "-U", "postgres", "-d", scratch, "--no-owner", "--no-privileges")
		restore.Stdin = bytes.NewReader(payload)
		if output, err := restore.CombinedOutput(); err != nil {
			t.Fatalf("restore retained installation: %v (%s)", err, output)
		}
	} else {
		if output, err := execIn("pg_dump", "-U", "postgres", "-Fc", "-f", "/tmp/cutover.dump", cutoverSourceURL(t)); err != nil {
			t.Fatalf("dump retained installation: %v (%s)", err, output)
		}
		if output, err := execIn("pg_restore", "-U", "postgres", "-d", scratch, "--no-owner", "--no-privileges", "/tmp/cutover.dump"); err != nil {
			t.Fatalf("restore retained installation: %v (%s)", err, output)
		}
	}
	return fmt.Sprintf("postgres://postgres@127.0.0.1:%d/%s?sslmode=disable", port, scratch)
}

// retainedHostReady publishes the documented HostReady handshake from the new
// Host artifact. Its authorization key deliberately differs from the retained
// pin so the case can prove the old pin is kept instead of silently re-pinned.
func retainedHostReady(t *testing.T, runtime *restoredRuntime, host *mqttClient, hostID string, authorizationKey []byte) {
	t.Helper()
	ready, err := proto.Marshal(&whalereinv1.HostReady{
		HostId:                 hostID,
		AuthorizationPublicKey: authorizationKey,
	})
	if err != nil {
		t.Fatalf("encode retained HostReady: %v", err)
	}
	runtime.publishHostResult(t, host, hostID, &whalereinv1.HostResult{
		Status:            whalereinv1.OnlineStatus_succeeded,
		ResultBody:        ready,
		CompletedAtUnixMs: time.Now().UnixMilli(),
		ProtocolVersion:   1,
	})
}

// TestCoordinatedCutoverPreservesRetainedState is the V06/AC-15 fixture. It
// seeds retained identity, pin, grant, invitation, Source-independent request
// state and a transient queue, dumps that installation as the stopped old
// stack, restores it into a disposable PostgreSQL container, and starts the
// current Access build with a fresh broker. It proves the new contract reopens
// retained state without rekeying, re-pairing or native replay.
//
// The case does not start the pre-change binary: the durable rows are the
// old-stack contract, and the current binary's migrations run over them. A
// real adjacent-version rehearsal remains the procedural step in the local
// cutover report; this fixture proves retained-state compatibility, not an
// image-to-image upgrade.
func TestCoordinatedCutoverPreservesRetainedState(t *testing.T) {
	s := newStack(t)
	ctx := context.Background()

	hostID := s.hostID
	device := uniqueID("cutover-device")
	token := uniqueID("cutover-token")
	invitation := uniqueID("cutover-invitation")
	completedCommand := uniqueID("cutover-completed-command")
	completedRequest := uniqueID("cutover-completed-request")
	uncertainCommand := uniqueID("cutover-uncertain-command")
	uncertainRequest := uniqueID("cutover-uncertain-request")

	seedGrant(t, s.db, hostID, device, token)
	hostPin := make([]byte, 32)
	if _, err := rand.Read(hostPin); err != nil {
		t.Fatalf("generate retained host pin: %v", err)
	}
	if _, err := s.db.Exec(ctx, `UPDATE hosts SET fingerprint='cutover-host-fingerprint', authorization_public_key=$2, protocol_version=1 WHERE host_id=$1`, hostID, hostPin); err != nil {
		t.Fatalf("seed retained host pin: %v", err)
	}
	if _, err := s.db.Exec(ctx, `UPDATE devices SET public_key_fingerprint='cutover-device-fingerprint' WHERE device_id=$1`, device); err != nil {
		t.Fatalf("seed retained device pin: %v", err)
	}
	invitationHash := sha256.Sum256([]byte(invitation))
	if _, err := s.db.Exec(ctx, `INSERT INTO pairing_tokens (token_hash, host_id, expires_at) VALUES ($1,$2,now() + interval '10 minutes')`, invitationHash[:], hostID); err != nil {
		t.Fatalf("seed retained invitation: %v", err)
	}
	insertRequest(t, s.db, hostID, device, completedRequest, completedCommand, 3, "succeeded", time.Now().Add(time.Hour), "none", []byte("cutover-completed-body"), []byte("cutover-completed-result"))
	insertRequest(t, s.db, hostID, device, uncertainRequest, uncertainCommand, 3, "host_accepted", time.Now().Add(time.Hour), "published", []byte("cutover-uncertain-body"), nil)
	if _, err := s.db.Exec(ctx, `UPDATE requests SET forward_attempts=1, forward_provenance_known=true WHERE request_id=$1`, uncertainRequest); err != nil {
		t.Fatalf("record retained uncertain invocation: %v", err)
	}
	// The pre-cutover stack already negotiated protocol version one. The
	// fixture helper leaves the column at its schema default, so set exactly
	// the value the retained installation carries.
	if _, err := s.db.Exec(ctx, `UPDATE requests SET protocol_version=1 WHERE request_id IN ($1,$2)`, completedRequest, uncertainRequest); err != nil {
		t.Fatalf("record retained protocol version: %v", err)
	}
	t.Cleanup(func() {
		ctx := context.Background()
		_, _ = s.db.Exec(ctx, "DELETE FROM requests WHERE request_id IN ($1,$2)", completedRequest, uncertainRequest)
		_, _ = s.db.Exec(ctx, "DELETE FROM pairing_tokens WHERE token_hash=$1", invitationHash[:])
		_, _ = s.db.Exec(ctx, "DELETE FROM device_credentials WHERE device_id=$1", device)
		_, _ = s.db.Exec(ctx, "DELETE FROM devices WHERE device_id=$1", device)
	})

	restoredURL := startCutoverPostgres(t, s)
	restored, err := pgxpool.New(ctx, restoredURL)
	if err != nil {
		t.Fatalf("open cutover database: %v", err)
	}
	t.Cleanup(restored.Close)

	runtime := startRestoredRuntimeFor(t, s, restoredURL)

	host := &mqttClient{}
	host.connect(t, runtime.mqttAddr, "whalerein-host-"+hostID, "host:"+hostID, runtime.hostToken)
	t.Cleanup(host.close)
	if err := host.subscribe(t, "whalerein/v1/h/"+hostID+"/request"); err != nil {
		t.Fatalf("cutover host subscribe: %v", err)
	}

	t.Run("paired identity and active grant survive without re-pairing", func(t *testing.T) {
		status, body := runtime.request(t, http.MethodGet, "/v1/hosts", token, nil)
		if status != http.StatusOK {
			t.Fatalf("retained device token status = %d, want 200 (%s)", status, body)
		}
		var listed struct {
			Hosts []struct {
				HostID string `json:"hostId"`
				Online bool   `json:"online"`
			} `json:"hosts"`
		}
		if err := json.Unmarshal(body, &listed); err != nil {
			t.Fatalf("decode retained host list: %v", err)
		}
		found := false
		for _, entry := range listed.Hosts {
			if entry.HostID == hostID {
				found = true
			}
		}
		if !found {
			t.Fatalf("retained grant did not list host %s", hostID)
		}
		if err := tryDeviceConnect(runtime.mqttAddr, device, token); err != nil {
			t.Fatalf("retained device credential was rejected after cutover: %v", err)
		}
	})

	t.Run("Host pin is retained and not replaced by the new artifact", func(t *testing.T) {
		replacement := make([]byte, 32)
		if _, err := rand.Read(replacement); err != nil {
			t.Fatalf("generate replacement pin: %v", err)
		}
		retainedHostReady(t, runtime, host, hostID, replacement)
		waitFor(t, 20*time.Second, "rebuilt Host presence", func() bool {
			status, body := runtime.request(t, http.MethodGet, "/v1/hosts", token, nil)
			if status != http.StatusOK {
				return false
			}
			var listed struct {
				Hosts []struct {
					HostID string `json:"hostId"`
					Online bool   `json:"online"`
				} `json:"hosts"`
			}
			if json.Unmarshal(body, &listed) != nil {
				return false
			}
			for _, entry := range listed.Hosts {
				if entry.HostID == hostID && entry.Online {
					return true
				}
			}
			return false
		})

		var retained []byte
		var fingerprint string
		if err := restored.QueryRow(ctx, `SELECT authorization_public_key, fingerprint FROM hosts WHERE host_id=$1`, hostID).Scan(&retained, &fingerprint); err != nil {
			t.Fatalf("read retained host pin: %v", err)
		}
		if !bytes.Equal(retained, hostPin) {
			t.Fatal("cutover replaced the retained Host authorization pin")
		}
		if fingerprint != "cutover-host-fingerprint" {
			t.Fatalf("retained host fingerprint = %q, want the pre-cutover value", fingerprint)
		}
		var deviceFingerprint string
		if err := restored.QueryRow(ctx, `SELECT public_key_fingerprint FROM devices WHERE device_id=$1`, device).Scan(&deviceFingerprint); err != nil {
			t.Fatalf("read retained device pin: %v", err)
		}
		if deviceFingerprint != "cutover-device-fingerprint" {
			t.Fatalf("retained device fingerprint = %q, want the pre-cutover value", deviceFingerprint)
		}
	})

	t.Run("retained invitation survives the cutover", func(t *testing.T) {
		var used *time.Time
		if err := restored.QueryRow(ctx, `SELECT used_at FROM pairing_tokens WHERE token_hash=$1`, invitationHash[:]).Scan(&used); err != nil {
			t.Fatalf("read retained invitation: %v", err)
		}
		if used != nil {
			t.Fatal("cutover consumed or invalidated the retained invitation")
		}
	})

	t.Run("legacy rows keep unknown liveness instead of being swept", func(t *testing.T) {
		// This pre-cutover row has not been read through the new API yet, so it
		// keeps the historical unrecorded liveness that the migration never
		// guesses and never treats as elapsed.
		var leaseState string
		var leaseUntil *time.Time
		if err := restored.QueryRow(ctx, `SELECT client_lease_state, client_lease_until FROM requests WHERE request_id=$1`, uncertainRequest).Scan(&leaseState, &leaseUntil); err != nil {
			t.Fatalf("read migrated uncertain row: %v", err)
		}
		if leaseState != "live" || leaseUntil != nil {
			t.Fatalf("historical uncertain liveness = %s/%v, want the legacy unrecorded state", leaseState, leaseUntil)
		}

		var statusName, forwardState string
		var attempts int
		var cancelPending bool
		if err := restored.QueryRow(ctx, `SELECT status, forward_state, forward_attempts, cancel_pending FROM requests WHERE request_id=$1`, uncertainRequest).Scan(&statusName, &forwardState, &attempts, &cancelPending); err != nil {
			t.Fatalf("read migrated uncertain row: %v", err)
		}
		// The migration stops forwarding a retained row. That is not a new
		// outcome: its proven host_accepted status, attempt history and absent
		// cancellation metadata stay as recorded.
		if statusName != "host_accepted" || forwardState != "none" || attempts != 1 || cancelPending {
			t.Fatalf("retained uncertain row = %s/%s attempts=%d cancel_pending=%v, want host_accepted/none attempts=1 no cancellation", statusName, forwardState, attempts, cancelPending)
		}

		// Terminal and already-published uncertain rows are retained outcomes,
		// not queue entries: the new runtime must not forward them again.
		seen := map[string]*whalereinv1.HostRequest{}
		deadline := time.Now().Add(3 * time.Second)
		for time.Now().Before(deadline) {
			request := nextSentRequest(t, host, hostID, time.Until(deadline))
			if request == nil {
				break
			}
			seen[request.GetRequestId()] = request
		}
		for _, requestID := range []string{completedRequest, uncertainRequest} {
			if delivered := seen[requestID]; delivered != nil {
				t.Fatalf("cutover forwarded retained request %s (kind %s); recorded outcomes must not be replayed", requestID, delivered.GetKind())
			}
		}
	})

	t.Run("terminal and uncertain command outcomes keep their identities", func(t *testing.T) {
		state, status := requestStateFor(runtime, t, token, completedRequest)
		if status != http.StatusOK {
			t.Fatalf("retained terminal command read = %d", status)
		}
		if state.GetStatus() != whalereinv1.OnlineStatus_succeeded {
			t.Fatalf("retained terminal command status = %s, want succeeded", state.GetStatus())
		}
		if string(state.GetResultBody()) != "cutover-completed-result" {
			t.Fatalf("retained terminal result = %q, want the pre-cutover bytes", state.GetResultBody())
		}
		if state.GetCommandId() != completedCommand {
			t.Fatalf("retained terminal command id = %q, want %q", state.GetCommandId(), completedCommand)
		}

		uncertain, status := requestStateFor(runtime, t, token, uncertainRequest)
		if status != http.StatusOK {
			t.Fatalf("retained uncertain command read = %d", status)
		}
		if uncertain.GetStatus() != whalereinv1.OnlineStatus_host_accepted {
			t.Fatalf("retained uncertain command status = %s, want host_accepted (never falsely settled or expired)", uncertain.GetStatus())
		}
		if uncertain.GetCommandId() != uncertainCommand {
			t.Fatalf("retained uncertain command id = %q, want %q", uncertain.GetCommandId(), uncertainCommand)
		}
	})

	t.Run("protocol version stays one for retained and new work", func(t *testing.T) {
		for _, requestID := range []string{completedRequest, uncertainRequest} {
			var version int
			if err := restored.QueryRow(ctx, `SELECT protocol_version FROM requests WHERE request_id=$1`, requestID).Scan(&version); err != nil {
				t.Fatalf("read retained protocol version: %v", err)
			}
			if version != 1 {
				t.Fatalf("retained request %s protocol_version = %d, want 1", requestID, version)
			}
		}

		// Positive control: the coordinated new artifact still forwards a fresh
		// version-one command through the same retained identity and broker.
		freshRequest := uniqueID("cutover-fresh-request")
		freshCommand := uniqueID("cutover-fresh-command")
		body := []byte("cutover-fresh-body")
		digest := sha256.Sum256(body)
		status, response := runtime.submit(t, token, hostID, &whalereinv1.HostRequest{
			RequestId:       freshRequest,
			HostId:          hostID,
			DeviceId:        device,
			Kind:            whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
			CommandId:       freshCommand,
			PayloadDigest:   digest[:],
			ExpiresAtUnixMs: time.Now().Add(time.Hour).UnixMilli(),
			ProtocolVersion: 1,
			Body:            body,
		})
		if status != http.StatusOK {
			t.Fatalf("fresh version-one submit status = %d (%s)", status, response)
		}
		deadline := time.Now().Add(20 * time.Second)
		delivered := false
		for time.Now().Before(deadline) {
			request := nextSentRequest(t, host, hostID, time.Until(deadline))
			if request == nil {
				break
			}
			if request.GetRequestId() == freshRequest {
				delivered = true
				if request.GetProtocolVersion() != 1 {
					t.Fatalf("fresh forwarded protocol_version = %d, want 1", request.GetProtocolVersion())
				}
				break
			}
		}
		if !delivered {
			t.Fatal("cutover runtime never forwarded the fresh version-one positive control")
		}
	})
}
