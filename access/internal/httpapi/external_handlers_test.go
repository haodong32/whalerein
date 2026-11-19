package httpapi

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

// fakeExternal scripts the external account service boundary.
type fakeExternal struct {
	enrollResponse    *whalereinv1.HostEnrollmentResponse
	enrollErr         error
	statusResponse    *whalereinv1.HostEnrollmentStatus
	statusErr         error
	hostStatus        *whalereinv1.HostEnrollmentStatus
	hostStatusErr     error
	operation         *whalereinv1.OperationStatus
	operationErr      error
	disabledAt        time.Time
	disableErr        error
	replacement       *external.CredentialReplacement
	recoveryErr       error
	lastHost          string
	lastProof         external.HostOwnershipProof
	lastAuthorization *whalereinv1.ExternalAuthorization
}

func (f *fakeExternal) EnrollHost(context.Context, *whalereinv1.HostEnrollmentRequest) (*whalereinv1.HostEnrollmentResponse, error) {
	return f.enrollResponse, f.enrollErr
}

func (f *fakeExternal) EnrollmentStatus(_ context.Context, hostID string) (*whalereinv1.HostEnrollmentStatus, error) {
	f.lastHost = hostID
	return f.statusResponse, f.statusErr
}

func (f *fakeExternal) EnrollmentStatusForHost(_ context.Context, hostID string, proof external.HostOwnershipProof) (*whalereinv1.HostEnrollmentStatus, error) {
	f.lastHost = hostID
	f.lastProof = proof
	return f.hostStatus, f.hostStatusErr
}

func (f *fakeExternal) OperationStatus(context.Context, string) (*whalereinv1.OperationStatus, error) {
	return f.operation, f.operationErr
}

func (f *fakeExternal) DisableHost(_ context.Context, hostID string) (time.Time, error) {
	f.lastHost = hostID
	return f.disabledAt, f.disableErr
}

func (f *fakeExternal) RecoverCredential(_ context.Context, hostID string, authorization *whalereinv1.ExternalAuthorization, proof external.HostOwnershipProof) (*external.CredentialReplacement, error) {
	f.lastHost = hostID
	f.lastAuthorization = authorization
	f.lastProof = proof
	return f.replacement, f.recoveryErr
}

// newExternalFixture builds a server with external mode enabled.
func newExternalFixture(t *testing.T, fake ExternalService) *Server {
	t.Helper()
	server, err := New(Dependencies{
		Auth:          &fakeAuth{adminToken: "admin-token"},
		Pairing:       &fakePairing{},
		Requests:      &fakeRequests{},
		Readiness:     diagnostics.NewReadiness(),
		Logger:        slog.New(slog.NewTextHandler(io.Discard, nil)),
		InternalToken: "internal",
		MaxBodyBytes:  1 << 20,
		External:      fake,
	})
	if err != nil {
		t.Fatal(err)
	}
	return server
}

const adminHeaderName = "Authorization"

func TestEnrollHostEndpointReturnsTheStatusAndCredential(t *testing.T) {
	fake := &fakeExternal{enrollResponse: &whalereinv1.HostEnrollmentResponse{
		Status:     &whalereinv1.HostEnrollmentStatus{HostId: "host-1", State: whalereinv1.EnrollmentState_enrollment_pending},
		Credential: &whalereinv1.HostCredential{HostId: "host-1", Username: "host:host-1", Password: "secret"},
	}}
	server := newExternalFixture(t, fake)
	body, err := protojson.Marshal(&whalereinv1.HostEnrollmentRequest{HostId: "host-1"})
	if err != nil {
		t.Fatal(err)
	}
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	decoded := &whalereinv1.HostEnrollmentResponse{}
	if err := protojson.Unmarshal(recorder.Body.Bytes(), decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.GetCredential().GetPassword() != "secret" {
		t.Fatalf("the committing response must carry the credential: %v", decoded)
	}
}

func TestEnrollHostEndpointRejectsMalformedBody(t *testing.T) {
	server := newExternalFixture(t, &fakeExternal{})
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts", []byte("{"), nil)
	if recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400, got %d", recorder.Code)
	}
}

func TestEnrollHostEndpointMapsRefusals(t *testing.T) {
	for name, testCase := range map[string]struct {
		err  error
		want int
	}{
		"expired":   {external.ErrAuthorizationExpired, http.StatusGone},
		"mismatch":  {external.ErrAuthorizationMismatch, http.StatusForbidden},
		"signature": {external.ErrAuthorizationSignature, http.StatusForbidden},
		"proof":     {external.ErrProofInvalid, http.StatusForbidden},
		"key":       {external.ErrKeyNotValid, http.StatusForbidden},
		"disabled":  {external.ErrEnrollmentDisabled, http.StatusForbidden},
		"conflict":  {store.ErrOperationConflict, http.StatusConflict},
		"stale":     {external.ErrStaleRevision, http.StatusConflict},
		"missing":   {store.ErrNotFound, http.StatusNotFound},
		"internal":  {errors.New("boom"), http.StatusInternalServerError},
	} {
		t.Run(name, func(t *testing.T) {
			server := newExternalFixture(t, &fakeExternal{enrollErr: testCase.err})
			body, err := protojson.Marshal(&whalereinv1.HostEnrollmentRequest{HostId: "host-1"})
			if err != nil {
				t.Fatal(err)
			}
			recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts", body, nil)
			if recorder.Code != testCase.want {
				t.Fatalf("expected %d, got %d: %s", testCase.want, recorder.Code, recorder.Body.String())
			}
		})
	}
}

func TestExternalStatusEndpointsRequireTheAdminBearer(t *testing.T) {
	fake := &fakeExternal{statusResponse: &whalereinv1.HostEnrollmentStatus{HostId: "host-1"}}
	server := newExternalFixture(t, fake)

	recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil, nil)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 without a token, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil,
		map[string]string{adminHeaderName: "Bearer wrong"})
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 for a wrong token, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200 with the admin token, got %d", recorder.Code)
	}
	if got := recorder.Header().Get("WhaleRein-Host-Presence"); got != "unavailable" {
		t.Fatalf("expected an explicit unavailable presence without broker evidence, got %q", got)
	}
	if fake.lastHost != "host-1" {
		t.Fatalf("the path parameter was not passed through: %q", fake.lastHost)
	}
}

func TestEnrollmentStatusKeepsReadyEnrollmentAcrossBrokerReconnect(t *testing.T) {
	fake := &fakeExternal{statusResponse: &whalereinv1.HostEnrollmentStatus{
		HostId: "host-1", State: whalereinv1.EnrollmentState_enrollment_ready,
	}}
	server := newExternalFixture(t, fake)
	presence := broker.NewPresence(time.Minute, nil, nil)
	server.deps.HostPresence = presence
	want := func(expected string) {
		t.Helper()
		recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil,
			map[string]string{adminHeaderName: "Bearer admin-token"})
		var status whalereinv1.HostEnrollmentStatus
		if recorder.Code != http.StatusOK || protojson.Unmarshal(recorder.Body.Bytes(), &status) != nil ||
			status.GetState() != whalereinv1.EnrollmentState_enrollment_ready {
			t.Fatalf("enrollment changed: %d %s", recorder.Code, recorder.Body.String())
		}
		if got := recorder.Header().Get("WhaleRein-Host-Presence"); got != expected {
			t.Fatalf("presence = %q, want %q", got, expected)
		}
	}
	want("unavailable")
	presence.ApplySnapshot(nil, 0)
	presence.HandleNotification(&broker.Notification{Sequence: 1, Kind: "host", HostID: "host-1", ConnID: 1, Event: "connected"})
	want("offline")
	presence.MarkHostReady("host-1", time.Now())
	want("online")
	presence.HandleNotification(&broker.Notification{Sequence: 2, Kind: "host", HostID: "host-1", ConnID: 1, Event: "disconnected"})
	want("offline")
	presence.HandleNotification(&broker.Notification{Sequence: 3, Kind: "host", HostID: "host-1", ConnID: 2, Event: "connected"})
	presence.MarkHostReady("host-1", time.Now())
	presence.HandleNotification(&broker.Notification{Sequence: 4, Kind: "host", HostID: "host-1", ConnID: 1, Event: "disconnected"})
	want("online")
}

func TestOperationStatusEndpointReportsRefusals(t *testing.T) {
	fake := &fakeExternal{operationErr: store.ErrNotFound}
	server := newExternalFixture(t, fake)
	recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/operations/op-1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusNotFound {
		t.Fatalf("expected 404 for an unknown operation, got %d", recorder.Code)
	}
}

func TestExternalRoutesReportAnExplicitCapabilityErrorWithoutTheMode(t *testing.T) {
	server := newAPIFixture(t).server
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts", []byte(`{"hostId":"host-1"}`), nil)
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/operations/op-1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
	// A mistyped path stays a plain 404 so the two conditions differ.
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollment", nil, nil)
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
}

func hostProofHeader(t *testing.T, hostID string) string {
	t.Helper()
	document := map[string]any{
		"hostId":    hostID,
		"issuedAt":  time.Now().UTC().UnixMilli(),
		"nonce":     "bm9uY2U",
		"signature": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
	}
	data, err := json.Marshal(document)
	if err != nil {
		t.Fatal(err)
	}
	return base64.RawURLEncoding.EncodeToString(data)
}

func TestHostEnrollmentProofStatusRequiresAHostProof(t *testing.T) {
	fake := &fakeExternal{hostStatus: &whalereinv1.HostEnrollmentStatus{
		HostId: "host-1", EnrollmentRevision: "2", OperationId: "op-1",
		State: whalereinv1.EnrollmentState_enrollment_ready,
	}}
	server := newExternalFixture(t, fake)
	headers := map[string]string{HostProofHeader: hostProofHeader(t, "host-1")}

	recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/hosts/host-1/enrollment", nil, nil)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("a bare host id must be unauthorized, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/hosts/host-1/enrollment", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("the admin bearer must not replace the Host proof, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/hosts/host-1/enrollment", nil,
		map[string]string{HostProofHeader: "%%%"})
	if recorder.Code != http.StatusForbidden {
		t.Fatalf("a malformed proof must be forbidden, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/hosts/host-1/enrollment", nil, headers)
	if recorder.Code != http.StatusOK {
		t.Fatalf("a valid proof must read the status, got %d: %s", recorder.Code, recorder.Body.String())
	}
	if fake.lastProof.HostID != "host-1" {
		t.Fatalf("the proof was not passed through: %+v", fake.lastProof)
	}
	decoded := &whalereinv1.HostEnrollmentStatus{}
	if err := protojson.Unmarshal(recorder.Body.Bytes(), decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.GetState() != whalereinv1.EnrollmentState_enrollment_ready || decoded.GetEnrollmentRevision() != "2" {
		t.Fatalf("unexpected status %+v", decoded)
	}
}

func TestHostDisableRequiresTheAdminBearerAndIsIdempotent(t *testing.T) {
	disabledAt := time.Date(2026, 11, 16, 12, 0, 0, 0, time.UTC)
	fake := &fakeExternal{disabledAt: disabledAt}
	server := newExternalFixture(t, fake)
	path := "/v1/enrollments/hosts/host-1/disable"

	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path, nil, nil)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 without the admin bearer, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodPost, path, nil,
		map[string]string{adminHeaderName: "Bearer wrong"})
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 for a wrong admin bearer, got %d", recorder.Code)
	}
	for attempt := 0; attempt < 2; attempt++ {
		recorder = doRequest(t, server.PublicHandler(), http.MethodPost, path, nil,
			map[string]string{adminHeaderName: "Bearer admin-token"})
		if recorder.Code != http.StatusOK {
			t.Fatalf("attempt %d: expected 200, got %d: %s", attempt, recorder.Code, recorder.Body.String())
		}
		var body struct {
			State      string `json:"state"`
			DisabledAt string `json:"disabledAt"`
		}
		if err := json.Unmarshal(recorder.Body.Bytes(), &body); err != nil {
			t.Fatal(err)
		}
		if body.State != "disabled" || body.DisabledAt != disabledAt.Format(time.RFC3339) {
			t.Fatalf("attempt %d: unexpected body %+v", attempt, body)
		}
	}
}

func TestCredentialReplacementReturnsTheCredentialOnlyOnce(t *testing.T) {
	fake := &fakeExternal{replacement: &external.CredentialReplacement{
		Credential: external.IssuedCredential{
			HostID: "host-1", MQTTURL: "mqtts://broker.test:8883",
			Username: "host:host-1", Password: "new-secret",
		},
		State: "enrollment_ready", Revision: "2",
	}}
	server := newExternalFixture(t, fake)
	authorization := &whalereinv1.ExternalAuthorization{
		AuthorizationId: "auth-recover-1", Purpose: external.PurposeHostCredentialRecovery,
		HostId: "host-1", WorkspaceId: "ws-1", EnrollmentRevision: "2",
	}
	body, err := json.Marshal(map[string]any{"authorization": authorization})
	if err != nil {
		t.Fatal(err)
	}
	path := "/v1/enrollments/hosts/host-1/credential"
	headers := map[string]string{
		HostProofHeader: hostProofHeader(t, "host-1"),
		"Content-Type":  "application/json",
	}
	// A bare id without the proof is refused before any authorization work.
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path, body,
		map[string]string{"Content-Type": "application/json"})
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("a bare id must be unauthorized, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodPost, path, body, headers)
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	var response struct {
		Credential struct {
			HostID   string `json:"hostId"`
			MQTTURL  string `json:"mqttUrl"`
			Username string `json:"username"`
			Password string `json:"password"`
		} `json:"credential"`
		State    string `json:"state"`
		Revision string `json:"revision"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response.Credential.Password != "new-secret" || response.Credential.Username != "host:host-1" {
		t.Fatalf("unexpected credential %+v", response.Credential)
	}
	if response.State != "enrollment_ready" || response.Revision != "2" {
		t.Fatalf("unexpected replacement metadata %+v", response)
	}
	if fake.lastAuthorization.GetAuthorizationId() != "auth-recover-1" {
		t.Fatalf("the authorization was not passed through: %+v", fake.lastAuthorization)
	}
}

func TestNewExternalRoutesReportAnExplicitCapabilityErrorWithoutTheMode(t *testing.T) {
	server := newAPIFixture(t).server
	recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/hosts/host-1/enrollment", nil,
		map[string]string{HostProofHeader: hostProofHeader(t, "host-1")})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
	recorder = doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/disable", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
	recorder = doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/credential", nil,
		map[string]string{HostProofHeader: hostProofHeader(t, "host-1")})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
}
