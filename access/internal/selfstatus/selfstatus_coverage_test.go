package selfstatus

import (
	"bytes"
	"context"
	"errors"
	"math"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestNewRejectsMissingBoundedDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected a missing presence/publisher error")
	}
	if _, err := New(Dependencies{Publisher: &fakePublisher{}}); err == nil {
		t.Fatal("expected a missing presence error")
	}
	if _, err := New(Dependencies{Presence: fakePresence{}}); err == nil {
		t.Fatal("expected a missing publisher error")
	}
	// Defaults fill every optional dependency and bound.
	service, err := New(Dependencies{Presence: fakePresence{}, Publisher: &fakePublisher{}})
	if err != nil {
		t.Fatal(err)
	}
	if service.MaxFrameSize() != MaxFrameBytes {
		t.Fatalf("max frame = %d", service.MaxFrameSize())
	}
	custom, err := New(Dependencies{
		Presence: fakePresence{}, Publisher: &fakePublisher{},
		Config: Config{MaxFrameBytes: 128},
	})
	if err != nil || custom.MaxFrameSize() != 128 {
		t.Fatalf("custom max frame = %d err=%v", custom.MaxFrameSize(), err)
	}
}

func TestRemainingSecondsIsBounded(t *testing.T) {
	if remainingSeconds(0) != nil || remainingSeconds(-time.Second) != nil {
		t.Fatal("a non-positive timeout must not publish")
	}
	if got := remainingSeconds(1500 * time.Millisecond); got == nil || *got != 2 {
		t.Fatalf("rounded expiry = %v", got)
	}
	if remainingSeconds(time.Duration(math.MaxInt64)) != nil {
		t.Fatal("an expiry above the MQTT interval ceiling must be omitted")
	}
}

func TestOutcomeNameCoversEveryRefusal(t *testing.T) {
	cases := map[error]string{
		ErrInaccessible:     "inaccessible",
		ErrHostOffline:      "offline",
		ErrHostUnavailable:  "unavailable",
		ErrTimeout:          "timeout",
		ErrBusy:             "busy",
		ErrReplay:           "replay",
		ErrInvalid:          "invalid",
		errors.New("other"): "invalid",
	}
	for err, want := range cases {
		if got := outcomeName(err); got != want {
			t.Fatalf("outcomeName(%v) = %q, want %q", err, got, want)
		}
	}
}

func TestCompleteRejectsNilMalformedAndForeignReplies(t *testing.T) {
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	if service.Complete(context.Background(), "h1", nil) {
		t.Fatal("a nil reply settled an exchange")
	}
	if service.Complete(context.Background(), "h1", &whalereinv1.HostResult{}) {
		t.Fatal("an empty request id settled an exchange")
	}
	if service.Complete(context.Background(), "h1", &whalereinv1.HostResult{RequestId: "x", HostId: "h2"}) {
		t.Fatal("a foreign host reply settled an exchange")
	}

	value := challenge(10)
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		for name, reply := range map[string]*whalereinv1.HostResult{
			"empty body": {RequestId: request.GetRequestId(), HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: broker.RelayEpoch},
			"invalid body": {RequestId: request.GetRequestId(), HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded,
				ResultBody: []byte("not a SelfStatus"), RelayVersion: broker.RelayEpoch},
			"oversized body": {RequestId: request.GetRequestId(), HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded,
				ResultBody: make([]byte, MaxFrameBytes+1), RelayVersion: broker.RelayEpoch},
		} {
			if service.Complete(context.Background(), "h1", reply) {
				t.Errorf("%s settled the exchange", name)
			}
		}
		service.Complete(context.Background(), "h1", successResult(t, request.GetRequestId(), "g1", value, true, 0))
	}
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); err != nil {
		t.Fatal(err)
	}
}

func TestCompleteMapsNonTerminalStatusToPending(t *testing.T) {
	value := challenge(11)
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		service.Complete(context.Background(), "h1", &whalereinv1.HostResult{
			RequestId: request.GetRequestId(), HostId: "h1",
			Status: whalereinv1.OnlineStatus_failed, RelayVersion: broker.RelayEpoch,
		})
	}
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("err = %v, want pending", err)
	}
}

func TestSubmitCarriesTheBoundedTraceAndDropsAnOversizedOne(t *testing.T) {
	value := challenge(12)
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		service.Complete(context.Background(), "h1", successResult(t, request.GetRequestId(), "g1", value, true, 0))
	}
	if _, err := service.Submit(context.Background(), Input{
		HostID: "h1", GrantID: "g1", Challenge: value, TraceID: "trace-1",
	}); err != nil {
		t.Fatal(err)
	}
	if got := decodeRequest(t, publisher.last(t)).GetTraceId(); got != "trace-1" {
		t.Fatalf("trace = %q", got)
	}

	other := challenge(13)
	otherService := newTestService(t, fakePresence{online: true, known: true}, publisher, nil)
	publisher.onPublish = func(message broker.Message) {
		request := decodeRequest(t, message)
		otherService.Complete(context.Background(), "h1", successResult(t, request.GetRequestId(), "g1", other, true, 0))
	}
	if _, err := otherService.Submit(context.Background(), Input{
		HostID: "h1", GrantID: "g1", Challenge: other, TraceID: string(bytes.Repeat([]byte("t"), 65)),
	}); err != nil {
		t.Fatal(err)
	}
	if got := decodeRequest(t, publisher.last(t)).GetTraceId(); got != "" {
		t.Fatalf("oversized trace was forwarded: %q", got)
	}
}

func TestSubmitTreatsAnUnreadableRandomSourceAsPending(t *testing.T) {
	service := newTestService(t, fakePresence{online: true, known: true}, &fakePublisher{}, func(cfg *Config) {
		cfg.Random = bytes.NewReader(nil)
	})
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: challenge(14)}); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("err = %v", err)
	}
}

func TestSubmitTreatsARepeatedCorrelationIdAsPending(t *testing.T) {
	value := challenge(15)
	publisher := &fakePublisher{}
	published := make(chan struct{}, 1)
	publisher.onPublish = func(broker.Message) { published <- struct{}{} }
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, func(cfg *Config) {
		cfg.ExchangeTimeout = 2 * time.Second
		cfg.MaxInflightPerHost = 4
		cfg.MaxInflightPerCaller = 4
		// Every read yields the same sixteen bytes, so the second submission
		// collides with the live waiter's correlation id on every retry.
		cfg.Random = bytes.NewReader(bytes.Repeat([]byte{0}, 128))
	})
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := service.Submit(ctx, Input{HostID: "h1", GrantID: "g1", Challenge: value})
		done <- err
	}()
	<-published
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g2", Challenge: challenge(16)}); !errors.Is(err, ErrHostUnavailable) {
		t.Fatalf("err = %v", err)
	}
	cancel()
	<-done
}

func TestPruneExpiresAndEvictsTheOldestChallenge(t *testing.T) {
	value := challenge(17)
	now := time.Unix(1_700_000_000, 0).UTC()
	publisher := &fakePublisher{}
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, func(cfg *Config) {
		cfg.Now = func() time.Time { return now }
		cfg.ReplayTTL = time.Minute
		cfg.MaxChallenges = 1
		cfg.MaxInflightPerHost = 2
		cfg.MaxInflightPerCaller = 2
	})
	complete := func(challengeValue []byte) {
		publisher.onPublish = func(message broker.Message) {
			request := decodeRequest(t, message)
			service.Complete(context.Background(), "h1",
				successResult(t, request.GetRequestId(), "g1", challengeValue, true, 0))
		}
	}
	complete(value)
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); err != nil {
		t.Fatal(err)
	}
	// An expired tombstone no longer blocks reuse.
	now = now.Add(2 * time.Minute)
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); err != nil {
		t.Fatalf("expired challenge was still rejected: %v", err)
	}
	// The one-entry ceiling evicts the oldest tombstone instead of growing.
	second := challenge(18)
	complete(second)
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: second}); err != nil {
		t.Fatal(err)
	}
	complete(value)
	if _, err := service.Submit(context.Background(), Input{HostID: "h1", GrantID: "g1", Challenge: value}); err != nil {
		t.Fatalf("evicted challenge was still rejected: %v", err)
	}
}

func TestReleaseLockedKeepsSiblingWaitersCounted(t *testing.T) {
	publisher := &fakePublisher{}
	published := make(chan struct{}, 2)
	publisher.onPublish = func(broker.Message) { published <- struct{}{} }
	service := newTestService(t, fakePresence{online: true, known: true}, publisher, func(cfg *Config) {
		cfg.ExchangeTimeout = 2 * time.Second
		cfg.MaxInflightPerHost = 2
		cfg.MaxInflightPerCaller = 2
	})
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{}, 2)
	for _, value := range [][]byte{challenge(19), challenge(20)} {
		go func(challengeValue []byte) {
			_, _ = service.Submit(ctx, Input{HostID: "h1", GrantID: "g1", Challenge: challengeValue, Caller: "shared"})
			done <- struct{}{}
		}(value)
	}
	<-published
	<-published
	cancel()
	<-done
	<-done
	service.mu.Lock()
	defer service.mu.Unlock()
	if service.inflight != 0 || len(service.byHost) != 0 || len(service.byCaller) != 0 {
		t.Fatalf("leaked admission counters: inflight=%d host=%v caller=%v",
			service.inflight, service.byHost, service.byCaller)
	}
}
