package httpapi

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

// scopedOperationExternal also implements the proof-scoped operation read that
// the handler discovers through a type assertion.
type scopedOperationExternal struct {
	*fakeExternal
	hostOperation    *whalereinv1.OperationStatus
	hostOperationErr error
	lastOperation    string
	lastExpires      int64
}

func (f *scopedOperationExternal) OperationStatusForHost(_ context.Context, hostID, operationID string, authorizationExpiresAtUnixMs int64, proof external.HostOwnershipProof) (*whalereinv1.OperationStatus, error) {
	f.lastHost = hostID
	f.lastOperation = operationID
	f.lastExpires = authorizationExpiresAtUnixMs
	f.lastProof = proof
	return f.hostOperation, f.hostOperationErr
}

// removalExternal implements the scoped durable removal contract on top of the
// legacy fake.
type removalExternal struct {
	*fakeExternal
	receipt   external.RemovalReceipt
	removeErr error
	requests  []external.RemovalRequest
}

func (f *removalExternal) RemoveHost(_ context.Context, hostID string, request external.RemovalRequest) (external.RemovalReceipt, error) {
	f.lastHost = hostID
	f.requests = append(f.requests, request)
	return f.receipt, f.removeErr
}

// deviceSessionExternal exposes the external device-session endpoints without
// replacing the other fakes.
type deviceSessionExternal struct {
	*fakeExternal
	installErr error
}

func (f *deviceSessionExternal) InstallDeviceSession(context.Context, string, *whalereinv1.ExternalAuthorization) error {
	return f.installErr
}

func (f *deviceSessionExternal) RevokeDeviceSession(context.Context, string, int64) error {
	return f.installErr
}

func (f *deviceSessionExternal) DeviceSessionPairingStatus(context.Context, string, string, string) (string, error) {
	return "terminal", nil
}

func (f *deviceSessionExternal) HostClients(context.Context, string, string, string) (external.HostClientList, error) {
	return external.HostClientList{Presence: "known"}, nil
}

// invokeHandler calls one handler with explicit path params, matching how the
// router dispatches it.
func invokeHandler(t *testing.T, server *Server, handler func(http.ResponseWriter, *http.Request, map[string]string), method, target string, body []byte, headers map[string]string, params map[string]string) *httptest.ResponseRecorder {
	t.Helper()
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	request := httptest.NewRequest(method, target, reader)
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}
	for key, value := range headers {
		request.Header.Set(key, value)
	}
	recorder := httptest.NewRecorder()
	handler(recorder, request, params)
	return recorder
}

func TestHostOperationStatusHandlerReturnsTheScopedOperation(t *testing.T) {
	scoped := &scopedOperationExternal{
		fakeExternal: &fakeExternal{},
		hostOperation: &whalereinv1.OperationStatus{
			OperationId: "op-1", HostId: "host-1", Purpose: external.PurposeHostEnrollment,
		},
	}
	server := newExternalFixture(t, scoped)
	headers := map[string]string{HostProofHeader: hostProofHeader(t, "host-1")}
	params := map[string]string{"host_id": "host-1", "operation_id": "op-1"}

	recorder := invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/host-1/operations/op-1?authorizationExpiresAtUnixMs=1700000000000", nil, headers, params)
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	if scoped.lastOperation != "op-1" || scoped.lastExpires != 1700000000000 {
		t.Fatalf("unexpected scoped request: operation=%q expires=%d", scoped.lastOperation, scoped.lastExpires)
	}
	if scoped.lastProof.HostID != "host-1" {
		t.Fatalf("the proof was not passed through: %+v", scoped.lastProof)
	}
	var body map[string]any
	if err := json.Unmarshal(recorder.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body["operation_id"] != "op-1" {
		t.Fatalf("unexpected response body %v", body)
	}

	// An absent query keeps the default zero deadline.
	recorder = invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/host-1/operations/op-1", nil, headers, params)
	if recorder.Code != http.StatusOK || scoped.lastExpires != 0 {
		t.Fatalf("default deadline: status=%d expires=%d", recorder.Code, scoped.lastExpires)
	}
}

func TestHostOperationStatusHandlerRejectsBadRequests(t *testing.T) {
	scoped := &scopedOperationExternal{fakeExternal: &fakeExternal{},
		hostOperation: &whalereinv1.OperationStatus{OperationId: "op-1"}}
	server := newExternalFixture(t, scoped)
	headers := map[string]string{HostProofHeader: hostProofHeader(t, "host-1")}
	validParams := map[string]string{"host_id": "host-1", "operation_id": "op-1"}

	if recorder := invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/bad!/operations/op-1", nil, headers, map[string]string{"host_id": "bad!", "operation_id": "op-1"}); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid Host id, got %d", recorder.Code)
	}
	if recorder := invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/host-1/operations/bad!", nil, headers, map[string]string{"host_id": "host-1", "operation_id": "bad!"}); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid operation id, got %d", recorder.Code)
	}
	if recorder := invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/host-1/operations/op-1", nil, nil, validParams); recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 without a proof, got %d", recorder.Code)
	}
	// This handler forwards the parser refusal directly instead of routing it
	// through classifyExternal, so a malformed proof currently maps to an
	// internal error rather than a 403.
	if recorder := invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/host-1/operations/op-1", nil, map[string]string{HostProofHeader: "%%%"}, validParams); recorder.Code != http.StatusInternalServerError {
		t.Fatalf("expected the forwarded proof error to surface as 500, got %d", recorder.Code)
	}
	for _, query := range []string{"?authorizationExpiresAtUnixMs=abc", "?authorizationExpiresAtUnixMs=-1"} {
		if recorder := invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
			"/v1/hosts/host-1/operations/op-1"+query, nil, headers, validParams); recorder.Code != http.StatusBadRequest {
			t.Fatalf("%s: expected 400, got %d", query, recorder.Code)
		}
	}

	legacy := newExternalFixture(t, &fakeExternal{})
	if recorder := invokeHandler(t, legacy, legacy.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/host-1/operations/op-1", nil, headers, validParams); recorder.Code != http.StatusNotImplemented {
		t.Fatalf("expected 501 for an unscoped service, got %d", recorder.Code)
	}
	disabled := newAPIFixture(t).server
	if recorder := invokeHandler(t, disabled, disabled.handleHostOperationStatus, http.MethodGet,
		"/v1/hosts/host-1/operations/op-1", nil, headers, validParams); recorder.Code != http.StatusNotImplemented {
		t.Fatalf("expected 501 without external mode, got %d", recorder.Code)
	}
}

func TestHostOperationStatusHandlerMapsRefusals(t *testing.T) {
	for name, testCase := range map[string]struct {
		err  error
		want int
	}{
		"expired":     {external.ErrAuthorizationExpired, http.StatusGone},
		"refused":     {external.ErrProofInvalid, http.StatusForbidden},
		"conflict":    {external.ErrOperationConflict, http.StatusConflict},
		"unavailable": {external.ErrEnforcementUnavailable, http.StatusServiceUnavailable},
		"missing":     {store.ErrNotFound, http.StatusNotFound},
		"internal":    {errors.New("boom"), http.StatusInternalServerError},
	} {
		t.Run(name, func(t *testing.T) {
			scoped := &scopedOperationExternal{fakeExternal: &fakeExternal{}, hostOperationErr: testCase.err}
			server := newExternalFixture(t, scoped)
			recorder := invokeHandler(t, server, server.handleHostOperationStatus, http.MethodGet,
				"/v1/hosts/host-1/operations/op-1", nil,
				map[string]string{HostProofHeader: hostProofHeader(t, "host-1")},
				map[string]string{"host_id": "host-1", "operation_id": "op-1"})
			if recorder.Code != testCase.want {
				t.Fatalf("expected %d, got %d: %s", testCase.want, recorder.Code, recorder.Body.String())
			}
		})
	}
}

func TestDisableHostEnrollmentScopedReceipt(t *testing.T) {
	disabledAt := time.Date(2026, 11, 16, 12, 0, 0, 0, time.UTC)
	removal := &removalExternal{fakeExternal: &fakeExternal{}, receipt: external.RemovalReceipt{
		State:              "pending",
		RemovalOperationID: "rem-1",
		Target: store.RemovalTarget{
			WorkspaceID: "ws-1", EnrollmentOperationID: "op-1", EnrollmentRevision: "2",
		},
		DisabledAt:  disabledAt,
		Revocations: map[string]uint64{"device-1": 1, "device-2": 2},
	}}
	server := newExternalFixture(t, removal)
	body := []byte(`{"removal_operation_id":"rem-1","workspace_id":"ws-1","enrollment_operation_id":"op-1","enrollment_revision":"2"}`)
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/disable", body,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	var response struct {
		State              string `json:"state"`
		RemovalOperationID string `json:"removal_operation_id"`
		DisabledAtUnixMs   int64  `json:"disabled_at_unix_ms"`
		EnforcedAtUnixMs   int64  `json:"enforced_at_unix_ms"`
		RevocationCount    int    `json:"revocation_count"`
		Target             struct {
			WorkspaceID           string `json:"workspace_id"`
			HostID                string `json:"host_id"`
			EnrollmentOperationID string `json:"enrollment_operation_id"`
			EnrollmentRevision    string `json:"enrollment_revision"`
		} `json:"target"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response.State != "pending" || response.RemovalOperationID != "rem-1" ||
		response.DisabledAtUnixMs != disabledAt.UnixMilli() || response.EnforcedAtUnixMs != 0 ||
		response.RevocationCount != 2 {
		t.Fatalf("unexpected removal receipt %+v", response)
	}
	if response.Target.HostID != "host-1" || response.Target.WorkspaceID != "ws-1" ||
		response.Target.EnrollmentOperationID != "op-1" || response.Target.EnrollmentRevision != "2" {
		t.Fatalf("unexpected removal target %+v", response.Target)
	}
	if len(removal.requests) != 1 || removal.requests[0].RemovalOperationID != "rem-1" ||
		removal.requests[0].Target == nil || removal.requests[0].Target.WorkspaceID != "ws-1" {
		t.Fatalf("the scoped removal was not passed through: %+v", removal.requests)
	}

	// An enforced receipt carries the recorded enforcement time.
	enforcedAt := disabledAt.Add(time.Minute)
	removal.receipt = external.RemovalReceipt{
		State: "enforced", RemovalOperationID: "rem-1", DisabledAt: disabledAt,
		EnforcedAt: &enforcedAt,
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/disable", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	var enforced struct {
		State            string `json:"state"`
		EnforcedAtUnixMs int64  `json:"enforced_at_unix_ms"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &enforced); err != nil {
		t.Fatal(err)
	}
	if enforced.State != "enforced" || enforced.EnforcedAtUnixMs != enforcedAt.UnixMilli() {
		t.Fatalf("unexpected enforced receipt %+v", enforced)
	}
	if len(removal.requests) != 2 || removal.requests[1].RemovalOperationID != "" || removal.requests[1].Target != nil {
		t.Fatalf("a bodyless removal must stay unscoped: %+v", removal.requests)
	}
}

func TestDisableHostEnrollmentRejectsBadScopedRequests(t *testing.T) {
	removal := &removalExternal{fakeExternal: &fakeExternal{}}
	server := newExternalFixture(t, removal)
	admin := map[string]string{adminHeaderName: "Bearer admin-token"}

	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/bad!/disable", nil, admin); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid Host id, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/disable", []byte("{"), admin); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for a malformed removal body, got %d", recorder.Code)
	}

	// A legacy service cannot silently ignore a frozen removal target.
	legacy := newExternalFixture(t, &fakeExternal{})
	recorder := doRequest(t, legacy.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/disable",
		[]byte(`{"removal_operation_id":"rem-1"}`), admin)
	if recorder.Code != http.StatusNotImplemented {
		t.Fatalf("expected 501 for an unscoped target, got %d: %s", recorder.Code, recorder.Body.String())
	}

	// A legacy disable failure keeps its classified status.
	legacyFake := &fakeExternal{disableErr: external.ErrAuthorizationMismatch}
	legacy = newExternalFixture(t, legacyFake)
	recorder = doRequest(t, legacy.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/disable", nil, admin)
	if recorder.Code != http.StatusForbidden {
		t.Fatalf("expected 403 for a legacy refusal, got %d", recorder.Code)
	}

	removal.removeErr = store.ErrNotFound
	recorder = doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/host-1/disable", nil, admin)
	if recorder.Code != http.StatusNotFound {
		t.Fatalf("expected 404 for an unknown removal, got %d", recorder.Code)
	}
}

func TestCredentialReplacementRejectsBadRequests(t *testing.T) {
	fake := &fakeExternal{}
	server := newExternalFixture(t, fake)
	path := "/v1/enrollments/hosts/host-1/credential"
	proof := map[string]string{HostProofHeader: hostProofHeader(t, "host-1"), "Content-Type": "application/json"}

	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts/bad!/credential", nil, proof); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid Host id, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path, []byte("{}"),
		map[string]string{HostProofHeader: "%%%", "Content-Type": "application/json"}); recorder.Code != http.StatusForbidden {
		t.Fatalf("expected 403 for a malformed proof, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path, []byte("{}"),
		map[string]string{HostProofHeader: hostProofHeader(t, "host-1"), "Content-Type": "text/plain"}); recorder.Code != http.StatusUnsupportedMediaType {
		t.Fatalf("expected 415 without JSON, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path, []byte("{"), proof); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for malformed JSON, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path, []byte(`{"other":1}`), proof); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 without an authorization, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path,
		[]byte(`{"authorization":{"authVersion":123}}`), proof); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid authorization message, got %d", recorder.Code)
	}

	for name, testCase := range map[string]struct {
		err  error
		want int
	}{
		"refused":     {external.ErrAuthorizationMismatch, http.StatusForbidden},
		"missing":     {store.ErrNotFound, http.StatusNotFound},
		"unavailable": {external.ErrEnforcementUnavailable, http.StatusServiceUnavailable},
		"expired":     {external.ErrAuthorizationExpired, http.StatusGone},
		"internal":    {errors.New("boom"), http.StatusInternalServerError},
	} {
		t.Run(name, func(t *testing.T) {
			fake := &fakeExternal{recoveryErr: testCase.err}
			server := newExternalFixture(t, fake)
			body := []byte(`{"authorization":{"authorizationId":"auth-1"}}`)
			recorder := doRequest(t, server.PublicHandler(), http.MethodPost, path, body, proof)
			if recorder.Code != testCase.want {
				t.Fatalf("expected %d, got %d: %s", testCase.want, recorder.Code, recorder.Body.String())
			}
		})
	}
}

func TestHostEnrollmentProofStatusRejectsInvalidHostAndMapsErrors(t *testing.T) {
	fake := &fakeExternal{hostStatusErr: external.ErrAuthorizationMismatch}
	server := newExternalFixture(t, fake)
	proof := map[string]string{HostProofHeader: hostProofHeader(t, "host-1")}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/hosts/bad!/enrollment", nil, proof); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid Host id, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/hosts/host-1/enrollment", nil, proof); recorder.Code != http.StatusForbidden {
		t.Fatalf("expected 403 for a refused proof, got %d", recorder.Code)
	}
}

func TestEnrollmentStatusPresenceAndErrors(t *testing.T) {
	admin := map[string]string{adminHeaderName: "Bearer admin-token"}
	for name, testCase := range map[string]struct {
		status   *whalereinv1.HostEnrollmentStatus
		err      error
		wantCode int
		presence string
	}{
		"pending":  {status: &whalereinv1.HostEnrollmentStatus{HostId: "host-1", State: whalereinv1.EnrollmentState_enrollment_pending}, wantCode: http.StatusOK, presence: "offline"},
		"disabled": {status: &whalereinv1.HostEnrollmentStatus{HostId: "host-1", State: whalereinv1.EnrollmentState_enrollment_disabled}, wantCode: http.StatusOK, presence: "offline"},
		"missing":  {err: store.ErrNotFound, wantCode: http.StatusNotFound},
	} {
		t.Run(name, func(t *testing.T) {
			server := newExternalFixture(t, &fakeExternal{statusResponse: testCase.status, statusErr: testCase.err})
			recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil, admin)
			if recorder.Code != testCase.wantCode {
				t.Fatalf("expected %d, got %d: %s", testCase.wantCode, recorder.Code, recorder.Body.String())
			}
			if testCase.presence != "" {
				if got := recorder.Header().Get("WhaleRein-Host-Presence"); got != testCase.presence {
					t.Fatalf("presence = %q, want %q", got, testCase.presence)
				}
			}
		})
	}

	if recorder := doRequest(t, newExternalFixture(t, &fakeExternal{}).PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/bad!", nil, admin); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid Host id, got %d", recorder.Code)
	}
}

func TestOperationStatusEndpointServesSuccessAndRejectsBadRequests(t *testing.T) {
	fake := &fakeExternal{operation: &whalereinv1.OperationStatus{OperationId: "op-1", HostId: "host-1"}}
	server := newExternalFixture(t, fake)
	admin := map[string]string{adminHeaderName: "Bearer admin-token"}

	recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/operations/op-1", nil, admin)
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	status := &whalereinv1.OperationStatus{}
	if err := protojson.Unmarshal(recorder.Body.Bytes(), status); err != nil {
		t.Fatal(err)
	}
	if status.GetOperationId() != "op-1" {
		t.Fatalf("unexpected operation status %+v", status)
	}

	if recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/operations/op-1", nil, nil); recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 without the admin bearer, got %d", recorder.Code)
	}
	if recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/operations/bad!", nil, admin); recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 for an invalid operation id, got %d", recorder.Code)
	}
}

func TestWriteJSONReportsAnEncodingFailure(t *testing.T) {
	server := newExternalFixture(t, &fakeExternal{})
	recorder := httptest.NewRecorder()
	server.writeJSON(recorder, http.StatusOK, make(chan int))
	if recorder.Code != http.StatusInternalServerError {
		t.Fatalf("expected 500, got %d", recorder.Code)
	}
	if !strings.Contains(recorder.Body.String(), `"internal"`) {
		t.Fatalf("unexpected error body %q", recorder.Body.String())
	}
}

type readFailureReader struct{}

func (readFailureReader) Read([]byte) (int, error) { return 0, errors.New("read failed") }

func TestDecodeOptionalJSONHandlesEmptyMalformedAndUnreadableBodies(t *testing.T) {
	var target struct {
		RemovalOperationID string `json:"removal_operation_id"`
	}
	if err := decodeOptionalJSON(strings.NewReader(" \n"), &target); err != nil {
		t.Fatalf("an empty body must be accepted: %v", err)
	}
	if err := decodeOptionalJSON(strings.NewReader(`{"removal_operation_id":"rem-1"}`), &target); err != nil {
		t.Fatal(err)
	}
	if target.RemovalOperationID != "rem-1" {
		t.Fatalf("unexpected decoded target %+v", target)
	}
	if err := decodeOptionalJSON(readFailureReader{}, &target); err == nil {
		t.Fatal("a read failure must surface")
	}
	if err := decodeOptionalJSON(strings.NewReader("{"), &target); err == nil {
		t.Fatal("malformed JSON must surface")
	}
}

func TestEnforcedAtMillisReportsZeroForAnAbsentTime(t *testing.T) {
	if got := enforcedAtMillis(nil); got != 0 {
		t.Fatalf("absent enforcement = %d", got)
	}
	at := time.Date(2026, 11, 16, 12, 0, 0, 0, time.UTC)
	if got := enforcedAtMillis(&at); got != at.UnixMilli() {
		t.Fatalf("enforcement time = %d, want %d", got, at.UnixMilli())
	}
}

func TestClassifyMapsTheRemainingExternalRefusals(t *testing.T) {
	for name, testCase := range map[string]struct {
		err    error
		status int
		code   whalereinv1.ErrorCode
	}{
		"authority unavailable": {
			err:    &auth.AuthorityDenial{Reason: "rolled back", StatusCode: http.StatusServiceUnavailable},
			status: http.StatusServiceUnavailable,
			code:   whalereinv1.ErrorCode_unavailable,
		},
		"claim busy": {
			err:    pairing.ErrClaimBusy,
			status: http.StatusConflict,
			code:   whalereinv1.ErrorCode_conflict,
		},
		"result too large": {
			err:    requests.ErrResultTooLarge,
			status: http.StatusRequestEntityTooLarge,
			code:   whalereinv1.ErrorCode_payload_too_large,
		},
		"request too large": {
			err:    requests.ErrRequestTooLarge,
			status: http.StatusRequestEntityTooLarge,
			code:   whalereinv1.ErrorCode_payload_too_large,
		},
	} {
		t.Run(name, func(t *testing.T) {
			info := classify(testCase.err)
			if info.status != testCase.status || info.code != testCase.code {
				t.Fatalf("classify = %+v, want status %d code %v", info, testCase.status, testCase.code)
			}
		})
	}
}

func TestExternalDeviceRoutesReportAnExplicitCapabilityError(t *testing.T) {
	server := newAPIFixture(t).server
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/external/sessions/session-1/revocation",
		[]byte(`{"revision":1}`), map[string]string{adminHeaderName: "Bearer admin-token"})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)

	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/external/hosts/host-1/pairings/op-1", nil, nil)
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)

	recorder = doRequest(t, server.PublicHandler(), http.MethodGet,
		"/v1/enrollments/hosts/host-1/clients?workspaceId=ws-1&enrollmentRevision=1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)

	// A body-less session install is rejected before any service call.
	sessionServer := newExternalFixture(t, &deviceSessionExternal{fakeExternal: &fakeExternal{}})
	recorder = doRequest(t, sessionServer.PublicHandler(), http.MethodPost, "/v1/external/sessions/session-1/permission",
		nil, map[string]string{adminHeaderName: "Bearer admin-token"})
	assertError(t, recorder, http.StatusUnsupportedMediaType, whalereinv1.ErrorCode_invalid_argument)
}

func TestHostClientsRouteServesTheConfirmedList(t *testing.T) {
	service := &deviceSessionExternal{fakeExternal: &fakeExternal{}}
	server := newExternalFixture(t, service)
	recorder := doRequest(t, server.PublicHandler(), http.MethodGet,
		"/v1/enrollments/hosts/host-1/clients?workspaceId=ws-1&enrollmentRevision=1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	var list external.HostClientList
	if err := json.Unmarshal(recorder.Body.Bytes(), &list); err != nil {
		t.Fatal(err)
	}
	if list.Presence != "known" {
		t.Fatalf("unexpected client list %+v", list)
	}
}
