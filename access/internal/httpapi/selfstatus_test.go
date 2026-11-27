package httpapi

import (
	"bytes"
	"context"
	"io"
	"log/slog"
	"net/http"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/selfstatus"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

type fakeSelfStatusService struct {
	calls  int
	input  selfstatus.Input
	status *whalereinv1.SelfStatus
	err    error
}

func (f *fakeSelfStatusService) Submit(_ context.Context, input selfstatus.Input) (*whalereinv1.SelfStatus, error) {
	f.calls++
	f.input = input
	return f.status, f.err
}

func selfStatusEnvelope(t *testing.T, hostID string, kind whalereinv1.RequestKind, grantID string, challenge []byte) []byte {
	t.Helper()
	frame, err := proto.Marshal(&whalereinv1.SelfStatusRequest{GrantId: grantID, Challenge: challenge})
	if err != nil {
		t.Fatal(err)
	}
	envelope, err := protojson.Marshal(&whalereinv1.HostRequest{HostId: hostID, Kind: kind, Body: frame})
	if err != nil {
		t.Fatal(err)
	}
	return envelope
}

func testChallenge(seed byte) []byte {
	value := make([]byte, 24)
	for index := range value {
		value[index] = seed + byte(index)
	}
	return value
}

func installSelfStatus(f *apiFixture, service SelfStatusService) {
	f.server.deps.SelfStatus = service
	f.server.deps.SelfStatusMaxBytes = selfstatus.MaxRequestBytes
}

func TestSelfStatusRouteRelaysOnlyTheExactBoundedPurpose(t *testing.T) {
	f := newAPIFixture(t)
	fake := &fakeSelfStatusService{status: &whalereinv1.SelfStatus{
		GrantId: "g1", Challenge: testChallenge(1), Active: false, RevokedAtUnixMs: 1_700_000_000_000,
	}}
	installSelfStatus(f, fake)
	body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", testChallenge(1))
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var status whalereinv1.SelfStatus
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &status); err != nil {
		t.Fatal(err)
	}
	if status.GetGrantId() != "g1" || status.GetActive() || status.GetRevokedAtUnixMs() != 1_700_000_000_000 {
		t.Fatalf("status = %+v", &status)
	}
	if fake.calls != 1 || fake.input.HostID != "h1" || fake.input.GrantID != "g1" ||
		!bytes.Equal(fake.input.Challenge, testChallenge(1)) {
		t.Fatalf("relayed input = %+v (calls=%d)", fake.input, fake.calls)
	}
	if recorder.Header().Get("Cache-Control") != "no-store" {
		t.Fatal("a status response must not be cacheable")
	}
}

func TestSelfStatusRouteAdmitsNoOrdinaryRequestKind(t *testing.T) {
	for _, kind := range []whalereinv1.RequestKind{
		whalereinv1.RequestKind_REQUEST_KIND_QUERY,
		whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		whalereinv1.RequestKind_REQUEST_KIND_PAIRING_CLAIM,
		whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE,
		whalereinv1.RequestKind_REQUEST_KIND_ACCESS_READY,
	} {
		t.Run(kind.String(), func(t *testing.T) {
			f := newAPIFixture(t)
			fake := &fakeSelfStatusService{}
			installSelfStatus(f, fake)
			body := selfStatusEnvelope(t, "h1", kind, "g1", testChallenge(1))
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
			assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
			if fake.calls != 0 {
				t.Fatalf("a %s frame reached the relay", kind)
			}
		})
	}
}

func TestSelfStatusRouteRejectsOrdinaryEnvelopeFieldsAndWrongClaims(t *testing.T) {
	frame, _ := proto.Marshal(&whalereinv1.SelfStatusRequest{GrantId: "g1", Challenge: testChallenge(2)})
	cases := map[string]*whalereinv1.HostRequest{
		"rejects a caller-supplied request id": {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, RequestId: "r1"},
		"rejects a caller-supplied device id":  {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, DeviceId: "d1"},
		"rejects a command id":                 {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, CommandId: "c1"},
		"rejects a channel id":                 {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, ChannelId: "ch1"},
		"rejects a body reference":             {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, BodyReference: &whalereinv1.PayloadReference{ObjectId: "o1"}},
		"rejects a foreign host claim":         {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, HostId: "h2"},
		"rejects an unsupported protocol":      {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, ProtocolVersion: 2},
		"rejects a foreign relay epoch":        {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame, RelayVersion: 4},
		"rejects a missing frame body":         {Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS},
	}
	for name, envelope := range cases {
		t.Run(name, func(t *testing.T) {
			f := newAPIFixture(t)
			fake := &fakeSelfStatusService{}
			installSelfStatus(f, fake)
			body, err := protojson.Marshal(envelope)
			if err != nil {
				t.Fatal(err)
			}
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
			assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
			if fake.calls != 0 {
				t.Fatal("a widened envelope reached the relay")
			}
		})
	}
}

func TestSelfStatusRouteBoundsTheFrameAndEnvelope(t *testing.T) {
	f := newAPIFixture(t)
	fake := &fakeSelfStatusService{}
	installSelfStatus(f, fake)

	// The HTTPS envelope ceiling is enforced before decoding.
	oversized := bytes.Repeat([]byte("a"), selfstatus.MaxRequestBytes+1)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", oversized, nil)
	assertError(t, recorder, http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large)

	// A body over the status-frame ceiling is refused even inside the envelope.
	frame := bytes.Repeat([]byte{0x0a}, selfstatus.MaxFrameBytes+1)
	envelope, _ := protojson.Marshal(&whalereinv1.HostRequest{
		Kind: whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, Body: frame,
	})
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", envelope, nil)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
	if fake.calls != 0 {
		t.Fatal("an oversized frame reached the relay")
	}
}

func TestSelfStatusRouteRequiresJSONAndPost(t *testing.T) {
	f := newAPIFixture(t)
	installSelfStatus(f, &fakeSelfStatusService{})
	body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", testChallenge(3))
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, map[string]string{"Content-Type": "text/plain"})
	assertError(t, recorder, http.StatusUnsupportedMediaType, whalereinv1.ErrorCode_invalid_argument)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/self-status", nil, nil)
	assertError(t, recorder, http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument)
}

func TestSelfStatusRouteMapsServiceRefusalsToBoundedAnswers(t *testing.T) {
	cases := []struct {
		name   string
		err    error
		status int
		code   whalereinv1.ErrorCode
		reason string
	}{
		{"unknown or foreign tuple", selfstatus.ErrInaccessible, http.StatusNotFound, whalereinv1.ErrorCode_not_found, ""},
		{"offline host stays pending", selfstatus.ErrHostOffline, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "host_offline"},
		{"timed-out exchange stays pending", selfstatus.ErrTimeout, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "self_status_pending"},
		{"unavailable relay stays pending", selfstatus.ErrHostUnavailable, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "self_status_pending"},
		{"reused challenge", selfstatus.ErrReplay, http.StatusConflict, whalereinv1.ErrorCode_conflict, ""},
		{"admission limit", selfstatus.ErrBusy, http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited, "admission_limit"},
		{"malformed frame", selfstatus.ErrInvalid, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, ""},
	}
	body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", testChallenge(4))
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newAPIFixture(t)
			installSelfStatus(f, &fakeSelfStatusService{err: tc.err})
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
			assertError(t, recorder, tc.status, tc.code)
			if tc.reason != "" {
				var response whalereinv1.ErrorResponse
				if err := protojson.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
					t.Fatal(err)
				}
				if response.GetReason() != tc.reason {
					t.Fatalf("reason = %q, want %q", response.GetReason(), tc.reason)
				}
			}
		})
	}
}

func TestSelfStatusUnavailableTupleIsIndistinguishableFromAnUnknownRoute(t *testing.T) {
	f := newAPIFixture(t)
	installSelfStatus(f, &fakeSelfStatusService{err: selfstatus.ErrInaccessible})
	body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", testChallenge(5))
	inaccessible := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
	var first whalereinv1.ErrorResponse
	if err := protojson.Unmarshal(inaccessible.Body.Bytes(), &first); err != nil {
		t.Fatal(err)
	}

	// The same Host route with a different unknown grant must produce exactly
	// the same public error shape.
	other := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g2", testChallenge(6))
	secondRecorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", other, nil)
	var second whalereinv1.ErrorResponse
	if err := protojson.Unmarshal(secondRecorder.Body.Bytes(), &second); err != nil {
		t.Fatal(err)
	}
	if first.GetCode() != second.GetCode() || first.GetMessage() != second.GetMessage() || first.GetReason() != second.GetReason() {
		t.Fatalf("inaccessible answers differ: %+v vs %+v", &first, &second)
	}
}

func TestSelfStatusWithoutConfiguredRelayIsExplicitlyUnsupported(t *testing.T) {
	f := newAPIFixture(t)
	body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", testChallenge(7))
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
}

func TestSelfStatusRouteIsNotOnTheInternalListener(t *testing.T) {
	f := newAPIFixture(t)
	installSelfStatus(f, &fakeSelfStatusService{})
	recorder := doRequest(t, f.server.InternalHandler(), http.MethodPost, "/v1/hosts/h1/self-status", []byte("{}"),
		map[string]string{InternalHeader: "internal-token"})
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
}

// TestSelfStatusRouteCompletesThroughTheRealRelay wires HTTP -> selfstatus ->
// a fake Host publisher and proves the whole bounded exchange without a broker.
func TestSelfStatusRouteCompletesThroughTheRealRelay(t *testing.T) {
	f := newAPIFixture(t)
	value := testChallenge(8)
	publisher := &relayTestPublisher{}
	service, err := selfstatus.New(selfstatus.Dependencies{
		Presence:  relayTestPresence{online: true, known: true},
		Publisher: publisher,
		Config:    selfstatus.Config{ExchangeTimeout: 2 * time.Second},
		Metrics:   diagnostics.NewMetrics(),
		Logger:    slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	publisher.onPublish = func(message broker.Message) {
		var request whalereinv1.HostRequest
		if err := proto.Unmarshal(message.Payload, &request); err != nil {
			t.Error(err)
			return
		}
		body, _ := proto.Marshal(&whalereinv1.SelfStatus{
			GrantId: "g1", Challenge: value, Active: false, RevokedAtUnixMs: 99,
		})
		service.Complete(context.Background(), "h1", &whalereinv1.HostResult{
			RequestId: request.GetRequestId(), HostId: "h1",
			Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body, RelayVersion: broker.RelayEpoch,
		})
	}
	installSelfStatus(f, service)
	body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", value)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var status whalereinv1.SelfStatus
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &status); err != nil {
		t.Fatal(err)
	}
	if status.GetGrantId() != "g1" || status.GetActive() || status.GetRevokedAtUnixMs() != 99 {
		t.Fatalf("status = %+v", &status)
	}
}

func TestSelfStatusRouteReturnsPendingForOfflineAndUnknownHosts(t *testing.T) {
	value := testChallenge(9)
	for name, presence := range map[string]relayTestPresence{
		"offline": {known: true},
		"unknown": {},
	} {
		t.Run(name, func(t *testing.T) {
			f := newAPIFixture(t)
			service, err := selfstatus.New(selfstatus.Dependencies{
				Presence: presence, Publisher: &relayTestPublisher{},
				Config:  selfstatus.Config{ExchangeTimeout: 50 * time.Millisecond},
				Metrics: diagnostics.NewMetrics(),
				Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
			})
			if err != nil {
				t.Fatal(err)
			}
			installSelfStatus(f, service)
			body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", value)
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
			// Both a known-offline Host and an unreconciled directory stay
			// bounded pending: Access cannot prove an absent Host.
			assertError(t, recorder, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable)
		})
	}
}

type relayTestPresence struct {
	online bool
	known  bool
}

func (p relayTestPresence) HostPresence(string) (bool, bool) { return p.online, p.known }

type relayTestPublisher struct {
	onPublish func(broker.Message)
}

func (p *relayTestPublisher) PublishMessage(_ context.Context, message broker.Message) error {
	if p.onPublish != nil {
		p.onPublish(message)
	}
	return nil
}

// TestSelfStatusRouteDoesNotRequireAManagedAppSession pins the cutover
// requirement: the bounded status exchange works in managed mode without a
// Cloud login, App session or renewable ticket. The Host remains the authority.
func TestSelfStatusRouteDoesNotRequireAManagedAppSession(t *testing.T) {
	f := newAPIFixture(t)
	f.server.deps.RequireDeviceSession = true
	service := &fakeSelfStatusService{status: &whalereinv1.SelfStatus{
		GrantId: "g1", Challenge: testChallenge(21), Active: true,
	}}
	installSelfStatus(f, service)
	body := selfStatusEnvelope(t, "h1", whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS, "g1", testChallenge(21))
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/self-status", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("managed-mode self-status status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if service.calls != 1 {
		t.Fatalf("relay calls = %d", service.calls)
	}
}
