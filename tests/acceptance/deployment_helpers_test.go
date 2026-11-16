//go:build acceptance

package acceptance

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
	"github.com/jackc/pgx/v5/pgxpool"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

func uniqueID(prefix string) string {
	return fmt.Sprintf("%s-%d", prefix, time.Now().UnixNano())
}

// insertRequest writes one durable request row. Command rows (non-empty
// command_id) are deduplication records; query rows (empty command_id, kind 1
// or 2) are disposable request metadata.
func insertRequest(t *testing.T, db *pgxpool.Pool, hostID, deviceID, requestID, commandID string, kind int, status string, expiresAt time.Time, forwardState string, requestBody, resultBody []byte) {
	t.Helper()
	_, err := db.Exec(context.Background(), `
		INSERT INTO requests (
			request_id, host_id, device_id, command_id, kind, status,
			request_body, result_body, expires_at, forward_state, forward_after, published_once
		) VALUES ($1,$2,$3,$4,$5,$6,COALESCE($7,''::bytea),COALESCE($8,''::bytea),$9,$10,now(),$10 <> 'pending')`,
		requestID, hostID, deviceID, commandID, kind, status, requestBody, resultBody, expiresAt, forwardState)
	if err != nil {
		t.Fatalf("insert request %s: %v", requestID, err)
	}
}

func requestState(t *testing.T, s *stack, token, requestID string) (*whalereinv1.RequestState, int) {
	t.Helper()
	status, body := s.request(t, "GET", "/v1/requests/"+requestID, token)
	if status != 200 {
		return nil, status
	}
	state := &whalereinv1.RequestState{}
	if err := protojson.Unmarshal(body, state); err != nil {
		t.Fatalf("decode request state: %v (%s)", err, body)
	}
	return state, status
}

func waitFor(t *testing.T, timeout time.Duration, description string, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if condition() {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", description)
}

func dockerOutput(t *testing.T, args ...string) string {
	t.Helper()
	output, err := exec.Command("docker", args...).CombinedOutput()
	if err != nil {
		t.Fatalf("docker %s: %v (%s)", strings.Join(args, " "), err, output)
	}
	return strings.TrimSpace(string(output))
}

func dockerInput(t *testing.T, input []byte, args ...string) string {
	t.Helper()
	command := exec.Command("docker", args...)
	command.Stdin = strings.NewReader(string(input))
	output, err := command.CombinedOutput()
	if err != nil {
		t.Fatalf("docker %s: %v (%s)", strings.Join(args, " "), err, output)
	}
	return strings.TrimSpace(string(output))
}

func inspectContainer(t *testing.T, container, format string) string {
	t.Helper()
	return dockerOutput(t, "inspect", "--format", format, container)
}

func containerLogs(t *testing.T, container string) string {
	t.Helper()
	return dockerOutput(t, "logs", container)
}

func logConfig(t *testing.T, container string) map[string]string {
	t.Helper()
	var raw map[string]any
	if err := json.Unmarshal([]byte(inspectContainer(t, container, "{{json .HostConfig.LogConfig}}")), &raw); err != nil {
		t.Fatalf("decode log config: %v", err)
	}
	config := map[string]string{}
	for key, value := range raw["Config"].(map[string]any) {
		config[key] = fmt.Sprint(value)
	}
	config["Type"] = fmt.Sprint(raw["Type"])
	return config
}

func requireConfig(t *testing.T, key, value string) string {
	t.Helper()
	if strings.TrimSpace(value) == "" {
		t.Fatalf("%s is required for this deployment acceptance case; see docs/testing.md#deployment-acceptance", key)
	}
	return value
}

func waitDatabase(t *testing.T, db *pgxpool.Pool) {
	t.Helper()
	waitFor(t, 60*time.Second, "database connection recovery", func() bool {
		return db.Ping(context.Background()) == nil
	})
}

func terminateDatabaseSessions(t *testing.T, db *pgxpool.Pool) {
	t.Helper()
	_, err := db.Exec(context.Background(), `
		SELECT pg_terminate_backend(pid)
		FROM pg_stat_activity
		WHERE datname = current_database() AND pid <> pg_backend_pid()`)
	if err != nil {
		t.Fatalf("terminate database sessions: %v", err)
	}
}

func databaseURLFor(database string) string {
	base := os.Getenv("WHALEREIN_ACCEPTANCE_DATABASE_URL")
	if base == "" {
		base = "postgres://postgres@127.0.0.1:55432/whalerein_dev?sslmode=disable"
	}
	parsed, err := url.Parse(base)
	if err != nil {
		return base
	}
	parsed.Path = "/" + database
	return parsed.String()
}

func repositoryRoot() string {
	working, err := os.Getwd()
	if err != nil {
		return "."
	}
	return filepath.Clean(filepath.Join(working, "..", ".."))
}

func runCommandBestEffort(command string) {
	parts := strings.Fields(command)
	if len(parts) == 0 {
		return
	}
	child := exec.Command(parts[0], parts[1:]...)
	child.Dir = repositoryRoot()
	_ = child.Run()
}

func execBestEffort(db *pgxpool.Pool, statement string) {
	_, _ = db.Exec(context.Background(), statement)
}

// tryDeviceConnect attempts an MQTT CONNECT without failing the test, so a
// caller can assert that a revoked credential is rejected.
func tryDeviceConnect(address, device, token string) error {
	conn, err := net.DialTimeout("tcp", address, 5*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	client := paho.NewClient(paho.ClientConfig{
		ClientID: "whalerein-device-" + device,
		Conn:     packets.NewThreadSafeConn(conn),
	})
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	connack, err := client.Connect(ctx, &paho.Connect{
		ClientID:     "whalerein-device-" + device,
		CleanStart:   true,
		KeepAlive:    15,
		UsernameFlag: true,
		Username:     "device:" + device,
		PasswordFlag: token != "",
		Password:     []byte(token),
	})
	if err != nil {
		return err
	}
	if connack.ReasonCode != 0 {
		return fmt.Errorf("connack %d", connack.ReasonCode)
	}
	return nil
}
