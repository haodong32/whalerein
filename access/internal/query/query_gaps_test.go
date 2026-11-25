package query

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestNewRejectsMissingDependenciesAndAppliesDefaults(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("missing dependencies accepted")
	}
	service, err := New(Dependencies{
		Store:       &fakeStore{grants: map[string]store.AdmissionGrant{}},
		Connections: newFakeConnections(),
		Publisher:   &fakePublisher{},
	})
	if err != nil {
		t.Fatal(err)
	}
	if service.config.MaxWaiters == 0 || service.config.MaxWaitersPerConnection == 0 ||
		service.config.MaxTTL == 0 || service.now().IsZero() {
		t.Fatal("defaults were not applied")
	}
}

func TestQueryAdmissionRejectsMalformedEnvelopes(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*whalereinv1.HostRequest, *broker.Message)
		want   error
	}{
		{
			name: "zero qos",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.QoS = 0
			},
			want: ErrInvalidTopic,
		},
		{
			name: "unparseable payload",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Payload = []byte{0xff, 0xff}
			},
			want: ErrInvalidTopic,
		},
		{
			name: "wrong protocol version",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.ProtocolVersion = 2
			},
			want: ErrProtocol,
		},
		{
			name: "missing request id",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.RequestId = ""
			},
			want: ErrInvalidBody,
		},
		{
			name: "handshake with a command id",
			mutate: func(request *whalereinv1.HostRequest, message *broker.Message) {
				request.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
				request.CommandId = "command-1"
				message.Properties.CorrelationData = []byte(request.RequestId)
			},
			want: ErrInvalidBody,
		},
		{
			name: "handshake with a reference",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.Kind = whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE
				request.BodyReference = &whalereinv1.PayloadReference{ObjectId: "0123456789abcdef0123456789abcdef"}
			},
			want: ErrInvalidBody,
		},
		{
			name: "query with no body",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.Body = nil
			},
			want: ErrInvalidBody,
		},
		{
			name: "zero message expiry",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				expiry := uint32(0)
				message.Properties.MessageExpiry = &expiry
			},
			want: ErrMessageExpiry,
		},
		{
			name: "deadline beyond the configured maximum",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.ExpiresAtUnixMs = time.UnixMilli(1_800_000_000_000).Add(time.Hour).UnixMilli()
			},
			want: ErrExpired,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if err := h.service.Admit(context.Background(), h.queryMessage(t, tc.mutate)); !errors.Is(err, tc.want) {
				t.Fatalf("admit error = %v, want %v", err, tc.want)
			}
		})
	}
}

func TestQueryAdmissionPropagatesStoreFailure(t *testing.T) {
	h := newHarness(t)
	delete(h.store.grants, testHost+"\x00"+testDevice)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); !errors.Is(err, ErrGrant) {
		t.Fatalf("missing grant = %v", err)
	}
}

func TestQueryCompleteIgnoresMalformedAndForeignResults(t *testing.T) {
	h := newHarness(t)
	if h.service.Complete(context.Background(), testHost, broker.Message{Payload: []byte{0xff}}) {
		t.Fatal("malformed result was consumed")
	}
	if h.service.Complete(context.Background(), testHost, broker.Message{Payload: mustMarshal(t, &whalereinv1.HostResult{RelayVersion: broker.RelayEpoch})}) {
		t.Fatal("result without a request id was consumed")
	}
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	foreign := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, DeviceId: "d2", RelayVersion: broker.RelayEpoch,
	}
	if h.service.Complete(context.Background(), testHost, broker.Message{Payload: mustMarshal(t, foreign)}) {
		t.Fatal("foreign device result was consumed")
	}
	if _, ok := h.service.Waiter(testHost, testDevice, "request-1"); !ok {
		t.Fatal("foreign result removed the waiter")
	}
	// A result that omits device_id still resolves the single waiter.
	anonymous := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, RelayVersion: broker.RelayEpoch,
	}
	if !h.service.Complete(context.Background(), testHost, broker.Message{Payload: mustMarshal(t, anonymous)}) {
		t.Fatal("anonymous result was not consumed")
	}
	if len(h.publisher.snapshot()) != 2 {
		t.Fatal("anonymous result did not publish a reply")
	}
}

func TestQueryCompleteDropsAChangedGrant(t *testing.T) {
	h := newHarness(t)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	grant := h.store.grants[testHost+"\x00"+testDevice]
	grant.RevocationGeneration++
	h.store.grants[testHost+"\x00"+testDevice] = grant
	result := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, DeviceId: testDevice,
		Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: broker.RelayEpoch,
	}
	if !h.service.Complete(context.Background(), testHost, broker.Message{Payload: mustMarshal(t, result)}) {
		t.Fatal("changed-grant result was not consumed")
	}
	if len(h.publisher.snapshot()) != 1 {
		t.Fatal("changed-grant result was published")
	}
}

func TestQueryIssueReferenceRejectsMalformedObjects(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*harness, *store.PayloadObject)
		object *store.PayloadObject
	}{
		{name: "missing object", object: nil},
		{name: "missing grant fence", mutate: func(_ *harness, object *store.PayloadObject) {
			object.GrantGeneration = nil
		}},
		{name: "missing enrollment", mutate: func(_ *harness, object *store.PayloadObject) {
			object.EnrollmentGeneration = nil
		}},
		{name: "missing connection", mutate: func(_ *harness, object *store.PayloadObject) {
			object.ConnectionRef = ""
		}},
		{name: "wrong purpose", mutate: func(_ *harness, object *store.PayloadObject) {
			object.Purpose = whalereinv1.PayloadPurpose_name[int32(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_EVENT)]
		}},
		{name: "issue refused", mutate: func(h *harness, _ *store.PayloadObject) {
			h.store.issueOK = false
		}},
		{name: "issue failed", mutate: func(h *harness, _ *store.PayloadObject) {
			h.store.issueErr = errors.New("db down")
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			objectID := "0123456789abcdef0123456789abcdef"
			enrollment := int64(3)
			grant, revocation, deviceRevocation := int64(7), int64(2), int64(1)
			object := store.PayloadObject{
				ObjectID: objectID, HostID: testHost, DeviceID: testDevice,
				ChannelID: "channel-1", RequestID: "request-1",
				Purpose: whalereinv1.PayloadPurpose_name[int32(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT)],
				State:   store.PayloadReady, ExpiresAt: h.now.Add(20 * time.Second),
				GrantGeneration: &grant, RevocationGeneration: &revocation,
				DeviceRevocationGeneration: &deviceRevocation,
				EnrollmentGeneration:       &enrollment, ConnectionRef: testRef.String(),
			}
			h.store.issueOK = true
			if tc.mutate != nil {
				tc.mutate(h, &object)
			}
			if tc.object != nil {
				object = *tc.object
			} else if tc.name == "missing object" {
				object = store.PayloadObject{}
			}
			if object.ObjectID != "" {
				h.store.objects[objectID] = object
			}
			if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
				t.Fatal(err)
			}
			result := &whalereinv1.HostResult{
				RequestId: "request-1", HostId: testHost, DeviceId: testDevice,
				Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: broker.RelayEpoch,
				ResultReference: &whalereinv1.PayloadReference{
					ObjectId: objectID,
					Purpose:  whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
				},
			}
			if !h.service.Complete(context.Background(), testHost, broker.Message{
				Topic: broker.ResultTopic(testHost), Payload: mustMarshal(t, result),
			}) {
				t.Fatal("reference result was not consumed")
			}
			if len(h.store.issued) != 0 || len(h.publisher.snapshot()) != 1 {
				t.Fatal("malformed reference was issued or published")
			}
		})
	}
}

func TestQueryAuthorizeObjectReserveRejectsChangedAuthority(t *testing.T) {
	h := newHarness(t)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	deadline := h.now.Add(20 * time.Second).UnixMilli()
	if _, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-2", "request-1", testRef.String(), 3, deadline); !errors.Is(err, ErrScope) {
		t.Fatalf("foreign channel = %v", err)
	}
	if _, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-1", "request-1", testRef.String(), 3, h.now.Add(time.Hour).UnixMilli()); !errors.Is(err, ErrExpired) {
		t.Fatalf("object beyond the waiter deadline = %v", err)
	}
	grant := h.store.grants[testHost+"\x00"+testDevice]
	grant.GrantGeneration++
	h.store.grants[testHost+"\x00"+testDevice] = grant
	if _, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-1", "request-1", testRef.String(), 3, deadline); !errors.Is(err, ErrGrant) {
		t.Fatalf("changed grant = %v", err)
	}
}

func TestQueryRemainingExpiryRoundsUpAndCaps(t *testing.T) {
	now := time.UnixMilli(1_800_000_000_000)
	if value := remainingExpiry(now.Add(1500*time.Millisecond), now, nil); value == nil || *value != 2 {
		t.Fatalf("rounded expiry = %v", value)
	}
	upstream := uint32(1)
	if value := remainingExpiry(now.Add(time.Minute), now, &upstream); value == nil || *value != 1 {
		t.Fatalf("capped expiry = %v", value)
	}
	zero := uint32(0)
	if value := remainingExpiry(now.Add(time.Minute), now, &zero); value != nil {
		t.Fatalf("zero upstream expiry = %v", value)
	}
	if value := remainingExpiry(now, now, nil); value != nil {
		t.Fatalf("elapsed deadline = %v", value)
	}
	large := uint32(^uint32(0))
	if value := remainingExpiry(now.Add(time.Hour), now, &large); value == nil || *value != 3600 {
		t.Fatalf("long expiry = %v", value)
	}
}

func TestQueryWaiterLookupAndRemoval(t *testing.T) {
	h := newHarness(t)
	if _, ok := h.service.Waiter(testHost, testDevice, "missing"); ok {
		t.Fatal("unknown waiter found")
	}
	if h.service.remove("missing") {
		t.Fatal("missing waiter removed")
	}
}
