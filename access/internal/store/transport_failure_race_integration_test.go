//go:build integration

package store_test

import (
	"bytes"
	"context"
	"errors"
	"sync/atomic"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type transportRaceBroker struct {
	entered     chan struct{}
	release     chan struct{}
	calls       atomic.Int32
	firstError  error
	secondError error
}

func (b *transportRaceBroker) PublishPresence(context.Context, string, []byte) error { return nil }

func (b *transportRaceBroker) PublishRequest(ctx context.Context, _ string, _ *whalereinv1.HostRequest) error {
	if b.calls.Add(1) == 1 {
		close(b.entered)
		select {
		case <-b.release:
			return b.firstError
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	return b.secondError
}

type transportRacePresence struct{}

func (transportRacePresence) IsHostOnline(context.Context, string) bool { return true }

// A stale preflight failure must not finalize a command published by a later lease.
func TestTransportPreflightCannotRejectConcurrentPublication(t *testing.T) {
	db, _ := storetest.New(t)
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	now := time.Now().UTC().Truncate(time.Microsecond)
	seedDevice(t, db, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := db.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	transport := &transportRaceBroker{entered: make(chan struct{}), release: make(chan struct{}), firstError: broker.ErrPeerTransportUnsupported}
	service := func(clock time.Time) *requests.Service {
		s, err := requests.New(requests.Dependencies{Store: db, Broker: transport, Presence: transportRacePresence{},
			Now: func() time.Time { return clock }, Config: requests.Config{ProtocolVersion: 1, MaxTTL: time.Hour,
				ForwardInterval: time.Second, Lease: time.Second, RetryInterval: time.Second, MaxAttempts: 3, BatchSize: 10}})
		if err != nil {
			t.Fatal(err)
		}
		return s
	}
	first, second := service(now), service(now.Add(2*time.Second))
	done := make(chan error, 1)
	go func() { done <- first.ForwardDue(ctx) }()
	select {
	case <-transport.entered:
	case <-ctx.Done():
		t.Fatal("first worker did not reach transport")
	}
	if err := second.ForwardDue(ctx); err != nil {
		t.Fatal(err)
	}
	close(transport.release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	stored, err := db.GetRequest(ctx, "r1")
	if err != nil {
		t.Fatal(err)
	}
	if !stored.PublishedOnce {
		t.Fatal("second worker publication not recorded")
	}
	if stored.Status == whalereinv1.OnlineStatus_rejected || stored.Status == whalereinv1.OnlineStatus_host_offline {
		t.Fatalf("possibly executed command was falsely finalized: %s", stored.Status)
	}
	if err := first.HandleHostResult(ctx, "h1", &whalereinv1.HostResult{RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: whalereinv1.OnlineStatus_succeeded}); err != nil {
		t.Fatal(err)
	}
	stored, err = db.GetRequest(ctx, "r1")
	if err != nil || stored.Status != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("late result status=%s err=%v", stored.Status, err)
	}
}

type preflightFailureBroker struct{}

func (preflightFailureBroker) PublishPresence(context.Context, string, []byte) error { return nil }

func (preflightFailureBroker) PublishRequest(context.Context, string, *whalereinv1.HostRequest) error {
	return broker.ErrMessageBudget
}

// Only a first, proven never-published request is definitively rejected by transport preflight.
func TestTransportFailureDistinguishesFreshFromUncertainHistory(t *testing.T) {
	for _, mode := range []string{"fresh", "legacy", "prior-lease", "published"} {
		t.Run(mode, func(t *testing.T) {
			db, _ := storetest.New(t)
			ctx := context.Background()
			now := time.Now().UTC().Truncate(time.Microsecond)
			seedDevice(t, db, "h1", "d1")
			row := newRequest("r1", "h1", "d1", "c1", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
			if _, err := db.CreateRequest(ctx, row); err != nil {
				t.Fatal(err)
			}
			if mode == "legacy" {
				if _, err := db.Pool().Exec(ctx, `ALTER TABLE requests DROP COLUMN forward_provenance_known; DELETE FROM schema_migrations WHERE name='011_forward_provenance.sql'`); err != nil {
					t.Fatal(err)
				}
				if err := db.Migrate(ctx); err != nil {
					t.Fatal(err)
				}
			}
			if mode == "prior-lease" || mode == "published" {
				claimed, err := db.ClaimDueRequests(ctx, now, 1, time.Second)
				if err != nil || len(claimed) != 1 {
					t.Fatalf("prior lease=%d err=%v", len(claimed), err)
				}
				if mode == "published" {
					if err := db.MarkPublished(ctx, "r1", now, time.Second); err != nil {
						t.Fatal(err)
					}
				}
				now = now.Add(2 * time.Second)
			}
			service, err := requests.New(requests.Dependencies{Store: db, Broker: preflightFailureBroker{}, Presence: transportRacePresence{},
				Now: func() time.Time { return now }, Config: requests.Config{ProtocolVersion: 1, MaxTTL: time.Hour,
					ForwardInterval: time.Second, Lease: time.Second, RetryInterval: time.Second, MaxAttempts: 3, BatchSize: 10}})
			if err != nil {
				t.Fatal(err)
			}
			if err := service.ForwardDue(ctx); err != nil {
				t.Fatal(err)
			}
			stored, err := db.GetRequest(ctx, "r1")
			if err != nil {
				t.Fatal(err)
			}
			expected := whalereinv1.OnlineStatus_unknown
			if mode == "fresh" {
				expected = whalereinv1.OnlineStatus_rejected
			}
			if stored.Status != expected {
				t.Fatalf("status=%s, want %s", stored.Status, expected)
			}
			if mode == "fresh" && stored.ErrorCode != whalereinv1.ErrorCode_payload_too_large {
				t.Fatal("first preflight rejection lost the transport reason")
			}
			if mode != "fresh" && stored.ErrorCode != whalereinv1.ErrorCode_unavailable {
				t.Fatal("uncertain execution was reported as a definitive transport rejection")
			}
			if err := service.HandleHostResult(ctx, "h1", &whalereinv1.HostResult{RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: whalereinv1.OnlineStatus_succeeded}); err != nil {
				t.Fatal(err)
			}
			stored, err = db.GetRequest(ctx, "r1")
			if err != nil {
				t.Fatal(err)
			}
			if mode == "fresh" && stored.Status != whalereinv1.OnlineStatus_rejected {
				t.Fatal("definitive first rejection accepted a late result")
			}
			if mode != "fresh" && stored.Status != whalereinv1.OnlineStatus_succeeded {
				t.Fatal("uncertain attempt rejected a late success")
			}
		})
	}
}

// A retry preflight failure cannot finalize while an earlier publication is still unresolved.
func TestTransportPreflightCannotRejectAnInFlightEarlierLease(t *testing.T) {
	db, _ := storetest.New(t)
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	now := time.Now().UTC().Truncate(time.Microsecond)
	seedDevice(t, db, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := db.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	transport := &transportRaceBroker{entered: make(chan struct{}), release: make(chan struct{}), secondError: broker.ErrMessageBudget}
	service := func(clock time.Time) *requests.Service {
		s, err := requests.New(requests.Dependencies{Store: db, Broker: transport, Presence: transportRacePresence{},
			Now: func() time.Time { return clock }, Config: requests.Config{ProtocolVersion: 1, MaxTTL: time.Hour,
				ForwardInterval: time.Second, Lease: time.Second, RetryInterval: time.Second, MaxAttempts: 3, BatchSize: 10}})
		if err != nil {
			t.Fatal(err)
		}
		return s
	}
	first, second := service(now), service(now.Add(2*time.Second))
	done := make(chan error, 1)
	go func() { done <- first.ForwardDue(ctx) }()
	select {
	case <-transport.entered:
	case <-ctx.Done():
		t.Fatal("first worker did not reach transport")
	}
	if err := second.ForwardDue(ctx); err != nil {
		t.Fatal(err)
	}
	stored, err := db.GetRequest(ctx, "r1")
	if err != nil || stored.Status != whalereinv1.OnlineStatus_unknown {
		t.Fatalf("concurrent status=%s err=%v", stored.Status, err)
	}
	close(transport.release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if err := first.HandleHostResult(ctx, "h1", &whalereinv1.HostResult{RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: whalereinv1.OnlineStatus_succeeded}); err != nil {
		t.Fatal(err)
	}
	stored, err = db.GetRequest(ctx, "r1")
	if err != nil || stored.Status != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("late result status=%s err=%v", stored.Status, err)
	}
}

// Exhausting an ambiguous retry cannot prove the earlier in-flight publish failed.
func TestTransportAmbiguousFailureCannotAssertOfflineDuringEarlierPublication(t *testing.T) {
	db, _ := storetest.New(t)
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	now := time.Now().UTC().Truncate(time.Microsecond)
	seedDevice(t, db, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := db.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	transport := &transportRaceBroker{entered: make(chan struct{}), release: make(chan struct{}), secondError: errors.New("ambiguous transport failure")}
	service := func(clock time.Time) *requests.Service {
		s, err := requests.New(requests.Dependencies{Store: db, Broker: transport, Presence: transportRacePresence{},
			Now: func() time.Time { return clock }, Config: requests.Config{ProtocolVersion: 1, MaxTTL: time.Hour,
				ForwardInterval: time.Second, Lease: time.Second, RetryInterval: time.Second, MaxAttempts: 2, BatchSize: 10}})
		if err != nil {
			t.Fatal(err)
		}
		return s
	}
	first, second := service(now), service(now.Add(2*time.Second))
	done := make(chan error, 1)
	go func() { done <- first.ForwardDue(ctx) }()
	select {
	case <-transport.entered:
	case <-ctx.Done():
		t.Fatal("first worker did not reach transport")
	}
	if err := second.ForwardDue(ctx); err != nil {
		t.Fatal(err)
	}
	stored, err := db.GetRequest(ctx, "r1")
	if err != nil || stored.Status == whalereinv1.OnlineStatus_host_offline || stored.Status == whalereinv1.OnlineStatus_rejected {
		t.Fatalf("ambiguous concurrent publication was falsely finalized: status=%s err=%v", stored.Status, err)
	}
	close(transport.release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if err := first.HandleHostResult(ctx, "h1", &whalereinv1.HostResult{RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: whalereinv1.OnlineStatus_succeeded}); err != nil {
		t.Fatal(err)
	}
	stored, err = db.GetRequest(ctx, "r1")
	if err != nil || stored.Status != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("late result status=%s err=%v", stored.Status, err)
	}
}

type ambiguousFailureBroker struct{}

func (ambiguousFailureBroker) PublishPresence(context.Context, string, []byte) error { return nil }

func (ambiguousFailureBroker) PublishRequest(context.Context, string, *whalereinv1.HostRequest) error {
	return errors.New("ambiguous transport failure")
}

// Even the first publish may have reached the broker when its acknowledgement is lost.
func TestTransportAmbiguousFirstFailureAllowsLateSuccess(t *testing.T) {
	db, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC().Truncate(time.Microsecond)
	seedDevice(t, db, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := db.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	service, err := requests.New(requests.Dependencies{Store: db, Broker: ambiguousFailureBroker{}, Presence: transportRacePresence{},
		Now: func() time.Time { return now }, Config: requests.Config{ProtocolVersion: 1, MaxTTL: time.Hour,
			ForwardInterval: time.Second, Lease: time.Second, RetryInterval: time.Second, MaxAttempts: 1, BatchSize: 10}})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.ForwardDue(ctx); err != nil {
		t.Fatal(err)
	}
	stored, err := db.GetRequest(ctx, "r1")
	if err != nil || stored.Status == whalereinv1.OnlineStatus_host_offline || stored.Status == whalereinv1.OnlineStatus_rejected {
		t.Fatalf("ambiguous first publish was falsely finalized: status=%s err=%v", stored.Status, err)
	}
	if err := service.HandleHostResult(ctx, "h1", &whalereinv1.HostResult{RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: whalereinv1.OnlineStatus_succeeded}); err != nil {
		t.Fatal(err)
	}
	stored, err = db.GetRequest(ctx, "r1")
	if err != nil || stored.Status != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("late result status=%s err=%v", stored.Status, err)
	}
}

// Pairing COMMIT remains retryable after transport preflight failures until its deadline.
func TestTransportPreflightDoesNotAbandonPairingCommit(t *testing.T) {
	db, _ := storetest.New(t)
	ctx := context.Background()
	at := time.Now().UTC().Truncate(time.Microsecond)
	if _, err := db.EnsureHost(ctx, "h"); err != nil {
		t.Fatal(err)
	}
	hash := bytes.Repeat([]byte{12}, 32)
	if err := db.CreatePairingToken(ctx, hash, "h", at.Add(time.Minute), at); err != nil {
		t.Fatal(err)
	}
	notice := &whalereinv1.HostRequest{RequestId: "preflight-activation", HostId: "h", DeviceId: "d",
		Kind: whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE, Body: []byte{}, ExpiresAtUnixMs: at.Add(time.Minute).UnixMilli()}
	op, notice := fixturePairingOperation(t, db, hash, "h", "d", "preflight-claim", at, notice)
	if used, err := db.CommitPairing(ctx, hash, "h", "d", fixtureKeyPin(3), []byte("credential"), at, notice, &op); err != nil || !used {
		t.Fatal("pairing commit failed")
	}
	clock := at
	service, err := requests.New(requests.Dependencies{Store: db, Broker: preflightFailureBroker{}, Presence: transportRacePresence{},
		Now: func() time.Time { return clock }, Config: requests.Config{ProtocolVersion: 1, MaxTTL: time.Hour,
			ForwardInterval: time.Second, Lease: time.Second, RetryInterval: time.Second, MaxAttempts: 1, BatchSize: 10}})
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 3; i++ {
		if err := service.ForwardDue(ctx); err != nil {
			t.Fatal(err)
		}
		saved, err := db.GetRequest(ctx, notice.RequestId)
		if err != nil || saved.Status != whalereinv1.OnlineStatus_received || saved.ForwardState != store.ForwardPending || saved.ForwardAttempts != i+1 {
			t.Fatalf("pairing commit was abandoned at attempt %d: status=%s state=%s err=%v", i+1, saved.Status, saved.ForwardState, err)
		}
		clock = clock.Add(2 * time.Second)
	}
}

// An uncertain command accepts only the existing authoritative terminal transitions.
func TestTransportUnknownPreservesLateResultTransitionContract(t *testing.T) {
	db, _ := storetest.New(t)
	ctx := context.Background()
	now := time.Now().UTC().Truncate(time.Microsecond)
	seedDevice(t, db, "h1", "d1")
	row := newRequest("r1", "h1", "d1", "c1", []byte{1}, whalereinv1.OnlineStatus_received, store.ForwardPending, now)
	if _, err := db.CreateRequest(ctx, row); err != nil {
		t.Fatal(err)
	}
	service, err := requests.New(requests.Dependencies{Store: db, Broker: ambiguousFailureBroker{}, Presence: transportRacePresence{},
		Now: func() time.Time { return now }, Config: requests.Config{ProtocolVersion: 1, MaxTTL: time.Hour,
			ForwardInterval: time.Second, Lease: time.Second, RetryInterval: time.Second, MaxAttempts: 1, BatchSize: 10}})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.ForwardDue(ctx); err != nil {
		t.Fatal(err)
	}
	for _, status := range []whalereinv1.OnlineStatus{whalereinv1.OnlineStatus_host_accepted, whalereinv1.OnlineStatus_rejected} {
		if err := service.HandleHostResult(ctx, "h1", &whalereinv1.HostResult{RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: status}); err != nil {
			t.Fatal(err)
		}
		current, err := db.GetRequest(ctx, "r1")
		if err != nil || current.Status != whalereinv1.OnlineStatus_unknown {
			t.Fatalf("non-authoritative status=%s err=%v", current.Status, err)
		}
	}
	if err := service.HandleHostResult(ctx, "h1", &whalereinv1.HostResult{RequestId: "r1", HostId: "h1", DeviceId: "d1", Status: whalereinv1.OnlineStatus_failed}); err != nil {
		t.Fatal(err)
	}
	current, err := db.GetRequest(ctx, "r1")
	if err != nil || current.Status != whalereinv1.OnlineStatus_failed {
		t.Fatalf("late failure status=%s err=%v", current.Status, err)
	}
}
