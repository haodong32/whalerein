package broker

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/observation"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func openBrokerSink(t *testing.T) (*observation.Sink, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "access.ndjson")
	sink, err := observation.Open(observation.Config{
		Mode: observation.ModeTrace, Outlet: path, RunID: "run-broker-test",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-broker-test",
	})
	if err != nil {
		t.Fatal(err)
	}
	return sink, path
}

func observedBroker(t *testing.T, sink *observation.Sink) *Service {
	t.Helper()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	metrics := diagnostics.NewMetrics()
	presence := NewPresence(50*time.Millisecond, metrics, logger)
	presence.SetBrokerEpoch("epoch1")
	service, err := New(Dependencies{
		Config:      config.BrokerConfig{PublishTimeout: config.Duration(time.Second), ReconnectMin: config.Duration(time.Millisecond), ReconnectMax: config.Duration(5 * time.Millisecond)},
		Presence:    presence,
		Connector:   &fakeConnector{sessions: []*fakeSession{newFakeSession()}},
		Metrics:     metrics,
		Masker:      diagnostics.NewMaskerWithKey([]byte("broker-obs")),
		Logger:      logger,
		Observation: sink,
	})
	if err != nil {
		t.Fatal(err)
	}
	return service
}

func noticeEvents(t *testing.T, path string) []map[string]any {
	t.Helper()
	records, err := observation.ReadRecords(path)
	if err != nil {
		t.Fatal(err)
	}
	events := make([]map[string]any, 0, len(records))
	for _, record := range records {
		if record["event"] == "access_notice" {
			events = append(events, record)
		}
	}
	return events
}

// latency/AC-06.subscription latency/AC-11.normal: broker connection and
// subscription readiness are reported per connection generation with a
// host/client role, including a replacement that does not confirm the newer
// connection. The generation sequence is the normal, non-shaped socket
// evidence Access owns.
func TestBrokerConnectionAndReadyNoticesCarryGenerationAndRole(t *testing.T) {
	sink, path := openBrokerSink(t)
	service := observedBroker(t, sink)

	service.handleNotification(&Notification{Kind: "host", HostID: "host-1", ConnID: 1, Epoch: "epoch1", Event: "connected", Sequence: 1})
	readyBody, _ := proto.Marshal(&whalereinv1.HostReady{HostId: "host-1", Receive: testAdvertisement(1), RelayVersion: RelayEpoch})
	service.handleReady(&whalereinv1.HostResult{RequestId: "", HostId: "host-1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: readyBody, RelayVersion: RelayEpoch})

	// A reconnect creates generation 2. A readiness proof still carrying the
	// superseded connection time must be marked replaced.
	service.handleNotification(&Notification{Kind: "host", HostID: "host-1", ConnID: 2, Epoch: "epoch1", Event: "connected", Sequence: 2})
	stale, _ := proto.Marshal(&whalereinv1.HostReady{
		HostId: "host-1", Receive: testAdvertisement(2), RelayVersion: RelayEpoch,
		ConnectedAtUnixMs: 1,
	})
	service.handleReady(&whalereinv1.HostResult{RequestId: "", HostId: "host-1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: stale, RelayVersion: RelayEpoch})
	service.handleNotification(&Notification{Kind: "host", HostID: "host-1", ConnID: 2, Epoch: "epoch1", Event: "disconnected", Sequence: 3})

	service.handleNotification(&Notification{Kind: "device", DeviceID: "device-1", ConnID: 3, Epoch: "epoch1", Event: "connected", Sequence: 4})
	advertisement, _ := proto.Marshal(testAdvertisement(3))
	service.handleDeviceReady("host-1", "device-1", advertisement)
	service.handleDeviceReady("host-1", "device-1", advertisement)
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := noticeEvents(t, path)
	var seen []string
	for _, event := range events {
		data := event["data"].(map[string]any)
		seen = append(seen, data["role"].(string)+"/"+data["stage"].(string)+"/"+data["outcome"].(string))
	}
	want := []string{
		"host/subscribe/ok", "host/ready/ok",
		"host/subscribe/ok", "host/ready/replaced", "host/disconnect/ok",
		"client/subscribe/ok", "client/ready/ok", "client/ready/replaced",
	}
	if len(seen) != len(want) {
		t.Fatalf("notices = %v want %v", seen, want)
	}
	for index := range want {
		if seen[index] != want[index] {
			t.Fatalf("notice %d = %q want %q", index, seen[index], want[index])
		}
	}
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(contents), "host-1") || strings.Contains(string(contents), "device-1") {
		t.Fatal("raw peer identity leaked into the outlet")
	}
}

// latency/AC-01.direct-live: a QoS 0 direct preview never enters an Access
// application interval, so the outlet stays empty for that route.
func TestDirectLivePreviewHasNoAccessInterval(t *testing.T) {
	sink, path := openBrokerSink(t)
	service := observedBroker(t, sink)
	handler := &fakeQueryHandler{}
	service.SetQueryHandler(handler)
	service.HandleMessage(Message{Topic: DeviceLiveTopic("host-1", "device-1"), Payload: []byte("preview"), QoS: 0})
	if handler.admits != 0 {
		t.Fatal("direct preview entered query admission")
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	records, err := observation.ReadRecords(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 0 {
		t.Fatalf("direct preview emitted Access facts: %v", records)
	}
}

// latency/AC-07.aggregate: connection-notice dispositions are aggregated even
// when the process runs without a trace outlet.
func TestBrokerNoticeMetricsRemainAvailableWithoutTraceSink(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	service.handleNotification(&Notification{Kind: "host", HostID: "host-1", ConnID: 1, Epoch: "epoch1", Event: "connected", Sequence: 1})
	if got := testutil.ToFloat64(service.metrics.AccessStageTotal.WithLabelValues("notice_subscribe", "ok")); got != 1 {
		t.Fatalf("notice metric = %v", got)
	}
}

func observedServiceWithSession(t *testing.T, sink *observation.Sink) (*Service, *fakeSession) {
	t.Helper()
	service := observedBroker(t, sink)
	session := newFakeSession()
	service.setSession(session, true)
	return service, session
}

func registerObservedDevice(service *Service, deviceID string, connID, sequence uint64) {
	service.handleNotification(&Notification{
		Kind: "device", DeviceID: deviceID, ConnID: connID,
		Epoch: "epoch1", Event: "connected", Sequence: sequence,
	})
}

func transportEvents(t *testing.T, path string) []map[string]any {
	t.Helper()
	records, err := observation.ReadRecords(path)
	if err != nil {
		t.Fatal(err)
	}
	events := make([]map[string]any, 0, len(records))
	for _, record := range records {
		if record["event"] == "access_transport" {
			events = append(events, record["data"].(map[string]any))
		}
	}
	return events
}

func observedNumber(t *testing.T, value any) uint64 {
	t.Helper()
	number, ok := value.(json.Number)
	if !ok {
		t.Fatalf("observed value %v is %T, want json.Number", value, value)
	}
	parsed, err := strconv.ParseUint(number.String(), 10, 64)
	if err != nil {
		t.Fatalf("observed number %q: %v", number, err)
	}
	return parsed
}

// latency/AC-01.inline latency/AC-02.query-inline: the three device-bound
// forwarding boundaries emit one access_transport fact each, naming the exact
// client connection generation, the broker-to-client direction and the bytes
// Access handed to the broker.
func TestBrokerTransportRecordsAcceptedClientForwarding(t *testing.T) {
	sink, path := openBrokerSink(t)
	service, session := observedServiceWithSession(t, sink)
	registerObservedDevice(service, "device-1", 7, 1)

	eventPayload, err := proto.Marshal(&whalereinv1.EventEnvelope{
		HostId: "host-1", DeviceId: "device-1", ChannelId: "channel-1",
		Ciphertext: []byte("event-ciphertext"), RelayVersion: RelayEpoch,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.PublishDeviceEvent(context.Background(), "host-1", "device-1", eventPayload); err != nil {
		t.Fatal(err)
	}
	notice := &whalereinv1.RequestStateNotice{
		RequestId: "request-1", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch,
	}
	if err := service.PublishDeviceNotice(context.Background(), "host-1", "device-1", notice); err != nil {
		t.Fatal(err)
	}
	reply := []byte("query-reply")
	target := ConnectionRef{Epoch: "epoch1", ID: 7}
	if err := service.PublishMessage(context.Background(), Message{
		Topic: DeviceReplyTopic("host-1", "device-1"), Payload: reply, QoS: 1,
		Properties: MessageProperties{}.WithUserProperty(PropertyTarget, target.String()),
	}); err != nil {
		t.Fatal(err)
	}
	if len(session.payloads) != 3 {
		t.Fatalf("published = %d", len(session.payloads))
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := transportEvents(t, path)
	if len(events) != 3 {
		t.Fatalf("transport events = %d want 3", len(events))
	}
	wantBytes := []uint64{
		uint64(len(session.payloads[0])),
		uint64(len(session.payloads[1])),
		uint64(len(session.payloads[2])),
	}
	connectionToken := ""
	for index, event := range events {
		if event["role"] != "client" || event["direction"] != "broker_to_client" {
			t.Fatalf("event %d = %v", index, event)
		}
		if event["outcome"] != "ok" {
			t.Fatalf("event %d outcome = %v", index, event["outcome"])
		}
		if got := observedNumber(t, event["generation"]); got != 7 {
			t.Fatalf("event %d generation = %d", index, got)
		}
		if got := observedNumber(t, event["bytes"]); got != wantBytes[index] {
			t.Fatalf("event %d bytes = %d want %d", index, got, wantBytes[index])
		}
		token, _ := event["connection"].(string)
		if len(token) != 32 {
			t.Fatalf("event %d connection token = %q", index, token)
		}
		if connectionToken == "" {
			connectionToken = token
		} else if token != connectionToken {
			t.Fatalf("event %d changed the connection token", index)
		}
	}
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, raw := range []string{"host-1", "device-1", "epoch1", "event-ciphertext", "query-reply"} {
		if strings.Contains(string(contents), raw) {
			t.Fatalf("raw identity or payload %q leaked into the outlet", raw)
		}
	}
}

// latency/AC-01.drop-final latency/AC-06.sink-failure: a queue or budget
// refusal is a drop and a failed publish is a failure; both still name the
// client generation and the bytes that never completed the hop.
func TestBrokerTransportRecordsDropAndFailureOutcomes(t *testing.T) {
	cases := []struct {
		name string
		err  error
		want string
	}{
		{name: "queue full", err: ErrQueueFull, want: "dropped"},
		{name: "queue bytes", err: ErrQueueBytes, want: "dropped"},
		{name: "queue closed", err: ErrQueueClosed, want: "dropped"},
		{name: "message budget", err: ErrMessageBudget, want: "dropped"},
		{name: "publish failed", err: errors.New("mqtt down"), want: "failed"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			sink, path := openBrokerSink(t)
			service, session := observedServiceWithSession(t, sink)
			registerObservedDevice(service, "d1", 7, 1)
			session.publishErr = test.err
			payload := observedEventPayload(t, "h1", "d1")
			if err := service.PublishDeviceEvent(context.Background(), "h1", "d1", payload); err == nil {
				t.Fatal("expected publish error")
			}
			if err := sink.Close(); err != nil {
				t.Fatal(err)
			}
			events := transportEvents(t, path)
			if len(events) != 1 {
				t.Fatalf("transport events = %d want 1", len(events))
			}
			if events[0]["outcome"] != test.want {
				t.Fatalf("outcome = %v want %s", events[0]["outcome"], test.want)
			}
			if got := observedNumber(t, events[0]["bytes"]); got != uint64(len(payload)) {
				t.Fatalf("bytes = %d want %d", got, len(payload))
			}
			if got := observedNumber(t, events[0]["generation"]); got != 7 {
				t.Fatalf("generation = %d", got)
			}
		})
	}

	// A notice that exceeds the local envelope budget never reaches the
	// session and is recorded as dropped at the boundary that refused it.
	sink, path := openBrokerSink(t)
	service, _ := observedServiceWithSession(t, sink)
	registerObservedDevice(service, "device-1", 7, 1)
	service.transport.EnvelopeBytes = 1
	notice := &whalereinv1.RequestStateNotice{RequestId: "request-1", RelayVersion: RelayEpoch}
	if err := service.PublishDeviceNotice(context.Background(), "host-1", "device-1", notice); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("notice budget error = %v", err)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	events := transportEvents(t, path)
	if len(events) != 1 || events[0]["outcome"] != "dropped" {
		t.Fatalf("budget events = %v", events)
	}
	if observedNumber(t, events[0]["bytes"]) == 0 {
		t.Fatal("budget drop did not record its bytes")
	}
}

// latency/AC-05.restart latency/AC-11.socket-replaced: a replaced client
// connection keeps its own generation and token, so the two forwarding facts
// cannot be merged into one connection.
func TestBrokerTransportKeepsReplacedGenerationsDistinct(t *testing.T) {
	sink, path := openBrokerSink(t)
	service, _ := observedServiceWithSession(t, sink)
	registerObservedDevice(service, "d1", 4, 1)
	if err := service.PublishDeviceEvent(context.Background(), "h1", "d1", observedEventPayload(t, "h1", "d1")); err != nil {
		t.Fatal(err)
	}
	service.handleNotification(&Notification{
		Kind: "device", DeviceID: "d1", ConnID: 4,
		Epoch: "epoch1", Event: "disconnected", Sequence: 2,
	})
	registerObservedDevice(service, "d1", 5, 3)
	if err := service.PublishDeviceEvent(context.Background(), "h1", "d1", observedEventPayload(t, "h1", "d1")); err != nil {
		t.Fatal(err)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	events := transportEvents(t, path)
	if len(events) != 2 {
		t.Fatalf("transport events = %d want 2", len(events))
	}
	firstGeneration := observedNumber(t, events[0]["generation"])
	secondGeneration := observedNumber(t, events[1]["generation"])
	if firstGeneration != 4 || secondGeneration != 5 {
		t.Fatalf("generations = %d/%d", firstGeneration, secondGeneration)
	}
	firstToken, _ := events[0]["connection"].(string)
	secondToken, _ := events[1]["connection"].(string)
	if firstToken == "" || firstToken == secondToken {
		t.Fatalf("replaced connection token = %q/%q", firstToken, secondToken)
	}
}

// latency/AC-05.ambiguous: a device topic has no directed target, so zero or
// sibling client connections leave the generation unprovable and the hop
// unobserved instead of inventing a connection.
func TestBrokerTransportLeavesUnprovableClientHopsUnobserved(t *testing.T) {
	sink, path := openBrokerSink(t)
	service, _ := observedServiceWithSession(t, sink)

	// No live client connection.
	if err := service.PublishDeviceEvent(context.Background(), "h1", "d1", observedEventPayload(t, "h1", "d1")); err != nil {
		t.Fatal(err)
	}
	// Two sibling connections: neither one can be named as the receiver.
	registerObservedDevice(service, "device-1", 4, 1)
	registerObservedDevice(service, "device-1", 5, 2)
	if err := service.PublishDeviceEvent(context.Background(), "h1", "device-1", observedEventPayload(t, "h1", "device-1")); err != nil {
		t.Fatal(err)
	}
	// A non-reply publish toward a host is not a client forwarding leg.
	if err := service.PublishMessage(context.Background(), Message{
		Topic: PresenceTopic("h1"), Payload: []byte("presence"), QoS: 1,
	}); err != nil {
		t.Fatal(err)
	}
	// A directed reply without its reserved target cannot name a connection.
	if err := service.PublishMessage(context.Background(), Message{
		Topic: DeviceReplyTopic("h1", "device-1"), Payload: []byte("reply"), QoS: 1,
	}); err != nil {
		t.Fatal(err)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	if events := transportEvents(t, path); len(events) != 0 {
		t.Fatalf("unprovable hops emitted transport facts: %v", events)
	}
}

// latency/AC-06.cleanup: off mode creates no writer, so broker forwarding
// emits nothing.
func TestBrokerTransportDisabledModeCreatesNoOutlet(t *testing.T) {
	path := filepath.Join(t.TempDir(), "off.ndjson")
	sink, err := observation.Open(observation.Config{Mode: observation.ModeOff, Outlet: path})
	if err != nil {
		t.Fatal(err)
	}
	if sink != nil {
		t.Fatal("off mode created a sink")
	}
	service, _ := observedServiceWithSession(t, sink)
	registerObservedDevice(service, "d1", 7, 1)
	if err := service.PublishDeviceEvent(context.Background(), "h1", "d1", observedEventPayload(t, "h1", "d1")); err != nil {
		t.Fatal(err)
	}
	notice := &whalereinv1.RequestStateNotice{RequestId: "request-1", RelayVersion: RelayEpoch}
	if err := service.PublishDeviceNotice(context.Background(), "h1", "d1", notice); err != nil {
		t.Fatal(err)
	}
	target := ConnectionRef{Epoch: "epoch1", ID: 7}
	if err := service.PublishMessage(context.Background(), Message{
		Topic: DeviceReplyTopic("h1", "d1"), Payload: []byte("reply"), QoS: 1,
		Properties: MessageProperties{}.WithUserProperty(PropertyTarget, target.String()),
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("off mode outlet exists: %v", err)
	}
}

func TestTransportOutcomeVocabularyIsBounded(t *testing.T) {
	cases := map[string]observation.TransportOutcome{
		"ok":                 observation.TransportOutcomeOK,
		"queue full":         observation.TransportOutcomeDropped,
		"queue bytes":        observation.TransportOutcomeDropped,
		"queue closed":       observation.TransportOutcomeDropped,
		"message budget":     observation.TransportOutcomeDropped,
		"invalid topic":      observation.TransportOutcomeDropped,
		"unsupported relay":  observation.TransportOutcomeDropped,
		"wrapped queue full": observation.TransportOutcomeDropped,
		"publish error":      observation.TransportOutcomeFailed,
		"context deadline":   observation.TransportOutcomeFailed,
	}
	got := map[string]observation.TransportOutcome{
		"ok":                 transportOutcome(nil),
		"queue full":         transportOutcome(ErrQueueFull),
		"queue bytes":        transportOutcome(ErrQueueBytes),
		"queue closed":       transportOutcome(ErrQueueClosed),
		"message budget":     transportOutcome(ErrMessageBudget),
		"invalid topic":      transportOutcome(ErrInvalidTopic),
		"unsupported relay":  transportOutcome(ErrPeerTransportUnsupported),
		"wrapped queue full": transportOutcome(fmt.Errorf("submit: %w", ErrQueueFull)),
		"publish error":      transportOutcome(errors.New("mqtt down")),
		"context deadline":   transportOutcome(context.DeadlineExceeded),
	}
	for name, want := range cases {
		if got[name] != want {
			t.Fatalf("%s = %q want %q", name, got[name], want)
		}
	}
}

func TestClientTransportConnectionResolution(t *testing.T) {
	// A disabled outlet short-circuits before touching presence or parsing a
	// topic, so off and aggregate mode add no work to the publish path.
	disabled := &Service{}
	if _, ok := disabled.clientForwardConnection("device-1"); ok {
		t.Fatal("disabled outlet resolved a connection")
	}
	if _, ok := disabled.clientReplyConnection(Message{
		Topic:      DeviceReplyTopic("h1", "d1"),
		Properties: MessageProperties{}.WithUserProperty(PropertyTarget, ConnectionRef{Epoch: "epoch1", ID: 7}.String()),
	}); ok {
		t.Fatal("disabled outlet resolved a reply connection")
	}

	sink, _ := openBrokerSink(t)
	defer sink.Close()
	service := &Service{observation: sink}
	if _, ok := service.clientForwardConnection("device-1"); ok {
		t.Fatal("nil presence resolved a connection")
	}
	if _, ok := service.clientForwardConnection(""); ok {
		t.Fatal("empty device resolved a connection")
	}
	if _, ok := service.clientReplyConnection(Message{Topic: "bad topic"}); ok {
		t.Fatal("malformed topic resolved a reply connection")
	}
	if _, ok := service.clientReplyConnection(Message{Topic: PresenceTopic("h1")}); ok {
		t.Fatal("non-reply topic resolved a reply connection")
	}
	if _, ok := service.clientReplyConnection(Message{Topic: DeviceReplyTopic("h1", "d1")}); ok {
		t.Fatal("reply without a target resolved a connection")
	}
	ref := ConnectionRef{Epoch: "epoch1", ID: 7}
	resolved, ok := service.clientReplyConnection(Message{
		Topic:      DeviceReplyTopic("h1", "d1"),
		Properties: MessageProperties{}.WithUserProperty(PropertyTarget, ref.String()),
	})
	if !ok || resolved != ref {
		t.Fatalf("resolved = %+v %v", resolved, ok)
	}

	// A zero generation or a negative byte count is never exported.
	service.observeClientTransport(ConnectionRef{Epoch: "epoch1", ID: 0}, 1, nil)
	service.observeClientTransport(ref, -1, nil)
}

func observedEventPayload(t *testing.T, hostID, deviceID string) []byte {
	t.Helper()
	payload, err := proto.Marshal(&whalereinv1.EventEnvelope{
		HostId: hostID, DeviceId: deviceID, RelayVersion: RelayEpoch,
	})
	if err != nil {
		t.Fatal(err)
	}
	return payload
}
