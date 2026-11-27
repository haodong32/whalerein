package route

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/control"
	"github.com/whalerein/whalerein/access/internal/routeproof"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

type enrollmentFake struct {
	enrollment store.ExternalEnrollment
	err        error
}

func (f enrollmentFake) Enrollment(context.Context, string) (store.ExternalEnrollment, error) {
	return f.enrollment, f.err
}

func fixture(t *testing.T) (ed25519.PublicKey, ed25519.PrivateKey, []byte, store.ExternalEnrollment) {
	t.Helper()
	signing, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	noise := make([]byte, 32)
	if _, err := rand.Read(noise); err != nil {
		t.Fatal(err)
	}
	return signing, private, noise, store.ExternalEnrollment{
		HostID: "host", WorkspaceID: "workspace", State: "enrollment_ready",
		HostSigningPublicKey: signing, HostNoisePublicKey: noise,
	}
}

func readyProof(signing ed25519.PrivateKey, noise []byte, now time.Time) *whalereinv1.HostReady {
	const service, environment, hostID, nodeID, incarnation, challenge = "svc", "env", "host", "node-a", "inc-1", "challenge"
	generation := uint64(3)
	signature := ed25519.Sign(signing, []byte(routeproof.Canonical(service, environment, hostID, nodeID, incarnation, challenge, generation, noise, now)))
	return &whalereinv1.HostReady{
		HostId: hostID, RouteGeneration: generation, RouteChallenge: challenge,
		RouteSignedAtUnixMs: now.UnixMilli(), RouteSignature: signature,
		RouteService: service, RouteEnvironment: environment,
		RouteSigningPublicKey: signing.Public().(ed25519.PublicKey), RouteNoisePublicKey: noise,
	}
}

func controlFixture(t *testing.T) (*control.Client, *[]byte) {
	t.Helper()
	body := make([]byte, 0)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		body = raw
		w.WriteHeader(http.StatusNoContent)
	}))
	t.Cleanup(server.Close)
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	return client, &body
}

func TestAttachVerifiesAndForwardsTheExactTuple(t *testing.T) {
	signing, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	client, captured := controlFixture(t)
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	if err := service.Attach(context.Background(), "host", readyProof(private, noise, now)); err != nil {
		t.Fatal(err)
	}
	attach := &whalereinv1.HostRouteAttach{}
	if err := protojson.Unmarshal(*captured, attach); err != nil {
		t.Fatal(err)
	}
	if attach.GetNodeId() != "node-a" || attach.GetGeneration() != 3 || !attach.GetNodeObserved() || attach.GetWorkspaceId() != "workspace" {
		t.Fatalf("forwarded attach = %#v", attach)
	}
	if string(attach.GetHostNoisePublicKey()) != string(noise) || string(attach.GetHostSigningPublicKey()) != string(signing) {
		t.Fatal("forwarded keys changed")
	}
}

func TestAttachRefusesWrongKeysClockAndDisabledEnrollment(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	client, _ := controlFixture(t)
	base := Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	proof := readyProof(private, noise, now)
	tampered := proto.Clone(proof).(*whalereinv1.HostReady)
	tampered.RouteNoisePublicKey = make([]byte, 32)
	if err := base.Attach(context.Background(), "host", tampered); err == nil {
		t.Fatal("wrong noise key accepted")
	}
	old := proto.Clone(proof).(*whalereinv1.HostReady)
	old.RouteSignedAtUnixMs = now.Add(-time.Hour).UnixMilli()
	if err := base.Attach(context.Background(), "host", old); err == nil {
		t.Fatal("stale signature accepted")
	}
	disabled := enrollment
	disabledAt := now
	disabled.DisabledAt = &disabledAt
	base.Enrollments = enrollmentFake{enrollment: disabled}
	if err := base.Attach(context.Background(), "host", proof); err == nil {
		t.Fatal("disabled enrollment attached")
	}
	base.Enrollments = enrollmentFake{err: errors.New("missing")}
	if err := base.Attach(context.Background(), "host", proof); err == nil {
		t.Fatal("missing enrollment attached")
	}
}

func TestAttachIgnoresOldHostsWithoutAProof(t *testing.T) {
	service := &Service{Control: nil}
	if err := service.Attach(context.Background(), "host", &whalereinv1.HostReady{HostId: "host"}); err != nil {
		t.Fatal(err)
	}
}

func TestAttachValidationBranches(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	client, _ := controlFixture(t)
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	proof := readyProof(private, noise, now)
	if err := service.Attach(context.Background(), "other", proof); err == nil {
		t.Fatal("host identity mismatch accepted")
	}
	empty := proto.Clone(proof).(*whalereinv1.HostReady)
	empty.RouteService = ""
	if err := service.Attach(context.Background(), "host", empty); err == nil {
		t.Fatal("empty service accepted")
	}
	bad := proto.Clone(proof).(*whalereinv1.HostReady)
	bad.RouteSignature = make([]byte, ed25519.SignatureSize)
	if err := service.Attach(context.Background(), "host", bad); err == nil {
		t.Fatal("invalid signature accepted")
	}
	if err := service.Attach(context.Background(), "host", &whalereinv1.HostReady{HostId: "host"}); err != nil {
		t.Fatalf("no-proof host: %v", err)
	}
	service.Enrollments = nil
	if err := service.Attach(context.Background(), "host", proof); err == nil {
		t.Fatal("missing enrollment reader accepted")
	}
	service.Enrollments = enrollmentFake{enrollment: enrollment}
	service.NodeID = "other"
	if err := service.Attach(context.Background(), "host", proof); err == nil {
		t.Fatal("wrong serving node accepted")
	}
}

func TestAttachBootstrapsRegistrationAfterAnEarlyAttachFailure(t *testing.T) {
	signing, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	attempts := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v1/nodes/hosts/attach":
			attempts++
			if attempts == 1 {
				http.Error(w, "not registered", http.StatusNotFound)
				return
			}
			w.WriteHeader(http.StatusNoContent)
		case "/api/v1/nodes/register":
			w.WriteHeader(http.StatusNoContent)
		default:
			t.Errorf("unexpected path %s", r.URL.Path)
		}
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	if err := service.Attach(context.Background(), "host", readyProof(private, noise, now)); err != nil {
		t.Fatal(err)
	}
	if attempts != 2 {
		t.Fatalf("attach attempts = %d", attempts)
	}
	_ = signing
}

func TestAttachReportsTheOriginalFailureWhenBootstrapAlsoFails(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	if err := service.Attach(context.Background(), "host", readyProof(private, noise, now)); err == nil {
		t.Fatal("attach failure was swallowed")
	}
}

func TestCloseForwardsTheExactOpenTuple(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	client, captured := controlFixture(t)
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	proof := readyProof(private, noise, now)
	proof.RouteClosing = true
	if err := service.Close(context.Background(), "host", proof); err != nil {
		t.Fatal(err)
	}
	closeMessage := &whalereinv1.HostRouteClose{}
	if err := protojson.Unmarshal(*captured, closeMessage); err != nil {
		t.Fatal(err)
	}
	if closeMessage.GetGeneration() != 3 || closeMessage.GetNodeId() != "node-a" || closeMessage.GetIncarnation() != "inc-1" {
		t.Fatalf("forwarded close = %#v", closeMessage)
	}
	if err := service.Close(context.Background(), "other", proof); err == nil {
		t.Fatal("close for another Host accepted")
	}
}

func TestCloseValidationBranches(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	client, _ := controlFixture(t)
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	proof := readyProof(private, noise, now)
	proof.RouteClosing = true
	if err := (&Service{}).Close(context.Background(), "host", proof); err != nil {
		t.Fatalf("no control: %v", err)
	}
	missing := proto.Clone(proof).(*whalereinv1.HostReady)
	missing.RouteGeneration = 0
	if err := service.Close(context.Background(), "host", missing); err != nil {
		t.Fatalf("no generation: %v", err)
	}
	service.Enrollments = nil
	if err := service.Close(context.Background(), "host", proof); err == nil {
		t.Fatal("nil enrollment reader closed")
	}
	service.Enrollments = enrollmentFake{err: errors.New("missing")}
	if err := service.Close(context.Background(), "host", proof); err == nil {
		t.Fatal("missing enrollment closed")
	}
	service.Enrollments = enrollmentFake{enrollment: enrollment}
	wrong := proto.Clone(proof).(*whalereinv1.HostReady)
	wrong.RouteNoisePublicKey = make([]byte, 32)
	if err := service.Close(context.Background(), "host", wrong); err == nil {
		t.Fatal("wrong key closed")
	}
	bad := proto.Clone(proof).(*whalereinv1.HostReady)
	bad.RouteSignature = make([]byte, ed25519.SignatureSize)
	if err := service.Close(context.Background(), "host", bad); err == nil {
		t.Fatal("invalid signature closed")
	}
}

func TestRouteAttemptsAreObserved(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	client, _ := controlFixture(t)
	var observations []string
	service := &Service{
		Enrollments: enrollmentFake{enrollment: enrollment}, Control: client,
		NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now },
		Observe: func(operation string, ok bool) {
			observations = append(observations, fmt.Sprintf("%s:%t", operation, ok))
		},
	}
	if err := service.Attach(context.Background(), "host", readyProof(private, noise, now)); err != nil {
		t.Fatal(err)
	}
	proof := readyProof(private, noise, now)
	proof.RouteClosing = true
	if err := service.Close(context.Background(), "host", proof); err != nil {
		t.Fatal(err)
	}
	if strings.Join(observations, ",") != "attach:true,close:true" {
		t.Fatalf("observations = %v", observations)
	}
}

func TestCloseQueuesAndReconcilesThroughInventory(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	failClose := true
	inventoryCalls := 0
	var inventory *whalereinv1.NodeInventory
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/hosts/close") && failClose {
			http.Error(w, "central restarting", http.StatusServiceUnavailable)
			return
		}
		if strings.HasSuffix(r.URL.Path, "/nodes/inventory") {
			inventoryCalls++
			inventory = &whalereinv1.NodeInventory{}
			raw, _ := io.ReadAll(r.Body)
			if err := protojson.Unmarshal(raw, inventory); err != nil {
				t.Errorf("inventory decode: %v", err)
			}
			payload, _ := protojson.Marshal(&whalereinv1.NodeInventoryAck{InventorySequence: 1})
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write(payload)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	pending := &pendingFake{}
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }, Pending: pending}
	ready := readyProof(private, noise, now)
	ready.RouteClosing = true
	if err := service.Close(context.Background(), "host", ready); err == nil {
		t.Fatal("close unexpectedly succeeded")
	}
	if len(pending.routes) != 1 || pending.routes[0].Kind != store.PendingClose {
		t.Fatalf("pending close = %#v", pending.routes)
	}
	if inventory != nil {
		t.Fatal("inventory was sent before close failed")
	}
	if err := service.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if inventory == nil || len(inventory.GetRoutes()) != 1 {
		t.Fatalf("inventory = %#v", inventory)
	}
	route := inventory.GetRoutes()[0]
	if route.GetHostId() != "host" || route.GetGeneration() != 3 || route.GetState() != whalereinv1.RouteState_ROUTE_CLOSED {
		t.Fatalf("inventoried route = %#v", route)
	}
	if err := service.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if inventoryCalls != 1 {
		t.Fatalf("reconcile resent a cleared inventory: %d calls", inventoryCalls)
	}
}

func TestReconcileHandlesEmptyAndFailedInventory(t *testing.T) {
	if err := (&Service{}).Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "central restarting", http.StatusServiceUnavailable)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	ready := readyProof(private, noise, now)
	ready.RouteClosing = true
	if err := service.Close(context.Background(), "host", ready); err == nil {
		t.Fatal("close unexpectedly succeeded")
	}
	if err := service.Reconcile(context.Background()); err == nil {
		t.Fatal("inventory failure was not reported")
	}
	service.mu.Lock()
	queued := len(service.pending)
	service.mu.Unlock()
	if queued != 1 {
		t.Fatalf("pending after failed inventory = %d", queued)
	}
}

func TestReconcileLoopStopsWithContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	(&Service{}).ReconcileLoop(ctx, time.Millisecond)
}

func TestAttachQueuesAndRetriesAfterCentralRecovery(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	failAttach := true
	attachCalls := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/hosts/attach") {
			attachCalls++
			if failAttach {
				http.Error(w, "central restarting", http.StatusServiceUnavailable)
				return
			}
		}
		if strings.HasSuffix(r.URL.Path, "/nodes/inventory") {
			http.Error(w, "no inventory expected", http.StatusInternalServerError)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	if err := service.Attach(context.Background(), "host", readyProof(private, noise, now)); err == nil {
		t.Fatal("attach unexpectedly succeeded")
	}
	sent := attachCalls
	failAttach = false
	if err := service.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if attachCalls != sent+1 {
		t.Fatalf("attach retries = %d, want %d", attachCalls, sent+1)
	}
	if err := service.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if attachCalls != sent+1 {
		t.Fatal("reconcile retried a cleared attach")
	}
}

type pendingFake struct {
	routes  []store.PendingRoute
	deleted []uint64
}

func (p *pendingFake) SavePendingRoute(_ context.Context, route store.PendingRoute) error {
	p.routes = append(p.routes, route)
	return nil
}
func (p *pendingFake) DeletePendingRoute(_ context.Context, hostID string, generation uint64) error {
	p.deleted = append(p.deleted, generation)
	kept := p.routes[:0]
	for _, route := range p.routes {
		if route.HostID == hostID && route.Generation <= generation {
			continue
		}
		kept = append(kept, route)
	}
	p.routes = kept
	return nil
}
func (p *pendingFake) ListPendingRoutes(context.Context) ([]store.PendingRoute, error) {
	return append([]store.PendingRoute(nil), p.routes...), nil
}

func TestPendingOperationsSurviveRestoreAndRetry(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	fail := true
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if fail {
			http.Error(w, "central restarting", http.StatusServiceUnavailable)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	pending := &pendingFake{}
	service := &Service{
		Enrollments: enrollmentFake{enrollment: enrollment}, Control: client,
		NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }, Pending: pending,
	}
	if err := service.Attach(context.Background(), "host", readyProof(private, noise, now)); err == nil {
		t.Fatal("attach unexpectedly succeeded")
	}
	if len(pending.routes) != 1 || pending.routes[0].Kind != store.PendingAttach {
		t.Fatalf("pending attach = %#v", pending.routes)
	}
	restored := &Service{
		Enrollments: enrollmentFake{enrollment: enrollment}, Control: client,
		NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }, Pending: pending,
	}
	if err := restored.Restore(context.Background()); err != nil {
		t.Fatal(err)
	}
	fail = false
	if err := restored.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(pending.routes) != 0 || len(pending.deleted) != 1 {
		t.Fatalf("restored retry did not clear: %#v %#v", pending.routes, pending.deleted)
	}
}

func TestPendingCountReportsBothQueues(t *testing.T) {
	var empty *Service
	if empty.PendingCount() != 0 {
		t.Fatal("nil service reported pending work")
	}
	service := &Service{
		pending:         map[string]*whalereinv1.HostRoute{"h1": {HostId: "h1"}},
		pendingAttaches: map[string]*whalereinv1.HostRouteAttach{"h2": {HostId: "h2"}},
	}
	if service.PendingCount() != 2 {
		t.Fatalf("pending count = %d", service.PendingCount())
	}
}

func TestSuccessfulCloseClearsAnEarlierPendingRetry(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	client, err := control.New(control.Config{BaseURL: server.URL, Token: "token", NodeID: "node-a", Incarnation: "inc-1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	pending := &pendingFake{routes: []store.PendingRoute{{
		HostID: "host", Kind: store.PendingClose, Generation: 3, Payload: []byte("close"),
	}}}
	service := &Service{
		Enrollments: enrollmentFake{enrollment: enrollment}, Control: client,
		NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }, Pending: pending,
		pending:         map[string]*whalereinv1.HostRoute{"host": {HostId: "host", Generation: 3}},
		pendingAttaches: map[string]*whalereinv1.HostRouteAttach{"host": {HostId: "host", Generation: 3}},
	}
	ready := readyProof(private, noise, now)
	ready.RouteClosing = true
	if err := service.Close(context.Background(), "host", ready); err != nil {
		t.Fatal(err)
	}
	if service.PendingCount() != 0 || len(pending.routes) != 0 {
		t.Fatalf("pending remained: count=%d store=%#v", service.PendingCount(), pending.routes)
	}
}

func TestOpenRouteIsRememberedAndRemovedOnClose(t *testing.T) {
	_, private, noise, enrollment := fixture(t)
	now := time.Now().UTC()
	client, _ := controlFixture(t)
	service := &Service{Enrollments: enrollmentFake{enrollment: enrollment}, Control: client, NodeID: "node-a", Incarnation: "inc-1", Now: func() time.Time { return now }}
	if err := service.Attach(context.Background(), "host", readyProof(private, noise, now)); err != nil {
		t.Fatal(err)
	}
	route, ok := service.Route("host")
	if !ok || route.GetGeneration() != 3 || route.GetState() != whalereinv1.RouteState_ROUTE_OPEN {
		t.Fatalf("open route = %#v %v", route, ok)
	}
	ready := readyProof(private, noise, now)
	ready.RouteClosing = true
	if err := service.Close(context.Background(), "host", ready); err != nil {
		t.Fatal(err)
	}
	if _, ok := service.Route("host"); ok {
		t.Fatal("closed route remained open")
	}
}
