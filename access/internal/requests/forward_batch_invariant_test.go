package requests

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Storage may fail after observing a partial lease batch. Forwarding must not
// publish or record outcomes for any row unless the batch read succeeded.
func TestForwardDueDoesNotPublishOrMarkPartialFailedLeaseBatch(t *testing.T) {
	f := newFixture(t)
	claimErr := errors.New("synthetic claim failure")
	f.store.claimRows = []store.Request{{
		RequestID: "fixture-request", HostID: "fixture-host", DeviceID: "fixture-device",
		Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND, Status: whalereinv1.OnlineStatus_received,
		ExpiresAt: f.now.Add(time.Hour),
	}}
	f.store.claimErr = claimErr
	if err := f.service.ForwardDue(context.Background()); !errors.Is(err, claimErr) {
		t.Fatal("claim failure was not returned to the caller")
	}
	if len(f.broker.published) != 0 || len(f.store.published) != 0 || len(f.store.failures) != 0 {
		t.Fatal("a partially observed batch was published or marked")
	}
}
