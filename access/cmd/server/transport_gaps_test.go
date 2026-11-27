package main

import (
	"bytes"
	"context"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

// fullBatchRecorder always returns a full cleanup batch, modelling a backlog
// that cannot drain within one tick.
type fullBatchRecorder struct {
	calls int
}

func (r *fullBatchRecorder) record() (int64, error) {
	r.calls++
	return store.CleanupBatchSize, nil
}

func (r *fullBatchRecorder) ClearExpiredBodies(context.Context, time.Time) (int64, error) {
	return r.record()
}
func (r *fullBatchRecorder) DeleteExpiredPairingTokens(context.Context, time.Time) (int64, error) {
	return r.record()
}
func (r *fullBatchRecorder) DeleteExpiredQueries(context.Context, time.Time) (int64, error) {
	return r.record()
}

func TestDrainCleanupTaskStopsAtRoundLimitOrCancellation(t *testing.T) {
	now := time.Now().UTC()
	task := cleanupTask{
		operation: "bodies",
		success:   "expired request bodies cleared",
		failure:   "expired request body cleanup failed",
		cutoff:    func(time.Time) time.Time { return now },
	}
	for _, tc := range []struct {
		name         string
		ctx          context.Context
		wantLog      string
		wantCalls    int
		wantBacklogs int
	}{
		{name: "round limit", ctx: context.Background(), wantLog: "round limit", wantCalls: maxCleanupRounds, wantBacklogs: maxCleanupRounds},
		{name: "cancelled between rounds", ctx: func() context.Context {
			ctx, cancel := context.WithCancel(context.Background())
			cancel()
			return ctx
		}(), wantLog: "more work remains", wantCalls: 1, wantBacklogs: 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			recorder := &fullBatchRecorder{}
			task.run = func(context.Context, time.Time) (int64, error) {
				return recorder.record()
			}
			var logs bytes.Buffer
			metrics := diagnostics.NewMetrics()
			drainCleanupTask(tc.ctx, task, now, slog.New(slog.NewTextHandler(&logs, nil)), metrics)
			if recorder.calls != tc.wantCalls {
				t.Fatalf("batch calls = %d, want %d", recorder.calls, tc.wantCalls)
			}
			if !strings.Contains(logs.String(), tc.wantLog) {
				t.Fatalf("missing %q in %s", tc.wantLog, logs.String())
			}
			if got := testutil.ToFloat64(metrics.CleanupBacklogTotal); got != float64(tc.wantBacklogs) {
				t.Fatalf("backlog evidence = %v", got)
			}
		})
	}
}

// An invalid transport profile must fail startup before the database or any
// listener is opened: the process cannot claim readiness with an unknown
// budget.
func TestRunServerRejectsInvalidTransportBeforeOpeningStore(t *testing.T) {
	cfg := config.Default()
	cfg.DatabasePath = "fixture"
	cfg.Broker.URL = "tcp://127.0.0.1:1"
	cfg.Transport = config.TransportConfig{Profile: "not-a-profile"}
	opened := false
	ops := startupStoreOps{
		open: func(context.Context, string) (*store.Store, error) {
			opened = true
			return store.NewWithPool(nil), nil
		},
		migrate: func(context.Context, *store.Store) error { return nil },
		close:   func(*store.Store) {},
	}
	var logs bytes.Buffer
	err := runServerWithStore(context.Background(), cfg, slog.New(slog.NewTextHandler(&logs, nil)), ops)
	if err == nil || !strings.Contains(err.Error(), "transport configuration") {
		t.Fatalf("startup error = %v", err)
	}
	if opened {
		t.Fatal("store opened before transport validation")
	}
}
