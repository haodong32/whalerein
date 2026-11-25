package broker

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/whalerein/whalerein/access/internal/config"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func TestFlowLabelCoversEveryWireKind(t *testing.T) {
	cases := map[string]string{
		RequestTopic("h1"):            "request",
		ResultTopic("h1"):             "result",
		EventTopic("h1"):              "event",
		DeviceEventTopic("h1", "d1"):  "device_event",
		DeviceLiveTopic("h1", "d1"):   "other",
		DeviceNoticeTopic("h1", "d1"): "notice",
		PresenceTopic("h1"):           "presence",
		DeviceReadyTopic("h1", "d1"):  "other",
		DeviceQueryTopic("h1", "d1"):  "query",
		DeviceReplyTopic("h1", "d1"):  "reply",
		"not-a-wire-topic":            "other",
	}
	for topic, want := range cases {
		if got := flowLabel(topic); got != want {
			t.Fatalf("flowLabel(%q) = %q, want %q", topic, got, want)
		}
	}
}

func TestMqttVarintSizeBoundaries(t *testing.T) {
	cases := map[int]int{-1: 1, 0: 1, 127: 1, 128: 2, 16383: 2, 16384: 3, 2_097_151: 3, 2_097_152: 4}
	for value, want := range cases {
		if got := mqttVarintSize(value); got != want {
			t.Fatalf("mqttVarintSize(%d) = %d, want %d", value, got, want)
		}
	}
}

func TestValidateAdvertisementRejectsEveryUnsafeShape(t *testing.T) {
	valid := defaultAdvertisement(t, 1)
	cases := map[string]*whalereinv1.ReceiveAdvertisement{
		"missing": nil,
		"foreign epoch": func() *whalereinv1.ReceiveAdvertisement {
			a := defaultAdvertisement(t, 1)
			a.RelayVersion = 2
			return a
		}(),
		"missing generation": func() *whalereinv1.ReceiveAdvertisement { a := defaultAdvertisement(t, 0); return a }(),
		"unknown profile": func() *whalereinv1.ReceiveAdvertisement {
			a := defaultAdvertisement(t, 1)
			a.Profile = whalereinv1.TransportProfile_TRANSPORT_PROFILE_UNSPECIFIED
			return a
		}(),
		"missing budget": func() *whalereinv1.ReceiveAdvertisement { a := defaultAdvertisement(t, 1); a.Budget = nil; return a }(),
		"zero packet": func() *whalereinv1.ReceiveAdvertisement {
			a := defaultAdvertisement(t, 1)
			a.Budget.PacketBytes = 0
			return a
		}(),
		"envelope below packet": func() *whalereinv1.ReceiveAdvertisement {
			a := defaultAdvertisement(t, 1)
			a.Budget.EnvelopeBytes = 1024
			return a
		}(),
		"unrepresentable envelope": func() *whalereinv1.ReceiveAdvertisement {
			a := defaultAdvertisement(t, 1)
			a.Budget.EnvelopeBytes = 1 << 63
			a.Budget.ReservedBytes = 1 << 63
			return a
		}(),
	}
	for name, advertisement := range cases {
		t.Run(name, func(t *testing.T) {
			if _, _, err := validateAdvertisement(advertisement); err == nil {
				t.Fatal("unsafe advertisement accepted")
			}
		})
	}
	budget, generation, err := validateAdvertisement(valid)
	if err != nil || generation != 1 || budget.PacketBytes != 65536 {
		t.Fatalf("valid advertisement = %+v/%d/%v", budget, generation, err)
	}
}

func TestRewriteToReferenceCoversEveryCarrier(t *testing.T) {
	reference := &whalereinv1.PayloadReference{
		ObjectId: "a", OwnerNodeId: "n", CiphertextBytes: 4,
		Sha256: sha256Of([]byte("body")), ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
	}
	stager := &fixedStager{reference: reference}
	request, _ := proto.Marshal(&whalereinv1.HostRequest{
		RequestId: "r1", HostId: "h1", DeviceId: "d1", CommandId: "c1",
		Body: []byte("body"), ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
	})
	rewritten, referenced, err := rewriteToReference(context.Background(), RequestTopic("h1"), request, stager)
	if err != nil || !referenced {
		t.Fatalf("request rewrite = %v/%v", referenced, err)
	}
	var sentRequest whalereinv1.HostRequest
	if err := proto.Unmarshal(rewritten, &sentRequest); err != nil {
		t.Fatal(err)
	}
	if len(sentRequest.Body) != 0 || sentRequest.BodyReference == nil {
		t.Fatal("request body was not replaced by a reference")
	}

	result, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r1", HostId: "h1", ResultBody: []byte("body")})
	rewritten, referenced, err = rewriteToReference(context.Background(), ResultTopic("h1"), result, stager)
	if err != nil || !referenced {
		t.Fatalf("result rewrite = %v/%v", referenced, err)
	}
	var sentResult whalereinv1.HostResult
	if err := proto.Unmarshal(rewritten, &sentResult); err != nil {
		t.Fatal(err)
	}
	if len(sentResult.ResultBody) != 0 || sentResult.ResultReference == nil {
		t.Fatal("result body was not replaced by a reference")
	}

	// A durable body with no stager fails explicitly.
	if _, _, err := rewriteToReference(context.Background(), RequestTopic("h1"), request, nil); !errors.Is(err, ErrStagingUnavailable) {
		t.Fatalf("missing stager = %v", err)
	}
	// Bootstrap and metadata-only envelopes have no reference representation.
	bootstrap, _ := proto.Marshal(&whalereinv1.HostRequest{RequestId: "r2", HostId: "h1"})
	if _, _, err := rewriteToReference(context.Background(), RequestTopic("h1"), bootstrap, stager); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("bootstrap rewrite = %v", err)
	}
	notice, _ := proto.Marshal(&whalereinv1.RequestStateNotice{RequestId: "r1", HostId: "h1", DeviceId: "d1"})
	if _, _, err := rewriteToReference(context.Background(), DeviceNoticeTopic("h1", "d1"), notice, stager); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("notice rewrite = %v", err)
	}
	// Malformed envelopes and unknown topics are typed rejections.
	if _, _, err := rewriteToReference(context.Background(), RequestTopic("h1"), []byte("bad"), stager); err == nil {
		t.Fatal("malformed request accepted")
	}
	if _, _, err := rewriteToReference(context.Background(), "bogus", request, stager); err == nil {
		t.Fatal("unknown topic accepted")
	}
}

type fixedStager struct {
	reference *whalereinv1.PayloadReference
}

func (s *fixedStager) Stage(_ context.Context, request StageRequest) (*whalereinv1.PayloadReference, error) {
	reference := proto.Clone(s.reference).(*whalereinv1.PayloadReference)
	reference.Purpose = request.Purpose
	reference.CiphertextBytes = uint64(len(request.Ciphertext))
	reference.Sha256 = sha256Of(request.Ciphertext)
	return reference, nil
}

func TestAlreadyReferencedRequestAndResultAreNotRestaged(t *testing.T) {
	reference := &whalereinv1.PayloadReference{
		ObjectId: "a", OwnerNodeId: "n", CiphertextBytes: 4,
		Sha256: sha256Of([]byte("body")), ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
	}
	for name, payload := range map[string][]byte{
		"request": func() []byte {
			data, _ := proto.Marshal(&whalereinv1.HostRequest{RequestId: "r1", HostId: "h1", BodyReference: reference})
			return data
		}(),
		"result": func() []byte {
			data, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r1", HostId: "h1", ResultReference: reference})
			return data
		}(),
		"event": func() []byte {
			data, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h1", DeviceId: "d1", PayloadReference: reference})
			return data
		}(),
	} {
		t.Run(name, func(t *testing.T) {
			topic := RequestTopic("h1")
			switch name {
			case "result":
				topic = ResultTopic("h1")
			case "event":
				topic = DeviceEventTopic("h1", "d1")
			}
			rewritten, referenced, err := rewriteToReference(context.Background(), topic, payload, nil)
			if err != nil || !referenced || !bytes.Equal(rewritten, payload) {
				t.Fatalf("already-referenced %s = %v/%v", name, referenced, err)
			}
		})
	}
}

func TestVerifyReferenceRejectsPurposeAndDigestMismatch(t *testing.T) {
	body := []byte("body")
	base := &whalereinv1.PayloadReference{
		ObjectId: "a", OwnerNodeId: "n", CiphertextBytes: uint64(len(body)),
		Sha256: sha256Of(body), ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST,
	}
	cases := map[string]func(*whalereinv1.PayloadReference){
		"nil":            func(*whalereinv1.PayloadReference) {},
		"missing object": func(r *whalereinv1.PayloadReference) { r.ObjectId = "" },
		"missing owner":  func(r *whalereinv1.PayloadReference) { r.OwnerNodeId = "" },
		"wrong purpose":  func(r *whalereinv1.PayloadReference) { r.Purpose = whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT },
		"wrong length":   func(r *whalereinv1.PayloadReference) { r.CiphertextBytes++ },
		"wrong digest":   func(r *whalereinv1.PayloadReference) { r.Sha256 = []byte("other") },
		"missing expiry": func(r *whalereinv1.PayloadReference) { r.ExpiresAtUnixMs = 0 },
	}
	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			if name == "nil" {
				if err := verifyReference(nil, body, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST); err == nil {
					t.Fatal("nil reference accepted")
				}
				return
			}
			reference := proto.Clone(base).(*whalereinv1.PayloadReference)
			mutate(reference)
			if err := verifyReference(reference, body, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST); err == nil {
				t.Fatal("invalid reference accepted")
			}
		})
	}
	if err := verifyReference(base, body, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST); err != nil {
		t.Fatalf("valid reference rejected: %v", err)
	}
}

func TestAccessReceiveAdvertisementBindsEpochAndGeneration(t *testing.T) {
	local := config.DefaultTransportEffective()
	advertisement := accessReceiveAdvertisement(local, 9)
	if advertisement.RelayVersion != RelayEpoch || advertisement.ConnectionGeneration != 9 {
		t.Fatalf("advertisement = %+v", advertisement)
	}
	if advertisement.Budget.PacketBytes != local.PacketBytes ||
		advertisement.Budget.EnvelopeBytes != local.EnvelopeBytes ||
		advertisement.Budget.TransferTimeoutMs != uint32(local.TransferTimeout.Milliseconds()) {
		t.Fatalf("budget = %+v", advertisement.Budget)
	}
	// A zero transfer timeout still yields a safe bounded default.
	advertisement = accessReceiveAdvertisement(config.TransportEffective{PacketBytes: 1, EnvelopeBytes: 1, BulkConcurrency: 1, ReservedBytes: 1}, 1)
	if advertisement.Budget.TransferTimeoutMs == 0 {
		t.Fatal("advertisement timeout was not defaulted")
	}
}

func TestConnectorDeliverMarksOnlyEpochFiveResultsAsCapable(t *testing.T) {
	var delivered int
	connector := &pahoConnector{dispatch: func(Message) { delivered++ }, peers: sync.Map{}}
	capable, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r1", HostId: "h1", ProtocolVersion: 1, RelayVersion: RelayEpoch})
	connector.deliver(Message{Topic: ResultTopic("h1"), Payload: capable})
	if value, ok := connector.peers.Load(RequestTopic("h1")); !ok || value != true {
		t.Fatal("valid epoch-5 result did not establish capability")
	}
	foreign, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r2", HostId: "h2", ProtocolVersion: 1, RelayVersion: 2})
	connector.deliver(Message{Topic: ResultTopic("h2"), Payload: foreign})
	if _, ok := connector.peers.Load(RequestTopic("h2")); ok {
		t.Fatal("foreign epoch established capability")
	}
	readyBody, _ := proto.Marshal(&whalereinv1.HostReady{HostId: "h3", RelayVersion: RelayEpoch})
	ready, _ := proto.Marshal(&whalereinv1.HostResult{HostId: "h3", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: readyBody, RelayVersion: RelayEpoch})
	connector.deliver(Message{Topic: ResultTopic("h3"), Payload: ready})
	if _, ok := connector.peers.Load(RequestTopic("h3")); ok {
		t.Fatal("HostReady alone established pairing capability")
	}
	connector.deliver(Message{Topic: "bogus"})
	if delivered != 4 {
		t.Fatalf("dispatch count = %d", delivered)
	}
}

func TestOrderedPublisherFailReleasesOneItem(t *testing.T) {
	publisher := &publisher{
		depth: 4, maxBytes: 1024, inFlight: make(chan struct{}, 1),
		done: make(chan struct{}), flows: map[string]*sendFlow{},
	}
	flow := &sendFlow{key: "flow", parent: publisher}
	item := &sendItem{done: make(chan error, 1), message: Message{Payload: []byte("x")}, size: 3}
	publisher.waiting = 3
	flow.fail(item, nil)
	if err := <-item.done; !errors.Is(err, ErrQueueClosed) {
		t.Fatalf("fail error = %v", err)
	}
	if publisher.WaitingBytes() != 0 || item.message.Payload != nil {
		t.Fatal("failed item was not released")
	}
}

func TestPublisherNilSubmitAndResetRegistry(t *testing.T) {
	var nilPublisher *publisher
	if err := nilPublisher.Submit(context.Background(), "t", []byte("x")); !errors.Is(err, ErrQueueClosed) {
		t.Fatalf("nil publisher = %v", err)
	}
	if nilPublisher.WaitingBytes() != 0 {
		t.Fatal("nil publisher waiting bytes")
	}
	nilPublisher.Close()
	empty := newPublisher(func(context.Context, Message) error { return nil }, config.DefaultTransportEffective(), nil)
	if err := empty.Submit(context.Background(), "t", nil); err == nil {
		t.Fatal("empty packet accepted")
	}
	// An empty publisher queue has no metrics and still closes cleanly.
	empty.Close()
	empty.Close()

	registry := NewReceiverBudgets()
	registry.NoteConnection(receiverRoute{kind: "host", hostID: "h1"}, 1, "c", true)
	registry.Register(receiverRoute{kind: "host", hostID: "h1"}, 1, 1, config.DefaultTransportEffective())
	registry.Reset()
	if registry.Count() != 0 || registry.Connections(receiverRoute{kind: "host", hostID: "h1"}) != 0 {
		t.Fatal("reset retained registry state")
	}
}

// TestPahoConnectorHonorsConnackLimits proves that the broker's advertised
// MaximumPacketSize and Receive Maximum lower the publishing hop instead of
// raising the configured budget.
func TestPahoConnectorHonorsConnackLimits(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	serverDone := make(chan struct{})
	go func() {
		defer close(serverDone)
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		reader := bufio.NewReader(conn)
		for {
			packet, err := packets.ReadPacket(reader)
			if err != nil {
				return
			}
			switch packet.Type {
			case packets.CONNECT:
				maximum := uint32(4096)
				receiveMaximum := uint16(2)
				ack := &packets.Connack{
					ReasonCode: packets.ConnackSuccess,
					Properties: &packets.Properties{
						MaximumPacketSize: &maximum,
						ReceiveMaximum:    &receiveMaximum,
					},
				}
				if _, err := ack.WriteTo(conn); err != nil {
					return
				}
			case packets.DISCONNECT:
				return
			}
		}
	}()
	local := config.DefaultTransportEffective()
	connector, err := NewPahoConnectorWithTransport(
		config.BrokerConfig{URL: "tcp://" + listener.Addr().String(), ClientID: "whalerein-access-test"},
		local, nil, func(Message) {}, discardLogger())
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	session, err := connector.Connect(ctx)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	pahoSession := session.(*pahoSession)
	if pahoSession.brokerLimit != 4096 {
		t.Fatalf("broker limit = %d, want the CONNACK MaximumPacketSize", pahoSession.brokerLimit)
	}
	if cap(pahoSession.publisher.inFlight) != 2 {
		t.Fatalf("in-flight bound = %d, want the CONNACK Receive Maximum", cap(pahoSession.publisher.inFlight))
	}
	// The configured packet bytes can never raise the broker's limit during a
	// publish selection on this session.
	if limit := selectPacketLimit(pahoSession.budget.PacketBytes, pahoSession.brokerLimit, nil); limit != 4096 {
		t.Fatalf("selected limit = %d", limit)
	}
	_ = session.Close()
	select {
	case <-serverDone:
	case <-time.After(2 * time.Second):
		t.Fatal("fake broker did not stop")
	}
}

func TestControlPublishStaysRawAndPacketBounded(t *testing.T) {
	client := &fakeMQTTClient{}
	session := newRelayTestSession(client)
	payload := []byte("whalerein/1\nid access-1\nop ping\n")
	if err := session.Publish(context.Background(), Message{Topic: ControlRequestTopic, Payload: payload, QoS: 1}); err != nil {
		t.Fatalf("control publish: %v", err)
	}
	if !bytes.Equal(client.publishPayload, payload) {
		t.Fatal("control payload was rewritten")
	}
	// A control payload above the packet limit fails before publish.
	tight, _ := relayTestSession(t, 32, nil)
	if err := tight.Publish(context.Background(), Message{Topic: ControlRequestTopic, Payload: bytes.Repeat([]byte("x"), 128), QoS: 1}); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("oversized control publish = %v", err)
	}
	// Any other unknown topic remains invalid.
	if err := session.Publish(context.Background(), Message{Topic: "whalerein/v1/control/reply", Payload: []byte("x"), QoS: 1}); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("unknown topic = %v", err)
	}
}
