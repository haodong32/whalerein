package httpapi

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

type fakeAuth struct {
	adminToken     string
	deviceToken    string
	deviceID       string
	activeHosts    map[string]bool
	list           []auth.HostView
	revoked        []string
	deviceErr      error
	authorizeErr   error
	hostsErr       error
	revokeErr      error
	hostErr        error
	hostGrants     []string
	brokerIdentity auth.BrokerIdentity
	brokerErr      error
	brokerPolicy   policy.Connection
	policyErr      error
}

func (f *fakeAuth) AuthenticateAdmin(token string) error {
	if token == f.adminToken {
		return nil
	}
	return auth.ErrUnauthorized
}

func (f *fakeAuth) AuthenticateDeviceToken(_ context.Context, token string) (auth.Identity, error) {
	if f.deviceErr != nil {
		return auth.Identity{}, f.deviceErr
	}
	if token == f.deviceToken {
		return auth.Identity{DeviceID: f.deviceID}, nil
	}
	return auth.Identity{}, auth.ErrUnauthorized
}

func (f *fakeAuth) AuthorizeHost(_ context.Context, _ auth.Identity, hostID string) error {
	if f.authorizeErr != nil {
		return f.authorizeErr
	}
	if f.activeHosts[hostID] {
		return nil
	}
	return auth.ErrForbidden
}

func (f *fakeAuth) ListHosts(_ context.Context, _ auth.Identity, limit, offset int) ([]auth.HostView, bool, error) {
	if f.hostsErr != nil {
		return nil, false, f.hostsErr
	}
	if offset >= len(f.list) {
		return nil, false, nil
	}
	page := f.list[offset:]
	truncated := len(page) > limit
	if truncated {
		page = page[:limit]
	}
	return page, truncated, nil
}

func (f *fakeAuth) AuthenticateHostRequest(_ context.Context, proof auth.HostProof) error {
	if f.hostErr != nil {
		return f.hostErr
	}
	if proof.HostID == "" || len(proof.Signature) == 0 {
		return auth.ErrUnauthorized
	}
	return nil
}

func (f *fakeAuth) RevokeHostGrant(_ context.Context, hostID, deviceID string) error {
	if f.revokeErr != nil {
		return f.revokeErr
	}
	f.hostGrants = append(f.hostGrants, hostID+"|"+deviceID)
	return nil
}

func (f *fakeAuth) RevokeDevice(_ context.Context, deviceID string) error {
	if f.revokeErr != nil {
		return f.revokeErr
	}
	f.revoked = append(f.revoked, deviceID)
	return nil
}

func (f *fakeAuth) AuthenticateBroker(context.Context, string, string, string) (auth.BrokerIdentity, error) {
	return f.brokerIdentity, f.brokerErr
}

func (f *fakeAuth) BrokerPolicy(context.Context, auth.BrokerIdentity) (policy.Connection, error) {
	return f.brokerPolicy, f.policyErr
}

type fakePairing struct {
	invitation *whalereinv1.PairingInvitation
	claim      *whalereinv1.PairingClaimResponse
	inviteErr  error
	claimErr   error
	invited    string
}

func (f *fakePairing) CreateInvitation(_ context.Context, hostID string) (*whalereinv1.PairingInvitation, error) {
	f.invited = hostID
	return f.invitation, f.inviteErr
}

func (f *fakePairing) Claim(context.Context, *whalereinv1.PairingClaimRequest) (*whalereinv1.PairingClaimResponse, error) {
	return f.claim, f.claimErr
}

type fakeRequests struct {
	row            store.Request
	created        bool
	submitErr      error
	getErr         error
	releaseErr     error
	touchErr       error
	heartbeatErr   error
	heartbeatRow   store.Request
	touchRenewed   bool
	submitted      []requests.SubmitInput
	gotDevice      string
	gotRequest     string
	releaseDevice  string
	releaseSession string
	releaseLimit   int64
	touchDevice    string
	touchRequest   string
}

func (f *fakeRequests) Submit(_ context.Context, input requests.SubmitInput) (store.Request, bool, error) {
	f.submitted = append(f.submitted, input)
	return f.row, f.created, f.submitErr
}

func (f *fakeRequests) Release(_ context.Context, deviceID, sessionID, requestID string, deliveryLimitBytes int64) (store.Request, error) {
	f.gotDevice = deviceID
	f.gotRequest = requestID
	f.releaseDevice = deviceID
	f.releaseSession = sessionID
	f.releaseLimit = deliveryLimitBytes
	if f.releaseErr != nil {
		return store.Request{}, f.releaseErr
	}
	return f.row, f.getErr
}

func (f *fakeRequests) Get(_ context.Context, deviceID, requestID string) (store.Request, error) {
	f.gotDevice = deviceID
	f.gotRequest = requestID
	return f.row, f.getErr
}

func (f *fakeRequests) Heartbeat(_ context.Context, deviceID, sessionID, requestID string) (store.Request, error) {
	f.touchDevice = deviceID
	f.touchRequest = requestID
	if f.heartbeatErr != nil {
		return store.Request{}, f.heartbeatErr
	}
	if f.touchErr != nil {
		return store.Request{}, f.touchErr
	}
	if f.heartbeatRow.RequestID != "" {
		return f.heartbeatRow, nil
	}
	return f.row, nil
}

type apiFixture struct {
	server   *Server
	auth     *fakeAuth
	pairing  *fakePairing
	requests *fakeRequests
	ready    *diagnostics.Readiness
}

func newAPIFixture(t *testing.T) *apiFixture {
	t.Helper()
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	fa := &fakeAuth{
		adminToken: "admin-token", deviceToken: "device-token", deviceID: "d1",
		activeHosts:    map[string]bool{"h1": true},
		list:           []auth.HostView{{HostID: "h1", Fingerprint: "fp", ProtocolVersion: 1, Online: true}},
		brokerIdentity: auth.BrokerIdentity{Kind: auth.KindHost, HostID: "h1"},
		brokerPolicy:   policy.Build(policy.KindHost, "h1", "", nil),
	}
	fp := &fakePairing{
		invitation: &whalereinv1.PairingInvitation{Token: "pair-token", HostId: "h1", ExpiresAtUnixMs: now.Add(time.Minute).UnixMilli(), PairingUri: "whalerein://pair?v=1"},
		claim:      &whalereinv1.PairingClaimResponse{ClaimId: "c1", HostId: "h1", DeviceId: "d1", DeviceToken: "device-token"},
	}
	fr := &fakeRequests{row: store.Request{
		RequestID: "r1", HostID: "h1", DeviceID: "d1", CommandID: "c1", PayloadDigest: []byte("digest"),
		Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND, ProtocolVersion: 1, Status: whalereinv1.OnlineStatus_received,
		ResultBody: []byte("result"), ChannelID: "channel-1", ExpiresAt: now.Add(time.Hour), UpdatedAt: now,
	}, created: true}
	readiness := diagnostics.NewReadiness()
	readiness.Add("database", func(context.Context) error { return nil })
	server, err := New(Dependencies{
		Auth: fa, Pairing: fp, Requests: fr, Readiness: readiness,
		Metrics: diagnostics.NewMetrics(), Masker: diagnostics.NewMaskerWithKey([]byte("http")),
		Logger:        slog.New(slog.NewTextHandler(io.Discard, nil)),
		InternalToken: "internal-token", MaxBodyBytes: 1 << 20,
	})
	if err != nil {
		t.Fatal(err)
	}
	return &apiFixture{server: server, auth: fa, pairing: fp, requests: fr, ready: readiness}
}

func doRequest(t *testing.T, handler http.Handler, method, path string, body []byte, headers map[string]string) *httptest.ResponseRecorder {
	t.Helper()
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	req := httptest.NewRequest(method, path, reader)
	if body != nil {
		// JSON bodies are the wire contract; a case that exercises a missing or
		// wrong media type overrides this header explicitly.
		req.Header.Set("Content-Type", "application/json")
	}
	for key, value := range headers {
		req.Header.Set(key, value)
	}
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, req)
	return recorder
}

func TestPublicInvitationAuthenticatesAndReturnsInvitation(t *testing.T) {
	f := newAPIFixture(t)
	body := []byte(`{"hostId":"h1"}`)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", body, map[string]string{"Authorization": "Bearer admin-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var invitation whalereinv1.PairingInvitation
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &invitation); err != nil {
		t.Fatal(err)
	}
	if invitation.GetToken() != "pair-token" || f.pairing.invited != "h1" {
		t.Fatalf("invitation = %+v", &invitation)
	}
	if recorder.Header().Get(TraceHeader) == "" {
		t.Fatal("trace header missing")
	}

	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", body, nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)

	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", []byte("bad"), map[string]string{"Authorization": "Bearer admin-token"})
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)

	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", []byte(`{"hostId":""}`), map[string]string{"Authorization": "Bearer admin-token"})
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)

	for name, tc := range map[string]struct {
		err    error
		status int
		code   whalereinv1.ErrorCode
	}{
		"returns not found when invitation host is missing":    {pairing.ErrHostNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found},
		"returns internal error when invitation storage fails": {errors.New("db down"), http.StatusInternalServerError, whalereinv1.ErrorCode_internal},
	} {
		t.Run(name, func(t *testing.T) {
			f := newAPIFixture(t)
			f.pairing.inviteErr = tc.err
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", body, map[string]string{"Authorization": "Bearer admin-token"})
			assertError(t, recorder, tc.status, tc.code)
		})
	}
}

func TestPublicClaimRoutesPairingPhasesAndErrors(t *testing.T) {
	f := newAPIFixture(t)
	body := []byte(`{"token":"tok","claimId":"c1","phase":"PAIRING_PHASE_HELLO","noiseMessage":"bXNn","deviceId":"d1","protocolVersion":1}`)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/claims", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var response whalereinv1.PairingClaimResponse
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response.GetClaimId() != "c1" || response.GetDeviceToken() != "device-token" {
		t.Fatalf("response = %+v", &response)
	}
	for name, tc := range map[string]struct {
		err    error
		status int
		code   whalereinv1.ErrorCode
	}{
		"maps used invitation to conflict response":                           {pairing.ErrTokenUsed, http.StatusConflict, whalereinv1.ErrorCode_conflict},
		"maps expired invitation to gone response":                            {pairing.ErrTokenExpired, http.StatusGone, whalereinv1.ErrorCode_expired},
		"maps exhausted invitation failures to rate limited response":         {pairing.ErrFailureBudget, http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited},
		"maps offline host claim to unavailable response":                     {pairing.ErrHostOffline, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		"maps host transport failure to unavailable response":                 {pairing.ErrHostUnavailable, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		"maps host claim rejection to forbidden response":                     {pairing.ErrHostRejected, http.StatusForbidden, whalereinv1.ErrorCode_forbidden},
		"maps missing prior HELLO to conflict response":                       {pairing.ErrClaimRequired, http.StatusConflict, whalereinv1.ErrorCode_conflict},
		"maps unsupported claim version to protocol error response":           {pairing.ErrProtocolVersion, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible},
		"maps committed unconfirmed pairing to an explicit terminal response": {pairing.ErrPairingCommittedUnconfirmed, http.StatusConflict, whalereinv1.ErrorCode_pairing_committed_unconfirmed},
		"maps completed unavailable pairing to an explicit terminal response": {pairing.ErrPairingCommittedUnavailable, http.StatusConflict, whalereinv1.ErrorCode_pairing_committed_unavailable},
		"maps fenced pairing to an explicit terminal response":                {pairing.ErrPairingFenced, http.StatusConflict, whalereinv1.ErrorCode_pairing_fenced},
		"maps invalid claim to bad request response":                          {pairing.ErrInvalidClaim, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument},
		"maps claim service failure to internal error response":               {pairing.ErrInternal, http.StatusInternalServerError, whalereinv1.ErrorCode_internal},
	} {
		t.Run(name, func(t *testing.T) {
			f := newAPIFixture(t)
			f.pairing.claimErr = tc.err
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/claims", body, nil)
			assertError(t, recorder, tc.status, tc.code)
		})
	}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/claims", []byte("bad"), nil)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
}

func TestHostsRouteReturnsAuthorizedHostsAndPresence(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var list whalereinv1.HostList
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &list); err != nil {
		t.Fatal(err)
	}
	if len(list.GetHosts()) != 1 || !list.GetHosts()[0].GetOnline() || list.GetHosts()[0].GetHostId() != "h1" {
		t.Fatalf("list = %+v", &list)
	}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts", nil, nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	f.auth.hostsErr = errors.New("db down")
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusInternalServerError, whalereinv1.ErrorCode_internal)
}

func submitBody() []byte {
	return []byte(`{"requestId":"r1","kind":"REQUEST_KIND_COMMAND","commandId":"c1","payloadDigest":"ZGlnZXN0","expiresAtUnixMs":4102444800000,"protocolVersion":1,"body":"Ym9keQ=="}`)
}

func TestSubmitRouteValidatesAndReturnsRequestReceipt(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", submitBody(), map[string]string{
		"Authorization": "Bearer device-token", TraceHeader: "trace-1",
	})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var receipt whalereinv1.RequestReceipt
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &receipt); err != nil {
		t.Fatal(err)
	}
	if receipt.GetRequestId() != "r1" || receipt.GetStatus() != whalereinv1.OnlineStatus_received || receipt.GetCommandId() != "c1" {
		t.Fatalf("receipt = %+v", &receipt)
	}
	if len(f.requests.submitted) != 1 {
		t.Fatalf("submitted = %d", len(f.requests.submitted))
	}
	submitted := f.requests.submitted[0]
	if submitted.HostID != "h1" || submitted.DeviceID != "d1" || submitted.Request.GetHostId() != "h1" ||
		submitted.Request.GetDeviceId() != "d1" || submitted.Request.GetTraceId() != "trace-1" {
		t.Fatalf("submitted = %+v", submitted)
	}

	// Host spoofing is rejected.
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", []byte(`{"requestId":"r1","hostId":"h2"}`), map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", []byte(`{"requestId":"r1","deviceId":"d2"}`), map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusForbidden, whalereinv1.ErrorCode_forbidden)

	f.auth.activeHosts["h1"] = false
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", submitBody(), map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusForbidden, whalereinv1.ErrorCode_forbidden)

	f = newAPIFixture(t)
	f.requests.submitErr = requests.ErrDigestMismatch
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", submitBody(), map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusConflict, whalereinv1.ErrorCode_payload_digest_mismatch)

	f = newAPIFixture(t)
	f.requests.row.Status = whalereinv1.OnlineStatus_host_offline
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", submitBody(), map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("offline status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var offline whalereinv1.RequestReceipt
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &offline); err != nil {
		t.Fatal(err)
	}
	if offline.GetStatus() != whalereinv1.OnlineStatus_host_offline {
		t.Fatalf("offline receipt = %+v", &offline)
	}

	f = newAPIFixture(t)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", []byte("bad"), map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
	f.auth.authorizeErr = errors.New("db down")
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/requests", submitBody(), map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusInternalServerError, whalereinv1.ErrorCode_internal)
}

func TestGetRequestRouteReturnsAuthorizedRequest(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var state whalereinv1.RequestState
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &state); err != nil {
		t.Fatal(err)
	}
	if state.GetRequestId() != "r1" || string(state.GetResultBody()) != "result" || state.GetChannelId() != "channel-1" {
		t.Fatalf("state = %+v", &state)
	}
	if f.requests.gotDevice != "d1" || f.requests.gotRequest != "r1" {
		t.Fatalf("get args = %q %q", f.requests.gotDevice, f.requests.gotRequest)
	}
	if f.requests.touchRequest != "" {
		t.Fatalf("get renewed %q instead of leaving liveness to heartbeat", f.requests.touchRequest)
	}
	if f.requests.releaseDevice != "d1" || f.requests.releaseLimit != requests.ResultValueLimit(1<<20) {
		t.Fatalf("release args = device %q limit %d", f.requests.releaseDevice, f.requests.releaseLimit)
	}
	// Read-only recovery: a failed heartbeat bookkeeping path cannot affect a
	// status read, and a read never reopens an expired lease.
	f.requests.touchErr = errors.New("lease update failed")
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("read-only recovery status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if f.requests.touchRequest != "" {
		t.Fatal("failed read path renewed the lease")
	}
	f.requests.touchErr = nil
	f.requests.getErr = requests.ErrNotFound
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
	f.requests.getErr = requests.ErrExpired
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusGone, whalereinv1.ErrorCode_expired)
}

func TestGetRequestDeniesFencedCiphertextAndKeepsMetadata(t *testing.T) {
	f := newAPIFixture(t)
	f.requests.releaseErr = requests.ErrReleaseFenced
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusForbidden, whalereinv1.ErrorCode_forbidden)
	if f.requests.touchRequest != "" {
		t.Fatal("a fenced read renewed the client lease")
	}

	// COMMAND recovery is metadata-only: the body is withheld without turning
	// the recorded outcome into an error.
	f = newAPIFixture(t)
	f.requests.row.Kind = whalereinv1.RequestKind_REQUEST_KIND_COMMAND
	f.requests.row.ResultBody = nil
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("command metadata status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var state whalereinv1.RequestState
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &state); err != nil {
		t.Fatal(err)
	}
	if state.GetRequestId() != "r1" || state.GetCommandId() != "c1" || len(state.GetResultBody()) != 0 {
		t.Fatalf("command metadata = %+v", &state)
	}
}

func TestDeleteDeviceRouteRevokesAuthorizedDevice(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodDelete, "/v1/devices/d1", nil, map[string]string{"Authorization": "Bearer admin-token"})
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("admin status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if len(f.auth.revoked) != 1 || f.auth.revoked[0] != "d1" {
		t.Fatalf("revoked = %v", f.auth.revoked)
	}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodDelete, "/v1/devices/d1", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("device status = %d", recorder.Code)
	}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodDelete, "/v1/devices/d2", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusForbidden, whalereinv1.ErrorCode_forbidden)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodDelete, "/v1/devices/d1", nil, nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	f.auth.revokeErr = auth.ErrNotFound
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodDelete, "/v1/devices/d1", nil, map[string]string{"Authorization": "Bearer admin-token"})
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
}

func TestDiagnosticsRoutesReportHealthReadinessAndMetrics(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/healthz", nil, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("health = %d", recorder.Code)
	}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/readyz", nil, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("ready = %d body=%s", recorder.Code, recorder.Body.String())
	}
	const authorityHeader = "WhaleRein-Session-Authority-Revision"
	f.server.deps.SessionAuthorityRevision = "42"
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/readyz", nil, nil)
	if got := recorder.Header().Get(authorityHeader); got != "" {
		t.Fatalf("unmanaged readiness advertised authority: %q", got)
	}
	f.server.deps.RequireDeviceSession = true
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/readyz", nil, nil)
	if got := recorder.Header().Get(authorityHeader); got != "42" {
		t.Fatalf("managed readiness authority: %q", got)
	}
	f.ready.Add("broker", func(context.Context) error { return errors.New("down") })
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/readyz", nil, nil)
	if recorder.Code != http.StatusServiceUnavailable {
		t.Fatalf("not ready = %d", recorder.Code)
	}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/metrics", nil, nil)
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), "http_request_duration_ms") {
		t.Fatalf("metrics = %d", recorder.Code)
	}
}

func TestRouterReturnsNotFoundAndMethodErrors(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/missing", nil, nil)
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/pairing/claims", nil, nil)
	assertError(t, recorder, http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/requests", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument)
}

func TestBodyLimitRejectsOversizedRequestsAndPreservesTraceID(t *testing.T) {
	f := newAPIFixture(t)
	f.server.deps.MaxBodyBytes = 10
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/claims", bytes.Repeat([]byte("x"), 100), nil)
	assertError(t, recorder, http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/healthz", nil, map[string]string{TraceHeader: "client-trace"})
	if recorder.Header().Get(TraceHeader) != "client-trace" {
		t.Fatalf("trace = %q", recorder.Header().Get(TraceHeader))
	}
}

func TestInternalBrokerConnectReturnsPolicyTextAndDeniesBadCredentials(t *testing.T) {
	f := newAPIFixture(t)
	form := func(values url.Values) []byte { return []byte(values.Encode()) }
	valid := form(url.Values{"clientid": {"whalerein-host-h1"}, "username": {"host:h1"}, "password": {"host-token"}})

	// The internal token is mandatory.
	recorder := doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", valid, nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	recorder = doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", valid, map[string]string{InternalHeader: "wrong"})
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)

	f.auth.brokerPolicy = policy.Build(policy.KindDevice, "", "d1", []store.AdmissionGrant{
		{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 3},
	})
	f.auth.brokerIdentity = auth.BrokerIdentity{Kind: auth.KindDevice, DeviceID: "d1"}
	device := form(url.Values{
		"clientid": {"whalerein-device-d1"}, "username": {"device:d1"}, "password": {"device-token"},
		"will": {"whalerein/v1/presence/device/d1"},
	})
	recorder = doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", device, map[string]string{InternalHeader: "internal-token"})
	if recorder.Code != http.StatusOK || recorder.Header().Get("Content-Type") != "text/plain; charset=utf-8" {
		t.Fatalf("connect = %d %q", recorder.Code, recorder.Header().Get("Content-Type"))
	}
	want := string(policy.Format(f.auth.brokerPolicy))
	if recorder.Body.String() != want {
		t.Fatalf("policy body = %q, want %q", recorder.Body.String(), want)
	}
	if !strings.Contains(recorder.Body.String(), "kind device") || !strings.Contains(recorder.Body.String(), "will whalerein/v1/presence/device/d1") {
		t.Fatalf("policy body = %q", recorder.Body.String())
	}

	// Bad credentials produce a versioned deny body, not an HTTP error.
	f.auth.brokerErr = auth.ErrUnauthorized
	recorder = doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", valid, map[string]string{InternalHeader: "internal-token"})
	if recorder.Code != http.StatusOK || !strings.HasPrefix(recorder.Body.String(), "whalerein/1\ndeny ") {
		t.Fatalf("deny = %d %q", recorder.Code, recorder.Body.String())
	}
	f.auth.brokerErr = nil

	// A Will outside the projected policy is denied even after authentication.
	foreign := form(url.Values{
		"clientid": {"whalerein-device-d1"}, "username": {"device:d1"}, "password": {"device-token"},
		"will": {"whalerein/v1/presence/device/other"},
	})
	recorder = doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", foreign, map[string]string{InternalHeader: "internal-token"})
	if !strings.Contains(recorder.Body.String(), "deny will_not_authorized") {
		t.Fatalf("foreign will = %q", recorder.Body.String())
	}

	// Policy projection failures also fail closed.
	f.auth.policyErr = errors.New("db down")
	recorder = doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", device, map[string]string{InternalHeader: "internal-token"})
	if !strings.Contains(recorder.Body.String(), "deny unavailable") {
		t.Fatalf("policy failure = %q", recorder.Body.String())
	}
	f.auth.policyErr = nil

	// Malformed form bodies are denied.
	recorder = doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", []byte("clientid=%zz"), map[string]string{InternalHeader: "internal-token"})
	if !strings.Contains(recorder.Body.String(), "deny ") {
		t.Fatalf("malformed = %q", recorder.Body.String())
	}
}

func TestInternalBrokerConnectEnforcesBodyBound(t *testing.T) {
	f := newAPIFixture(t)
	oversized := make([]byte, 64<<10+1)
	for index := range oversized {
		oversized[index] = 'a'
	}
	recorder := doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/connect", oversized, map[string]string{
		InternalHeader: "internal-token",
		"Content-Type": "application/x-www-form-urlencoded",
	})
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), "deny malformed_request") {
		t.Fatalf("oversized = %d %q", recorder.Code, recorder.Body.String())
	}
}

func TestInternalPresenceRouteWasRemoved(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.InternalHandler(), http.MethodPost, "/internal/broker/presence", []byte(`{"event":"client.connected"}`), map[string]string{InternalHeader: "internal-token"})
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
}

func TestClassifyErrorsMapsProtocolAndServiceFailures(t *testing.T) {
	cases := []struct {
		name   string
		err    error
		status int
		code   whalereinv1.ErrorCode
	}{
		{"nil_maps_OK", nil, http.StatusOK, whalereinv1.ErrorCode_error_code_unspecified},
		{"errRouteNotFound_maps_NotFound", errRouteNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found},
		{"errMethod_maps_MethodNotAllowed", errMethod, http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument},
		{"errBodyTooLarge_maps_RequestEntityTooLarge", errBodyTooLarge, http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large},
		{"errInvalidBody_maps_BadRequest", errInvalidBody, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument},
		{"errMissingAuth_maps_Unauthorized", errMissingAuth, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized},
		{"auth_ErrUnauthorized_maps_Unauthorized", auth.ErrUnauthorized, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized},
		{"pairing_ErrUnauthorized_maps_Unauthorized", pairing.ErrUnauthorized, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized},
		{"errInternalToken_maps_Unauthorized", errInternalToken, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized},
		{"auth_ErrForbidden_maps_Forbidden", auth.ErrForbidden, http.StatusForbidden, whalereinv1.ErrorCode_forbidden},
		{"pairing_ErrHostRejected_maps_Forbidden", pairing.ErrHostRejected, http.StatusForbidden, whalereinv1.ErrorCode_forbidden},
		{"auth_ErrBadRequest_maps_BadRequest", auth.ErrBadRequest, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument},
		{"pairing_ErrInvalidClaim_maps_BadRequest", pairing.ErrInvalidClaim, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument},
		{"requests_ErrInvalidRequest_maps_BadRequest", requests.ErrInvalidRequest, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument},
		{"auth_ErrNotFound_maps_NotFound", auth.ErrNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found},
		{"pairing_ErrHostNotFound_maps_NotFound", pairing.ErrHostNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found},
		{"requests_ErrNotFound_maps_NotFound", requests.ErrNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found},
		{"store_ErrNotFound_maps_NotFound", store.ErrNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found},
		{"pairing_ErrTokenUsed_maps_Conflict", pairing.ErrTokenUsed, http.StatusConflict, whalereinv1.ErrorCode_conflict},
		{"pairing_ErrTokenExpired_maps_Gone", pairing.ErrTokenExpired, http.StatusGone, whalereinv1.ErrorCode_expired},
		{"pairing_ErrFailureBudget_maps_TooManyRequests", pairing.ErrFailureBudget, http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited},
		{"pairing_ErrHostOffline_maps_ServiceUnavailable", pairing.ErrHostOffline, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		{"pairing_ErrHostUnavailable_maps_ServiceUnavailable", pairing.ErrHostUnavailable, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		{"auth_ErrUnavailable_maps_ServiceUnavailable", auth.ErrUnavailable, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		{"requests_ErrUnavailable_maps_ServiceUnavailable", requests.ErrUnavailable, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		{"pairing_ErrClaimRequired_maps_Conflict", pairing.ErrClaimRequired, http.StatusConflict, whalereinv1.ErrorCode_conflict},
		{"pairing_ErrProtocolVersion_maps_BadRequest", pairing.ErrProtocolVersion, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible},
		{"requests_ErrProtocolVersion_maps_BadRequest", requests.ErrProtocolVersion, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible},
		{"requests_ErrDigestMismatch_maps_Conflict", requests.ErrDigestMismatch, http.StatusConflict, whalereinv1.ErrorCode_payload_digest_mismatch},
		{"requests_ErrConflict_maps_Conflict", requests.ErrConflict, http.StatusConflict, whalereinv1.ErrorCode_conflict},
		{"store_ErrConflict_maps_Conflict", store.ErrConflict, http.StatusConflict, whalereinv1.ErrorCode_conflict},
		{"requests_ErrExpired_maps_Gone", requests.ErrExpired, http.StatusGone, whalereinv1.ErrorCode_expired},
		{"requests_ErrForbidden_maps_Forbidden", requests.ErrForbidden, http.StatusForbidden, whalereinv1.ErrorCode_forbidden},
		{"requests_ErrResultMismatch_maps_Forbidden", requests.ErrResultMismatch, http.StatusForbidden, whalereinv1.ErrorCode_forbidden},
		{"requests_ErrInvalidResult_maps_Forbidden", requests.ErrInvalidResult, http.StatusForbidden, whalereinv1.ErrorCode_forbidden},
		{"context_Canceled_maps_ServiceUnavailable", context.Canceled, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		{"context_DeadlineExceeded_maps_ServiceUnavailable", context.DeadlineExceeded, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable},
		{"pairing_ErrInternal_maps_InternalServerError", pairing.ErrInternal, http.StatusInternalServerError, whalereinv1.ErrorCode_internal},
		{"errors_New_other_maps_InternalServerError", errors.New("other"), http.StatusInternalServerError, whalereinv1.ErrorCode_internal},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := classify(tc.err)
			if got.status != tc.status || got.code != tc.code {
				t.Fatalf("HTTP/wire classification mismatch: status=%d code=%s", got.status, got.code)
			}
		})
	}
}

func TestBearerTokenParsesAndRejectsMalformedHeaders(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	if _, err := bearerToken(req); err == nil {
		t.Fatal("missing header")
	}
	req.Header.Set("Authorization", "Basic abc")
	if _, err := bearerToken(req); err == nil {
		t.Fatal("wrong scheme")
	}
	req.Header.Set("Authorization", "Bearer ")
	if _, err := bearerToken(req); err == nil {
		t.Fatal("empty token")
	}
	req.Header.Set("Authorization", "bearer token")
	token, err := bearerToken(req)
	if err != nil || token != "token" {
		t.Fatalf("token = %q err=%v", token, err)
	}
}

func TestStatusWriterAndInternalTokenEnforceCallbackContract(t *testing.T) {
	recorder := httptest.NewRecorder()
	writer := &statusWriter{ResponseWriter: recorder}
	if writer.Status() != http.StatusOK {
		t.Fatal("default status")
	}
	if _, err := writer.Write([]byte("body")); err != nil {
		t.Fatal(err)
	}
	if writer.Status() != http.StatusOK || recorder.Body.String() != "body" {
		t.Fatal("write")
	}
	fresh := &statusWriter{ResponseWriter: httptest.NewRecorder()}
	fresh.WriteHeader(http.StatusNoContent)
	fresh.WriteHeader(http.StatusTeapot)
	if fresh.Status() != http.StatusNoContent {
		t.Fatalf("status = %d", fresh.Status())
	}
	writer.WriteHeader(http.StatusNoContent)
	if writer.Status() != http.StatusOK {
		t.Fatalf("late header changed status: %d", writer.Status())
	}
	writer.Flush()
	if _, ok := writer.Unwrap().(*httptest.ResponseRecorder); !ok {
		t.Fatal("unwrap")
	}
	f := newAPIFixture(t)
	if f.server.internalTokenOK("") || f.server.internalTokenOK("wrong") || !f.server.internalTokenOK("internal-token") {
		t.Fatal("internal token check")
	}
	empty := &Server{deps: Dependencies{InternalToken: ""}}
	if empty.internalTokenOK("x") {
		t.Fatal("empty configured token")
	}
}

func assertError(t *testing.T, recorder *httptest.ResponseRecorder, status int, code whalereinv1.ErrorCode) {
	t.Helper()
	if recorder.Code != status {
		t.Fatalf("status = %d, want %d, body=%s", recorder.Code, status, recorder.Body.String())
	}
	var response whalereinv1.ErrorResponse
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatalf("decode error response: %v body=%s", err, recorder.Body.String())
	}
	if response.GetCode() != code {
		t.Fatalf("code = %v, want %v body=%s", response.GetCode(), code, recorder.Body.String())
	}
	if recorder.Header().Get(TraceHeader) == "" {
		t.Fatal("error response missing trace header")
	}
}

func TestWriteProtoReturnsErrorWhenMarshalFails(t *testing.T) {
	f := newAPIFixture(t)
	recorder := httptest.NewRecorder()
	f.server.writeProto(recorder, http.StatusOK, &whalereinv1.ErrorResponse{Message: string([]byte{0xff})})
	if recorder.Code != http.StatusInternalServerError {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if got := codeName(whalereinv1.ErrorCode(999)); got != "internal" {
		t.Fatalf("code name = %q", got)
	}
}

func TestNewRejectsInvalidDependenciesAndAppliesDefaults(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected missing dependencies error")
	}
	readiness := diagnostics.NewReadiness()
	authFake := &fakeAuth{adminToken: "a", deviceToken: "d", deviceID: "d1", activeHosts: map[string]bool{}}
	if _, err := New(Dependencies{Auth: authFake, Pairing: &fakePairing{}, Requests: &fakeRequests{}}); err == nil {
		t.Fatal("expected missing readiness error")
	}
	server, err := New(Dependencies{
		Auth: authFake, Pairing: &fakePairing{}, Requests: &fakeRequests{},
		Readiness: readiness, InternalToken: "x",
	})
	if err != nil {
		t.Fatal(err)
	}
	if server.deps.Metrics == nil || server.deps.Masker == nil || server.deps.Logger == nil || server.deps.MaxBodyBytes <= 0 {
		t.Fatalf("defaults not applied: %+v", server.deps)
	}
}

func TestRouterMatchesRoutesAndMethodFallbacks(t *testing.T) {
	handler := func(http.ResponseWriter, *http.Request, map[string]string) {}
	router := newRouter([]routeSpec{{method: http.MethodGet, pattern: "/", handler: handler}})
	if match, path, method := router.match(http.MethodGet, "/"); !path || !method || match.route.pattern != "/" {
		t.Fatalf("root match = %+v %v %v", match, path, method)
	}
	if _, path, method := router.match(http.MethodPost, "/"); !path || method {
		t.Fatal("root method mismatch")
	}
	if _, path, _ := router.match(http.MethodGet, "/a/"); path {
		t.Fatal("trailing slash should not match")
	}
	if _, path, _ := router.match(http.MethodGet, "noslash"); path {
		t.Fatal("missing slash should not match")
	}
	if parts := compilePattern("/"); len(parts) != 0 {
		t.Fatalf("root pattern parts = %v", parts)
	}
	recorder := httptest.NewRecorder()
	router.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/", nil))
	if recorder.Code != http.StatusOK {
		t.Fatalf("root serve = %d", recorder.Code)
	}
	// Without a configured method fallback, a method mismatch reaches the standard 404 fallback.
	recorder = httptest.NewRecorder()
	router.ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/", nil))
	if recorder.Code != http.StatusNotFound {
		t.Fatalf("method fallback = %d", recorder.Code)
	}
	// No route and no not-found fallback uses the standard 404.
	recorder = httptest.NewRecorder()
	router.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/missing", nil))
	if recorder.Code != http.StatusNotFound {
		t.Fatalf("not found fallback = %d", recorder.Code)
	}
}

func TestObserveReturnsSanitizedInternalErrorWithTraceAfterPanic(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.observe(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		panic("private fixture panic detail")
	}))
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/panic", nil))
	assertError(t, recorder, http.StatusInternalServerError, whalereinv1.ErrorCode_internal)
	if recorder.Header().Get(TraceHeader) == "" || strings.Contains(recorder.Body.String(), "private fixture panic detail") {
		t.Fatal("panic recovery lost correlation or exposed private detail")
	}
	healthy := httptest.NewRecorder()
	f.server.PublicHandler().ServeHTTP(healthy, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if healthy.Code != http.StatusOK {
		t.Fatal("server did not remain usable after panic recovery")
	}
}

type failingReader struct{}

func (failingReader) Read([]byte) (int, error) { return 0, errors.New("read failed") }

func TestDecodeProtoRejectsReadErrorsAndEmptyBody(t *testing.T) {
	f := newAPIFixture(t)
	request := httptest.NewRequest(http.MethodPost, "/v1/pairing/claims", failingReader{})
	if err := f.server.decodeProto(httptest.NewRecorder(), request, &whalereinv1.PairingClaimRequest{}); err == nil {
		t.Fatal("read error")
	}
	request = httptest.NewRequest(http.MethodPost, "/v1/pairing/claims", bytes.NewReader(nil))
	if err := f.server.decodeProto(httptest.NewRecorder(), request, &whalereinv1.PairingClaimRequest{}); err == nil {
		t.Fatal("empty body")
	}
}

func TestTraceIDRejectsOverlongHeader(t *testing.T) {
	request := httptest.NewRequest(http.MethodGet, "/", nil)
	request.Header.Set(TraceHeader, strings.Repeat("x", 200))
	if got := traceID(request); got == strings.Repeat("x", 200) || len(got) != 32 {
		t.Fatalf("trace = %q", got)
	}
	request.Header.Set(TraceHeader, "short")
	if got := traceID(request); got != "short" {
		t.Fatalf("trace = %q", got)
	}
}

func TestRoutesRejectInvalidAuthAndOversizedBodies(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodDelete, "/v1/devices/d1", nil, map[string]string{"Authorization": "Bearer wrong"})
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	f.server.deps.MaxBodyBytes = 5
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/claims", []byte(`{"claimId":"claim-1"}`), nil)
	assertError(t, recorder, http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large)
}

func TestHostSignatureAuthorizesInvitationAndSingleGrantRevocation(t *testing.T) {
	f := newAPIFixture(t)
	body := []byte(`{"hostId":"h1","signedAtUnixMs":"1700000000000","signature":"AQ=="}`)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("host invitation status %d body %s", recorder.Code, recorder.Body.Bytes())
	}
	if f.pairing.invited != "h1" {
		t.Fatalf("invited %q", f.pairing.invited)
	}
	admin := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", []byte(`{"hostId":"h2"}`), map[string]string{"Authorization": "Bearer admin-token"})
	if admin.Code != http.StatusOK || f.pairing.invited != "h2" {
		t.Fatalf("admin invitation status %d host %q", admin.Code, f.pairing.invited)
	}
	f.pairing.invited = ""
	wrong := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", body, map[string]string{"Authorization": "Bearer wrong"})
	assertError(t, wrong, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	if f.pairing.invited != "" {
		t.Fatalf("invalid bearer fell through, invited %q", f.pairing.invited)
	}
	f.auth.hostErr = auth.ErrSignatureExpired
	expired := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/pairing/invitations", body, nil)
	assertError(t, expired, http.StatusGone, whalereinv1.ErrorCode_expired)
	f.auth.hostErr = nil

	revokeBody := []byte(`{"signedAtUnixMs":"1700000000001","signature":"AQ=="}`)
	revoked := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/devices/d1/revocation", revokeBody, nil)
	if revoked.Code != http.StatusNoContent {
		t.Fatalf("revoke status %d", revoked.Code)
	}
	if len(f.auth.hostGrants) != 1 || f.auth.hostGrants[0] != "h1|d1" {
		t.Fatalf("grants %v", f.auth.hostGrants)
	}
	bad := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/hosts/h1/devices/bad+id/revocation", revokeBody, nil)
	assertError(t, bad, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
}

func TestHeartbeatRouteTouchesTheAuthorizedRequest(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/requests/r1/heartbeat", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if f.requests.gotDevice != "d1" || f.requests.gotRequest != "r1" {
		t.Fatalf("heartbeat read device=%q request=%q", f.requests.gotDevice, f.requests.gotRequest)
	}
	if f.requests.touchDevice != "d1" || f.requests.touchRequest != "r1" {
		t.Fatalf("heartbeat renewed %q %q instead of the exact request", f.requests.touchDevice, f.requests.touchRequest)
	}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/requests/r1/heartbeat", nil, nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	f.requests.touchErr = errors.New("lease update failed")
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/requests/r1/heartbeat", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusInternalServerError, whalereinv1.ErrorCode_internal)
	f.requests.touchErr = nil
	f.requests.getErr = errors.New("db down")
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/requests/r1/heartbeat", nil, map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusInternalServerError, whalereinv1.ErrorCode_internal)
}

// TestSessionObservationAC26HeartbeatRenewsOnlyTheAddressedRequest proves the
// HTTP heartbeat forwards the path identity, never a device-wide renewal.
func TestSessionObservationAC26HeartbeatRenewsOnlyTheAddressedRequest(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/requests/r1/heartbeat", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("heartbeat status = %d", recorder.Code)
	}
	if f.requests.touchDevice != "d1" || f.requests.touchRequest != "r1" {
		t.Fatalf("heartbeat renewed %q %q", f.requests.touchDevice, f.requests.touchRequest)
	}
}

// TestSessionObservationAC27ReadRecoversOutcomeWithoutRenewal proves renewal
// failure is bounded I/O: the recorded outcome is still returned and no lease
// is reopened by the read path.
func TestSessionObservationAC27ReadRecoversOutcomeWithoutRenewal(t *testing.T) {
	f := newAPIFixture(t)
	f.requests.touchErr = errors.New("lease update failed")
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("read status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var state whalereinv1.RequestState
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &state); err != nil {
		t.Fatal(err)
	}
	if state.GetRequestId() != "r1" || string(state.GetResultBody()) != "result" {
		t.Fatalf("recorded outcome lost: %+v", &state)
	}
}
