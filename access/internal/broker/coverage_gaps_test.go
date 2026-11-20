package broker

import (
	"context"
	"crypto/ed25519"
	"errors"
	"io"
	"log/slog"
	"os"
	"sync"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/paho"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func discardLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

func TestTransportRejectionsUseBoundedCodes(t *testing.T) {
	rejected := transportReject("not-a-code", errors.New("failure"))
	var transportErr *TransportError
	if !errors.As(rejected, &transportErr) || transportErr.Code != transportCodeHeader {
		t.Fatalf("unknown code = %+v", rejected)
	}
	if transportCodeOf(errors.New("plain failure")) != transportCodeHeader {
		t.Fatal("plain error did not map to the header code")
	}
	empty := &TransportError{}
	if empty.Error() != "logical transport rejected" || empty.Unwrap() != nil {
		t.Fatalf("empty transport error = %q", empty.Error())
	}
}

func TestNegotiateBudgetKeepsTheStricterOfEachLimit(t *testing.T) {
	local := profileBudget(t, "default")
	compact := profileBudget(t, "compact")
	roomy := profileBudget(t, "roomy")
	zero := config.TransportEffective{}
	intersectedCompact := compact
	intersectedCompact.FragmentCount = 274
	narrow := local
	narrow.FragmentPayloadBytes = local.FragmentPayloadBytes / 2
	tightened := local
	tightened.EnvelopeBytes = 8 << 20
	tightened.FragmentPayloadBytes = 32 << 10
	tightened.FragmentCount = 256
	tightened.ReservedBytes = 8 << 20
	wantTightened := tightened
	for name, tc := range map[string]struct {
		local, advertised, want config.TransportEffective
	}{
		"unregistered peer":       {local, zero, local},
		"smaller peer envelope":   {local, compact, intersectedCompact},
		"larger peer envelope":    {local, roomy, local},
		"equal envelope narrower": {local, narrow, narrow},
		"equal envelope wider": {local, func() config.TransportEffective {
			wider := local
			wider.FragmentPayloadBytes++
			return wider
		}(), local},
		"peer tightens width and envelope": {local, tightened, wantTightened},
	} {
		t.Run(name, func(t *testing.T) {
			if got := negotiateBudget(tc.local, tc.advertised); got != tc.want {
				t.Fatalf("negotiated = %+v, want %+v", got, tc.want)
			}
		})
	}
}

func TestReceiverBudgetsFenceSupersededConnectionsAndRoutes(t *testing.T) {
	registry := NewReceiverBudgets()
	host := receiverRoute{kind: "host", hostID: "h1"}
	registry.NoteConnection(host, 5, true)
	if !registry.Register(host, 5, 1, profileBudget(t, "default")) {
		t.Fatal("current advertisement rejected")
	}
	if registry.Register(host, 4, 2, profileBudget(t, "roomy")) {
		t.Fatal("older connection overwrote the current one")
	}

	// A broker device notification is not host-scoped: every route for that
	// device is invalidated so a reconnect must re-advertise.
	first := receiverRoute{kind: "device", hostID: "h1", deviceID: "d1"}
	second := receiverRoute{kind: "device", hostID: "h2", deviceID: "d1"}
	for _, route := range []receiverRoute{first, second} {
		if !registry.Register(route, 0, 1, profileBudget(t, "compact")) {
			t.Fatalf("device advertisement %+v rejected", route)
		}
	}
	registry.NoteConnection(receiverRoute{kind: "device", deviceID: "d1"}, 0, true)
	if registry.Count() != 1 {
		t.Fatalf("device reconnect retained %d routes", registry.Count())
	}

	if _, ok := registry.Lookup("not-a-topic"); ok {
		t.Fatal("invalid topic produced a negotiated budget")
	}
	if _, ok := registry.Lookup(ResultTopic("h1")); ok {
		t.Fatal("result topic produced a receiver budget")
	}
	registry.Clear(host)
	if registry.Count() != 0 {
		t.Fatal("clear retained the route")
	}
}

func TestReceiverBudgetNilRegistryIsInert(t *testing.T) {
	var registry *ReceiverBudgets
	registry.NoteConnection(receiverRoute{kind: "host", hostID: "h1"}, 1, true)
	if registry.Register(receiverRoute{kind: "host", hostID: "h1"}, 1, 1, config.TransportEffective{}) {
		t.Fatal("nil registry accepted an advertisement")
	}
	if _, ok := registry.Lookup(RequestTopic("h1")); ok {
		t.Fatal("nil registry produced a budget")
	}
	registry.Clear(receiverRoute{kind: "host", hostID: "h1"})
	registry.Reset()
	if registry.Count() != 0 {
		t.Fatal("nil registry count")
	}
}

// newBudgetRecordingConnector exposes the registry installed during New.
type budgetRecordingConnector struct {
	*fakeConnector
	installed *ReceiverBudgets
}

func (c *budgetRecordingConnector) SetReceiverBudgets(receivers *ReceiverBudgets) {
	c.installed = receivers
}

func TestNewInstallsReceiverBudgetsOnAwareConnectors(t *testing.T) {
	connector := &budgetRecordingConnector{fakeConnector: &fakeConnector{sessions: []*fakeSession{newFakeSession()}}}
	service, err := New(Dependencies{
		Connector: connector,
		Presence:  newPresence(),
		Logger:    discardLogger(),
	})
	if err != nil {
		t.Fatal(err)
	}
	if connector.installed == nil || connector.installed != service.Receivers() {
		t.Fatal("connector did not receive the shared registry")
	}
	if service.Available() {
		t.Fatal("empty presence reported available")
	}
}

func TestHandleNotificationIgnoresNilAndBindsAdvertisement(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	service.handleNotification(nil)

	service.handleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: time.Now().UnixMilli()})
	// The host is connected but HostReady has not confirmed subscriptions, so
	// no advertisement may bind yet.
	service.registerHostAdvertisement("h1", defaultAdvertisement(t, 1))
	if service.Receivers().Count() != 0 {
		t.Fatal("unready host bound a receive budget")
	}

	readyAt := time.Now().UTC()
	service.handleReady(hostReadyResult(t, "h1", readyAt, defaultAdvertisement(t, 1)))
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok {
		t.Fatal("ready host did not bind its budget")
	}
	// A replayed generation is ignored without refreshing the entry.
	service.registerHostAdvertisement("h1", defaultAdvertisement(t, 1))
	if budget, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok || budget.Profile != "default" {
		t.Fatalf("replayed generation changed the budget: %+v/%v", budget, ok)
	}
	// Contradictory metadata clears the entry instead of leaving stale
	// authority. The named profile stays valid, but the fragment count no
	// longer covers the overridden envelope.
	invalid := defaultAdvertisement(t, 2)
	invalid.Budget.EnvelopeBytes = 17 << 20
	service.registerHostAdvertisement("h1", invalid)
	if service.Receivers().Count() != 0 {
		t.Fatal("invalid advertisement left a stale route")
	}
	service.handleReady(hostReadyResult(t, "h1", readyAt.Add(time.Millisecond), defaultAdvertisement(t, 2)))
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok {
		t.Fatal("fresh advertisement after a rejection was not registered")
	}
	service.registerHostAdvertisement("h1", nil)
	if service.Receivers().Count() != 0 {
		t.Fatal("missing advertisement left a stale route")
	}
}

func hostReceiverRoute(hostID string) receiverRoute {
	return receiverRoute{kind: "host", hostID: hostID}
}

func hostReadyResult(t *testing.T, hostID string, at time.Time, advertisement *whalereinv1.ReceiveAdvertisement) *whalereinv1.HostResult {
	t.Helper()
	body, err := proto.Marshal(&whalereinv1.HostReady{
		HostId: hostID, ConnectedAtUnixMs: at.UnixMilli(), TransportVersion: transportVersion, Receive: advertisement,
	})
	if err != nil {
		t.Fatal(err)
	}
	return &whalereinv1.HostResult{HostId: hostID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body}
}

func TestHostReadyValidatesIdentityAndAuthorizationKey(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	now := time.Now().UTC()

	// A ready proof without a host id falls back to the result host, and a
	// mismatched result host is rejected.
	service.handleReady(&whalereinv1.HostResult{
		HostId: "h1",
		Status: whalereinv1.OnlineStatus_succeeded,
		ResultBody: func() []byte {
			body, _ := proto.Marshal(&whalereinv1.HostReady{ConnectedAtUnixMs: now.UnixMilli()})
			return body
		}(),
	})
	if _, _, ready, known := service.Presence().HostConnection("h1"); !known || !ready {
		t.Fatal("result host fallback did not mark the host ready")
	}

	// A handler failure and a wrong key length must each be contained without
	// losing readiness.
	keyErr := errors.New("key store unavailable")
	service.SetHostKeyHandler(func(context.Context, string, []byte) error { return keyErr })
	keyResult := hostReadyResult(t, "h1", now, nil)
	var ready whalereinv1.HostReady
	if err := proto.Unmarshal(keyResult.ResultBody, &ready); err != nil {
		t.Fatal(err)
	}
	ready.AuthorizationPublicKey = []byte("short")
	keyResult.ResultBody, _ = proto.Marshal(&ready)
	service.handleReady(keyResult)

	keyResult = hostReadyResult(t, "h1", now, nil)
	if err := proto.Unmarshal(keyResult.ResultBody, &ready); err != nil {
		t.Fatal(err)
	}
	ready.AuthorizationPublicKey = ed25519.PublicKey(make([]byte, ed25519.PublicKeySize))
	keyResult.ResultBody, _ = proto.Marshal(&ready)
	service.handleReady(keyResult)

	// The result handler error path is counted without changing the outcome.
	service.SetResultHandler(func(context.Context, string, *whalereinv1.HostResult) error { return keyErr })
	result := &whalereinv1.HostResult{HostId: "h1", RequestId: "r1", Status: whalereinv1.OnlineStatus_succeeded}
	payload, err := proto.Marshal(result)
	if err != nil {
		t.Fatal(err)
	}
	service.HandleMessage(ResultTopic("h1"), payload)

	if budget, ok := service.Receivers().Lookup(RequestTopic("h1")); ok || budget.Profile != "" {
		t.Fatalf("readiness without an advertisement registered %+v", budget)
	}
}

func TestPahoConnectorDefaultsAddressAndTransportBudget(t *testing.T) {
	for name, tc := range map[string]struct {
		url     string
		address string
		useTLS  bool
	}{
		"plain default port": {"tcp://mqtt.example.com", "mqtt.example.com:1883", false},
		"tls default port":   {"tls://mqtt.example.com", "mqtt.example.com:8883", true},
	} {
		t.Run(name, func(t *testing.T) {
			connector, err := NewPahoConnectorWithTransport(config.BrokerConfig{URL: tc.url}, config.TransportEffective{}, nil, func(string, []byte) {}, nil)
			if err != nil {
				t.Fatal(err)
			}
			conn, ok := connector.(*pahoConnector)
			if !ok {
				t.Fatalf("connector type = %T", connector)
			}
			if conn.address != tc.address || conn.useTLS != tc.useTLS {
				t.Fatalf("address=%q tls=%v", conn.address, conn.useTLS)
			}
			if conn.transport.EnvelopeBytes != config.DefaultTransportEffective().EnvelopeBytes {
				t.Fatal("empty transport budget did not default")
			}
			replacement := NewReceiverBudgets()
			conn.SetReceiverBudgets(nil)
			if conn.receivers == replacement {
				t.Fatal("nil registry replaced the shared one")
			}
			conn.SetReceiverBudgets(replacement)
			if conn.receivers != replacement {
				t.Fatal("replacement registry not installed")
			}
		})
	}
}

func TestTLSConfigForRejectsUnreadableAndEmptyCAs(t *testing.T) {
	if _, err := tlsConfigFor("/absent/ca.pem", false, "mqtt.example.com"); err == nil {
		t.Fatal("missing CA file accepted")
	}
	path := t.TempDir() + "/ca.pem"
	if err := os.WriteFile(path, []byte("not a certificate"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := tlsConfigFor(path, false, "mqtt.example.com"); err == nil {
		t.Fatal("CA file without certificates accepted")
	}
}

func TestPahoSessionPublishFallsBackForDeviceEvents(t *testing.T) {
	metrics := diagnostics.NewMetrics()
	var published string
	transport := newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		published = topic
		return nil
	}, func(string, []byte) {})
	session := &pahoSession{
		transport: transport, peers: &sync.Map{}, budget: profileBudget(t, "compact"),
		receivers: NewReceiverBudgets(), metrics: metrics,
	}
	topic := DeviceEventTopic("h1", "d1")
	if err := session.Publish(context.Background(), topic, []byte("small-event"), 1); err != nil {
		t.Fatal(err)
	}
	if published != topic {
		t.Fatalf("published topic = %q", published)
	}
	if got := testutil.ToFloat64(metrics.TransportBudgetFallbacks.WithLabelValues("device")); got != 1 {
		t.Fatalf("fallback metric = %v", got)
	}
}

func TestPahoSessionUsesRegisteredReceiverBudget(t *testing.T) {
	var published string
	transport := newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		published = topic
		return nil
	}, func(string, []byte) {})
	receivers := NewReceiverBudgets()
	receivers.NoteConnection(hostReceiverRoute("h1"), 0, true)
	if !receivers.Register(hostReceiverRoute("h1"), 0, 1, profileBudget(t, "compact")) {
		t.Fatal("receiver budget not registered")
	}
	session := &pahoSession{transport: transport, peers: &sync.Map{}, budget: profileBudget(t, "default"), receivers: receivers}
	request, err := proto.Marshal(&whalereinv1.HostRequest{
		HostId: "h1", DeviceId: "d1", RequestId: "r1", ClientTransportVersion: transportVersion,
		Kind: whalereinv1.RequestKind_REQUEST_KIND_QUERY,
	})
	if err != nil {
		t.Fatal(err)
	}
	topic := RequestTopic("h1")
	if err := session.Publish(context.Background(), topic, request, 1); err != nil {
		t.Fatal(err)
	}
	if published != topic {
		t.Fatalf("published topic = %q", published)
	}
	// The request recorded the device's transport capability under the device
	// event route; the host request route itself remains legacy-sized.
	if capable, ok := session.peers.Load(DeviceEventTopic("h1", "d1")); !ok || capable != true {
		t.Fatalf("device capability = %v/%v", capable, ok)
	}
}

// scriptedMQTTClient drives pahoSession without a broker connection.
type scriptedMQTTClient struct {
	publishResponse *paho.PublishResponse
	publishErr      error
	suback          *paho.Suback
	subscribeErr    error
	disconnectErr   error
	done            chan struct{}
	published       int
}

func (c *scriptedMQTTClient) Publish(context.Context, *paho.Publish) (*paho.PublishResponse, error) {
	c.published++
	return c.publishResponse, c.publishErr
}
func (c *scriptedMQTTClient) Subscribe(context.Context, *paho.Subscribe) (*paho.Suback, error) {
	return c.suback, c.subscribeErr
}
func (c *scriptedMQTTClient) Done() <-chan struct{} {
	if c.done == nil {
		c.done = make(chan struct{})
	}
	return c.done
}
func (c *scriptedMQTTClient) Disconnect(*paho.Disconnect) error { return c.disconnectErr }

func TestPahoSessionReportsPublishRejectionAndSubscribeFailures(t *testing.T) {
	client := &scriptedMQTTClient{publishResponse: &paho.PublishResponse{ReasonCode: 0x80}}
	session := &pahoSession{client: client}
	if err := session.Publish(context.Background(), "topic", []byte("payload"), 1); err == nil {
		t.Fatal("rejected publish acknowledged")
	}
	if err := session.Subscribe(context.Background(), []string{"topic"}, 1); err == nil {
		t.Fatal("empty suback accepted")
	}
	client.suback = &paho.Suback{Reasons: []byte{0x80}}
	if err := session.Subscribe(context.Background(), []string{"topic"}, 1); err == nil {
		t.Fatal("rejected subscription accepted")
	}
	client.suback = &paho.Suback{Reasons: []byte{0x00}}
	if err := session.Subscribe(context.Background(), []string{"topic"}, 1); err != nil {
		t.Fatal(err)
	}
}

func TestPahoSessionClosePreservesRealDisconnectErrors(t *testing.T) {
	client := &scriptedMQTTClient{disconnectErr: errors.New("connection lost")}
	if err := (&pahoSession{client: client}).Close(); err != nil {
		t.Fatalf("connection lost = %v", err)
	}
	client.disconnectErr = errors.New("protocol error")
	if err := (&pahoSession{client: client}).Close(); err == nil {
		t.Fatal("real disconnect error swallowed")
	}
	transport := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	if err := (&pahoSession{client: client, transport: transport}).Close(); err == nil {
		t.Fatal("close with transport hid the disconnect error")
	}
}

func TestPresenceConnectionLookupsBoundAbsentIdentities(t *testing.T) {
	presence := newPresence()
	if _, _, _, known := presence.HostConnection(""); known {
		t.Fatal("empty host reported known")
	}
	if _, _, _, known := presence.HostConnection("missing"); known {
		t.Fatal("missing host reported known")
	}
	if _, known := presence.DeviceConnection(""); known {
		t.Fatal("empty device reported known")
	}
	if _, known := presence.DeviceConnection("missing"); known {
		t.Fatal("missing device reported known")
	}
	if presence.MarkHostReady("", time.Now()) {
		t.Fatal("empty host accepted readiness")
	}
	now := time.Now()
	presence.SetHostOnline("h1", true, now)
	presence.SetHostOnline("h1", true, now.Add(-time.Minute))
	_, observedAt, ready, known := presence.HostConnection("h1")
	if !known || !ready || !observedAt.Equal(now) {
		t.Fatalf("stale host update rewrote the observation: %v/%v/%v", observedAt, ready, known)
	}
	presence.SetDeviceOnline("d1", true, now)
	if connID, known := presence.DeviceConnection("d1"); !known || connID != 0 {
		t.Fatalf("device connection = %d/%v", connID, known)
	}
}

func TestPresenceRepeatedConnectKeepsReadinessAndSnapshotsFenceNewerEdges(t *testing.T) {
	presence := newPresence()
	at := time.Now().UTC()
	presence.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host", HostID: "h1", ConnID: 7, AtUnixMilli: at.UnixMilli()})
	if !presence.MarkHostReady("h1", at) {
		t.Fatal("readiness rejected")
	}
	// A repeated connect on the same connection keeps the HostReady proof.
	presence.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "host", HostID: "h1", ConnID: 7, AtUnixMilli: at.Add(time.Millisecond).UnixMilli()})
	if _, _, ready, _ := presence.HostConnection("h1"); !ready {
		t.Fatal("repeated connect dropped readiness")
	}

	// A live device edge newer than a snapshot boundary is retained, and a
	// stale snapshot cannot roll back a newer connection.
	presence.HandleNotification(&Notification{Sequence: 5, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 9, AtUnixMilli: at.UnixMilli()})
	presence.HandleNotification(&Notification{Sequence: 6, Event: "connected", Kind: "device", DeviceID: "d2", ConnID: 3, AtUnixMilli: at.UnixMilli()})
	presence.HandleNotification(&Notification{Sequence: 7, Event: "disconnected", Kind: "device", DeviceID: "d2", ConnID: 3, AtUnixMilli: at.Add(time.Millisecond).UnixMilli()})
	presence.ApplySnapshot([]SnapshotClient{
		{Kind: "device", DeviceID: "d1", ConnID: 8, State: "connected", Connected: at.UnixMilli()},
	}, 4)
	if connID, known := presence.DeviceConnection("d1"); !known || connID != 9 {
		t.Fatalf("newer live device edge rolled back: %d/%v", connID, known)
	}
}

// Pairing capability is evidence from the broker session, while explicit
// session ending and renewal must fence through the control channel.
func TestServicePairingCapabilityAndSessionControl(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	if service.SupportsPairing("h1") {
		t.Fatal("unconnected service claimed pairing support")
	}
	paho := &pahoSession{peers: &sync.Map{}}
	service.setSession(paho, true)
	if service.SupportsPairing("h1") {
		t.Fatal("unproven peer claimed pairing support")
	}
	paho.peers.Store(RequestTopic("h1"), true)
	if !service.SupportsPairing("h1") {
		t.Fatal("proven peer did not support pairing")
	}

	// A cancelled context stops the control wait after the request publish;
	// no broker reply is fabricated.
	service.setSession(session, true)
	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	if err := service.EndDeviceSession(cancelled, "session-1"); !errors.Is(err, context.Canceled) {
		t.Fatalf("end session = %v", err)
	}
	permission := store.DeviceSession{ID: "session-1", Revision: 2, ExpiresAt: time.Now().UTC().Add(time.Minute)}
	if err := service.RenewDeviceSession(cancelled, permission); !errors.Is(err, context.Canceled) {
		t.Fatalf("renew session = %v", err)
	}
}

// A zero reconnect configuration falls back to bounded defaults instead of a
// tight loop, and a connect error stops cleanly when the context ends.
func TestRunUsesDefaultReconnectBounds(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	connector := &cancelOnConnectConnector{cancel: cancel}
	service, err := New(Dependencies{
		Config:    config.BrokerConfig{},
		Connector: connector,
		Presence:  newPresence(),
		Logger:    discardLogger(),
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.Run(ctx); err != nil {
		t.Fatalf("run = %v", err)
	}
}

type cancelOnConnectConnector struct{ cancel context.CancelFunc }

func (c *cancelOnConnectConnector) Connect(context.Context) (Session, error) {
	c.cancel()
	return nil, errors.New("dial failed")
}
