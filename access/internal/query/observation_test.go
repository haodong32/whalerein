package query

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/observation"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func openTestSink(t *testing.T) (*observation.Sink, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "access.ndjson")
	sink, err := observation.Open(observation.Config{
		Mode: observation.ModeTrace, Outlet: path, RunID: "run-query-test",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-query-test",
	})
	if err != nil {
		t.Fatal(err)
	}
	return sink, path
}

func newObservedHarness(t *testing.T, sink *observation.Sink) *harness {
	t.Helper()
	h := newHarness(t)
	service, err := New(Dependencies{
		Store:       h.store,
		Connections: h.connections,
		Hosts:       h.hosts,
		Publisher:   h.publisher,
		Config: Config{
			MaxWaiters:              16,
			MaxWaitersPerConnection: 8,
			MaxTTL:                  time.Minute,
			Now:                     func() time.Time { return h.now },
		},
		Metrics:     diagnostics.NewMetrics(),
		Observation: sink,
	})
	if err != nil {
		t.Fatal(err)
	}
	h.service = service
	return h
}

func queryEvents(t *testing.T, path string) []map[string]any {
	t.Helper()
	records, err := observation.ReadRecords(path)
	if err != nil {
		t.Fatal(err)
	}
	events := make([]map[string]any, 0, len(records))
	for _, record := range records {
		if record["event"] == "access_query" {
			events = append(events, record)
		}
	}
	return events
}

func dataOf(record map[string]any) map[string]any {
	return record["data"].(map[string]any)
}

// latency/AC-02.query-inline: an inline query emits admission, forwarding and
// reply at the boundaries that own them, with one stable opaque request token.
func TestQueryInlineEmitsAdmissionForwardReply(t *testing.T) {
	sink, path := openTestSink(t)
	h := newObservedHarness(t, sink)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	result := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, DeviceId: testDevice,
		Status: whalereinv1.OnlineStatus_succeeded, ChannelId: "channel-1",
		ResultBody: []byte("answer"), ProtocolVersion: 1, RelayVersion: broker.RelayEpoch,
	}
	expiry := uint32(20)
	message := broker.Message{Topic: broker.ResultTopic(testHost), Payload: mustMarshal(t, result)}
	message.Properties.MessageExpiry = &expiry
	if !h.service.Complete(context.Background(), testHost, message) {
		t.Fatal("result was not consumed")
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := queryEvents(t, path)
	if len(events) != 3 {
		t.Fatalf("query events = %d: %v", len(events), events)
	}
	wantStages := []string{"admit", "forward", "reply"}
	var token any
	for index, event := range events {
		data := dataOf(event)
		if data["stage"] != wantStages[index] {
			t.Fatalf("stage %d = %v", index, data["stage"])
		}
		if data["route"] != "inline" {
			t.Fatalf("route = %v", data["route"])
		}
		if token == nil {
			token = data["request"]
		} else if data["request"] != token {
			t.Fatal("request token changed across the query lifecycle")
		}
	}
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(contents), "request-1") {
		t.Fatal("raw request id leaked into the outlet")
	}
}

// latency/AC-02.expired: a deadline miss is an explicit expiry stage, not a
// successful reply, and a sweep emits the same explicit disposition.
func TestQueryExpiredEmitsExpireDisposition(t *testing.T) {
	sink, path := openTestSink(t)
	h := newObservedHarness(t, sink)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	h.now = h.now.Add(31 * time.Second)
	result := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, DeviceId: testDevice,
		Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: broker.RelayEpoch,
	}
	message := broker.Message{Topic: broker.ResultTopic(testHost), Payload: mustMarshal(t, result)}
	if !h.service.Complete(context.Background(), testHost, message) {
		t.Fatal("expired waiter was not consumed")
	}

	if err := h.service.Admit(context.Background(), h.queryMessage(t, func(request *whalereinv1.HostRequest, message *broker.Message) {
		request.RequestId = "request-2"
		message.Properties.CorrelationData = []byte("request-2")
	})); err != nil {
		t.Fatal(err)
	}
	h.now = h.now.Add(31 * time.Second)
	if removed := h.service.Sweep(); removed != 1 {
		t.Fatalf("swept = %d", removed)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := queryEvents(t, path)
	var occurred []string
	for _, event := range events {
		data := dataOf(event)
		if data["stage"] == "expire" {
			occurred = append(occurred, data["outcome"].(string))
		}
	}
	if len(occurred) != 2 {
		t.Fatalf("expiry events = %d (%v)", len(occurred), events)
	}
	if occurred[0] != "late" || occurred[1] != "expired" {
		t.Fatalf("expiry outcomes = %v want [late expired]", occurred)
	}
}

// latency/AC-02.late: a Host result that arrives after the caller deadline is
// consumed as a late settlement, records the late outcome and publishes no
// reply that could turn the observed timeout into success.
func TestQueryLateResultSettlesWithoutReply(t *testing.T) {
	sink, path := openTestSink(t)
	h := newObservedHarness(t, sink)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	h.now = h.now.Add(31 * time.Second)
	result := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, DeviceId: testDevice,
		Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: broker.RelayEpoch,
	}
	message := broker.Message{Topic: broker.ResultTopic(testHost), Payload: mustMarshal(t, result)}
	if !h.service.Complete(context.Background(), testHost, message) {
		t.Fatal("late result was not consumed")
	}
	if got := len(h.publisher.snapshot()); got != 1 {
		t.Fatalf("late result published %d replies; want only the forward", got-1)
	}
	if h.service.waiterCount() != 0 {
		t.Fatal("late result did not settle the waiter")
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	var late bool
	for _, event := range queryEvents(t, path) {
		data := dataOf(event)
		if data["stage"] == "expire" && data["outcome"] == "late" {
			late = true
		}
		if data["stage"] == "reply" {
			t.Fatal("late result produced a reply event")
		}
	}
	if !late {
		t.Fatal("late settlement was not recorded")
	}
}

// latency/AC-05.concurrent: two devices may use the same client-generated
// request id concurrently; each operation keeps its own stable token.
func TestQuerySameIDAcrossDevicesStaysDistinct(t *testing.T) {
	sink, path := openTestSink(t)
	h := newObservedHarness(t, sink)
	otherRef := broker.ConnectionRef{Epoch: testEpoch, ID: 8}
	h.connections.refs["d2"] = map[broker.ConnectionRef]bool{otherRef: true}
	grant := h.store.grants[testHost+"\x00"+testDevice]
	grant.DeviceID = "d2"
	h.store.grants[testHost+"\x00d2"] = grant

	messages := []broker.Message{
		h.queryMessage(t, nil),
		h.queryMessage(t, func(request *whalereinv1.HostRequest, message *broker.Message) {
			request.DeviceId = "d2"
			request.RequestId = "request-1"
			message.Topic = broker.DeviceQueryTopic(testHost, "d2")
			message.Properties.ResponseTopic = broker.DeviceReplyTopic(testHost, "d2")
			message.Properties.CorrelationData = []byte("request-1")
			message.Properties.UserProperties = []broker.UserProperty{{Key: broker.PropertyConnection, Value: otherRef.String()}}
		}),
	}
	var wait sync.WaitGroup
	for _, message := range messages {
		message := message
		wait.Add(1)
		go func() {
			defer wait.Done()
			if err := h.service.Admit(context.Background(), message); err != nil {
				t.Errorf("admit: %v", err)
			}
		}()
	}
	wait.Wait()
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := queryEvents(t, path)
	requests := map[string]bool{}
	scopes := map[string]bool{}
	admissions := 0
	for _, event := range events {
		data := dataOf(event)
		requests[data["request"].(string)] = true
		scopes[data["scope"].(string)] = true
		if data["stage"] == "admit" {
			admissions++
		}
	}
	if admissions != 2 {
		t.Fatalf("admissions = %d", admissions)
	}
	if len(requests) != 1 {
		t.Fatalf("the same raw request id must join to one request token, got %d", len(requests))
	}
	if len(scopes) != 2 {
		t.Fatalf("same request id across devices did not produce distinct scope tokens: %d", len(scopes))
	}
}
