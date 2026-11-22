package requests

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

// TestSessionObservationAC29HeartbeatMapsMonotonicOutcomes covers the service
// side of late/hard-deadline and irreversible closure: the store's typed
// non-renewal is never reported as a successful renewal, and the exact
// request/device scope is preserved in the call.
func TestSessionObservationAC29HeartbeatMapsMonotonicOutcomes(t *testing.T) {
	for _, tc := range []struct {
		name    string
		failure store.HeartbeatFailure
		wantErr error
	}{
		{"not found", store.HeartbeatNotFound, ErrNotFound},
		{"foreign device", store.HeartbeatDeviceForeign, ErrNotFound},
		{"hard deadline", store.HeartbeatHardDeadline, ErrExpired},
		{"lease elapsed", store.HeartbeatLeaseElapsed, ErrExpired},
		{"terminal", store.HeartbeatTerminal, ErrConflict},
		{"session ended", store.HeartbeatSessionEnded, ErrConflict},
		{"unknown failure", store.HeartbeatFailure("unrecognized"), ErrConflict},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newFixture(t)
			// The row still looks active; only the store's typed decision says
			// renewal is impossible.
			f.store.heartbeatOutcome = store.HeartbeatOutcome{
				Request: store.Request{
					RequestID:        "req-1",
					DeviceID:         "d1",
					ClientLeaseState: store.ClientLeaseLive,
					ExpiresAt:        f.now.Add(time.Hour),
				},
				Failure: tc.failure,
			}
			row, err := f.service.Heartbeat(context.Background(), "d1", "session-1", "req-1")
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("heartbeat error = %v want %v", err, tc.wantErr)
			}
			if row.RequestID != "" {
				t.Fatalf("failed heartbeat returned a request: %+v", row)
			}
			if len(f.store.heartbeatCalls) != 1 {
				t.Fatalf("heartbeat calls = %d", len(f.store.heartbeatCalls))
			}
			call := f.store.heartbeatCalls[0]
			if call.deviceID != "d1" || call.requestID != "req-1" || call.lease != clientPollLease {
				t.Fatalf("heartbeat scope = %+v", call)
			}
		})
	}
}

func TestSessionObservationAC29HeartbeatRenewsOnlyExactRequest(t *testing.T) {
	f := newFixture(t)
	f.store.rows["req-1"] = store.Request{RequestID: "req-1", DeviceID: "d1"}
	f.store.rows["req-2"] = store.Request{RequestID: "req-2", DeviceID: "d1"}

	row, err := f.service.Heartbeat(context.Background(), "d1", "session-1", "req-1")
	if err != nil || row.RequestID != "req-1" {
		t.Fatalf("heartbeat = %+v err=%v", row, err)
	}
	if len(f.store.heartbeatCalls) != 1 || f.store.heartbeatCalls[0].requestID != "req-1" {
		t.Fatalf("heartbeat touched the wrong scope: %+v", f.store.heartbeatCalls)
	}
	if _, ok := f.store.rows["req-2"]; !ok {
		t.Fatal("heartbeat removed an unrelated request")
	}
}

func TestSessionObservationAC29HeartbeatStoreFailureIsReported(t *testing.T) {
	f := newFixture(t)
	f.store.heartbeatErr = errors.New("database unavailable")
	if _, err := f.service.Heartbeat(context.Background(), "d1", "", "req-1"); err == nil {
		t.Fatal("heartbeat store failure was reported as success")
	}
}
