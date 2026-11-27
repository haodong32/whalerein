package authority

import (
	"context"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/control"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

type scopeFake struct{ scopes []store.AuthorityScope }

func (f scopeFake) AuthorityScopes(context.Context, time.Time) ([]store.AuthorityScope, error) {
	return f.scopes, nil
}

type publisherFake struct {
	hostID string
	permit *whalereinv1.ScopedAuthority
	err    error
}

func (p *publisherFake) PublishAuthority(_ context.Context, hostID string, permit *whalereinv1.ScopedAuthority) error {
	p.hostID, p.permit = hostID, permit
	return p.err
}

type routeFake map[string]*whalereinv1.HostRoute

func (f routeFake) Route(hostID string) (*whalereinv1.HostRoute, bool) {
	route, ok := f[hostID]
	return route, ok
}

func TestRenewRequestsTheExactScopeAndCachesThePermit(t *testing.T) {
	proof := make([]byte, 32)
	var received *whalereinv1.AuthorityRenewalRequest
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		received = &whalereinv1.AuthorityRenewalRequest{}
		raw, _ := io.ReadAll(r.Body)
		if err := protojson.Unmarshal(raw, received); err != nil {
			t.Errorf("decode request: %v", err)
		}
		raw, _ = protojson.Marshal(&whalereinv1.AuthorityRenewalResponse{Authority: &whalereinv1.ScopedAuthority{
			AuthorityId: "authority-1", SessionId: received.GetSessionId(),
			HostId: received.GetHostId(), DeviceId: received.GetDeviceId(),
			RouteGeneration: received.GetRouteGeneration(), ServingNodeId: "node-a",
		}})
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(raw)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	worker := &Worker{
		Scopes: scopeFake{scopes: []store.AuthorityScope{{
			HostID: "host", DeviceID: "device", SessionID: "session",
			GrantGeneration: 2, ProofPublicKey: proof,
		}}},
		Routes: routeFake{"host": {
			HostId: "host", NodeId: "node-a", Incarnation: "inc-1", Generation: 4,
			State: whalereinv1.RouteState_ROUTE_OPEN,
		}},
		Control: client, NodeID: "node-a", Incarnation: "inc-1", Environment: "test",
	}
	publisher := &publisherFake{}
	worker.Publisher = publisher
	if err := worker.Renew(context.Background()); err != nil {
		t.Fatal(err)
	}
	if received.GetHostId() != "host" || received.GetSessionId() != "session" ||
		received.GetGrantGeneration() != 2 || received.GetRouteGeneration() != 4 ||
		string(received.GetSessionProofPublicKey()) != string(proof) {
		t.Fatalf("request = %#v", received)
	}
	if publisher.hostID != "host" || publisher.permit.GetAuthorityId() != "authority-1" {
		t.Fatalf("published = %#v %#v", publisher.hostID, publisher.permit)
	}
}

func TestRenewSkipsSessionsWithoutACurrentRoute(t *testing.T) {
	worker := &Worker{
		Scopes: scopeFake{scopes: []store.AuthorityScope{{
			HostID: "host", DeviceID: "device", SessionID: "session", GrantGeneration: 1,
			ProofPublicKey: make([]byte, 32),
		}}},
		Routes: routeFake{}, NodeID: "node-a", Incarnation: "inc-1",
		Publisher: &publisherFake{},
	}
	if err := worker.Renew(context.Background()); err != nil {
		t.Fatal(err)
	}
	if worker.Publisher.(*publisherFake).permit != nil {
		t.Fatal("permit published without a route")
	}
}

func TestWorkerFailsClosedAndStopsWithContext(t *testing.T) {
	var empty *Worker
	if err := empty.Renew(context.Background()); err != nil {
		t.Fatal(err)
	}
	empty.Run(context.Background())
	worker := &Worker{Scopes: scopeFake{}, NodeID: "node-a"}
	if err := worker.Renew(context.Background()); err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	worker = &Worker{
		Scopes: scopeFake{scopes: []store.AuthorityScope{{
			HostID: "host", DeviceID: "device", SessionID: "session",
			GrantGeneration: 1, ProofPublicKey: make([]byte, 32),
		}}},
		Routes:  routeFake{"host": {HostId: "host", NodeId: "node-a", Incarnation: "inc-1", Generation: 1}},
		Control: client, NodeID: "node-a", Incarnation: "inc-1", Environment: "test",
		Publisher: &publisherFake{},
		Logger:    slog.New(slog.NewTextHandler(io.Discard, nil)),
	}
	if err := worker.Renew(context.Background()); err == nil {
		t.Fatal("failed permit request was treated as success")
	}
	if worker.Publisher.(*publisherFake).permit != nil {
		t.Fatal("failed request reached the publisher")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	worker.Run(ctx)
}

type scopeError struct{}

func (scopeError) AuthorityScopes(context.Context, time.Time) ([]store.AuthorityScope, error) {
	return nil, context.DeadlineExceeded
}

func TestWorkerReportsScopeErrorsAndSkipsForeignRoutes(t *testing.T) {
	worker := &Worker{Scopes: scopeError{}, Control: &control.Client{}, NodeID: "node-a", Incarnation: "inc-1"}
	if err := worker.Renew(context.Background()); err == nil {
		t.Fatal("scope error was hidden")
	}
	worker = &Worker{
		Scopes: scopeFake{scopes: []store.AuthorityScope{{
			HostID: "host", DeviceID: "device", SessionID: "session",
			GrantGeneration: 1, ProofPublicKey: make([]byte, 32),
		}}},
		Routes: routeFake{"host": {HostId: "host", NodeId: "node-b", Incarnation: "inc-1", Generation: 1}},
		NodeID: "node-a", Incarnation: "inc-1", Publisher: &publisherFake{},
	}
	if err := worker.Renew(context.Background()); err != nil {
		t.Fatal(err)
	}
	if worker.Publisher.(*publisherFake).permit != nil {
		t.Fatal("foreign route reached the publisher")
	}
}

func TestWorkerReportsPermitDeliveryFailureAfterCaching(t *testing.T) {
	proof := make([]byte, 32)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		raw, _ := protojson.Marshal(&whalereinv1.AuthorityRenewalResponse{Authority: &whalereinv1.ScopedAuthority{
			AuthorityId: "authority-1", SessionId: "session", HostId: "host", DeviceId: "device",
		}})
		_, _ = w.Write(raw)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	worker := &Worker{
		Scopes: scopeFake{scopes: []store.AuthorityScope{{
			HostID: "host", DeviceID: "device", SessionID: "session",
			GrantGeneration: 1, ProofPublicKey: proof,
		}}},
		Routes:  routeFake{"host": {HostId: "host", NodeId: "node-a", Incarnation: "inc-1", Generation: 1}},
		Control: client, Publisher: &publisherFake{err: context.DeadlineExceeded},
		NodeID: "node-a", Incarnation: "inc-1", Environment: "test",
		Logger: slog.New(slog.NewTextHandler(io.Discard, nil)),
	}
	if err := worker.Renew(context.Background()); err == nil {
		t.Fatal("delivery failure was hidden")
	}
	if worker.Publisher.(*publisherFake).permit == nil {
		t.Fatal("verified permit was not handed to the publisher")
	}
}

func TestRenewWithoutRoutesOrClockIsANoOp(t *testing.T) {
	publisher := &publisherFake{}
	worker := &Worker{
		Scopes: scopeFake{scopes: []store.AuthorityScope{{
			HostID: "host", DeviceID: "device", SessionID: "session",
			GrantGeneration: 1, ProofPublicKey: make([]byte, 32),
		}}},
		Control: &control.Client{}, Publisher: publisher,
		Now: func() time.Time { return time.Unix(1, 0) },
	}
	if err := worker.Renew(context.Background()); err != nil {
		t.Fatal(err)
	}
	if publisher.permit != nil {
		t.Fatal("permit published without a route source")
	}
}
