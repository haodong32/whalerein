package broker

import (
	"context"
	"io"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func TestSelfStatusServiceTopicsArePrivateAndParseExactly(t *testing.T) {
	for _, tc := range []struct {
		topic string
		kind  TopicKind
	}{
		{SelfStatusRequestTopic("h1"), KindServiceSelfStatusRequest},
		{SelfStatusReplyTopic("h1"), KindServiceSelfStatusReply},
	} {
		parsed, err := ParseTopic(tc.topic)
		if err != nil {
			t.Fatalf("parse %q: %v", tc.topic, err)
		}
		if parsed.HostID != "h1" || parsed.DeviceID != "" || parsed.Kind != tc.kind {
			t.Fatalf("parsed %q = %+v", tc.topic, parsed)
		}
	}
	for _, bad := range []string{
		"whalerein/v1/h/h1/svc/self-status",
		"whalerein/v1/h/h1/svc/self-status/other",
		"whalerein/v1/h/h1/svc/other/request",
		"whalerein/v1/h/+/svc/self-status/request",
		"whalerein/v1/h/h1/d/svc/self-status/request",
		"whalerein/v1/h/h1/svc/self-status/request/extra",
	} {
		if _, err := ParseTopic(bad); err == nil {
			t.Fatalf("expected a parse error for %q", bad)
		}
	}
}

type fakeSelfStatus struct {
	mu      sync.Mutex
	hosts   []string
	results []*whalereinv1.HostResult
}

func (f *fakeSelfStatus) Complete(_ context.Context, hostID string, result *whalereinv1.HostResult) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.hosts = append(f.hosts, hostID)
	f.results = append(f.results, result)
	return true
}

func (f *fakeSelfStatus) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.results)
}

func TestSelfStatusReplyRoutesOnlyFromThePrivateServiceTopic(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	handler := &fakeSelfStatus{}
	service.SetSelfStatusHandler(handler)
	var durableResults int
	service.SetResultHandler(func(context.Context, string, *whalereinv1.HostResult) error {
		durableResults++
		return nil
	})

	body, _ := proto.Marshal(&whalereinv1.SelfStatus{GrantId: "g1", Challenge: []byte("challenge-000001"), Active: false})
	result, _ := proto.Marshal(&whalereinv1.HostResult{
		RequestId: "s1", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded,
		ResultBody: body, RelayVersion: RelayEpoch,
	})
	service.HandleMessage(Message{Topic: SelfStatusReplyTopic("h1"), Payload: result})
	if handler.count() != 1 {
		t.Fatalf("self-status handler calls = %d", handler.count())
	}
	if durableResults != 0 {
		t.Fatal("self-status reply entered the durable command path")
	}
	handler.mu.Lock()
	if handler.hosts[0] != "h1" || handler.results[0].GetRequestId() != "s1" {
		t.Fatalf("routed = %q %+v", handler.hosts[0], handler.results[0])
	}
	handler.mu.Unlock()

	// A reply on the ordinary Host result topic remains a durable result.
	ordinary, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "s2", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch})
	service.HandleMessage(Message{Topic: ResultTopic("h1"), Payload: ordinary})
	if handler.count() != 1 {
		t.Fatal("ordinary result reached the self-status handler")
	}
	if durableResults != 1 {
		t.Fatalf("ordinary durable results = %d", durableResults)
	}
}

func TestSelfStatusReplyRejectsForeignOrStaleEnvelopes(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	handler := &fakeSelfStatus{}
	service.SetSelfStatusHandler(handler)

	cases := map[string][]byte{
		"wrong relay epoch": mustResult(t, &whalereinv1.HostResult{RequestId: "s1", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch - 1}),
		"host mismatch":     mustResult(t, &whalereinv1.HostResult{RequestId: "s1", HostId: "h2", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch}),
		"malformed":         []byte("not a HostResult"),
	}
	for name, payload := range cases {
		t.Run(name, func(t *testing.T) {
			service.HandleMessage(Message{Topic: SelfStatusReplyTopic("h1"), Payload: payload})
			if handler.count() != 0 {
				t.Fatal("foreign envelope reached the self-status handler")
			}
		})
	}

	// A copy of Access's own service request is never input.
	service.HandleMessage(Message{Topic: SelfStatusRequestTopic("h1"), Payload: []byte("echo")})
	if handler.count() != 0 {
		t.Fatal("service request echo was admitted")
	}
}

func mustResult(t *testing.T, result *whalereinv1.HostResult) []byte {
	t.Helper()
	payload, err := proto.Marshal(result)
	if err != nil {
		t.Fatal(err)
	}
	return payload
}

func TestServiceWithoutSelfStatusHandlerDropsTheReply(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	service.SetSelfStatusHandler(nil)
	payload, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "s1", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch})
	// No panic and no handler; the bounded route records a routing error.
	service.HandleMessage(Message{Topic: SelfStatusReplyTopic("h1"), Payload: payload})
}

func TestSelfStatusHandlerCanBeReplacedConcurrently(t *testing.T) {
	service, err := New(Dependencies{
		Presence:  NewPresence(50*time.Millisecond, diagnostics.NewMetrics(), slog.New(slog.NewTextHandler(io.Discard, nil))),
		Connector: &fakeConnector{sessions: []*fakeSession{newFakeSession()}},
		Metrics:   diagnostics.NewMetrics(),
		Logger:    slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	var wait sync.WaitGroup
	for i := 0; i < 8; i++ {
		wait.Add(2)
		go func() {
			defer wait.Done()
			service.SetSelfStatusHandler(&fakeSelfStatus{})
		}()
		go func() {
			defer wait.Done()
			service.SetSelfStatusHandler(nil)
		}()
	}
	wait.Wait()
}
