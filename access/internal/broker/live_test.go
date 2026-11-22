package broker

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/paho"
	"github.com/prometheus/client_golang/prometheus/testutil"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func liveEnvelope(t *testing.T, hostID, deviceID string) []byte {
	t.Helper()
	payload, err := proto.Marshal(&whalereinv1.EventEnvelope{
		HostId: hostID, DeviceId: deviceID, Ciphertext: []byte("preview"),
		RelayVersion: RelayEpoch,
	})
	if err != nil {
		t.Fatal(err)
	}
	return payload
}

func TestLiveRouteUsesOnlyTheDeviceLiveQoSTopic(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	received := make(chan string, 1)
	service.SetLiveEventHandler(func(_ context.Context, hostID string, payload []byte) error {
		received <- hostID + ":" + string(payload)
		return nil
	})
	controlEvents := 0
	service.SetEventHandler(func(context.Context, string, []byte) error {
		controlEvents++
		return nil
	})
	payload := liveEnvelope(t, "h1", "d1")
	service.HandleMessage(LiveTopic("h1"), payload)
	select {
	case got := <-received:
		if got != "h1:"+string(payload) {
			t.Fatalf("live handler = %q", got)
		}
	case <-time.After(time.Second):
		t.Fatal("live handler was not called")
	}
	if controlEvents != 0 {
		t.Fatal("live preview entered control event routing")
	}
	if err := service.PublishDeviceLive(context.Background(), "h1", "d1", payload); err != nil {
		t.Fatal(err)
	}
	session.mu.Lock()
	defer session.mu.Unlock()
	if len(session.published) != 1 || session.published[0] != DeviceLiveTopic("h1", "d1") || session.qos[0] != 0 {
		t.Fatalf("published = %v qos=%v", session.published, session.qos)
	}
}

func TestLiveRouteRejectsForeignEpochAndIdentityBeforeHandling(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	calls := make(chan struct{}, 1)
	service.SetLiveEventHandler(func(context.Context, string, []byte) error {
		calls <- struct{}{}
		return nil
	})
	foreign, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h1", DeviceId: "d1", RelayVersion: 3})
	service.HandleMessage(LiveTopic("h1"), foreign)
	mismatch, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h2", DeviceId: "d1", RelayVersion: RelayEpoch})
	service.HandleMessage(LiveTopic("h1"), mismatch)
	missingDevice, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h1", RelayVersion: RelayEpoch})
	service.HandleMessage(LiveTopic("h1"), missingDevice)
	service.HandleMessage(LiveTopic("h1"), []byte("bad"))
	select {
	case <-calls:
		t.Fatal("invalid live preview reached the handler")
	case <-time.After(50 * time.Millisecond):
	}
	if got := testutil.ToFloat64(service.metrics.MQTTRouteErrors); got != 4 {
		t.Fatalf("route errors = %v", got)
	}
	if got := testutil.ToFloat64(service.metrics.LiveDrops.WithLabelValues("publish")); got != 0 {
		t.Fatalf("validation failures counted as publish drops: %v", got)
	}
}

func TestLiveRouteDropsUnderBackpressureWithoutBlocking(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	service.liveSlots = make(chan struct{}, 1)
	service.liveSlots <- struct{}{}
	called := make(chan struct{}, 1)
	service.SetLiveEventHandler(func(context.Context, string, []byte) error {
		called <- struct{}{}
		return nil
	})
	service.HandleMessage(LiveTopic("h1"), liveEnvelope(t, "h1", "d1"))
	select {
	case <-called:
		t.Fatal("preview was handed off with no free slot")
	case <-time.After(50 * time.Millisecond):
	}
	if got := testutil.ToFloat64(service.metrics.LiveDrops.WithLabelValues("backpressure")); got != 1 {
		t.Fatalf("backpressure drops = %v", got)
	}
}

func TestPublishDeviceLiveDropsOnDisconnectAndPublishFailure(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	payload := liveEnvelope(t, "h1", "d1")
	if err := service.PublishDeviceLive(context.Background(), "h1", "d1", payload); err != nil {
		t.Fatalf("disconnected drop returned an error: %v", err)
	}
	if got := testutil.ToFloat64(service.metrics.LiveDrops.WithLabelValues("disconnected")); got != 1 {
		t.Fatalf("disconnected drops = %v", got)
	}
	session := newFakeSession()
	service.setSession(session, true)
	session.publishErr = errors.New("mqtt down")
	if err := service.PublishDeviceLive(context.Background(), "h1", "d1", payload); err != nil {
		t.Fatalf("publish failure returned an error: %v", err)
	}
	if got := testutil.ToFloat64(service.metrics.LiveDrops.WithLabelValues("publish")); got != 1 {
		t.Fatalf("publish drops = %v", got)
	}
	session.mu.Lock()
	defer session.mu.Unlock()
	if len(session.published) != 1 || session.published[0] != DeviceLiveTopic("h1", "d1") || session.qos[0] != 0 {
		t.Fatalf("live publish reused a control path: %v qos=%v", session.published, session.qos)
	}
}

func TestPublishDeviceLiveValidatesIdentityWithoutPublishing(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	if err := service.PublishDeviceLive(context.Background(), "h1", "bad/d1", liveEnvelope(t, "h1", "d1")); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("bad device = %v", err)
	}
	if err := service.PublishDeviceLive(context.Background(), "h1", "d1", []byte("bad")); err == nil {
		t.Fatal("malformed envelope accepted")
	}
	if err := service.PublishDeviceLive(context.Background(), "h1", "d1", liveEnvelope(t, "h2", "d1")); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("host mismatch = %v", err)
	}
	foreign, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h1", DeviceId: "d1", RelayVersion: 3})
	if err := service.PublishDeviceLive(context.Background(), "h1", "d1", foreign); !errors.Is(err, ErrRelayEpoch) {
		t.Fatalf("foreign epoch = %v", err)
	}
}

func TestPahoSessionLivePublishUsesQoSZeroWithoutTheOrderedQueue(t *testing.T) {
	client := &fakeMQTTClient{publishResponse: &paho.PublishResponse{ReasonCode: 0}}
	session := newRelayTestSession(client)
	payload := liveEnvelope(t, "h1", "d1")
	if err := session.Publish(context.Background(), DeviceLiveTopic("h1", "d1"), payload, 0); err != nil {
		t.Fatal(err)
	}
	if client.publishTopic != DeviceLiveTopic("h1", "d1") || client.publishQoS != 0 || string(client.publishPayload) != string(payload) {
		t.Fatalf("live publish topic=%q qos=%d payload=%q", client.publishTopic, client.publishQoS, client.publishPayload)
	}
	// A QoS 0 live packet never enters the ordered control publisher.
	if got := session.publisher.WaitingBytes(); got != 0 {
		t.Fatalf("live publish entered the ordered queue: %d bytes", got)
	}
	client.publishResponse = &paho.PublishResponse{ReasonCode: 0x80}
	if err := session.Publish(context.Background(), DeviceLiveTopic("h1", "d1"), payload, 0); err == nil {
		t.Fatal("broker rejection ignored")
	}
	client.publishResponse = &paho.PublishResponse{ReasonCode: 0}
	client.publishErr = errors.New("network")
	if err := session.Publish(context.Background(), DeviceLiveTopic("h1", "d1"), payload, 0); err == nil {
		t.Fatal("publish error ignored")
	}
	client.publishErr = nil
	if err := session.Publish(context.Background(), EventTopic("h1"), payload, 0); err == nil {
		t.Fatal("live publish accepted a control topic")
	}
	if err := session.Publish(context.Background(), DeviceLiveTopic("h1", "d1"), nil, 0); err == nil {
		t.Fatal("empty live packet accepted")
	}
}

func TestPahoSessionLivePublishDoesNotFallBackToAReference(t *testing.T) {
	client := &fakeMQTTClient{publishResponse: &paho.PublishResponse{ReasonCode: 0}}
	session := newRelayTestSession(client)
	session.budget.PacketBytes = 16
	session.brokerLimit = 16
	payload := liveEnvelope(t, "h1", "d1")
	if err := session.Publish(context.Background(), DeviceLiveTopic("h1", "d1"), payload, 0); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("oversized live packet = %v", err)
	}
	if client.publishTopic != "" {
		t.Fatal("oversized live packet was published")
	}
}
