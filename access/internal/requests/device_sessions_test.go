package requests

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type managedRequestFixture struct {
	ended bool
	err   error
}

func (m *managedRequestFixture) SessionCancellationReason(context.Context, string) (bool, error) {
	return m.ended, m.err
}

func (m *managedRequestFixture) PublishDuringDeviceSession(_ context.Context, _ string, publish func() error) error {
	if m.ended {
		return store.ErrDeviceSessionEnded
	}
	if m.err != nil {
		return m.err
	}
	return publish()
}

// A revoked session cannot publish queued work. A reauthorized caller
// receives a retained outcome or an unknown status and an exact cancel is sent.
func TestManagedRequestFencePreservesOutcomeAndCancellation(t *testing.T) {
	f := newFixture(t)
	ctx := context.Background()
	sessions := &managedRequestFixture{}
	f.service.sessions = sessions
	request := validRequest(f.now, "command")
	input := input(request)
	input.SessionID = "original-session"
	f.store.createErr = store.ErrDeviceSessionEnded
	if _, _, err := f.service.Submit(ctx, input); !errors.Is(err, ErrForbidden) {
		t.Fatalf("submission through ended permit: %v", err)
	}
	f.store.createErr = nil
	row, _, err := f.service.Submit(ctx, input)
	if err != nil || row.DeviceSessionID != input.SessionID {
		t.Fatalf("session scope was not retained: %+v %v", row, err)
	}
	f.store.claimRows = []store.Request{row}
	sessions.ended = true
	if err := f.service.ForwardDue(ctx); err != nil || len(f.broker.published) != 0 {
		t.Fatalf("ended session reached the broker count=%d err=%v", len(f.broker.published), err)
	}
	if recovered, err := f.service.Get(ctx, input.DeviceID, row.RequestID); err != nil ||
		recovered.Status != whalereinv1.OnlineStatus_unknown || recovered.CommandID != "command" {
		t.Fatalf("uncertain execution not preserved: %+v %v", recovered, err)
	}
	f.service.publishCancellation(ctx, row, f.now)
	if len(f.broker.presence) != 1 || !strings.Contains(f.broker.presence[0], `"reason":"cloud_session_ended"`) ||
		!strings.Contains(f.broker.presence[0], `"request_id":"req-1"`) {
		t.Fatalf("exact managed cancellation not published: %v", f.broker.presence)
	}
	row.Status = whalereinv1.OnlineStatus_succeeded
	f.store.rows[row.RequestID] = row
	if recovered, err := f.service.Get(ctx, input.DeviceID, row.RequestID); err != nil || recovered.Status != row.Status {
		t.Fatalf("confirmed result discarded: %+v %v", recovered, err)
	}
}
