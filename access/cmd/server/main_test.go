package main

import (
	"bytes"
	"context"
	"errors"
	"log/slog"

	"fmt"
	"io"

	"net"
	"net/http"

	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/control"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/httpapi"
	"github.com/whalerein/whalerein/access/internal/route"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
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
database_path: "%s"
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
line:
  policy_ttl: 10s
  reconcile_interval: 1s
  heartbeat_interval: 5s
  operation_timeout: 1s
admission:
  max_entries: 1000
  ttl: 1m
  load_timeout: 100ms
  max_background_loads: 16
internal:
  callback_token: "internal-token-value"
http:
  read_header_timeout: 1s
  read_timeout: 5s
  write_timeout: 5s
  idle_timeout: 5s
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
  cache_ttl: 10s
cleanup:
  interval: %s
  pairing_token_retention: 1h
protocol:
  version: 1
log:
  level: "%s"
`, values.publicPort, values.internalPort, values.databaseURL, values.brokerURL, values.cleanup, values.logLevel)
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
database_path: "${MISSING_DATABASE_URL}"
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
	request.Header.Set("Content-Type", "application/x-www-form-urlencoded")
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

func TestDefaultStartupStoreOpsFailClosedWithoutAPool(t *testing.T) {
	ops := defaultStartupStoreOps()
	if ops.open == nil || ops.migrate == nil || ops.close == nil || ops.cleanup == nil {
		t.Fatal("every startup store operation must be configured")
	}
	if err := ops.migrate(context.Background(), &store.Store{}); err == nil {
		t.Fatal("migrating a store without a pool must fail closed")
	}
	ops.close(&store.Store{})
}

func TestDrainCleanupTaskStopsOnAFailedStatement(t *testing.T) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	task := cleanupTask{
		operation: "probe",
		success:   "probe cleaned",
		failure:   "probe cleanup failed",
		cutoff:    func(now time.Time) time.Time { return now },
		run: func(context.Context, time.Time) (int64, error) {
			return 0, errors.New("synthetic failure")
		},
	}
	drainCleanupTask(context.Background(), task, time.Now().UTC(), logger, diagnostics.NewMetrics())
}

func TestRunServerRejectsInvalidClientAddressConfiguration(t *testing.T) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	for _, test := range []struct {
		name   string
		mutate func(*config.Config)
		want   string
	}{
		{name: "malformed trusted proxy", mutate: func(c *config.Config) {
			c.HTTP.ClientAddress.TrustedProxies = []string{"not-a-cidr"}
		}, want: "client address configuration"},
		{name: "invalid reference key", mutate: func(c *config.Config) {
			c.HTTP.ClientReference.Key = "not-base64!"
		}, want: "client reference configuration"},
	} {
		t.Run(test.name, func(t *testing.T) {
			var cfg config.Config
			test.mutate(&cfg)
			err := runServerWithStore(context.Background(), cfg, logger, defaultStartupStoreOps())
			if err == nil || !strings.Contains(err.Error(), test.want) {
				t.Fatalf("error = %v, want %q", err, test.want)
			}
		})
	}
}

func TestRunReportsInvalidConfigurationBeforeStarting(t *testing.T) {
	var stderr strings.Builder
	code := run(context.Background(), []string{"--config", "/does-not-exist/access.yaml"}, io.Discard, &stderr)
	if code != 2 || !strings.Contains(stderr.String(), "invalid configuration") {
		t.Fatalf("code=%d stderr=%q", code, stderr.String())
	}
}

func TestRunMigrateOnlyReportsStoreOpenFailure(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: freePort(t), internalPort: freePort(t), databaseURL: "/"})
	var stderr strings.Builder
	code := run(context.Background(), []string{"--config", path, "--migrate-only"}, io.Discard, &stderr)
	if code != 1 || !strings.Contains(stderr.String(), "migration failed") {
		t.Fatalf("code=%d stderr=%q", code, stderr.String())
	}
}

func TestRunRejectsInvalidClientReferenceBeforeServing(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: freePort(t), internalPort: freePort(t), databaseURL: "unused.db"})
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	raw = []byte(strings.Replace(string(raw), "http:\n", "http:\n  client_reference:\n    key: \"not-base64!\"\n", 1))
	if err := os.WriteFile(path, raw, 0o600); err != nil {
		t.Fatal(err)
	}
	var stderr strings.Builder
	code := run(context.Background(), []string{"--config", path}, io.Discard, &stderr)
	if code != 2 || !strings.Contains(stderr.String(), "invalid configuration") {
		t.Fatalf("code=%d stderr=%q", code, stderr.String())
	}
}

func TestRunReportsObservationOutletFailure(t *testing.T) {
	t.Setenv("WHALEREIN_BENCH_MODE", "unsupported")
	err := runServerWithStore(context.Background(), config.Config{}, slog.New(slog.NewTextHandler(io.Discard, nil)), defaultStartupStoreOps())
	if err == nil || !strings.Contains(err.Error(), "latency observation outlet") {
		t.Fatalf("error = %v", err)
	}
}

func TestRunServerReportsCentralControlConfiguration(t *testing.T) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	missing := config.Config{}
	missing.External.ControlURL = "https://central.example"
	missing.DatabasePath = "/"
	if err := runServerWithStore(context.Background(), missing, logger, defaultStartupStoreOps()); err == nil || !strings.Contains(err.Error(), "open store") {
		t.Fatalf("incomplete control config error = %v", err)
	}
	complete := config.Config{}
	complete.External.ControlURL = "https://central.example"
	complete.External.NodeID = "node-a"
	complete.External.NodeToken = "node-token"
	complete.External.Incarnation = "inc-1"
	complete.DatabasePath = filepath.Join(t.TempDir(), "access.db")
	if err := runServerWithStore(context.Background(), complete, logger, defaultStartupStoreOps()); err == nil || !strings.Contains(err.Error(), "configure broker") {
		t.Fatalf("control client startup error = %v", err)
	}
	invalid := complete
	invalid.External.NodeToken = ""
	invalid.DatabasePath = filepath.Join(t.TempDir(), "access.db")
	if err := runServerWithStore(context.Background(), invalid, logger, defaultStartupStoreOps()); err == nil || !strings.Contains(err.Error(), "central node client") {
		t.Fatalf("control client validation error = %v", err)
	}
}

func TestStartControlReporterStartsAndStopsWithContext(t *testing.T) {
	client, err := control.New(control.Config{BaseURL: "http://127.0.0.1:1", Token: "t", NodeID: "n", Incarnation: "i"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	startControlReporter(ctx, client)
	startControlReporter(ctx, nil)
	cancel()
}

func TestProcessHostReadyClosingDoesNotAdvanceEnrollment(t *testing.T) {
	enrolled := false
	replayed := false
	err := processHostReady(context.Background(), "host-1", &whalereinv1.HostReady{HostId: "host-1", RouteClosing: true},
		&route.Service{},
		func(context.Context, string) error { replayed = true; return nil },
		func(context.Context, string) error { enrolled = true; return nil })
	if err != nil || replayed || enrolled {
		t.Fatalf("closing ready = %v replayed=%v enrolled=%v", err, replayed, enrolled)
	}
	err = processHostReady(context.Background(), "host-1", &whalereinv1.HostReady{HostId: "host-1"},
		&route.Service{},
		func(context.Context, string) error { replayed = true; return nil },
		func(context.Context, string) error { enrolled = true; return nil })
	if err != nil || !replayed || !enrolled {
		t.Fatalf("normal ready = %v replayed=%v enrolled=%v", err, replayed, enrolled)
	}
}

func TestObserveRouteCountsSuccessAndFailure(t *testing.T) {
	metrics := diagnostics.NewMetrics()
	observeRoute(metrics, "attach", true)
	observeRoute(metrics, "attach", false)
	observeRoute(metrics, "close", true)
	if got := testutil.ToFloat64(metrics.RouteAttach.WithLabelValues("success")); got != 1 {
		t.Fatalf("attach success = %v", got)
	}
	if got := testutil.ToFloat64(metrics.RouteAttach.WithLabelValues("failure")); got != 1 {
		t.Fatalf("attach failure = %v", got)
	}
	if got := testutil.ToFloat64(metrics.RouteClose.WithLabelValues("success")); got != 1 {
		t.Fatalf("close success = %v", got)
	}
}

func TestEarliestSessionExpiryMetricUsesTheStoreAggregate(t *testing.T) {
	ctx := context.Background()
	data, err := store.Open(ctx, filepath.Join(t.TempDir(), "access.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if got := earliestSessionExpiryUnix(data); got != 0 {
		t.Fatalf("empty expiry = %v", got)
	}
	expires := time.Now().UTC().Add(time.Hour).Truncate(time.Second)
	if err := data.InstallDeviceSession(ctx, store.DeviceSession{
		ID: "session", WorkspaceID: "workspace", RegionID: "region", DeviceID: "device",
		KeyFingerprint: "fingerprint", AuthorityRevision: "revision",
		CredentialHash: bytes.Repeat([]byte{1}, 32), Revision: 1, ExpiresAt: expires,
	}); err != nil {
		t.Fatal(err)
	}
	if got := earliestSessionExpiryUnix(data); got != float64(expires.Unix()) {
		t.Fatalf("expiry = %v, want %v", got, expires.Unix())
	}
}

func TestPendingRouteMetricHandlesNilAndQueue(t *testing.T) {
	if got := pendingRoutes(nil); got != 0 {
		t.Fatalf("nil pending = %v", got)
	}
	service := &route.Service{}
	if got := pendingRoutes(service); got != 0 {
		t.Fatalf("empty pending = %v", got)
	}
}

func TestStartAuthorityWorkerRequiresCompleteConfiguration(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	startAuthorityWorker(ctx, nil, nil, nil, nil, config.Config{}, nil)
	startAuthorityWorker(ctx, &control.Client{}, nil, nil, nil, config.Config{}, nil)
	cfg := config.Config{}
	cfg.External.SessionEnvironment = "production"
	startAuthorityWorker(ctx, &control.Client{}, nil, nil, nil, cfg, nil)
}

func TestAuthorityKeyForTrustIsEmptyWithoutKeys(t *testing.T) {
	if authorityKeyForTrust(external.Trust{}) != nil {
		t.Fatal("empty trust produced a key")
	}
	key := make([]byte, 32)
	got := authorityKeyForTrust(external.Trust{Keys: []external.VerificationKey{{PublicKey: key}}})
	if len(got) != 32 {
		t.Fatalf("key = %d", len(got))
	}
}

func TestAuthorityOptionsAcceptEmptyAndConfiguredTrust(t *testing.T) {
	if authorityOptions(external.Trust{}) == nil {
		t.Fatal("empty trust produced no option")
	}
	key := make([]byte, 32)
	if authorityOptions(external.Trust{Keys: []external.VerificationKey{{PublicKey: key}}}) == nil {
		t.Fatal("configured trust produced no option")
	}
}
