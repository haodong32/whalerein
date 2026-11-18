//go:build acceptance

package acceptance

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

// restoredRuntime is one isolated Access installation started against a
// restored scratch database and its own broker. The broker is a uniquely
// named container with its own port; Access is a local process with unique
// listeners and MQTT client id. Nothing here touches the running stack.
type restoredRuntime struct {
	accessURL string
	mqttAddr  string
	hostToken string
}

func randomSecret(t *testing.T) string {
	t.Helper()
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		t.Fatalf("generate acceptance secret: %v", err)
	}
	return hex.EncodeToString(raw)
}

func freeLoopbackPort(t *testing.T) int {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("allocate loopback port: %v", err)
	}
	defer listener.Close()
	return listener.Addr().(*net.TCPAddr).Port
}

// restoredAccessConfig is a deployment-shaped configuration. Secrets are
// environment references so the file itself carries none.
const restoredAccessConfig = `mode: standalone
listen: "127.0.0.1:%d"
internal_listen: "0.0.0.0:%d"
public_url: "http://127.0.0.1:%d"
database_url: "${WHALEREIN_RESTORE_DATABASE_URL}"
admin_token: "${WHALEREIN_RESTORE_ADMIN_TOKEN}"
host_provisioning_token: "${WHALEREIN_RESTORE_HOST_TOKEN}"

broker:
  url: "mqtt://127.0.0.1:%d"
  public_url: "mqtt://127.0.0.1:%d"
  client_id: "%s"
  username: "access"
  password: "${WHALEREIN_RESTORE_BROKER_PASSWORD}"
  connect_timeout: 10s
  publish_timeout: 5s
  keep_alive: 30s
  reconnect_min: 100ms
  reconnect_max: 2s

line:
  reconcile_interval: 2s
  heartbeat_interval: 1s
  policy_ttl: 5s
  operation_timeout: 5s

presence:
  cache_ttl: 10s

requests:
  forward_interval: 100ms
  retry_interval: 500ms
  lease: 2s

internal:
  callback_token: "${WHALEREIN_RESTORE_INTERNAL_TOKEN}"

log:
  level: debug
`

// restoredBrokerConfig is the pinned WhaleRein Mosquitto configuration with
// only the Access callback target and plugin secrets changed. The broker
// reaches the local restored Access process through Docker's host gateway.
const restoredBrokerConfig = `allow_anonymous false
persistence false
max_packet_size 1048576
sys_interval 0

listener 1883 0.0.0.0
protocol mqtt
set_tcp_nodelay true
max_connections -1

log_dest stdout
log_type error
log_type warning
log_type notice
connection_messages false

plugin /usr/lib/mosquitto/whalerein.so
plugin_opt_access_username access
plugin_opt_access_password_file /run/whalerein/mqtt-access-password
plugin_opt_access_auth_url http://host.docker.internal:%d/internal/broker/connect
plugin_opt_access_auth_token_file /run/whalerein/internal-token
plugin_opt_auth_timeout_ms 500
plugin_opt_auth_workers 8
plugin_opt_policy_ttl_ms 15000
`

// startRestoredRuntime builds the configured Access binary and starts it, plus
// a uniquely named broker container, against the restored scratch database.
// The required restart commands from newStack are still validated: this case
// belongs to the deployment suite and must not silently degrade to SQL checks.
func startRestoredRuntime(t *testing.T, s *stack, scratch string) *restoredRuntime {
	t.Helper()
	return startRestoredRuntimeFor(t, s, databaseURLFor(scratch))
}

// startRestoredRuntimeFor is the same isolated Access-plus-broker harness as
// startRestoredRuntime, but the caller supplies the retained database URL. The
// coordinated-cutover fixture uses this to start the current build against a
// snapshot that lives in its own disposable PostgreSQL container instead of
// the configured deployment server.
func startRestoredRuntimeFor(t *testing.T, s *stack, databaseURL string) *restoredRuntime {
	t.Helper()
	// newStack already required the deployment restart contract. Reading the
	// broker restart command here keeps that configuration failure explicit.
	requiredEnv(t, "WHALEREIN_ACCEPTANCE_RESTART_ACCESS")
	requiredEnv(t, "WHALEREIN_ACCEPTANCE_RESTART_BROKER")

	workDir := filepath.Join(repositoryRoot(), ".data", "acceptance", "restored-"+uniqueID("stack"))
	if err := os.MkdirAll(workDir, 0o700); err != nil {
		t.Fatalf("create restored runtime directory: %v", err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(workDir) })

	httpPort := freeLoopbackPort(t)
	internalPort := freeLoopbackPort(t)
	mqttPort := freeLoopbackPort(t)
	accessURL := fmt.Sprintf("http://127.0.0.1:%d", httpPort)
	mqttAddr := fmt.Sprintf("127.0.0.1:%d", mqttPort)

	adminToken := randomSecret(t)
	hostToken := randomSecret(t)
	brokerPassword := randomSecret(t)
	internalToken := randomSecret(t)
	clientID := uniqueID("whalerein-access-restored")
	brokerContainer := uniqueID("whalerein-restored-broker")
	brokerImage := strings.TrimSpace(os.Getenv("WHALEREIN_ACCEPTANCE_MOSQUITTO_IMAGE"))
	if brokerImage == "" {
		brokerImage = "whalerein-mosquitto:local"
	}

	if output, err := exec.Command("docker", "image", "inspect", brokerImage).CombinedOutput(); err != nil {
		t.Fatalf("restored broker image %q is required (set WHALEREIN_ACCEPTANCE_MOSQUITTO_IMAGE): %v (%s)", brokerImage, err, output)
	}

	brokerConfigPath := filepath.Join(workDir, "mosquitto.conf")
	if err := os.WriteFile(brokerConfigPath, []byte(fmt.Sprintf(restoredBrokerConfig, internalPort)), 0o644); err != nil {
		t.Fatalf("write restored broker config: %v", err)
	}
	accessConfigPath := filepath.Join(workDir, "access.yaml")
	config := fmt.Sprintf(restoredAccessConfig, httpPort, internalPort, httpPort, mqttPort, mqttPort, clientID)
	if err := os.WriteFile(accessConfigPath, []byte(config), 0o600); err != nil {
		t.Fatalf("write restored Access config: %v", err)
	}

	brokerRun := exec.Command("docker", "run", "-d",
		"--name", brokerContainer,
		"--add-host=host.docker.internal:host-gateway",
		"-p", fmt.Sprintf("127.0.0.1:%d:1883", mqttPort),
		"-v", brokerConfigPath+":/mosquitto/config/mosquitto.conf:ro",
		"-e", "WHALEREIN_BROKER_PASSWORD="+brokerPassword,
		"-e", "WHALEREIN_INTERNAL_TOKEN="+internalToken,
		brokerImage)
	if output, err := brokerRun.CombinedOutput(); err != nil {
		t.Fatalf("start restored broker: %v (%s)", err, output)
	}
	t.Cleanup(func() { _ = exec.Command("docker", "rm", "-f", brokerContainer).Run() })
	waitFor(t, 30*time.Second, "restored broker listener", func() bool {
		conn, err := net.DialTimeout("tcp", mqttAddr, time.Second)
		if err != nil {
			return false
		}
		_ = conn.Close()
		return true
	})

	binary := strings.TrimSpace(os.Getenv("WHALEREIN_ACCEPTANCE_ACCESS_BINARY"))
	if binary == "" {
		binary = filepath.Join(workDir, "whalerein-access")
		build := exec.Command("go", "build", "-o", binary, "./access/cmd/server")
		build.Dir = repositoryRoot()
		build.Env = append(os.Environ(), "CGO_ENABLED=0")
		if output, err := build.CombinedOutput(); err != nil {
			t.Fatalf("build restored Access: %v (%s)", err, output)
		}
	} else {
		absolute, err := filepath.Abs(binary)
		if err != nil {
			t.Fatalf("resolve WHALEREIN_ACCEPTANCE_ACCESS_BINARY: %v", err)
		}
		if info, err := os.Stat(absolute); err != nil || info.IsDir() {
			t.Fatalf("WHALEREIN_ACCEPTANCE_ACCESS_BINARY is not a file: %v", err)
		}
		binary = absolute
	}

	logPath := filepath.Join(workDir, "access.log")
	logFile, err := os.Create(logPath)
	if err != nil {
		t.Fatalf("create restored Access log: %v", err)
	}
	process := exec.Command(binary, "--config", accessConfigPath)
	process.Dir = repositoryRoot()
	process.Env = append(os.Environ(),
		"WHALEREIN_RESTORE_DATABASE_URL="+databaseURL,
		"WHALEREIN_RESTORE_ADMIN_TOKEN="+adminToken,
		"WHALEREIN_RESTORE_HOST_TOKEN="+hostToken,
		"WHALEREIN_RESTORE_BROKER_PASSWORD="+brokerPassword,
		"WHALEREIN_RESTORE_INTERNAL_TOKEN="+internalToken,
	)
	process.Stdout = logFile
	process.Stderr = logFile
	if err := process.Start(); err != nil {
		_ = logFile.Close()
		t.Fatalf("start restored Access: %v", err)
	}
	t.Cleanup(func() {
		_ = process.Process.Signal(syscall.SIGTERM)
		done := make(chan struct{})
		go func() {
			_ = process.Wait()
			close(done)
		}()
		select {
		case <-done:
		case <-time.After(20 * time.Second):
			_ = process.Process.Kill()
		}
		_ = logFile.Close()
		if t.Failed() {
			if raw, err := os.ReadFile(logPath); err == nil {
				lines := strings.Split(strings.TrimSpace(string(raw)), "\n")
				if len(lines) > 40 {
					lines = lines[len(lines)-40:]
				}
				t.Logf("restored Access log:\n%s", strings.Join(lines, "\n"))
			}
			if logs, err := exec.Command("docker", "logs", brokerContainer).CombinedOutput(); err == nil {
				t.Logf("restored broker log:\n%s", logs)
			}
		}
	})

	waitReady(t, accessURL)
	return &restoredRuntime{
		accessURL: accessURL,
		mqttAddr:  mqttAddr,
		hostToken: hostToken,
	}
}

func (r *restoredRuntime) request(t *testing.T, method, path, token string, body []byte) (int, []byte) {
	t.Helper()
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	request, err := http.NewRequest(method, r.accessURL+path, reader)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer "+token)
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}
	client := &http.Client{Timeout: 10 * time.Second}
	response, err := client.Do(request)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	defer response.Body.Close()
	payload, _ := io.ReadAll(response.Body)
	return response.StatusCode, payload
}

func (r *restoredRuntime) submit(t *testing.T, token, hostID string, request *whalereinv1.HostRequest) (int, []byte) {
	t.Helper()
	body, err := protojson.Marshal(request)
	if err != nil {
		t.Fatalf("encode request: %v", err)
	}
	return r.request(t, http.MethodPost, "/v1/hosts/"+hostID+"/requests", token, body)
}

// nextSentRequest reads the next HostRequest on the restored host topic.
func nextSentRequest(t *testing.T, host *mqttClient, hostID string, timeout time.Duration) *whalereinv1.HostRequest {
	t.Helper()
	payload := host.next(t, "whalerein/v1/h/"+hostID+"/request", timeout)
	if payload == nil {
		return nil
	}
	request := &whalereinv1.HostRequest{}
	if err := proto.Unmarshal(payload, request); err != nil {
		t.Fatalf("decode restored HostRequest: %v", err)
	}
	return request
}

func (r *restoredRuntime) publishHostResult(t *testing.T, host *mqttClient, hostID string, result *whalereinv1.HostResult) {
	t.Helper()
	result.HostId = hostID
	payload, err := proto.Marshal(result)
	if err != nil {
		t.Fatalf("encode HostResult: %v", err)
	}
	if err := host.publish(t, "whalerein/v1/h/"+hostID+"/result", payload); err != nil {
		t.Fatalf("publish HostResult: %v", err)
	}
}

// reconnectReplay drives the documented HostReady → revocation replay → final
// notice handshake from the restored runtime. It asserts that the restored
// pending revocation is only recorded after the Host acknowledges it.
func (r *restoredRuntime) reconnectReplay(t *testing.T, restored *pgxpool.Pool, host *mqttClient, hostID, oldDevice string) {
	t.Helper()
	ready, err := proto.Marshal(&whalereinv1.HostReady{HostId: hostID})
	if err != nil {
		t.Fatalf("encode HostReady: %v", err)
	}
	r.publishHostResult(t, host, hostID, &whalereinv1.HostResult{
		Status:            whalereinv1.OnlineStatus_succeeded,
		ResultBody:        ready,
		CompletedAtUnixMs: time.Now().UnixMilli(),
		ProtocolVersion:   1,
	})

	deadline := time.Now().Add(30 * time.Second)
	sawPending, sawFinal := false, false
	for time.Now().Before(deadline) {
		request := nextSentRequest(t, host, hostID, time.Until(deadline))
		if request == nil {
			break
		}
		if request.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_REVOCATION {
			t.Fatalf("restored Access sent %s before revocation replay finished", request.GetKind())
		}
		notice := &whalereinv1.RevocationNotice{}
		if err := proto.Unmarshal(request.GetBody(), notice); err != nil {
			t.Fatalf("decode RevocationNotice: %v", err)
		}
		if !notice.GetFinal() {
			if !sawPending {
				found := false
				for _, device := range notice.GetDeviceIds() {
					if device == oldDevice {
						found = true
					}
				}
				if !found {
					t.Fatalf("restored replay did not include the restored revoked device")
				}
				var replayed bool
				if err := restored.QueryRow(context.Background(), "SELECT replayed_at IS NOT NULL FROM grants WHERE host_id=$1 AND device_id=$2", hostID, oldDevice).Scan(&replayed); err != nil {
					t.Fatalf("read replay state before acknowledgement: %v", err)
				}
				if replayed {
					t.Fatal("restored Access marked a revocation replayed before the Host acknowledged it")
				}
			}
			sawPending = true
		}
		ack, err := proto.Marshal(&whalereinv1.RevocationAck{
			ReplayId:  notice.GetReplayId(),
			DeviceIds: notice.GetDeviceIds(),
		})
		if err != nil {
			t.Fatalf("encode RevocationAck: %v", err)
		}
		r.publishHostResult(t, host, hostID, &whalereinv1.HostResult{
			RequestId:         request.GetRequestId(),
			Status:            whalereinv1.OnlineStatus_succeeded,
			ResultBody:        ack,
			CompletedAtUnixMs: time.Now().UnixMilli(),
			ProtocolVersion:   1,
		})
		if notice.GetFinal() {
			sawFinal = true
			break
		}
	}
	if !sawPending {
		t.Fatal("restored Access never replayed the restored pending revocation")
	}
	if !sawFinal {
		t.Fatal("restored Access never sent the final revocation notice after acknowledgement")
	}
	waitFor(t, 10*time.Second, "recorded revocation replay", func() bool {
		var replayed bool
		return restored.QueryRow(context.Background(), "SELECT replayed_at IS NOT NULL FROM grants WHERE host_id=$1 AND device_id=$2", hostID, oldDevice).Scan(&replayed) == nil && replayed
	})
}

// TestHistoricalRestoreRunsRestoredInstallation is the E01 acceptance case. It
// dumps a live installation, restores it into a scratch database, applies the
// documented invalidation, and then starts a restored Access process and its
// own broker. It proves with real HTTP and MQTT behavior that old tokens and
// invitations are refused, restored revocations are replayed to a newly
// connected Host before replay is recorded, pre-restore queued rows are not
// autonomously forwarded, an unresolved command is not repeated, and a fresh
// identity is fully usable as the positive control.
func TestHistoricalRestoreRunsRestoredInstallation(t *testing.T) {
	s := newStack(t)
	ctx := context.Background()

	oldHost := uniqueID("restore-host")
	oldDevice := uniqueID("restore-old-device")
	oldToken := uniqueID("restore-old-token")
	oldInvitation := uniqueID("restore-invitation")
	completedCommand := uniqueID("restore-completed-command")
	completedRequest := uniqueID("restore-completed-request")
	unresolvedCommand := uniqueID("restore-unresolved-command")
	unresolvedRequest := uniqueID("restore-unresolved-request")
	queuedRequest := uniqueID("restore-queued-request")
	freshDevice := uniqueID("restore-fresh-device")
	freshToken := uniqueID("restore-fresh-token")
	freshCommand := uniqueID("restore-fresh-command")
	freshRequest := uniqueID("restore-fresh-request")

	seedGrant(t, s.db, oldHost, oldDevice, oldToken)
	tokenHash := sha256.Sum256([]byte(oldInvitation))
	if _, err := s.db.Exec(ctx, `INSERT INTO pairing_tokens (token_hash, host_id, expires_at) VALUES ($1,$2,now() + interval '10 minutes')`, tokenHash[:], oldHost); err != nil {
		t.Fatalf("seed old invitation: %v", err)
	}
	insertRequest(t, s.db, oldHost, oldDevice, completedRequest, completedCommand, 3, "succeeded", time.Now().Add(time.Hour), "none", []byte("completed-body"), []byte("completed-result"))
	insertRequest(t, s.db, oldHost, oldDevice, unresolvedRequest, unresolvedCommand, 3, "host_accepted", time.Now().Add(time.Hour), "published", []byte("unresolved-body"), nil)
	if _, err := s.db.Exec(ctx, "UPDATE requests SET forward_attempts=1, forward_provenance_known=true WHERE request_id=$1", unresolvedRequest); err != nil {
		t.Fatalf("record unresolved command source invocation: %v", err)
	}
	insertRequest(t, s.db, oldHost, oldDevice, queuedRequest, "", 1, "received", time.Now().Add(time.Hour), "pending", []byte("queued-body"), nil)
	t.Cleanup(func() {
		_, _ = s.db.Exec(context.Background(), "DELETE FROM requests WHERE request_id IN ($1,$2,$3)", completedRequest, unresolvedRequest, queuedRequest)
		_, _ = s.db.Exec(context.Background(), "DELETE FROM pairing_tokens WHERE token_hash=$1", tokenHash[:])
		_, _ = s.db.Exec(context.Background(), "DELETE FROM grants WHERE host_id=$1", oldHost)
		_, _ = s.db.Exec(context.Background(), "DELETE FROM device_credentials WHERE device_id IN ($1,$2)", oldDevice, freshDevice)
		_, _ = s.db.Exec(context.Background(), "DELETE FROM devices WHERE device_id IN ($1,$2)", oldDevice, freshDevice)
		_, _ = s.db.Exec(context.Background(), "DELETE FROM hosts WHERE host_id=$1", oldHost)
	})

	restored, scratch := scratchRestore(t, s)

	// An untouched restore is the hazard: the old credential, grant and
	// recorded command all still exist.
	var activeGrants int
	if err := restored.QueryRow(ctx, "SELECT count(*) FROM grants WHERE host_id=$1 AND device_id=$2 AND revoked_at IS NULL", oldHost, oldDevice).Scan(&activeGrants); err != nil {
		t.Fatalf("count untouched restored grant: %v", err)
	}
	if activeGrants != 1 {
		t.Fatalf("untouched restore active grants = %d, want 1 (proving the hazard)", activeGrants)
	}
	var invitationUnused bool
	if err := restored.QueryRow(ctx, "SELECT used_at IS NULL FROM pairing_tokens WHERE token_hash=$1", tokenHash[:]).Scan(&invitationUnused); err != nil {
		t.Fatalf("read untouched restored invitation: %v", err)
	}
	if !invitationUnused {
		t.Fatal("untouched restore already marked the old invitation used; the test would not prove invalidation")
	}
	if _, err := restored.Exec(ctx, restoreInvalidationSQL); err != nil {
		t.Fatalf("apply documented restore invalidation: %v", err)
	}

	// A fresh identity is enrolled after invalidation, exactly like the
	// documented re-enrollment step. It is not part of the restored credential
	// set and is the positive control for every restored-runtime assertion.
	seedGrant(t, restored, oldHost, freshDevice, freshToken)

	runtime := startRestoredRuntime(t, s, scratch)

	host := &mqttClient{}
	host.connect(t, runtime.mqttAddr, "whalerein-host-"+oldHost, "host:"+oldHost, runtime.hostToken)
	t.Cleanup(host.close)
	if err := host.subscribe(t, "whalerein/v1/h/"+oldHost+"/request"); err != nil {
		t.Fatalf("restored host subscribe: %v", err)
	}
	fresh := &mqttClient{}
	fresh.connect(t, runtime.mqttAddr, "whalerein-device-"+freshDevice, "device:"+freshDevice, freshToken)
	t.Cleanup(fresh.close)

	t.Run("old credential and invitation are refused", func(t *testing.T) {
		status, body := runtime.request(t, http.MethodGet, "/v1/hosts", oldToken, nil)
		if status != http.StatusUnauthorized {
			t.Fatalf("old device token status = %d, want 401 (%s)", status, body)
		}
		if err := tryDeviceConnect(runtime.mqttAddr, oldDevice, oldToken); err == nil {
			t.Fatal("restored broker accepted the old device credential")
		}

		claim, err := protojson.Marshal(&whalereinv1.PairingClaimRequest{
			Token:           oldInvitation,
			ClaimId:         uniqueID("restore-claim"),
			DeviceId:        oldDevice,
			Phase:           whalereinv1.PairingPhase_PAIRING_PHASE_HELLO,
			NoiseMessage:    []byte("restored-runtime-noise"),
			ProtocolVersion: 1,
		})
		if err != nil {
			t.Fatalf("encode restored claim: %v", err)
		}
		status, body = runtime.request(t, http.MethodPost, "/v1/pairing/claims", "", claim)
		if status != http.StatusConflict {
			t.Fatalf("old invitation status = %d, want 409 (%s)", status, body)
		}
		var failure struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		}
		if err := json.Unmarshal(body, &failure); err != nil ||
			failure.Code != "conflict" || failure.Message != "pairing token already used" {
			t.Fatalf("old invitation error = %s, want the used-invitation conflict", body)
		}
	})

	t.Run("fresh identity is usable and receives forwarded events", func(t *testing.T) {
		status, body := runtime.request(t, http.MethodGet, "/v1/hosts", freshToken, nil)
		if status != http.StatusOK {
			t.Fatalf("fresh device token status = %d, want 200 (%s)", status, body)
		}

		runtime.reconnectReplay(t, restored, host, oldHost, oldDevice)

		eventTopic := "whalerein/v1/h/" + oldHost + "/d/" + freshDevice + "/event"
		if err := fresh.subscribe(t, eventTopic); err != nil {
			t.Fatalf("fresh device subscribe: %v", err)
		}
		payload := eventPayload(oldHost, freshDevice)
		deadline := time.Now().Add(20 * time.Second)
		for {
			if err := host.publish(t, "whalerein/v1/h/"+oldHost+"/event", payload); err != nil {
				t.Fatalf("restored host event publish: %v", err)
			}
			if received := fresh.next(t, eventTopic, 2*time.Second); received != nil {
				if !bytes.Equal(received, payload) {
					t.Fatal("restored forwarding changed the event payload")
				}
				break
			}
			if time.Now().After(deadline) {
				t.Fatal("fresh identity never received a forwarded event from the restored runtime")
			}
		}
	})

	t.Run("unresolved command is not repeated and queued rows are not forwarded", func(t *testing.T) {
		// Positive control: a new explicit command from the fresh identity is
		// forwarded exactly once.
		commandBody := []byte("restore-fresh-command-body")
		digest := sha256.Sum256(commandBody)
		status, body := runtime.submit(t, freshToken, oldHost, &whalereinv1.HostRequest{
			RequestId:       freshRequest,
			HostId:          oldHost,
			DeviceId:        freshDevice,
			Kind:            whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
			CommandId:       freshCommand,
			PayloadDigest:   digest[:],
			ExpiresAtUnixMs: time.Now().Add(time.Hour).UnixMilli(),
			ProtocolVersion: 1,
			Body:            commandBody,
		})
		if status != http.StatusOK {
			t.Fatalf("fresh command submit status = %d (%s)", status, body)
		}

		// Forwarding retries an unacknowledged published command, so the
		// positive control asserts source delivery, not single publication.
		seen := map[string]*whalereinv1.HostRequest{}
		deadline := time.Now().Add(20 * time.Second)
		for time.Now().Before(deadline) {
			request := nextSentRequest(t, host, oldHost, time.Until(deadline))
			if request == nil {
				break
			}
			if request.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_COMMAND {
				continue
			}
			seen[request.GetRequestId()] = request
			if request.GetRequestId() == freshRequest {
				break
			}
		}
		if seen[freshRequest] == nil {
			t.Fatal("restored runtime never forwarded the fresh positive-control command")
		}
		if seen[freshRequest].GetCommandId() != freshCommand {
			t.Fatalf("forwarded command id = %q, want the fresh command", seen[freshRequest].GetCommandId())
		}

		// Drain briefly so a delayed autonomous retry of an old row would be
		// observed, then assert the pre-restore rows were never delivered.
		for {
			request := nextSentRequest(t, host, oldHost, time.Second)
			if request == nil {
				break
			}
			seen[request.GetRequestId()] = request
		}
		for _, stale := range []string{completedRequest, unresolvedRequest, queuedRequest} {
			if _, delivered := seen[stale]; delivered {
				t.Fatalf("restored runtime autonomously forwarded pre-restore request %s", stale)
			}
		}

		// Complete the fresh command so the user-visible outcome is checked
		// through the public API, not only through the database.
		resultBody := []byte("restore-fresh-result")
		runtime.publishHostResult(t, host, oldHost, &whalereinv1.HostResult{
			RequestId:         freshRequest,
			DeviceId:          freshDevice,
			CommandId:         freshCommand,
			Status:            whalereinv1.OnlineStatus_succeeded,
			ResultBody:        resultBody,
			CompletedAtUnixMs: time.Now().UnixMilli(),
			ProtocolVersion:   1,
		})
		waitFor(t, 10*time.Second, "fresh command success", func() bool {
			state, status := requestStateFor(runtime, t, freshToken, freshRequest)
			return status == http.StatusOK && state.GetStatus() == whalereinv1.OnlineStatus_succeeded
		})

		// The unresolved restored command keeps its unresolved outcome and no
		// autonomous retry: the source was never invoked again.
		var statusName, forwardState string
		var attempts int
		if err := restored.QueryRow(ctx, "SELECT status, forward_state, forward_attempts FROM requests WHERE request_id=$1", unresolvedRequest).Scan(&statusName, &forwardState, &attempts); err != nil {
			t.Fatalf("read unresolved restored command: %v", err)
		}
		if statusName != "unknown" || forwardState != "none" || attempts != 1 {
			t.Fatalf("unresolved command = %s/%s attempts=%d, want unknown/none attempts=1", statusName, forwardState, attempts)
		}
		if err := restored.QueryRow(ctx, "SELECT status, forward_state FROM requests WHERE request_id=$1", queuedRequest).Scan(&statusName, &forwardState); err != nil {
			t.Fatalf("read restored queued request: %v", err)
		}
		if statusName != "unknown" || forwardState != "none" {
			t.Fatalf("restored queued request = %s/%s, want unknown/none", statusName, forwardState)
		}
	})
}

// requestStateFor reads one request through the restored public API.
func requestStateFor(runtime *restoredRuntime, t *testing.T, token, requestID string) (*whalereinv1.RequestState, int) {
	t.Helper()
	status, body := runtime.request(t, http.MethodGet, "/v1/requests/"+requestID, token, nil)
	if status != http.StatusOK {
		return nil, status
	}
	state := &whalereinv1.RequestState{}
	if err := protojson.Unmarshal(body, state); err != nil {
		t.Fatalf("decode restored request state: %v", err)
	}
	return state, status
}
