package query

import (
	"bytes"
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

const (
	testHost   = "h1"
	testDevice = "d1"
	testEpoch  = "0123456789abcdef"
)

var testRef = broker.ConnectionRef{Epoch: testEpoch, ID: 7}

type fakeStore struct {
	mu       sync.Mutex
	grants   map[string]store.AdmissionGrant
	objects  map[string]store.PayloadObject
	issued   []store.PayloadIssueScope
	issueOK  bool
	issueErr error
}

func (f *fakeStore) AdmissionGrantFor(_ context.Context, hostID, deviceID string) (store.AdmissionGrant, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	grant, ok := f.grants[hostID+"\x00"+deviceID]
	if !ok {
		return store.AdmissionGrant{}, store.ErrNotFound
	}
	return grant, nil
}

func (f *fakeStore) GetPayload(_ context.Context, objectID string) (store.PayloadObject, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	object, ok := f.objects[objectID]
	if !ok {
		return store.PayloadObject{}, store.ErrNotFound
	}
	return object, nil
}

func (f *fakeStore) MarkPayloadIssued(_ context.Context, scope store.PayloadIssueScope, _ time.Time) (bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.issueErr != nil {
		return false, f.issueErr
	}
	if !f.issueOK {
		return false, nil
	}
	f.issued = append(f.issued, scope)
	return true, nil
}

type fakeConnections struct {
	sessionID string
	mu        sync.Mutex
	refs      map[string]map[broker.ConnectionRef]bool
}

func (f *fakeConnections) DeviceConnectionSession(deviceID string, ref broker.ConnectionRef) (string, bool) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.sessionID, f.refs[deviceID][ref]
}

func newFakeConnections() *fakeConnections {
	return &fakeConnections{refs: map[string]map[broker.ConnectionRef]bool{
		testDevice: {testRef: true},
	}}
}

type fakeHosts struct {
	online  bool
	unknown bool
}

func (f *fakeHosts) HostPresence(string) (bool, bool) { return f.online, !f.unknown }

type fakePublisher struct {
	mu       sync.Mutex
	messages []broker.Message
	err      error
}

func (f *fakePublisher) PublishMessage(_ context.Context, message broker.Message) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.err != nil {
		return f.err
	}
	f.messages = append(f.messages, message)
	return nil
}

func (f *fakePublisher) snapshot() []broker.Message {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]broker.Message(nil), f.messages...)
}

type harness struct {
	service     *Service
	store       *fakeStore
	connections *fakeConnections
	hosts       *fakeHosts
	publisher   *fakePublisher
	now         time.Time
}

func (s *Service) waiterCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.waiters)
}

func (s *Service) waiterFor(hostID, deviceID, requestID string) (*waiter, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	w, ok := s.waiters[waiterKey{hostID, deviceID, requestID}]
	return w, ok
}

func newHarness(t *testing.T) *harness {
	t.Helper()
	h := &harness{
		store: &fakeStore{
			grants: map[string]store.AdmissionGrant{
				testHost + "\x00" + testDevice: {
					HostID: testHost, DeviceID: testDevice, Active: true,
					GrantGeneration: 7, RevocationGeneration: 2,
					DeviceRevocationGeneration: 1, EnrollmentGeneration: 3,
				},
			},
			objects: map[string]store.PayloadObject{},
		},
		connections: newFakeConnections(),
		hosts:       &fakeHosts{online: true},
		publisher:   &fakePublisher{},
		now:         time.UnixMilli(1_800_000_000_000).UTC(),
	}
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
	})
	if err != nil {
		t.Fatal(err)
	}
	h.service = service
	return h
}

func (h *harness) queryMessage(t *testing.T, mutate func(*whalereinv1.HostRequest, *broker.Message)) broker.Message {
	t.Helper()
	request := &whalereinv1.HostRequest{
		RequestId:       "request-1",
		HostId:          testHost,
		DeviceId:        testDevice,
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_QUERY,
		Body:            []byte("ciphertext"),
		ProtocolVersion: 1,
		RelayVersion:    broker.RelayEpoch,
		ChannelId:       "channel-1",
		ExpiresAtUnixMs: h.now.Add(40 * time.Second).UnixMilli(),
	}
	expiry := uint32(30)
	message := broker.Message{
		Topic: broker.DeviceQueryTopic(testHost, testDevice),
		QoS:   1,
		Properties: broker.MessageProperties{
			CorrelationData: []byte("request-1"),
			ResponseTopic:   broker.DeviceReplyTopic(testHost, testDevice),
			MessageExpiry:   &expiry,
			UserProperties: []broker.UserProperty{
				{Key: broker.PropertyConnection, Value: testRef.String()},
			},
		},
	}
	if mutate != nil {
		mutate(request, &message)
	}
	if message.Payload == nil {
		message.Payload = mustMarshal(t, request)
	}
	return message
}

func mustMarshal(t *testing.T, message proto.Message) []byte {
	t.Helper()
	payload, err := proto.Marshal(message)
	if err != nil {
		t.Fatal(err)
	}
	return payload
}

func TestQueryAdmissionBindsConnectionGrantAndDeadline(t *testing.T) {
	h := newHarness(t)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatalf("admit: %v", err)
	}
	scope, ok := h.service.waiterFor(testHost, testDevice, "request-1")
	if !ok {
		t.Fatal("waiter was not registered")
	}
	if scope.Conn != testRef || scope.Grant.GrantGeneration != 7 ||
		scope.Grant.EnrollmentGeneration != 3 {
		t.Fatalf("scope = %+v", scope)
	}
	// The Broker's remaining expiry (30s) shortens the application deadline.
	if got := scope.Deadline.Sub(h.now); got != 30*time.Second {
		t.Fatalf("deadline horizon = %v", got)
	}
	messages := h.publisher.snapshot()
	if len(messages) != 1 {
		t.Fatalf("forwarded messages = %d", len(messages))
	}
	forward := messages[0]
	if forward.Topic != broker.RequestTopic(testHost) || forward.QoS != 1 {
		t.Fatalf("forward = %+v", forward)
	}
	if forward.Properties.ResponseTopic != broker.ResultTopic(testHost) ||
		string(forward.Properties.CorrelationData) != "request-1" ||
		forward.Properties.MessageExpiry == nil || *forward.Properties.MessageExpiry != 30 {
		t.Fatalf("forward properties = %+v", forward.Properties)
	}
	var forwarded whalereinv1.HostRequest
	if err := proto.Unmarshal(forward.Payload, &forwarded); err != nil {
		t.Fatal(err)
	}
	if forwarded.GetConnectionRef() != testRef.String() ||
		forwarded.GetGrantGeneration() != 7 || forwarded.GetEnrollmentGeneration() != 3 ||
		forwarded.GetTransport().GetEnrollmentGeneration() != 3 {
		t.Fatalf("forwarded request = %+v", &forwarded)
	}
	if forwarded.GetRelayVersion() != broker.RelayEpoch {
		t.Fatalf("relay version = %d", forwarded.GetRelayVersion())
	}
}

func TestQueryRejectsForgedStampAndForeignReplyTopic(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*whalereinv1.HostRequest, *broker.Message)
		setup  func(*harness)
		want   error
	}{
		{
			name: "wrong ingress topic",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Topic = broker.RequestTopic(testHost)
			},
			want: ErrInvalidTopic,
		},
		{
			name: "foreign host scope",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.HostId = "h2"
			},
			want: ErrScope,
		},
		{
			name: "old relay epoch",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.RelayVersion = 4
			},
			want: ErrProtocol,
		},
		{
			name: "command kind",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.Kind = whalereinv1.RequestKind_REQUEST_KIND_COMMAND
			},
			want: ErrInvalidKind,
		},
		{
			name: "both inline and reference body",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.BodyReference = &whalereinv1.PayloadReference{ObjectId: "0123456789abcdef0123456789abcdef"}
			},
			want: ErrInvalidBody,
		},
		{
			name: "correlation mismatch",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Properties.CorrelationData = []byte("other")
			},
			want: ErrCorrelation,
		},
		{
			name: "missing correlation",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Properties.CorrelationData = nil
			},
			want: ErrCorrelation,
		},
		{
			name: "foreign response topic",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Properties.ResponseTopic = broker.RequestTopic(testHost)
			},
			want: ErrResponseTopic,
		},
		{
			name: "missing message expiry",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Properties.MessageExpiry = nil
			},
			want: ErrMessageExpiry,
		},
		{
			name: "elapsed deadline",
			mutate: func(request *whalereinv1.HostRequest, _ *broker.Message) {
				request.ExpiresAtUnixMs = 1
			},
			want: ErrExpired,
		},
		{
			name: "missing connection stamp",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Properties.UserProperties = nil
			},
			want: ErrStaleConn,
		},
		{
			name: "forged connection stamp",
			mutate: func(_ *whalereinv1.HostRequest, message *broker.Message) {
				message.Properties.UserProperties = []broker.UserProperty{
					{Key: broker.PropertyConnection, Value: "ffffffffffffffff:9"},
				}
			},
			want: ErrStaleConn,
		},
		{
			name: "inactive grant",
			setup: func(h *harness) {
				grant := h.store.grants[testHost+"\x00"+testDevice]
				grant.Active = false
				h.store.grants[testHost+"\x00"+testDevice] = grant
			},
			want: ErrGrant,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t)
			if tc.setup != nil {
				tc.setup(h)
			}
			err := h.service.Admit(context.Background(), h.queryMessage(t, tc.mutate))
			if !errors.Is(err, tc.want) {
				t.Fatalf("admit error = %v, want %v", err, tc.want)
			}
			if h.service.waiterCount() != 0 || len(h.publisher.snapshot()) != 0 {
				t.Fatal("rejected query registered a waiter or was forwarded")
			}
		})
	}
}

func TestQueryPresenceRepliesOnlyToAdmittedDevice(t *testing.T) {
	for _, unknown := range []bool{false, true} {
		h := newHarness(t)
		h.hosts.online, h.hosts.unknown = false, unknown
		message := h.queryMessage(t, nil)
		if err := h.service.Admit(context.Background(), message); err != nil {
			t.Fatal(err)
		}
		messages := h.publisher.snapshot()
		if h.service.waiterCount() != 0 || len(messages) != 1 || messages[0].Topic != message.Properties.ResponseTopic ||
			messages[0].Properties.Get(broker.PropertyTarget) != testRef.String() ||
			!bytes.Equal(messages[0].Properties.CorrelationData, message.Properties.CorrelationData) {
			t.Fatal("presence refusal must resolve only its original caller")
		}
		var result whalereinv1.HostResult
		if err := proto.Unmarshal(messages[0].Payload, &result); err != nil {
			t.Fatal(err)
		}
		if (!unknown && result.Status != whalereinv1.OnlineStatus_host_offline) ||
			(unknown && result.ErrorCode != whalereinv1.ErrorCode_unavailable) {
			t.Fatal("presence refusal did not distinguish offline from unknown")
		}
	}
}

func TestQueryReplyAffinityAndExpiredPublish(t *testing.T) {
	h := newHarness(t)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	result := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, DeviceId: testDevice,
		Status: whalereinv1.OnlineStatus_succeeded, ChannelId: "channel-1",
		ResultBody: []byte("answer"), ProtocolVersion: 1, RelayVersion: broker.RelayEpoch,
	}
	expiry := uint32(20)
	message := broker.Message{
		Topic:   broker.ResultTopic(testHost),
		Payload: mustMarshal(t, result),
		QoS:     1,
		Properties: broker.MessageProperties{
			CorrelationData: []byte("request-1"),
			MessageExpiry:   &expiry,
		},
	}
	if !h.service.Complete(context.Background(), testHost, message) {
		t.Fatal("waiter result was not handled")
	}
	if h.service.waiterCount() != 0 {
		t.Fatal("waiter survived completion")
	}
	messages := h.publisher.snapshot()
	if len(messages) != 2 {
		t.Fatalf("published messages = %d", len(messages))
	}
	reply := messages[1]
	if reply.Topic != broker.DeviceReplyTopic(testHost, testDevice) || reply.QoS != 1 {
		t.Fatalf("reply = %+v", reply)
	}
	if string(reply.Properties.CorrelationData) != "request-1" ||
		reply.Properties.Get(broker.PropertyTarget) != testRef.String() {
		t.Fatalf("reply affinity = %+v", reply.Properties)
	}
	if reply.Properties.MessageExpiry == nil || *reply.Properties.MessageExpiry != 20 {
		t.Fatalf("reply expiry = %+v", reply.Properties.MessageExpiry)
	}
	var replyResult whalereinv1.HostResult
	if err := proto.Unmarshal(reply.Payload, &replyResult); err != nil {
		t.Fatal(err)
	}
	if replyResult.GetRequestId() != "request-1" || replyResult.GetDeviceId() != testDevice ||
		string(replyResult.GetResultBody()) != "answer" {
		t.Fatalf("reply result = %+v", &replyResult)
	}
	// A duplicate result for the same request never reaches the device again.
	if h.service.Complete(context.Background(), testHost, message) {
		t.Fatal("duplicate result was treated as a waiter")
	}
	if len(h.publisher.snapshot()) != 2 {
		t.Fatal("duplicate result was published")
	}
}

func TestQueryExpiredWaiterDropsLateResult(t *testing.T) {
	h := newHarness(t)
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
		t.Fatal("expired waiter result was not consumed")
	}
	if len(h.publisher.snapshot()) != 1 {
		t.Fatal("expired waiter reply was published")
	}
	if h.service.waiterCount() != 0 {
		t.Fatal("expired waiter survived")
	}
}

func TestQueryResultReferenceIssuance(t *testing.T) {
	h := newHarness(t)
	objectID := "0123456789abcdef0123456789abcdef"
	enrollment := int64(3)
	grant, revocation, deviceRevocation := int64(7), int64(2), int64(1)
	readyAt := h.now.Add(-time.Second)
	h.store.objects[objectID] = store.PayloadObject{
		ObjectID: objectID, HostID: testHost, DeviceID: testDevice,
		AppSession: "", ChannelID: "channel-1", RequestID: "request-1",
		Purpose: whalereinv1.PayloadPurpose_name[int32(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT)],
		State:   store.PayloadReady, ExpiresAt: h.now.Add(20 * time.Second), ReadyAt: &readyAt,
		GrantGeneration: &grant, RevocationGeneration: &revocation,
		DeviceRevocationGeneration: &deviceRevocation,
		EnrollmentGeneration:       &enrollment,
		ConnectionRef:              testRef.String(),
	}
	h.store.issueOK = true
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
		t.Fatal("reference result was not handled")
	}
	if len(h.store.issued) != 1 {
		t.Fatalf("issued scopes = %+v", h.store.issued)
	}
	scope := h.store.issued[0]
	if scope.ObjectID != objectID || scope.HostID != testHost || scope.DeviceID != testDevice ||
		scope.RequestID != "request-1" || scope.ConnectionRef != testRef.String() ||
		scope.GrantGeneration != 7 || scope.EnrollmentGeneration != 3 {
		t.Fatalf("issued scope = %+v", scope)
	}
	if len(h.publisher.snapshot()) != 2 {
		t.Fatal("reference reply was not published after issuance")
	}
}

func TestQueryRejectsUnboundResultReference(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*store.PayloadObject)
	}{
		{name: "foreign request", mutate: func(object *store.PayloadObject) { object.RequestID = "other" }},
		{name: "foreign connection", mutate: func(object *store.PayloadObject) { object.ConnectionRef = "ffffffffffffffff:1" }},
		{name: "foreign enrollment", mutate: func(object *store.PayloadObject) {
			enrollment := int64(9)
			object.EnrollmentGeneration = &enrollment
		}},
		{name: "not ready", mutate: func(object *store.PayloadObject) { object.State = store.PayloadReserved }},
		{name: "expired", mutate: func(object *store.PayloadObject) { object.ExpiresAt = hTimeBefore }},
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
			tc.mutate(&object)
			h.store.objects[objectID] = object
			h.store.issueOK = true
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
			if len(h.store.issued) != 0 {
				t.Fatalf("unbound reference was issued: %+v", h.store.issued)
			}
			if len(h.publisher.snapshot()) != 1 {
				t.Fatal("unbound reference reply was published")
			}
		})
	}
}

var hTimeBefore = time.UnixMilli(1_700_000_000_000).UTC()

func TestQueryRestartDropsWaiters(t *testing.T) {
	h := newHarness(t)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	h.service.CancelAll()
	if h.service.waiterCount() != 0 {
		t.Fatal("waiters survived a restart")
	}
	result := &whalereinv1.HostResult{
		RequestId: "request-1", HostId: testHost, DeviceId: testDevice,
		Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: broker.RelayEpoch,
	}
	if h.service.Complete(context.Background(), testHost, broker.Message{
		Topic: broker.ResultTopic(testHost), Payload: mustMarshal(t, result),
	}) {
		t.Fatal("stale result completed a waiter after restart")
	}
}

func TestQueryCancelConnectionDropsOnlyItsWaiters(t *testing.T) {
	h := newHarness(t)
	second := broker.ConnectionRef{Epoch: testEpoch, ID: 8}
	h.connections.refs[testDevice][second] = true
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	secondMessage := h.queryMessage(t, func(request *whalereinv1.HostRequest, message *broker.Message) {
		request.RequestId = "request-2"
		message.Properties.CorrelationData = []byte("request-2")
		message.Properties.UserProperties = []broker.UserProperty{
			{Key: broker.PropertyConnection, Value: second.String()},
		}
	})
	if err := h.service.Admit(context.Background(), secondMessage); err != nil {
		t.Fatal(err)
	}
	h.service.CancelConnection(second)
	if h.service.waiterCount() != 1 {
		t.Fatalf("waiters = %d", h.service.waiterCount())
	}
	if _, ok := h.service.waiterFor(testHost, testDevice, "request-1"); !ok {
		t.Fatal("sibling waiter was cancelled")
	}
}

func TestQueryDuplicateAndSecondConnectionRejected(t *testing.T) {
	h := newHarness(t)
	message := h.queryMessage(t, nil)
	if err := h.service.Admit(context.Background(), message); err != nil {
		t.Fatal(err)
	}
	// The same request republished on the same connection is idempotent.
	if err := h.service.Admit(context.Background(), message); err != nil {
		t.Fatalf("idempotent repeat: %v", err)
	}
	if len(h.publisher.snapshot()) != 1 {
		t.Fatal("idempotent repeat forwarded twice")
	}
	second := broker.ConnectionRef{Epoch: testEpoch, ID: 8}
	h.connections.refs[testDevice][second] = true
	foreign := h.queryMessage(t, func(_ *whalereinv1.HostRequest, message *broker.Message) {
		message.Properties.UserProperties = []broker.UserProperty{
			{Key: broker.PropertyConnection, Value: second.String()},
		}
	})
	if err := h.service.Admit(context.Background(), foreign); !errors.Is(err, ErrDuplicate) {
		t.Fatalf("duplicate on another connection = %v", err)
	}
}

func TestQueryAuthorizeObjectReserveChecksTheLiveWaiter(t *testing.T) {
	h := newHarness(t)
	h.connections.sessionID = "managed-login"
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	if session, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-1", "request-1", testRef.String(), 3, h.now.Add(20*time.Second).UnixMilli()); err != nil || session != "managed-login" {
		t.Fatalf("wrong admitted session or query error: %v", err)
	}
	if _, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-1", "request-9", testRef.String(), 3, h.now.Add(20*time.Second).UnixMilli()); !errors.Is(err, ErrNoWaiter) {
		t.Fatalf("unknown request = %v", err)
	}
	if _, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-1", "request-1", "ffffffffffffffff:1", 3, h.now.Add(20*time.Second).UnixMilli()); !errors.Is(err, ErrScope) {
		t.Fatalf("foreign connection = %v", err)
	}
	if _, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-1", "request-1", testRef.String(), 9, h.now.Add(20*time.Second).UnixMilli()); !errors.Is(err, ErrScope) {
		t.Fatalf("foreign enrollment = %v", err)
	}
	h.now = h.now.Add(31 * time.Second)
	if _, err := h.service.AuthorizeObjectReserve(context.Background(), testHost, testDevice, "channel-1", "request-1", testRef.String(), 3, h.now.Add(20*time.Second).UnixMilli()); !errors.Is(err, ErrExpired) {
		t.Fatalf("expired waiter = %v", err)
	}
}

func TestQuerySweepRemovesExpiredWaiters(t *testing.T) {
	h := newHarness(t)
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err != nil {
		t.Fatal(err)
	}
	h.now = h.now.Add(31 * time.Second)
	if removed := h.service.Sweep(); removed != 1 {
		t.Fatalf("swept = %d", removed)
	}
	if h.service.waiterCount() != 0 {
		t.Fatal("sweep left a waiter")
	}
}

func TestQueryPublishFailureDropsTheWaiter(t *testing.T) {
	h := newHarness(t)
	h.publisher.err = errors.New("mqtt down")
	if err := h.service.Admit(context.Background(), h.queryMessage(t, nil)); err == nil {
		t.Fatal("publish failure accepted")
	}
	if h.service.waiterCount() != 0 {
		t.Fatal("publish failure left a waiter")
	}
}

type publishFunc func(context.Context, broker.Message) error

func (f publishFunc) PublishMessage(ctx context.Context, message broker.Message) error {
	return f(ctx, message)
}

func TestQueryLatePublishFailurePreservesReadmittedWaiter(t *testing.T) {
	h := newHarness(t)
	message := h.queryMessage(t, nil)
	interrupted := errors.New("publish interrupted")
	first := true
	h.service.publisher = publishFunc(func(ctx context.Context, forward broker.Message) error {
		if !first {
			return h.publisher.PublishMessage(ctx, forward)
		}
		first = false
		h.service.CancelAll()
		if err := h.service.Admit(ctx, message); err != nil {
			t.Fatalf("readmit: %v", err)
		}
		return interrupted
	})
	if err := h.service.Admit(context.Background(), message); !errors.Is(err, interrupted) {
		t.Fatalf("original publish = %v", err)
	}
	if h.service.waiterCount() != 1 {
		t.Fatal("original publish failure removed the readmitted waiter")
	}
}

func TestQueryCapacityIsBounded(t *testing.T) {
	h := newHarness(t)
	for index := 0; index < 8; index++ {
		message := h.queryMessage(t, func(request *whalereinv1.HostRequest, message *broker.Message) {
			request.RequestId = "request-" + string(rune('a'+index))
			message.Properties.CorrelationData = []byte(request.RequestId)
		})
		if err := h.service.Admit(context.Background(), message); err != nil {
			t.Fatal(err)
		}
	}
	message := h.queryMessage(t, func(request *whalereinv1.HostRequest, message *broker.Message) {
		request.RequestId = "request-over"
		message.Properties.CorrelationData = []byte(request.RequestId)
	})
	if err := h.service.Admit(context.Background(), message); !errors.Is(err, ErrCapacity) {
		t.Fatalf("per-connection bound = %v", err)
	}
}
