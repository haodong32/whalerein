package main

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/line"
	"github.com/whalerein/whalerein/access/internal/store"
)

type runLineStore struct{}

func (runLineStore) ListAdmissionGrants(context.Context) ([]store.AdmissionGrant, error) {
	return nil, nil
}
func (runLineStore) ListDeviceGrants(context.Context, string) ([]store.AdmissionGrant, error) {
	return nil, nil
}
func (runLineStore) AdmissionGrantFor(context.Context, string, string) (store.AdmissionGrant, error) {
	return store.AdmissionGrant{}, store.ErrNotFound
}

type runLineAdmission struct{}

func (runLineAdmission) Allow(context.Context, string, string) error { return nil }

func lineControl(t *testing.T, publishErr error, ops chan<- string) *broker.Control {
	t.Helper()
	var control *broker.Control
	control, err := broker.NewControl(func(_ context.Context, _ string, payload []byte) error {
		if publishErr != nil {
			return publishErr
		}
		op := ""
		id := ""
		for _, field := range strings.Split(string(payload), "\n") {
			if strings.HasPrefix(field, "op ") {
				op = strings.TrimPrefix(field, "op ")
			}
			if strings.HasPrefix(field, "id ") {
				id = strings.TrimPrefix(field, "id ")
			}
		}
		if ops != nil {
			ops <- op
		}
		control.HandleMessage(broker.ControlReplyFilter, []byte("whalerein/1\nid "+id+"\nok\nseq 1\n"))
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return control
}

func testLineManager(t *testing.T, control *broker.Control) *line.Manager {
	t.Helper()
	manager, err := line.New(line.Dependencies{
		Store: runLineStore{}, Control: control, Admission: runLineAdmission{},
		Presence: broker.NewPresence(time.Minute, diagnostics.NewMetrics(), slog.New(slog.NewTextHandler(io.Discard, nil))),
		Config:   line.Config{PolicyTTL: time.Second, Timeout: time.Second},
		Metrics:  diagnostics.NewMetrics(),
		Masker:   diagnostics.NewMasker(),
		Logger:   slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	return manager
}

func TestRunLineRefreshesPolicyAndLogsFailures(t *testing.T) {
	ops := make(chan string, 8)
	manager := testLineManager(t, lineControl(t, nil, ops))
	cfg := config.Default()
	cfg.Line.HeartbeatInterval = config.Duration(10 * time.Millisecond)
	cfg.Line.ReconcileInterval = config.Duration(10 * time.Millisecond)
	cfg.Line.PolicyTTL = config.Duration(time.Second)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- runLine(ctx, manager, cfg, slog.New(slog.NewTextHandler(io.Discard, nil))) }()
	seen := map[string]bool{}
	deadline := time.After(time.Second)
	for !seen["ping"] || !seen["snapshot"] {
		select {
		case op := <-ops:
			seen[op] = true
		case <-deadline:
			cancel()
			t.Fatalf("line ops = %v", seen)
		}
	}
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}

	failing := testLineManager(t, lineControl(t, errors.New("broker down"), nil))
	var logs bytes.Buffer
	ctx, cancel = context.WithCancel(context.Background())
	done = make(chan error, 1)
	go func() { done <- runLine(ctx, failing, cfg, slog.New(slog.NewTextHandler(&logs, nil))) }()
	time.Sleep(60 * time.Millisecond)
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(logs.String(), "line policy heartbeat failed") || !strings.Contains(logs.String(), "line policy reconcile failed") {
		t.Fatalf("logs = %s", logs.String())
	}
}
