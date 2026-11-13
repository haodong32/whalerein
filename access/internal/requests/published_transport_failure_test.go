package requests

import (
	"context"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestRetryTransportFailureRequestsAtomicHistoryClassification(t *testing.T) {
	for _, failure := range []error{broker.ErrPeerTransportUnsupported, broker.ErrMessageBudget} {
		t.Run(failure.Error(), func(t *testing.T) {
			f := newFixture(t)
			f.store.claimRows = []store.Request{{RequestID: "r", HostID: "h1", DeviceID: "d1",
				Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND, Status: whalereinv1.OnlineStatus_received,
				PublishedOnce: true, ExpiresAt: f.now.Add(time.Minute)}}
			f.broker.err = failure
			if err := f.service.ForwardDue(context.Background()); err != nil {
				t.Fatal(err)
			}
			if len(f.store.applyCalls) != 1 || !f.store.applyCalls[0].TransportPreflight {
				t.Fatal("transport failure bypassed atomic publication-history classification")
			}

		})
	}
}
