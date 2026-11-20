package main

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/exec"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

type cleanupRecorder struct {
	calls []string
	times []time.Time
	fail  bool
}

func (r *cleanupRecorder) record(name string, at time.Time) (int64, error) {
	r.calls = append(r.calls, name)
	r.times = append(r.times, at)
	if r.fail {
		return 0, errors.New("synthetic cleanup failure")
	}
	return 1, nil
}
func (r *cleanupRecorder) ClearExpiredBodies(_ context.Context, at time.Time) (int64, error) {
	return r.record("bodies", at)
}
func (r *cleanupRecorder) DeleteExpiredPairingTokens(_ context.Context, at time.Time) (int64, error) {
	return r.record("tokens", at)
}
func (r *cleanupRecorder) DeleteExpiredQueries(_ context.Context, at time.Time) (int64, error) {
	return r.record("queries", at)
}
func TestCleanupUsesIndependentRetentionCutoffsAndReportsFailures(t *testing.T) {
	for _, fail := range []bool{false, true} {
		t.Run(map[bool]string{false: "successful cleanup", true: "all operations fail"}[fail], func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			cancel()
			store := &cleanupRecorder{fail: fail}
			var output bytes.Buffer
			logger := slog.New(slog.NewTextHandler(&output, nil))
			before := time.Now().UTC()
			runCleanup(ctx, store, 0, 2*time.Hour, logger, nil)
			after := time.Now().UTC()
			if strings.Join(store.calls, ",") != "bodies,tokens,queries" {
				t.Fatalf("calls=%v", store.calls)
			}
			for i, offset := range []time.Duration{0, 2 * time.Hour, 24 * time.Hour} {
				if store.times[i].Before(before.Add(-offset)) || store.times[i].After(after.Add(-offset)) {
					t.Fatalf("invalid cutoff for %s", store.calls[i])
				}
			}
			if fail && !strings.Contains(output.String(), "cleanup failed") {
				t.Fatal("failure diagnostics absent")
			}
			if !fail && !strings.Contains(output.String(), "expired request bodies cleared") {
				t.Fatal("success diagnostics absent")
			}
		})
	}
}

type failedListener struct{}

func (failedListener) Accept() (net.Conn, error) {
	return nil, errors.New("synthetic listener failure")
}
func (failedListener) Close() error   { return nil }
func (failedListener) Addr() net.Addr { return &net.TCPAddr{} }
func TestServeReportsListenerFailureWithoutBlockingOnFullErrorChannel(t *testing.T) {
	for _, full := range []bool{false, true} {
		t.Run(map[bool]string{false: "available", true: "full"}[full], func(t *testing.T) {
			ch := make(chan error, 1)
			if full {
				ch <- errors.New("occupied")
			}
			done := make(chan struct{})
			go func() { serve(&http.Server{}, failedListener{}, "fixture", ch); close(done) }()
			select {
			case <-done:
			case <-time.After(time.Second):
				t.Fatal("serve blocked")
			}
			if !full {
				if got := <-ch; !strings.Contains(got.Error(), "fixture server") {
					t.Fatal(got)
				}
			}
		})
	}
}
func TestWaitWithTimeoutReturnsForCompletedOrBlockedWork(t *testing.T) {
	var done sync.WaitGroup
	waitWithTimeout(&done, time.Second)
	var pending sync.WaitGroup
	pending.Add(1)
	start := time.Now()
	waitWithTimeout(&pending, 10*time.Millisecond)
	pending.Done()
	if time.Since(start) < 5*time.Millisecond {
		t.Fatal("returned before timeout")
	}
}

func TestCheckConfigSucceedsWithoutConnectingToInfrastructure(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture?sslmode=disable"})
	var stdout, stderr bytes.Buffer
	code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr)
	if code != 0 || !strings.Contains(stdout.String(), "config ok:") {
		t.Fatalf("code=%d output=%q error=%q", code, stdout.String(), stderr.String())
	}
	if strings.Contains(stdout.String(), "broker-password-value") || strings.Contains(stdout.String(), "admin-token-value") {
		t.Fatal("config check exposed credentials")
	}
}

func TestRunRejectsInvalidLineTimingBeforeConnectingToInfrastructure(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture?sslmode=disable"})
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	body = bytes.ReplaceAll(body, []byte("heartbeat_interval: 5s"), []byte("heartbeat_interval: 10s"))
	if err := os.WriteFile(path, body, 0o600); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	var stdout, stderr bytes.Buffer
	if code := run(ctx, []string{"-config", path}, &stdout, &stderr); code != 2 || !strings.Contains(stderr.String(), "heartbeat_interval") {
		t.Fatal("invalid line timing was not rejected during configuration loading")
	}
}

type blockingListener struct {
	closed chan struct{}
	once   sync.Once
}

func (l *blockingListener) Accept() (net.Conn, error) { <-l.closed; return nil, net.ErrClosed }
func (l *blockingListener) Close() error              { l.once.Do(func() { close(l.closed) }); return nil }
func (l *blockingListener) Addr() net.Addr            { return &net.TCPAddr{} }
func TestRunServicesCancelsWorkersWhenContextStopsOrListenerFails(t *testing.T) {
	for _, failure := range []bool{false, true} {
		t.Run(map[bool]string{false: "context cancelled", true: "public listener fails"}[failure], func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			public := net.Listener(&blockingListener{closed: make(chan struct{})})
			if failure {
				public = failedListener{}
			}
			internal := &blockingListener{closed: make(chan struct{})}
			started := make(chan struct{}, 3)
			stopped := make(chan struct{}, 3)
			cleanupStopped := make(chan struct{})
			worker := func(ctx context.Context) error {
				started <- struct{}{}
				<-ctx.Done()
				stopped <- struct{}{}
				return nil
			}
			cleanup := func(ctx context.Context) { <-ctx.Done(); close(cleanupStopped) }
			result := make(chan error, 1)
			go func() {
				result <- runServices(ctx, &http.Server{}, &http.Server{}, public, internal, worker, worker, worker, cleanup, slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)), time.Second)
			}()
			for i := 0; i < 3; i++ {
				select {
				case <-started:
				case <-time.After(time.Second):
					t.Fatal("worker failed to start")
				}
			}
			if !failure {
				cancel()
			}
			select {
			case err := <-result:
				if failure && (err == nil || !strings.Contains(err.Error(), "public server")) {
					t.Fatalf("missing listener error: %v", err)
				}
				if !failure && err != nil {
					t.Fatal(err)
				}
			case <-time.After(2 * time.Second):
				t.Fatal("service failed to stop")
			}
			for i := 0; i < 3; i++ {
				select {
				case <-stopped:
				default:
					t.Fatal("worker not stopped")
				}
			}
			select {
			case <-cleanupStopped:
			case <-time.After(time.Second):
				t.Fatal("cleanup not cancelled")
			}
		})
	}
}

func TestMalformedDatabaseConfigurationFailsBeforeServerOrMigrationStarts(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://%zz"})
	for _, tc := range []struct {
		name string
		args []string
	}{
		{"server", []string{"-config", path}},
		{"migration", []string{"-config", path, "-migrate-only"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			code := run(context.Background(), tc.args, &stdout, &stderr)
			if code != 1 {
				t.Fatalf("expected failure before startup, got %d", code)
			}
			if strings.Contains(stdout.String(), "access listening") {
				t.Fatal("server started with invalid database config")
			}
		})
	}
}

func TestServerStartupFailsClosedAndReleasesOpenedStore(t *testing.T) {
	marker := errors.New("synthetic startup failure")
	for _, tc := range []struct {
		name, fail, wantError    string
		wantClosed, wantMigrated int
	}{
		{"open failure", "open", "open store", 0, 0},
		{"migration failure", "migration", "migrate store", 1, 1},
		{"invalid broker configuration", "broker", "configure broker", 1, 1},
		{"invalid public listener", "public-listen", "listen public", 1, 1},
		{"invalid internal listener", "internal-listen", "listen internal", 1, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			closed, migrated := 0, 0
			cfg := config.Default()
			cfg.DatabaseURL = "fixture"
			cfg.Broker.URL = "tcp://127.0.0.1:1"
			cfg.Listen = "127.0.0.1:0"
			cfg.InternalListen = "127.0.0.1:0"
			if tc.fail == "broker" {
				cfg.Broker.URL = "://invalid"
			}
			if tc.fail == "public-listen" {
				cfg.Listen = "not-an-address"
			}
			if tc.fail == "internal-listen" {
				cfg.InternalListen = "not-an-address"
			}
			ops := startupStoreOps{
				open: func(context.Context, string) (*store.Store, error) {
					if tc.fail == "open" {
						return nil, marker
					}
					return store.NewWithPool(nil), nil
				},
				migrate: func(context.Context, *store.Store) error {
					migrated++
					if tc.fail == "migration" {
						return marker
					}
					return nil
				},
				close: func(*store.Store) { closed++ },
			}
			var logs bytes.Buffer
			err := runServerWithStore(context.Background(), cfg, slog.New(slog.NewTextHandler(&logs, nil)), ops)
			if err == nil || !strings.Contains(err.Error(), tc.wantError) || closed != tc.wantClosed || migrated != tc.wantMigrated {
				t.Fatalf("error=%v closed=%d migrated=%d", err, closed, migrated)
			}
			if strings.Contains(logs.String(), "access listening") {
				t.Fatal("server listened after failed startup")
			}
		})
	}
}

func TestCommandModesReturnCorrectStatusAndReleaseMigrationStore(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture"})
	for _, tc := range []struct {
		name, mode, fail                 string
		wantCode, wantClosed, wantServed int
	}{
		{"migration succeeds", "migration", "", 0, 1, 0},
		{"migration open fails", "migration", "open", 1, 0, 0},
		{"migration execution fails", "migration", "migration", 1, 1, 0},
		{"server succeeds", "server", "", 0, 0, 1},
		{"server fails", "server", "server", 1, 0, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			closed, served := 0, 0
			args := []string{"-config", path}
			if tc.mode == "migration" {
				args = append(args, "-migrate-only")
			}
			ops := startupStoreOps{open: func(context.Context, string) (*store.Store, error) {
				if tc.fail == "open" {
					return nil, errors.New("synthetic open failure")
				}
				return store.NewWithPool(nil), nil
			}, migrate: func(context.Context, *store.Store) error {
				if tc.fail == "migration" {
					return errors.New("synthetic migration failure")
				}
				return nil
			}, close: func(value *store.Store) { closed++; defaultStartupStoreOps().close(value) }}
			serve := func(context.Context, config.Config, *slog.Logger) error {
				served++
				if tc.fail == "server" {
					return errors.New("synthetic server failure")
				}
				return nil
			}
			var stdout, stderr bytes.Buffer
			code := runWith(context.Background(), args, &stdout, &stderr, ops, serve)
			if code != tc.wantCode || closed != tc.wantClosed || served != tc.wantServed {
				t.Fatalf("code=%d closed=%d served=%d", code, closed, served)
			}
		})
	}
}

func TestCleanupUsesDefaultRetentionWhenUnset(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	recorder := &cleanupRecorder{}
	before := time.Now().UTC()
	runCleanup(ctx, recorder, time.Second, 0, slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)), nil)
	after := time.Now().UTC()
	if len(recorder.times) != 3 {
		t.Fatal("expected one cleanup pass")
	}
	for _, i := range []int{1, 2} {
		if recorder.times[i].Before(before.Add(-24*time.Hour)) || recorder.times[i].After(after.Add(-24*time.Hour)) {
			t.Fatal("default retention cutoff differs")
		}
	}
}

func TestServeDoesNotReportAnIntentionalServerClose(t *testing.T) {
	server := &http.Server{}
	if err := server.Close(); err != nil {
		t.Fatal(err)
	}
	errors := make(chan error, 1)
	serve(server, failedListener{}, "fixture", errors)
	select {
	case err := <-errors:
		t.Fatalf("intentional close reported as failure: %v", err)
	default:
	}
}

func TestRunServicesRecordsWorkerFailureAndContinuesUntilCancelled(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	public := &blockingListener{closed: make(chan struct{})}
	internal := &blockingListener{closed: make(chan struct{})}
	finished := make(chan struct{}, 3)
	worker := func(context.Context) error { finished <- struct{}{}; return errors.New("synthetic worker failure") }
	var logs safeBuffer
	result := make(chan error, 1)
	go func() {
		result <- runServices(ctx, &http.Server{}, &http.Server{}, public, internal, worker, worker, worker, func(ctx context.Context) { <-ctx.Done() }, slog.New(slog.NewTextHandler(&logs, nil)), time.Second)
	}()
	for i := 0; i < 3; i++ {
		select {
		case <-finished:
		case <-time.After(time.Second):
			t.Fatal("worker did not finish")
		}
	}
	cancel()
	select {
	case err := <-result:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("service did not stop")
	}
	if !strings.Contains(logs.String(), "broker loop stopped") || !strings.Contains(logs.String(), "forward loop stopped") || !strings.Contains(logs.String(), "line loop stopped") {
		t.Fatal("worker failures were not reported")
	}
}

func TestRunServicesReportsShutdownDeadlineWhenARequestStaysActive(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	entered, release := make(chan struct{}), make(chan struct{})
	handler := http.HandlerFunc(func(http.ResponseWriter, *http.Request) { close(entered); <-release })
	publicServer := &http.Server{Handler: handler}
	internalServer := &http.Server{}
	internal := &blockingListener{closed: make(chan struct{})}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	worker := func(ctx context.Context) error { <-ctx.Done(); return nil }
	result := make(chan error, 1)
	go func() {
		result <- runServices(ctx, publicServer, internalServer, listener, internal, worker, worker, worker, func(ctx context.Context) { <-ctx.Done() }, slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)), 100*time.Millisecond)
	}()
	clientDone := make(chan struct{})
	go func() {
		response, _ := http.Get("http://" + listener.Addr().String())
		if response != nil {
			response.Body.Close()
		}
		close(clientDone)
	}()
	select {
	case <-entered:
	case <-time.After(3 * time.Second):
		close(release)
		t.Fatal("handler did not start")
	}
	cancel()
	select {
	case err := <-result:
		if err == nil || !strings.Contains(err.Error(), "shutdown") {
			close(release)
			t.Fatalf("missing shutdown error: %v", err)
		}
	case <-time.After(2 * time.Second):
		close(release)
		t.Fatal("shutdown deadline did not finish")
	}
	close(release)
	_ = publicServer.Close()
	select {
	case <-clientDone:
	case <-time.After(3 * time.Second):
		t.Fatal("client did not exit")
	}
}

func TestMainVersionCommandExitsWithoutStartingServer(t *testing.T) {
	if os.Getenv("WHALEREIN_TEST_VERSION_CHILD") == "1" {
		os.Args = []string{"whalerein-access", "-version"}
		main()
		return
	}
	cmd := exec.Command(os.Args[0], "-test.run=^TestMainVersionCommandExitsWithoutStartingServer$")
	cmd.Env = append(os.Environ(), "WHALEREIN_TEST_VERSION_CHILD=1")
	output, err := cmd.CombinedOutput()
	if err != nil || !strings.Contains(string(output), version) {
		t.Fatal("version command did not exit successfully")
	}
}

func TestServeReportsListenerFailureAndWaitWithTimeoutObservesCompletion(t *testing.T) {
	errCh := make(chan error, 1)
	serve(&http.Server{}, failingListener{}, "probe", errCh)
	select {
	case err := <-errCh:
		if err == nil || !strings.Contains(err.Error(), "probe server") {
			t.Fatalf("listener error %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("listener failure was not reported")
	}
	serve(&http.Server{}, failingListener{}, "probe", make(chan error))
	serve(&http.Server{}, closedListenerOnce(t), "probe", errCh)

	var group sync.WaitGroup
	group.Add(1)
	group.Done()
	waitWithTimeout(&group, time.Millisecond)
}

// failingListener returns a synthetic accept error.
type failingListener struct{}

func (failingListener) Accept() (net.Conn, error) { return nil, errors.New("synthetic accept failure") }
func (failingListener) Close() error              { return nil }
func (failingListener) Addr() net.Addr            { return &net.TCPAddr{} }

func closedListenerOnce(t *testing.T) net.Listener {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	_ = listener.Close()
	return listener
}

func TestRunServerWithStoreStartsServersAndStopsOnCancellation(t *testing.T) {
	cfg := config.Default()
	cfg.DatabaseURL = "fixture"
	cfg.Broker.URL = "tcp://127.0.0.1:1"
	cfg.Listen = "127.0.0.1:0"
	cfg.InternalListen = "127.0.0.1:0"
	cfg.Requests.ForwardInterval = config.Duration(time.Hour)
	cfg.Cleanup.Interval = config.Duration(time.Hour)
	cfg.Line.HeartbeatInterval = config.Duration(time.Hour)
	cfg.Line.ReconcileInterval = config.Duration(time.Hour)
	cfg.Presence.CacheTTL = config.Duration(time.Second)

	closed := 0
	ops := startupStoreOps{
		open: func(context.Context, string) (*store.Store, error) {
			return store.NewWithPool(nil), nil
		},
		migrate: func(context.Context, *store.Store) error { return nil },
		close:   func(*store.Store) { closed++ },
		cleanup: func(ctx context.Context, _ cleanupStore, _, _ time.Duration, _ *slog.Logger, _ *diagnostics.Metrics) {
			<-ctx.Done()
		},
	}
	ctx, cancel := context.WithCancel(context.Background())
	result := make(chan error, 1)
	var logs bytes.Buffer
	go func() {
		result <- runServerWithStore(ctx, cfg, slog.New(slog.NewTextHandler(&logs, nil)), ops)
	}()
	deadline := time.Now().Add(5 * time.Second)
	for !strings.Contains(logs.String(), "access listening") && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	if !strings.Contains(logs.String(), "access listening") {
		t.Fatalf("server did not start: %s", logs.String())
	}
	cancel()
	select {
	case err := <-result:
		if err != nil {
			t.Fatalf("shutdown: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("server did not stop")
	}
	if closed != 1 {
		t.Fatalf("store closed %d times", closed)
	}
}

// catchUpRecorder reports a full batch once per operation, then a short batch.
// It models a backlog created by a long pause.
type catchUpRecorder struct {
	calls map[string]int
}

func (r *catchUpRecorder) record(name string) (int64, error) {
	r.calls[name]++
	if r.calls[name] == 1 {
		return store.CleanupBatchSize, nil
	}
	return 1, nil
}

func (r *catchUpRecorder) ClearExpiredBodies(context.Context, time.Time) (int64, error) {
	return r.record("bodies")
}
func (r *catchUpRecorder) DeleteExpiredPairingTokens(context.Context, time.Time) (int64, error) {
	return r.record("tokens")
}
func (r *catchUpRecorder) DeleteExpiredQueries(context.Context, time.Time) (int64, error) {
	return r.record("queries")
}

func TestCleanupDrainsABacklogImmediatelyWithAlertEvidence(t *testing.T) {
	// The first pass after a start or a suspended worker runs immediately and
	// repeats each bounded batch until it drains. The long interval guarantees
	// no second pass runs during the assertion window.
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	recorder := &catchUpRecorder{calls: map[string]int{}}
	metrics := diagnostics.NewMetrics()
	var logs bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&logs, nil))
	done := make(chan struct{})
	go func() {
		runCleanup(ctx, recorder, time.Hour, 2*time.Hour, logger, metrics)
		close(done)
	}()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if recorder.calls["bodies"] >= 2 && recorder.calls["tokens"] >= 2 && recorder.calls["queries"] >= 2 {
			break
		}
		time.Sleep(time.Millisecond)
	}
	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("cleanup did not stop")
	}

	for _, operation := range []string{"bodies", "tokens", "queries"} {
		if recorder.calls[operation] != 2 {
			t.Fatalf("%s batches = %d, want 2", operation, recorder.calls[operation])
		}
	}
	if got := testutil.ToFloat64(metrics.CleanupBacklogTotal); got != 3 {
		t.Fatalf("backlog evidence = %v, want 3", got)
	}
	if got := testutil.ToFloat64(metrics.CleanupBatches.WithLabelValues("bodies")); got != 2 {
		t.Fatalf("body batches metric = %v", got)
	}
	for _, operation := range []string{"bodies", "tokens", "queries"} {
		if got := testutil.ToFloat64(metrics.CleanupLastRun.WithLabelValues(operation)); got <= 0 {
			t.Fatalf("%s completion timestamp metric = %v", operation, got)
		}
	}
	if !strings.Contains(logs.String(), "more work remains") {
		t.Fatalf("backlog warning missing: %s", logs.String())
	}
}

func TestCheckConfigRejectsAnUnsupportedProtocolVersionLoudly(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture?sslmode=disable"})
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	updated := bytes.Replace(body, []byte("version: 1"), []byte("version: 2"), 1)
	if bytes.Equal(updated, body) {
		t.Fatal("fixture protocol version not replaced")
	}
	if err := os.WriteFile(path, updated, 0o600); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr)
	if code != 2 {
		t.Fatalf("exit = %d stdout=%q stderr=%q", code, stdout.String(), stderr.String())
	}
	if !strings.Contains(stderr.String(), "supported versions: 1-1") {
		t.Fatalf("startup did not name the supported range: %s", stderr.String())
	}
	if strings.Contains(stdout.String(), "config ok") {
		t.Fatal("check-config accepted an unsupported protocol version")
	}
}

func TestCheckConfigRejectsAMissingBrokerCAFile(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture?sslmode=disable", brokerURL: "tls://mqtt.internal:8883"})
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	updated := bytes.Replace(body, []byte("  password: \"broker-password-value\""), []byte("  password: \"broker-password-value\"\n  ca_file: \"/absent/ca.pem\""), 1)
	if err := os.WriteFile(path, updated, 0o600); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr)
	if code != 2 || !strings.Contains(stderr.String(), "broker.ca_file") {
		t.Fatalf("exit=%d stdout=%q stderr=%q", code, stdout.String(), stderr.String())
	}
}

func TestRunCleanupRepeatsOnItsIntervalUntilCancelled(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	recorder := &cleanupRecorder{}
	done := make(chan struct{})
	go func() {
		defer close(done)
		runCleanup(ctx, recorder, 5*time.Millisecond, time.Hour, slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)), nil)
	}()
	time.Sleep(30 * time.Millisecond)
	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("cleanup did not stop after cancellation")
	}
	if len(recorder.calls) <= 3 {
		t.Fatalf("expected more than the first pass, got %d calls", len(recorder.calls))
	}
}
