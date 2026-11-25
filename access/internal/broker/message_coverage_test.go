package broker

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestMessagePropertyHelpers(t *testing.T) {
	properties := MessageProperties{}.WithUserProperty(PropertyTarget, "epoch:7")
	if !properties.Has(PropertyTarget) || properties.Get(PropertyTarget) != "epoch:7" {
		t.Fatalf("properties = %+v", properties)
	}
	if properties.Has(PropertyConnection) || properties.Get(PropertyConnection) != "" {
		t.Fatal("missing property reported present")
	}
	ref := ConnectionRef{Epoch: "0123456789abcdef", ID: 7}
	if ref.String() != "0123456789abcdef:7" || !ref.Valid() {
		t.Fatalf("ref = %q valid=%v", ref.String(), ref.Valid())
	}
	parsed, ok := ParseConnectionRef(ref.String())
	if !ok || parsed != ref {
		t.Fatalf("parse = %+v %v", parsed, ok)
	}
	for _, value := range []string{"", "epoch", ":7", "epoch:", "epoch:0", "epoch:not-a-number", "bad epoch:7"} {
		if _, ok := ParseConnectionRef(value); ok {
			t.Fatalf("invalid connection ref %q accepted", value)
		}
	}
	if (ConnectionRef{}).String() != "" || (ConnectionRef{Epoch: "e"}).Valid() {
		t.Fatal("invalid ref rendered or accepted")
	}
}

func TestPublishMessageAndNoticeUseTheSession(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)

	if err := service.PublishMessage(context.Background(), Message{
		Topic: PresenceTopic("h1"), Payload: []byte("presence"), QoS: 1,
		Properties: MessageProperties{MessageExpiry: uint32Ptr(5)},
	}); err != nil {
		t.Fatal(err)
	}
	if len(session.published) != 1 || session.published[0] != PresenceTopic("h1") {
		t.Fatalf("published = %v", session.published)
	}
	if err := service.PublishMessage(context.Background(), Message{}); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("empty topic = %v", err)
	}

	notice := &whalereinv1.RequestStateNotice{
		RequestId: "request-1", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch,
	}
	if err := service.PublishDeviceNotice(context.Background(), "h1", "d1", notice); err != nil {
		t.Fatal(err)
	}
	if session.published[1] != DeviceNoticeTopic("h1", "d1") {
		t.Fatalf("notice topic = %q", session.published[1])
	}
	if err := service.PublishDeviceNotice(context.Background(), "h1", "d1", nil); err == nil {
		t.Fatal("nil notice accepted")
	}
	if err := service.PublishDeviceNotice(context.Background(), "h1", "d1", &whalereinv1.RequestStateNotice{HostId: "h2", RequestId: "r"}); err == nil {
		t.Fatal("foreign notice host accepted")
	}
	if err := service.PublishDeviceNotice(context.Background(), "h1", "d1", &whalereinv1.RequestStateNotice{DeviceId: "d2", RequestId: "r"}); err == nil {
		t.Fatal("foreign notice device accepted")
	}
	if err := service.PublishDeviceNotice(context.Background(), "h1", "d1", &whalereinv1.RequestStateNotice{RequestId: "r", RelayVersion: 4}); err == nil {
		t.Fatal("old relay epoch notice accepted")
	}

	service.setSession(nil, false)
	if err := service.PublishMessage(context.Background(), Message{Topic: PresenceTopic("h1"), Payload: []byte("x")}); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("unavailable publish = %v", err)
	}
	if err := service.PublishDeviceNotice(context.Background(), "h1", "d1", notice); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("unavailable notice = %v", err)
	}
}

func TestPublishLiveQoSZeroNeverEntersTheOrderedQueue(t *testing.T) {
	client := &fakeMQTTClient{done: make(chan struct{})}
	local := config.DefaultTransportEffective()
	session := &pahoSession{
		client: client, budget: local, brokerLimit: uint64(local.PacketBytes),
		receivers: NewReceiverBudgets(),
	}
	payload := mustMarshalEvent(t, "h1", "d1")
	if err := session.Publish(context.Background(), Message{Topic: DeviceLiveTopic("h1", "d1"), Payload: payload, QoS: 0}); err != nil {
		t.Fatal(err)
	}
	if client.publishTopic != DeviceLiveTopic("h1", "d1") || client.publishQoS != 0 {
		t.Fatalf("live publish = %q qos=%d", client.publishTopic, client.publishQoS)
	}
	if err := session.Publish(context.Background(), Message{Topic: RequestTopic("h1"), Payload: payload, QoS: 0}); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("live request topic = %v", err)
	}
	if err := session.Publish(context.Background(), Message{Topic: DeviceLiveTopic("h1", "d1"), QoS: 0}); err == nil {
		t.Fatal("empty live payload accepted")
	}
	if err := session.Publish(context.Background(), Message{Topic: DeviceLiveTopic("h1", "d1"), Payload: make([]byte, local.EnvelopeBytes+1), QoS: 0}); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("oversize live payload = %v", err)
	}
	if encodedLivePublishBytes("topic", 4, 0) >= encodedLivePublishBytes("topic", 4, 2) {
		t.Fatal("live property bytes were not accounted")
	}
}

func TestPresenceTracksConnectionRefsByEpoch(t *testing.T) {
	presence := NewPresence(time.Minute, nil, nil)
	now := time.Now().UTC()
	presence.SetAvailable(true)
	ref := ConnectionRef{Epoch: "0123456789abcdef", ID: 7}
	presence.HandleNotification(&Notification{
		Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1",
		ConnID: ref.ID, Epoch: ref.Epoch, AtUnixMilli: now.UnixMilli(),
	})
	if !presence.DeviceConnectionActive("d1", ref) {
		t.Fatal("current connection ref was not tracked")
	}
	if refs := presence.DeviceConnectionRefs("d1"); len(refs) != 1 || refs[0] != ref {
		t.Fatalf("refs = %+v", refs)
	}
	// A same-id connection in a new broker epoch is a different reference.
	other := ConnectionRef{Epoch: "ffffffffffffffff", ID: ref.ID}
	if presence.DeviceConnectionActive("d1", other) {
		t.Fatal("stale epoch accepted")
	}
	presence.HandleNotification(&Notification{
		Sequence: 2, Event: "disconnected", Kind: "device", DeviceID: "d1",
		ConnID: ref.ID, Epoch: ref.Epoch, AtUnixMilli: now.Add(time.Second).UnixMilli(),
	})
	if presence.DeviceConnectionActive("d1", ref) || len(presence.DeviceConnectionRefs("d1")) != 0 {
		t.Fatal("disconnected ref survived")
	}

	presence.ApplySnapshot([]SnapshotClient{
		{Kind: "device", DeviceID: "d2", ConnID: 9, Epoch: "0123456789abcdef", State: "connected"},
	}, 3)
	if !presence.DeviceConnectionActive("d2", ConnectionRef{Epoch: "0123456789abcdef", ID: 9}) {
		t.Fatal("snapshot ref was not tracked")
	}
	if presence.DeviceConnectionActive("d1", ref) {
		t.Fatal("snapshot kept a stale ref")
	}
	if presence.DeviceConnectionRefs("") != nil {
		t.Fatal("empty device returned refs")
	}
}

func TestPresenceTracksManagedSessionConnectionRefs(t *testing.T) {
	presence := NewPresence(time.Minute, nil, nil)
	presence.SetAvailable(true)
	ref := ConnectionRef{Epoch: "0123456789abcdef", ID: 11}
	presence.HandleNotification(&Notification{
		Sequence: 1, Event: "connected", Kind: "session", SessionID: "managed-login", DeviceID: "d1",
		ConnID: ref.ID, Epoch: ref.Epoch, AtUnixMilli: time.Now().UTC().UnixMilli(),
	})
	if login, active := presence.DeviceConnectionSession("d1", ref); !active || login != "managed-login" {
		t.Fatal("managed session ref was not tracked")
	}
	presence.HandleNotification(&Notification{
		Sequence: 2, Event: "disconnected", Kind: "session", SessionID: "managed-login", DeviceID: "d1",
		ConnID: ref.ID, Epoch: ref.Epoch, AtUnixMilli: time.Now().UTC().UnixMilli(),
	})
	if presence.DeviceConnectionActive("d1", ref) {
		t.Fatal("managed session ref survived its disconnect")
	}
}

func TestBrokerHandleQueryWithoutAHandlerIsDropped(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	service.HandleMessage(Message{Topic: DeviceQueryTopic("h1", "d1"), Payload: []byte("query")})
}

func uint32Ptr(value uint32) *uint32 { return &value }

func TestPublishDeviceLiveWasRetired(t *testing.T) {
	// The Access preview relay is gone; the wire topic still parses so an
	// echoed copy can be classified and ignored rather than misrouted.
	parsed, err := ParseTopic(DeviceLiveTopic("h1", "d1"))
	if err != nil || parsed.Kind != KindDeviceLive {
		t.Fatalf("parse = %+v %v", parsed, err)
	}
	if _, err := ParseTopic(topicRoot + "h1/live"); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("retired host live topic parsed: %v", err)
	}
}
