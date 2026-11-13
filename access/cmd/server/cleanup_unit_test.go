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

	"github.com/whalerein/whalerein/access/internal/config"
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
			runCleanup(ctx, store, 0, 2*time.Hour, logger)
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
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture?sslmode=disable", restURL: "http://localhost:1"})
	var stdout, stderr bytes.Buffer
	code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr)
	if code != 0 || !strings.Contains(stdout.String(), "config ok:") {
		t.Fatalf("code=%d output=%q error=%q", code, stdout.String(), stderr.String())
	}
	if strings.Contains(stdout.String(), "broker-password-value") || strings.Contains(stdout.String(), "admin-token-value") {
		t.Fatal("config check exposed credentials")
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
			started := make(chan struct{}, 2)
			stopped := make(chan struct{}, 2)
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
				result <- runServices(ctx, &http.Server{}, &http.Server{}, public, internal, worker, worker, cleanup, slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)), time.Second)
			}()
			for i := 0; i < 2; i++ {
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
			for i := 0; i < 2; i++ {
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
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://%zz", restURL: "http://localhost:1"})
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
		{"invalid rest configuration", "rest", "configure EMQX REST", 1, 1},
		{"invalid disconnect configuration", "disconnect", "configure EMQX disconnect", 1, 1},
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
			if tc.fail == "rest" {
				cfg.EMQX.RESTURL = "://invalid"
			}
			if tc.fail == "disconnect" {
				cfg.EMQX.DisconnectMode = "invalid"
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
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture", restURL: "http://localhost:1"})
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
	runCleanup(ctx, recorder, time.Second, 0, slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)))
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
	finished := make(chan struct{}, 2)
	worker := func(context.Context) error { finished <- struct{}{}; return errors.New("synthetic worker failure") }
	var logs safeBuffer
	result := make(chan error, 1)
	go func() {
		result <- runServices(ctx, &http.Server{}, &http.Server{}, public, internal, worker, worker, func(ctx context.Context) { <-ctx.Done() }, slog.New(slog.NewTextHandler(&logs, nil)), time.Second)
	}()
	for i := 0; i < 2; i++ {
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
	if !strings.Contains(logs.String(), "broker loop stopped") || !strings.Contains(logs.String(), "forward loop stopped") {
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
		result <- runServices(ctx, publicServer, internalServer, listener, internal, worker, worker, func(ctx context.Context) { <-ctx.Done() }, slog.New(slog.NewTextHandler(&bytes.Buffer{}, nil)), 100*time.Millisecond)
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
