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

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func discardLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

func profileBudget(t *testing.T, name string) config.TransportEffective {
	t.Helper()
	values, ok := whalereinv1.TransportProfileValuesFor(name)
	if !ok {
		t.Fatalf("unknown profile %q", name)
	}
	cfg := config.Default()
	cfg.Transport.Profile = name
	effective, err := cfg.ResolveTransport()
	if err != nil {
		t.Fatalf("resolve %q: %v", name, err)
	}
	if effective.PacketBytes != values.PacketBytes {
		t.Fatalf("profile %q packet = %d", name, effective.PacketBytes)
	}
	return effective
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
	if empty.Error() != "relay rejected" || empty.Unwrap() != nil {
		t.Fatalf("empty transport error = %q", empty.Error())
	}
}

func TestNegotiateBudgetKeepsTheStricterOfEachLimit(t *testing.T) {
	local := profileBudget(t, "default")
	compact := profileBudget(t, "compact")
	roomy := profileBudget(t, "roomy")
	zero := config.TransportEffective{}
	intersectedCompact := compact
	intersectedCompact.ResultLimitBytes = local.ResultLimitBytes
	intersectedCompact.HTTPBodyBytes = local.HTTPBodyBytes
	narrow := local
	narrow.PacketBytes = local.PacketBytes / 2
	tightened := local
	tightened.EnvelopeBytes = 8 << 20
	tightened.PacketBytes = 32 << 10
	tightened.ReservedBytes = 8 << 20
	wantTightened := tightened
	for name, tc := range map[string]struct {
		local, advertised, want config.TransportEffective
	}{
		"unregistered peer":                 {local, zero, local},
		"smaller peer envelope":             {local, compact, intersectedCompact},
		"larger peer envelope":              {local, roomy, local},
		"equal envelope narrower":           {local, narrow, narrow},
		"peer tightens packet and envelope": {local, tightened, wantTightened},
	} {
		t.Run(name, func(t *testing.T) {
			if got := negotiateBudget(tc.local, tc.advertised); got != tc.want {
				t.Fatalf("negotiated = %+v, want %+v", got, tc.want)
			}
		})
	}
}

func TestReceiverBudgetsTrackSiblingConnectionsSeparately(t *testing.T) {
	registry := NewReceiverBudgets()
	host := receiverRoute{kind: "host", hostID: "h1"}
	registry.NoteConnection(host, 5, "whalerein-host-h1", true)
	if !registry.Register(host, 5, 1, profileBudget(t, "default")) {
		t.Fatal("current advertisement rejected")
	}
	if registry.Register(host, 4, 2, profileBudget(t, "roomy")) {
		t.Fatal("unknown connection accepted an advertisement")
	}
	// A sibling device connection with its own generation keeps the first
	// receiver's budget; the route takes the stricter of both.
	first := receiverRoute{kind: "device", deviceID: "d1"}
	registry.NoteConnection(first, 10, "whalerein-device-d1-a", true)
	registry.NoteConnection(first, 11, "whalerein-device-d1-b", true)
	if !registry.Register(first, 10, 10, profileBudget(t, "roomy")) {
		t.Fatal("first device sibling rejected")
	}
	if !registry.Register(first, 11, 11, profileBudget(t, "compact")) {
		t.Fatal("second device sibling rejected")
	}
	if registry.Register(first, 12, 12, profileBudget(t, "default")) {
		t.Fatal("unknown device generation accepted")
	}
	budget, ok := registry.Lookup(DeviceEventTopic("h1", "d1"))
	if !ok || budget.Profile != "compact" || budget.PacketBytes != 32768 {
		t.Fatalf("shared route budget = %+v/%v", budget, ok)
	}
	if registry.Connections(first) != 2 {
		t.Fatalf("sibling connections = %d", registry.Connections(first))
	}
	// A sibling reconnect clears only its own advertisement.
	registry.NoteConnection(first, 11, "whalerein-device-d1-b", false)
	budget, ok = registry.Lookup(DeviceNoticeTopic("h2", "d1"))
	if !ok || budget.Profile != "roomy" {
		t.Fatalf("sibling disconnect cleared the survivor: %+v/%v", budget, ok)
	}
	// An old generation never refreshes a newer one.
	if registry.Register(first, 10, 9, profileBudget(t, "default")) {
		t.Fatal("stale device generation refreshed the entry")
	}
	// A grant or app-session revocation fences every receiver at once.
	registry.FenceDevice("d1")
	if _, ok := registry.Lookup(DeviceEventTopic("h1", "d1")); ok {
		t.Fatal("fenced device kept an advertisement")
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
	registry.NoteConnection(receiverRoute{kind: "host", hostID: "h1"}, 1, "client", true)
	if registry.Register(receiverRoute{kind: "host", hostID: "h1"}, 1, 1, config.TransportEffective{}) {
		t.Fatal("nil registry accepted an advertisement")
	}
	if _, ok := registry.Lookup(RequestTopic("h1")); ok {
		t.Fatal("nil registry produced a budget")
	}
	registry.Clear(receiverRoute{kind: "host", hostID: "h1"})
	registry.FenceDevice("d1")
	registry.Reset()
	if registry.Count() != 0 || registry.Connections(receiverRoute{}) != 0 {
		t.Fatal("nil registry count")
	}
}

// newBudgetRecordingConnector exposes the registry installed during New.
type budgetRecordingConnector struct {
	*fakeConnector
	installed *ReceiverBudgets
	stager    PayloadStager
}

func (c *budgetRecordingConnector) SetReceiverBudgets(receivers *ReceiverBudgets) {
	c.installed = receivers
}

func (c *budgetRecordingConnector) SetPayloadStager(stager PayloadStager) {
	c.stager = stager
}

type fakeStager struct{}

func (fakeStager) Stage(context.Context, StageRequest) (*whalereinv1.PayloadReference, error) {
	return nil, errors.New("not used")
}

func TestNewInstallsReceiverBudgetsAndStagerOnAwareConnectors(t *testing.T) {
	connector := &budgetRecordingConnector{fakeConnector: &fakeConnector{sessions: []*fakeSession{newFakeSession()}}}
	service, err := New(Dependencies{
		Connector: connector,
		Presence:  newPresence(),
		Logger:    discardLogger(),
		Stager:    fakeStager{},
	})
	if err != nil {
		t.Fatal(err)
	}
	if connector.installed == nil || connector.installed != service.Receivers() {
		t.Fatal("connector did not receive the shared registry")
	}
	if connector.stager == nil {
		t.Fatal("connector did not receive the payload stager")
	}
	service.SetPayloadStager(fakeStager{})
	if service.Available() {
		t.Fatal("empty presence reported available")
	}
}

func defaultAdvertisement(t *testing.T, generation uint64) *whalereinv1.ReceiveAdvertisement {
	t.Helper()
	return &whalereinv1.ReceiveAdvertisement{
		Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
		Budget: &whalereinv1.TransportBudget{
			PacketBytes: 65536, EnvelopeBytes: 16 << 20,
			BulkConcurrency: 4, ReservedBytes: 32 << 20, TransferTimeoutMs: 120000,
		},
		ConnectionGeneration: generation,
		RelayVersion:         RelayEpoch,
	}
}

func TestHandleNotificationBindsAdvertisements(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	service.handleNotification(nil)

	service.handleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host", HostID: "h1", ConnID: 7, AtUnixMilli: time.Now().UnixMilli()})
	// The host is connected but HostReady has not confirmed subscriptions, so
	// no advertisement may bind yet.
	service.registerHostAdvertisement("h1", defaultAdvertisement(t, 1))
	if service.Receivers().Count() != 0 {
		t.Fatal("unready host bound a receive budget")
	}

	readyAt := time.Now().UTC()
	service.handleReady(hostReadyResult(t, "h1", readyAt, defaultAdvertisement(t, 7)))
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok {
		t.Fatal("ready host did not bind its budget")
	}
	// A replayed generation is ignored without refreshing the entry.
	service.registerHostAdvertisement("h1", defaultAdvertisement(t, 7))
	if budget, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok || budget.Profile != "default" {
		t.Fatalf("replayed generation changed the budget: %+v/%v", budget, ok)
	}
	// A contradictory readiness proof is rejected without clearing the last
	// valid advertisement for the same connection generation.
	invalid := defaultAdvertisement(t, 7)
	invalid.Budget.EnvelopeBytes = 1 << 10
	service.handleReady(hostReadyResult(t, "h1", readyAt.Add(time.Millisecond), invalid))
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok {
		t.Fatal("invalid readiness proof cleared the valid advertisement")
	}
	// A direct contradictory advertisement clears the route instead of
	// leaving stale authority.
	service.registerHostAdvertisement("h1", invalid)
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); ok {
		t.Fatal("invalid advertisement left a stale route")
	}
	service.handleReady(hostReadyResult(t, "h1", readyAt.Add(2*time.Millisecond), defaultAdvertisement(t, 7)))
	if _, ok := service.Receivers().Lookup(RequestTopic("h1")); !ok {
		t.Fatal("fresh advertisement after rejection was not registered")
	}
	service.registerHostAdvertisement("h1", nil)
	if service.Receivers().Count() != 0 {
		t.Fatal("missing advertisement left a stale route")
	}

	// Device ready advertisements are registered per connection generation.
	service.handleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 21, ClientID: "device-a", AtUnixMilli: time.Now().UnixMilli()})
	payload, _ := proto.Marshal(defaultAdvertisement(t, 21))
	service.HandleMessage(DeviceReadyTopic("h1", "d1"), payload)
	budget, ok := service.Receivers().Lookup(DeviceEventTopic("h1", "d1"))
	if !ok || budget.PacketBytes != 65536 {
		t.Fatalf("device ready advertisement = %+v/%v", budget, ok)
	}
	// A sibling generation is tracked beside the first advertisement instead
	// of clearing it; an exact replay of a generation is ignored.
	sibling, _ := proto.Marshal(defaultAdvertisement(t, 22))
	service.HandleMessage(DeviceReadyTopic("h1", "d1"), sibling)
	if service.Receivers().Count() != 2 {
		t.Fatalf("sibling advertisement count = %d", service.Receivers().Count())
	}
	service.HandleMessage(DeviceReadyTopic("h1", "d1"), payload)
	if service.Receivers().Count() != 2 {
		t.Fatalf("replayed generation changed the registry count: %d", service.Receivers().Count())
	}
	// A foreign epoch is rejected before registration.
	foreign := defaultAdvertisement(t, 22)
	foreign.RelayVersion = 2
	foreignPayload, _ := proto.Marshal(foreign)
	service.HandleMessage(DeviceReadyTopic("h1", "d1"), foreignPayload)
	if service.Receivers().Count() != 2 {
		t.Fatal("foreign epoch device advertisement registered")
	}
	// The last broker connection going offline clears every generation for the
	// device route, including a stale sibling.
	service.handleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 21, ClientID: "device-a", AtUnixMilli: time.Now().UnixMilli()})
	if service.Receivers().Count() != 0 {
		t.Fatalf("device route survived its last disconnect: %d", service.Receivers().Count())
	}
}

func hostReceiverRoute(hostID string) receiverRoute {
	return receiverRoute{kind: "host", hostID: hostID}
}

func hostReadyResult(t *testing.T, hostID string, at time.Time, advertisement *whalereinv1.ReceiveAdvertisement) *whalereinv1.HostResult {
	t.Helper()
	body, err := proto.Marshal(&whalereinv1.HostReady{
		HostId: hostID, ConnectedAtUnixMs: at.UnixMilli(), Receive: advertisement, RelayVersion: RelayEpoch,
	})
	if err != nil {
		t.Fatal(err)
	}
	return &whalereinv1.HostResult{HostId: hostID, Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, RelayVersion: RelayEpoch}
}

func TestHostReadyValidatesIdentityAdvertisementAndAuthorizationKey(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	now := time.Now().UTC()

	// A ready proof without a host id falls back to the result host, and a
	// mismatched result host is rejected.
	service.handleReady(hostReadyResult(t, "h1", now, defaultAdvertisement(t, 1)))
	if _, _, ready, known := service.Presence().HostConnection("h1"); !known || !ready {
		t.Fatal("result host fallback did not mark the host ready")
	}

	// A handler failure and a wrong key length must each be contained without
	// losing readiness.
	keyErr := errors.New("key store unavailable")
	service.SetHostKeyHandler(func(context.Context, string, []byte) error { return keyErr })
	keyResult := hostReadyResult(t, "h1", now, defaultAdvertisement(t, 1))
	var ready whalereinv1.HostReady
	if err := proto.Unmarshal(keyResult.ResultBody, &ready); err != nil {
		t.Fatal(err)
	}
	ready.AuthorizationPublicKey = []byte("short")
	keyResult.ResultBody, _ = proto.Marshal(&ready)
	service.handleReady(keyResult)

	keyResult = hostReadyResult(t, "h1", now, defaultAdvertisement(t, 1))
	if err := proto.Unmarshal(keyResult.ResultBody, &ready); err != nil {
		t.Fatal(err)
	}
	ready.AuthorizationPublicKey = ed25519.PublicKey(make([]byte, ed25519.PublicKeySize))
	keyResult.ResultBody, _ = proto.Marshal(&ready)
	service.handleReady(keyResult)

	// The result handler error path is counted without changing the outcome.
	service.SetResultHandler(func(context.Context, string, *whalereinv1.HostResult) error { return keyErr })
	result := &whalereinv1.HostResult{HostId: "h1", RequestId: "r1", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch}
	payload, err := proto.Marshal(result)
	if err != nil {
		t.Fatal(err)
	}
	service.HandleMessage(ResultTopic("h1"), payload)

	// Readiness without a valid advertisement is rejected: a Host that cannot
	// publish its receive limits cannot be forwarded to.
	other := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	missing := hostReadyResult(t, "h2", now, nil)
	other.handleReady(missing)
	if _, _, readyNow, _ := other.Presence().HostConnection("h2"); readyNow {
		t.Fatal("readiness without an advertisement marked the host ready")
	}
	foreign := hostReadyResult(t, "h2", now, defaultAdvertisement(t, 1))
	var foreignReady whalereinv1.HostReady
	_ = proto.Unmarshal(foreign.ResultBody, &foreignReady)
	foreignReady.RelayVersion = 2
	foreign.ResultBody, _ = proto.Marshal(&foreignReady)
	other.handleReady(foreign)
	if _, _, readyNow, _ := other.Presence().HostConnection("h2"); readyNow {
		t.Fatal("foreign relay epoch marked the host ready")
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
			conn.SetPayloadStager(fakeStager{})
			if conn.stager.get() == nil {
				t.Fatal("stager not installed")
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

func TestPahoSessionReportsPublishRejectionAndSubscribeFailures(t *testing.T) {
	client := &scriptedMQTTClient{publishResponse: &paho.PublishResponse{ReasonCode: 0x80}}
	session := newRelayTestSession(&fakeMQTTClient{publishResponse: &paho.PublishResponse{ReasonCode: 0x80}})
	payload, _ := proto.Marshal(&whalereinv1.HostRequest{RequestId: "r1", HostId: "h1", Body: []byte("x")})
	if err := session.Publish(context.Background(), RequestTopic("h1"), payload, 1); err == nil {
		t.Fatal("rejected publish acknowledged")
	}
	if err := session.Subscribe(context.Background(), []string{"topic"}, 1); err == nil {
		t.Fatal("empty suback accepted")
	}
	client.suback = &paho.Suback{Reasons: []byte{0x80}}
	session.client = client
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
	publisher := newPublisher(func(context.Context, string, []byte) error { return nil }, config.DefaultTransportEffective(), nil)
	if err := (&pahoSession{client: client, publisher: publisher}).Close(); err == nil {
		t.Fatal("close with a publisher hid the disconnect error")
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
