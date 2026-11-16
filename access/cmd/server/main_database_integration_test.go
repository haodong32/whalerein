//go:build integration

package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/url"

	"strings"

	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"

	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// When configuration-check or migration-only mode succeeds, Access exits successfully without
// starting the public listener.
func TestCheckConfigAndMigrateOnlyExitWithoutServing(t *testing.T) {
	db := storetest.NewSchema(t)
	port := freePort(t)
	path := writeConfig(t, configValues{publicPort: port, internalPort: freePort(t), databaseURL: db.DSN})
	var stdout, stderr bytes.Buffer
	if code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr); code != 0 {
		t.Fatalf("check-config code = %d stderr=%s", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "config ok") || !strings.Contains(stdout.String(), "mqtts://access.example.com:443") {
		t.Fatalf("check output = %q", stdout.String())
	}
	stdout.Reset()
	stderr.Reset()
	if code := run(context.Background(), []string{"-config", path, "-migrate-only"}, &stdout, &stderr); code != 0 {
		t.Fatalf("migrate-only code = %d stderr=%s", code, stderr.String())
	}
}

// With PostgreSQL available and the broker unavailable, liveness is 200 but readiness
// is 503; public unauthenticated hosts and wrong-token callbacks return 401, while
// authenticated broker callbacks return 200 and no configured credential is logged.
func TestRunHealthyServerAgainstDatabase(t *testing.T) {
	db := storetest.NewSchema(t)
	publicPort := freePort(t)
	internalPort := freePort(t)
	path := writeConfig(t, configValues{publicPort: publicPort, internalPort: internalPort, databaseURL: db.DSN})
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var stdout, stderr safeBuffer
	done := make(chan int, 1)
	go func() { done <- run(ctx, []string{"-config", path}, &stdout, &stderr) }()

	base := fmt.Sprintf("http://127.0.0.1:%d", publicPort)
	if err := waitFor(t, 5*time.Second, func() bool {
		response, err := http.Get(base + "/healthz")
		if err != nil {
			return false
		}
		_ = response.Body.Close()
		return response.StatusCode == http.StatusOK
	}); err != nil {
		cancel()
		t.Fatalf("server not healthy: %v stderr=%s", err, stderr.String())
	}

	response, err := http.Get(base + "/healthz")
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusOK {
		t.Fatalf("health = %d", response.StatusCode)
	}
	_ = response.Body.Close()

	response, err = http.Get(base + "/readyz")
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("ready should fail without broker = %d", response.StatusCode)
	}
	_ = response.Body.Close()

	response, err = http.Get(base + "/v1/hosts")
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusUnauthorized {
		t.Fatalf("hosts = %d", response.StatusCode)
	}
	_ = response.Body.Close()

	response, err = http.Get(base + "/metrics")
	if err != nil {
		t.Fatal(err)
	}
	metrics, _ := ioReadAll(response.Body)
	_ = response.Body.Close()
	if response.StatusCode != http.StatusOK || !strings.Contains(metrics, "http_request_duration_ms") {
		t.Fatalf("metrics = %d %s", response.StatusCode, metrics)
	}

	internalBase := fmt.Sprintf("http://127.0.0.1:%d", internalPort)
	connectBody := "clientid=whalerein-host-h1&username=host%3Ah1&password=host-token-value"
	response = postInternal(t, internalBase+"/internal/broker/connect", connectBody, "internal-token-value")
	policyBody := readBody(t, response)
	if response.StatusCode != http.StatusOK || !strings.HasPrefix(policyBody, "whalerein/1\nok\nkind host\n") {
		t.Fatalf("policy callback = %d %s", response.StatusCode, policyBody)
	}
	response = postInternal(t, internalBase+"/internal/broker/connect", connectBody, "wrong")
	if response.StatusCode != http.StatusUnauthorized {
		t.Fatalf("bad internal token = %d", response.StatusCode)
	}

	cancel()
	select {
	case code := <-done:
		if code != 0 {
			t.Fatalf("run code = %d stderr=%s", code, stderr.String())
		}
	case <-time.After(10 * time.Second):
		t.Fatal("run did not stop")
	}
	logs := stderr.String()
	for _, secret := range []string{"admin-token-value", "host-token-value", "broker-password-value", "internal-token-value"} {
		if strings.Contains(logs, secret) {
			t.Fatalf("logs contain secret %q: %s", secret, logs)
		}
	}
}

// When the database is unreachable or a listener cannot bind, Access startup returns an error.
func TestRunReportsDatabaseAndListenerBindFailures(t *testing.T) {

	path := writeConfig(t, configValues{publicPort: freePort(t), internalPort: freePort(t), databaseURL: "postgres://postgres@127.0.0.1:1/none?sslmode=disable&connect_timeout=1"})
	var stdout, stderr bytes.Buffer
	if code := run(context.Background(), []string{"-config", path}, &stdout, &stderr); code != 1 {
		t.Fatalf("database failure code = %d stderr=%s", code, stderr.String())
	}

	db := storetest.NewSchema(t)
	occupied, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer occupied.Close()
	path = writeConfig(t, configValues{
		publicPort:   occupied.Addr().(*net.TCPAddr).Port,
		internalPort: freePort(t), databaseURL: db.DSN,
	})
	stdout.Reset()
	stderr.Reset()
	if code := run(context.Background(), []string{"-config", path}, &stdout, &stderr); code != 1 {
		t.Fatalf("bind failure code = %d stderr=%s", code, stderr.String())
	}

	path = writeConfig(t, configValues{publicPort: freePort(t), internalPort: freePort(t), databaseURL: db.DSN, cleanup: "not-a-duration"})
	stdout.Reset()
	stderr.Reset()
	if code := run(context.Background(), []string{"-config", path}, &stdout, &stderr); code != 2 {
		t.Fatalf("invalid config code = %d stderr=%s", code, stderr.String())
	}
}

// When HTTP serving uses a closed listener, the serving error is reported without blocking on a
// full error channel.
func TestServeReturnsListenerFailure(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	_ = listener.Close()
	server := &http.Server{Handler: http.NotFoundHandler()}
	errCh := make(chan error, 1)
	serve(server, listener, "test", errCh)
	select {
	case err := <-errCh:
		if !strings.Contains(err.Error(), "test server") {
			t.Fatalf("err = %v", err)
		}
	default:
		t.Fatal("expected serve error")
	}

	full := make(chan error, 1)
	full <- errors.New("occupied")
	serve(server, listener, "test", full)
}

// When cleanup receives a canceled context, the worker stops within the shutdown budget.
func TestRunCleanupStopsPromptlyWhenContextIsCanceled(t *testing.T) {
	s, _ := storetest.New(t)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	done := make(chan struct{})
	go func() {
		runCleanup(ctx, s, 0, 0, slog.New(slog.NewTextHandler(io.Discard, nil)), nil)
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("cleanup did not stop")
	}
}

// When expired requests and pairing tokens exist, cleanup processes them and records cleanup
// counts.
func TestRunCleanupClearsExpiredDataAndLogsCleanupCounts(t *testing.T) {
	s, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, "h1"); err != nil {
		t.Fatal(err)
	}
	if err := s.CreatePairedDevice(ctx, "h1", "d1", "fp", []byte("auth"), now); err != nil {
		t.Fatal(err)
	}
	if _, err := s.CreateRequest(ctx, store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", Status: whalereinv1.OnlineStatus_received,
		Kind: whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE, ExpiresAt: now.Add(-time.Hour),
		RequestBody: []byte("body"), ForwardState: store.ForwardNone, CreatedAt: now, UpdatedAt: now,
	}); err != nil {
		t.Fatal(err)
	}
	tokenHash := []byte("expired-token")
	if err := s.CreatePairingToken(ctx, tokenHash, "h1", now.Add(-2*time.Hour), now.Add(-3*time.Hour)); err != nil {
		t.Fatal(err)
	}
	var logs safeBuffer
	logger := slog.New(slog.NewTextHandler(&logs, nil))
	runCtx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	go func() {
		runCleanup(runCtx, s, 5*time.Millisecond, time.Hour, logger, nil)
		close(done)
	}()
	waitFor(t, time.Second, func() bool {
		return strings.Contains(logs.String(), "expired request bodies cleared")
	})
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("cleanup did not stop")
	}
	if !strings.Contains(logs.String(), "expired pairing tokens deleted") {
		t.Fatalf("logs = %s", logs.String())
	}
}

// When cleanup uses a closed store, it records the storage failure instead of silently succeeding.
func TestRunCleanupLogsStoreFailures(t *testing.T) {
	s, _ := storetest.New(t)
	s.Close()
	var logs safeBuffer
	logger := slog.New(slog.NewTextHandler(&logs, nil))
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	runCleanup(ctx, s, time.Millisecond, time.Hour, logger, nil)
	if !strings.Contains(logs.String(), "cleanup failed") {
		t.Fatalf("logs = %s", logs.String())
	}
}

// When broker or listener configuration is invalid, Access rejects startup.
func TestRunServerRejectsInvalidStartupConfiguration(t *testing.T) {
	newConfig := func(t *testing.T, db *storetest.DB) config.Config {
		t.Helper()
		path := writeConfig(t, configValues{publicPort: freePort(t), internalPort: freePort(t), databaseURL: db.DSN})
		cfg, err := config.Load(path)
		if err != nil {
			t.Fatal(err)
		}
		return cfg
	}
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))

	t.Run("rejects server startup when broker connector is invalid", func(t *testing.T) {
		db := storetest.NewSchema(t)
		cfg := newConfig(t, db)
		cfg.Broker.URL = "bad://host"
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if err := runServer(ctx, cfg, logger); err == nil || !strings.Contains(err.Error(), "broker") {
			t.Fatalf("err = %v", err)
		}
	})
	t.Run("rejects server startup when internal listener cannot bind", func(t *testing.T) {
		db := storetest.NewSchema(t)
		occupied, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		defer occupied.Close()
		cfg := newConfig(t, db)
		cfg.InternalListen = occupied.Addr().String()
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if err := runServer(ctx, cfg, logger); err == nil || !strings.Contains(err.Error(), "internal") {
			t.Fatalf("err = %v", err)
		}
	})
}

// When database migration fails, both migration-only and normal startup exit with failure.
func TestMigrationFailureStopsServerStartup(t *testing.T) {
	db := storetest.NewSchema(t)
	parsed, err := url.Parse(db.DSN)
	if err != nil {
		t.Fatal(err)
	}
	query := parsed.Query()
	query.Set("search_path", "missing_schema_for_test")
	parsed.RawQuery = query.Encode()
	path := writeConfig(t, configValues{publicPort: freePort(t), internalPort: freePort(t), databaseURL: parsed.String()})
	var stdout, stderr bytes.Buffer
	if code := run(context.Background(), []string{"-config", path, "-migrate-only"}, &stdout, &stderr); code != 1 {
		t.Fatalf("migrate-only failure code = %d stderr=%s", code, stderr.String())
	}
	stdout.Reset()
	stderr.Reset()
	if code := run(context.Background(), []string{"-config", path}, &stdout, &stderr); code != 1 {
		t.Fatalf("run migrate failure code = %d stderr=%s", code, stderr.String())
	}
}
