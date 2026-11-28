package selfstatus

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type fakePresence struct {
	online bool
	known  bool
}

func (f fakePresence) HostPresence(string) (bool, bool) { return f.online, f.known }

type fakePublisher struct {
	mu        sync.Mutex
	messages  []broker.Message
	onPublish func(broker.Message)
	err       error
}

func (f *fakePublisher) PublishMessage(_ context.Context, message broker.Message) error {
	f.mu.Lock()
	f.messages = append(f.messages, message)
	callback := f.onPublish
	err := f.err
	f.mu.Unlock()
	if err != nil {
		return err
	}
	if callback != nil {
		callback(message)
	}
	return nil
}

func (f *fakePublisher) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.messages)
}

func (f *fakePublisher) last(t *testing.T) broker.Message {
	t.Helper()
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.messages) == 0 {
		t.Fatal("no published message")
	}
	return f.messages[len(f.messages)-1]
}

func newTestService(t *testing.T, presence Presence, publisher Publisher, mutate func(*Config)) *Service {
	t.Helper()
	cfg := Config{}
	if mutate != nil {
		mutate(&cfg)
	}
	service, err := New(Dependencies{
		Presence: presence, Publisher: publisher, Config: cfg,
		Metrics: diagnostics.NewMetrics(),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	return service
}

func challenge(seed byte) []byte {
	value := make([]byte, 24)
	for index := range value {
		value[index] = seed + byte(index)
	}
	return value
}

func decodeRequest(t *testing.T, message broker.Message) *whalereinv1.HostRequest {
	t.Helper()
	var request whalereinv1.HostRequest
	if err := proto.Unmarshal(message.Payload, &request); err != nil {
		t.Fatal(err)
	}
	return &request
}

func successResult(t *testing.T, requestID, grantID string, value []byte, active bool, revokedAt int64) *whalereinv1.HostResult {
	t.Helper()
	body, err := proto.Marshal(&whalereinv1.SelfStatus{
		GrantId: grantID, Challenge: value, Active: active, RevokedAtUnixMs: revokedAt,
	})
	if err != nil {
		t.Fatal(err)
	}
	return &whalereinv1.HostResult{
		RequestId:    requestID,
		HostId:       "h1",
		Status:       whalereinv1.OnlineStatus_succeeded,
		ResultBody:   body,
		RelayVersion: broker.RelayEpoch,
	}
}

func TestSubmitRelaysOneBoundedServiceFrameAndReturnsTheExactHostReply(t *testing.T) {
	value := challenge(1)
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		reply := successResult(t, request.GetRequestId(), "g1", value, false, 1_700_000_000_000)
		if !service.Complete(context.Background(), "h1", reply) {
			t.Error("exact tuple did not settle the waiter")
		}
		// A duplicate reply after the exchange ended is never a second answer.
		if service.Complete(context.Background(), "h1", reply) {
			t.Error("duplicate reply resolved a second time")
		}
	}
	status, err := service.Submit(context.Background(), Input{
		HostID: "h1", GrantID: "g1", Challenge: value, Caller: "caller-1",
	})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	if status.GetGrantId() != "g1" || status.GetActive() || status.GetRevokedAtUnixMs() != 1_700_000_000_000 {
		t.Fatalf("status = %+v", status)
	}

	message := publisher.last(t)
	if message.Topic != broker.SelfStatusRequestTopic("h1") {
		t.Fatalf("topic = %q", message.Topic)
	}
	if message.Topic == broker.RequestTopic("h1") {
		t.Fatal("self-status used the ordinary Host request topic")
	}
	if string(message.Properties.CorrelationData) == "" {
		t.Fatal("missing correlation data")
	}
	if message.Properties.ResponseTopic != broker.SelfStatusReplyTopic("h1") {
		t.Fatalf("response topic = %q", message.Properties.ResponseTopic)
	}
	if message.QoS != 1 || message.Properties.MessageExpiry == nil || *message.Properties.MessageExpiry == 0 {
		t.Fatalf("qos/expiry = %d/%v", message.QoS, message.Properties.MessageExpiry)
	}
	request := decodeRequest(t, message)
	if request.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS {
		t.Fatalf("kind = %v", request.GetKind())
	}
	if request.GetDeviceId() != "" || request.GetCommandId() != "" || request.GetChannelId() != "" ||
		request.GetBodyReference() != nil {
		t.Fatalf("ordinary fields leaked into the service frame: %+v", request)
	}
	if string(message.Properties.CorrelationData) != request.GetRequestId() {
		t.Fatal("correlation data is not the request id")
	}
	var frame whalereinv1.SelfStatusRequest
	if err := proto.Unmarshal(request.GetBody(), &frame); err != nil {
		t.Fatal(err)
	}
	if frame.GetGrantId() != "g1" || !bytes.Equal(frame.GetChallenge(), value) {
		t.Fatalf("frame = %+v", &frame)
	}
}

func TestSubmitIgnoresForeignTupleRepliesUntilTheBoundedDeadline(t *testing.T) {
	value := challenge(2)
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, func(cfg *Config) {
		cfg.ExchangeTimeout = 40 * time.Millisecond
	})
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		// Correct request id, wrong challenge.
		foreign := successResult(t, request.GetRequestId(), "g1", challenge(99), true, 0)
		if service.Complete(context.Background(), "h1", foreign) {
			t.Error("foreign challenge resolved the waiter")
		}
		// Correct request id, wrong grant.
		foreign = successResult(t, request.GetRequestId(), "g2", value, true, 0)
		if service.Complete(context.Background(), "h1", foreign) {
			t.Error("foreign grant resolved the waiter")
		}
		// Correct tuple on the wrong Host topic.
		foreign = successResult(t, request.GetRequestId(), "g1", value, true, 0)
		if service.Complete(context.Background(), "h2", foreign) {
			t.Error("foreign host resolved the waiter")
		}
	}
	_, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value})
	if !errors.Is(err, ErrTimeout) {
		t.Fatalf("err = %v, want timeout", err)
	}
	// A stale late reply cannot settle an exchange that already ended.
	if service.Complete(context.Background(), "h1", successResult(t, "stale", "g1", value, true, 0)) {
		t.Fatal("stale reply resolved an ended exchange")
	}
}

func TestSubmitMapsHostRejectionToTheInaccessibleAnswer(t *testing.T) {
	value := challenge(3)
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		service.Complete(context.Background(), "h1", &whalereinv1.HostResult{
			RequestId: request.GetRequestId(), HostId: "h1",
			Status: whalereinv1.OnlineStatus_rejected, RelayVersion: broker.RelayEpoch,
		})
	}
	_, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value})
	if !errors.Is(err, ErrInaccessible) {
		t.Fatalf("err = %v, want inaccessible", err)
	}
}

func TestSubmitReturnsPendingWhenTheHostIsOfflineOrPresenceIsUnreconciled(t *testing.T) {
	value := challenge(4)
	publisher := &fakePublisher{}
	offline := newTestService(t, fakePresence{known: true}, publisher, nil)
	if _, err := offline.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); !errors.Is(err, ErrHostOffline) {
		t.Fatalf("offline err = %v", err)
	}
	unreconciled := newTestService(t, fakePresence{}, publisher, nil)
	if _, err := unreconciled.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("unreconciled err = %v, want pending", err)
	}
	if publisher.count() != 0 {
		t.Fatal("an unavailable Host received a publish")
	}
}

func TestSubmitRejectsAReusedChallenge(t *testing.T) {
	value := challenge(5)
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		service.Complete(context.Background(), "h1", successResult(t, request.GetRequestId(), "g1", value, true, 0))
	}
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); err != nil {
		t.Fatal(err)
	}
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); !errors.Is(err, ErrReplay) {
		t.Fatalf("second submission err = %v, want replay refusal", err)
	}
	if publisher.count() != 1 {
		t.Fatalf("published %d frames, want 1", publisher.count())
	}
	// The same challenge on a different Host is a distinct tuple.
	otherPublisher := &fakePublisher{}
	other := newTestService(t, fakePresence{online: true, known: true}, otherPublisher, nil)
	otherPublisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		reply := successResult(t, request.GetRequestId(), "g1", value, true, 0)
		reply.HostId = "h2"
		other.Complete(context.Background(), "h2", reply)
	}
	if _, err := other.Submit(context.Background(), Input{HostID: "h2", GrantID: "g1", Challenge: value}); err != nil {
		t.Fatalf("foreign host challenge err = %v", err)
	}
}

func TestSubmitBoundsPerHostAndPerCallerAdmission(t *testing.T) {
	value := challenge(6)
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, func(cfg *Config) {
		cfg.MaxInflightPerHost = 1
		cfg.MaxInflightPerCaller = 1
		cfg.ExchangeTimeout = 5 * time.Second
	})
	published := make(chan struct{}, 1)
	publisher.onPublish = func(broker.Message) { published <- struct{}{} }
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := service.Submit(ctx, Input{HostID: "h1", GrantID: "g1", Challenge: value, Caller: "caller-1"})
		done <- err
	}()
	<-published
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g2", Challenge: challenge(7), Caller: "caller-2"}); !errors.Is(err, ErrBusy) {
		t.Fatalf("per-host admission err = %v, want busy", err)
	}
	cancel()
	<-done
}

func TestSubmitRejectsMalformedFramesAndIdentifiers(t *testing.T) {
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	cases := []struct {
		name  string
		input Input
	}{
		{"rejects a missing grant id", Input{HostID: "h1", Challenge: challenge(1)}},
		{"rejects a wildcard grant id", Input{HostID: "h1", GrantID: "g/+", Challenge: challenge(1)}},
		{"rejects a short challenge", Input{HostID: "h1", GrantID: "g1", Challenge: []byte("short")}},
		{"rejects an oversized challenge", Input{HostID: "h1", GrantID: "g1", Challenge: make([]byte, MaxChallengeBytes+1)}},
		{"rejects an invalid host id", Input{HostID: "h/+", GrantID: "g1", Challenge: challenge(1)}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := service.Submit(context.Background(), tc.input); !errors.Is(err, ErrInvalid) {
				t.Fatalf("err = %v, want invalid", err)
			}
		})
	}
	if publisher.count() != 0 {
		t.Fatal("invalid frames were published")
	}
}

func TestSubmitSurfacesAPublishFailureAsPendingAndKeepsTheChallengeUsable(t *testing.T) {
	publisher := &fakePublisher{err: broker.ErrUnavailable}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	value := challenge(8)
	_, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value})
	if !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("err = %v, want unavailable", err)
	}
	// The frame never reached the Host, so the same challenge may be retried.
	publisher.mu.Lock()
	publisher.err = nil
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		service.Complete(context.Background(), "h1",
			successResult(t, request.GetRequestId(), "g1", value, true, 0))
	}
	publisher.mu.Unlock()
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); err != nil {
		t.Fatalf("retry after publish failure: %v", err)
	}
}
