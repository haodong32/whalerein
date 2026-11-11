package main

import (
	"bytes"
	"context"

	"fmt"
	"io"

	"net"
	"net/http"

	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/httpapi"
)

func freePort(t *testing.T) int {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	_ = listener.Close()
	return port
}

type configValues struct {
	publicPort   int
	internalPort int
	databaseURL  string
	brokerURL    string
	restURL      string
	logLevel     string
	cleanup      string
}

func writeConfig(t *testing.T, values configValues) string {
	t.Helper()
	if values.brokerURL == "" {
		values.brokerURL = "tcp://127.0.0.1:1"
	}
	if values.logLevel == "" {
		values.logLevel = "error"
	}
	if values.cleanup == "" {
		values.cleanup = "20ms"
	}
	body := fmt.Sprintf(`
listen: "127.0.0.1:%d"
internal_listen: "127.0.0.1:%d"
public_url: "https://access.example.com"
database_url: "%s"
admin_token: "admin-token-value"
host_provisioning_token: "host-token-value"
broker:
  url: "%s"
  client_id: "whalerein-access"
  username: "access"
  password: "broker-password-value"
  connect_timeout: 100ms
  publish_timeout: 100ms
  keep_alive: 5s
  reconnect_min: 10ms
  reconnect_max: 50ms
emqx:
  rest_url: "%s"
  disconnect_mode: "ctl"
  ctl_path: "emqx"
  timeout: 100ms
internal:
  callback_token: "internal-token-value"
http:
  read_header_timeout: 1s
  read_timeout: 5s
  write_timeout: 5s
  idle_timeout: 5s
  max_body_bytes: 1048576
pairing:
  invitation_ttl: 1m
  claim_timeout: 100ms
  claim_ttl: 1m
  max_failures: 3
requests:
  max_ttl: 1h
  forward_interval: 10ms
  lease: 1s
  retry_interval: 50ms
  max_attempts: 2
  batch_size: 5
revocation:
  ack_timeout: 100ms
presence:
  cache_ttl: 1s
cleanup:
  interval: %s
  pairing_token_retention: 1h
protocol:
  version: 1
log:
  level: "%s"
`, values.publicPort, values.internalPort, values.databaseURL, values.brokerURL, values.restURL, values.cleanup, values.logLevel)
	path := filepath.Join(t.TempDir(), "config.yaml")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestParseOptionsAcceptsSupportedFlagsAndRejectsInvalidArguments(t *testing.T) {
	var stderr bytes.Buffer
	opts, err := parseOptions([]string{"-config", "x.yaml", "-check-config", "-migrate-only", "-version"}, &stderr)
	if err != nil {
		t.Fatal(err)
	}
	if opts.configPath != "x.yaml" || !opts.checkConfig || !opts.migrateOnly || !opts.showVersion {
		t.Fatalf("opts = %+v", opts)
	}
	if _, err := parseOptions([]string{"-unknown"}, &stderr); err == nil {
		t.Fatal("expected flag error")
	}
	if _, err := parseOptions([]string{"positional"}, &stderr); err == nil {
		t.Fatal("expected positional error")
	}
}

func TestEntrypointPrintsVersionAndRejectsMissingConfigOrExtraArguments(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := run(context.Background(), []string{"-version"}, &stdout, &stderr); code != 0 {
		t.Fatalf("version code = %d", code)
	}
	if !strings.Contains(stdout.String(), version) {
		t.Fatalf("stdout = %q", stdout.String())
	}
	stdout.Reset()
	stderr.Reset()
	if code := run(context.Background(), []string{"-config", filepath.Join(t.TempDir(), "missing.yaml")}, &stdout, &stderr); code != 2 {
		t.Fatalf("missing config code = %d", code)
	}
	stdout.Reset()
	stderr.Reset()
	if code := run(context.Background(), []string{"-config", "x", "extra"}, &stdout, &stderr); code != 2 {
		t.Fatalf("extra args code = %d", code)
	}
}

func TestCheckConfigReportsMissingEnvironmentVariable(t *testing.T) {
	path := filepath.Join(t.TempDir(), "config.yaml")
	if err := os.WriteFile(path, []byte(`
listen: "127.0.0.1:1"
internal_listen: "127.0.0.1:2"
public_url: "https://access.example.com"
database_url: "${MISSING_DATABASE_URL}"
admin_token: "a"
host_provisioning_token: "b"
broker:
  url: "tcp://127.0.0.1:1883"
  username: "access"
  password: "c"
internal:
  callback_token: "d"
`), 0o600); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := run(context.Background(), []string{"-config", path}, &stdout, &stderr); code != 2 {
		t.Fatalf("code = %d", code)
	}
	if !strings.Contains(stderr.String(), "MISSING_DATABASE_URL") {
		t.Fatalf("stderr = %s", stderr.String())
	}
}

type safeBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *safeBuffer) Write(data []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(data)
}

func (b *safeBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

func waitFor(t *testing.T, timeout time.Duration, condition func() bool) error {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if condition() {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("condition not met within %s", timeout)
}

func postInternal(t *testing.T, url, body, token string) *http.Response {
	t.Helper()
	request, err := http.NewRequest(http.MethodPost, url, strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set(httpapi.InternalHeader, token)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	return response
}

func readBody(t *testing.T, response *http.Response) string {
	t.Helper()
	if response == nil {
		return ""
	}
	data, _ := ioReadAll(response.Body)
	_ = response.Body.Close()
	return data
}

func ioReadAll(reader interface{ Read([]byte) (int, error) }) (string, error) {
	var buffer bytes.Buffer
	_, err := buffer.ReadFrom(reader)
	return buffer.String(), err
}

func TestEntrypointPrintsVersionWithoutStartingServer(t *testing.T) {
	oldArgs := os.Args
	oldStdout := os.Stdout
	oldStderr := os.Stderr
	defer func() {
		os.Args = oldArgs
		os.Stdout = oldStdout
		os.Stderr = oldStderr
	}()
	tmp, err := os.CreateTemp(t.TempDir(), "stdout")
	if err != nil {
		t.Fatal(err)
	}
	defer tmp.Close()
	os.Args = []string{"whalerein-access", "-version"}
	os.Stdout = tmp
	os.Stderr = tmp
	if code := entrypoint(); code != 0 {
		t.Fatalf("entrypoint code = %d", code)
	}
	if _, err := tmp.Seek(0, 0); err != nil {
		t.Fatal(err)
	}
	output, err := io.ReadAll(tmp)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(output), version) {
		t.Fatalf("output = %q", output)
	}
}
