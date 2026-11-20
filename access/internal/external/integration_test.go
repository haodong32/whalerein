//go:build integration

package external_test

// End-to-end acceptance for external account mode.
//
// The test starts the independent account-provider fixture as a real HTTP
// service, signs an authorization with the fixture's issuer key, and drives
// stock Access over HTTP. Nothing here imports Access from the fixture: the
// two sides meet only through the public contract.

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/httpapi"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

const (
	issuerID   = "fixture://issuer/a"
	serviceURL = "https://access.fixture.test"
	regionID   = "fixture-a"
	adminToken = "integration-admin"
)

// issuerKey is one fixture issuer key with its published window.
type issuerKey struct {
	id     string
	public ed25519.PublicKey
	sign   ed25519.PrivateKey
	from   time.Time
	to     time.Time
}

func newIssuerKey(t *testing.T) issuerKey {
	t.Helper()
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	return issuerKey{
		id:     "issuer-1",
		public: publicKey,
		sign:   privateKey,
		from:   time.Unix(0, 0),
		to:     time.Unix(0, 0).Add(876000 * time.Hour),
	}
}

// provider is the independent fixture: it issues authorizations and never
// shares a package with Access.
type provider struct {
	key    issuerKey
	server *httptest.Server
}

func newProvider(t *testing.T) *provider {
	t.Helper()
	p := &provider{key: newIssuerKey(t)}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	})
	p.server = httptest.NewServer(mux)
	t.Cleanup(p.server.Close)
	return p
}

// issue mints one portable authorization over the canonical text form.
func (p *provider) issue(t *testing.T, auth *whalereinv1.ExternalAuthorization) *whalereinv1.ExternalAuthorization {
	t.Helper()
	auth.Issuer = issuerID
	auth.Audience = serviceURL
	auth.KeyId = p.key.id
	auth.Signature = ed25519.Sign(p.key.sign, []byte(external.CanonicalInput(auth)))
	return auth
}

// harness is stock Access in external mode, over a real database.
type harness struct {
	server   *httptest.Server
	provider *provider
	// prepared keeps one signed request per Host so a retry replays the exact
	// payload instead of generating new keys.
	prepared    map[string]*whalereinv1.HostEnrollmentRequest
	signingKeys map[string]ed25519.PrivateKey
}

func newHarness(t *testing.T) harness {
	t.Helper()
	data, _ := storetest.New(t)
	issuer := newIssuerKey(t)
	provider := newProvider(t)
	// Access pins the fixture's public key and verifies independently.
	provider.key = issuer

	trust := external.Trust{
		Issuer:          issuerID,
		ServiceIdentity: serviceURL,
		Region:          regionID,
		Keys: []external.VerificationKey{{
			ID:        issuer.id,
			PublicKey: issuer.public,
			NotBefore: issuer.from,
			NotAfter:  issuer.to,
		}},
	}
	service := external.New(trust, data, "mqtts://access.fixture.test:8883",
		external.WithHostEnforcer(acceptEnforcer{}))

	api, err := httpapi.New(httpapi.Dependencies{
		Auth:          &acceptAdmin{},
		Pairing:       stubPairing{},
		Requests:      stubRequests{},
		Readiness:     diagnostics.NewReadiness(),
		Logger:        slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelDebug})),
		InternalToken: "internal",
		External:      service,
	})
	if err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(api.PublicHandler())
	t.Cleanup(server.Close)
	return harness{
		server: server, provider: provider,
		prepared:    map[string]*whalereinv1.HostEnrollmentRequest{},
		signingKeys: map[string]ed25519.PrivateKey{},
	}
}

// enroll signs and posts one enrollment request.
func (h harness) enroll(t *testing.T, hostID, workspace, operationID string, issuedAt, expiresAt time.Time) (int, *whalereinv1.HostEnrollmentResponse) {
	t.Helper()
	if cached, ok := h.prepared[hostID]; ok {
		return h.post(t, cached)
	}
	signingPublic, signingPrivate, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	noisePublic := make([]byte, 32)
	if _, err := rand.Read(noisePublic); err != nil {
		t.Fatal(err)
	}

	authorization := h.provider.issue(t, &whalereinv1.ExternalAuthorization{
		AuthVersion:     external.AuthVersion,
		AuthorizationId: "auth-" + operationID,
		Purpose:         external.PurposeHostEnrollment,
		WorkspaceId:     workspace,
		RegionId:        regionID,
		OperationId:     operationID,
		Precondition:    "initial_enrollment",
		HostId:          hostID,
		HostPublicKeys:  [][]byte{signingPublic, noisePublic},
		IssuedAtUnixMs:  issuedAt.UnixMilli(),
		ExpiresAtUnixMs: expiresAt.UnixMilli(),
		Nonce:           []byte("0123456789abcdef"),
	})

	request := &whalereinv1.HostEnrollmentRequest{
		Authorization:        authorization,
		OperationId:          operationID,
		HostId:               hostID,
		HostSigningPublicKey: signingPublic,
		HostNoisePublicKey:   noisePublic,
	}
	request.ProofSignature = ed25519.Sign(signingPrivate,
		[]byte(external.ProofInput(authorization, operationID, signingPublic)))

	h.prepared[hostID] = request
	h.signingKeys[hostID] = signingPrivate
	return h.post(t, request)
}

// post sends one prepared enrollment request.
func (h harness) post(t *testing.T, request *whalereinv1.HostEnrollmentRequest) (int, *whalereinv1.HostEnrollmentResponse) {
	t.Helper()
	body, err := protojson.Marshal(request)
	if err != nil {
		t.Fatal(err)
	}
	response, err := http.Post(h.server.URL+"/v1/enrollments/hosts", "application/json", strings.NewReader(string(body)))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = response.Body.Close() }()
	payload, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusOK {
		t.Logf("enrollment refused: %d %s", response.StatusCode, string(payload))
		return response.StatusCode, nil
	}
	decoded := &whalereinv1.HostEnrollmentResponse{}
	if err := protojson.Unmarshal(payload, decoded); err != nil {
		t.Fatal(err)
	}
	return response.StatusCode, decoded
}

func (h harness) get(t *testing.T, path, token string) (int, []byte) {
	t.Helper()
	request, err := http.NewRequest(http.MethodGet, h.server.URL+path, nil)
	if err != nil {
		t.Fatal(err)
	}
	if token != "" {
		request.Header.Set("Authorization", "Bearer "+token)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = response.Body.Close() }()
	payload, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	return response.StatusCode, payload
}

// acceptEnforcer records no state; the real durable disable is covered by the
// store lifecycle test. This fixture only needs the HTTP surface to succeed.
type acceptEnforcer struct{}

func (acceptEnforcer) DisableHostScope(context.Context, string, map[string]uint64) error {
	return nil
}

// do sends one request with explicit headers and returns the status and body.
func (h harness) do(t *testing.T, method, path string, body []byte, headers map[string]string) (int, []byte) {
	t.Helper()
	request, err := http.NewRequest(method, h.server.URL+path, strings.NewReader(string(body)))
	if err != nil {
		t.Fatal(err)
	}
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}
	for key, value := range headers {
		request.Header.Set(key, value)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = response.Body.Close() }()
	payload, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	return response.StatusCode, payload
}

// hostProof signs the documented newline-joined proof text with a Host key.
func hostProof(t *testing.T, signingKey ed25519.PrivateKey, purpose, hostID, subjectID string, at time.Time, nonce string) external.HostOwnershipProof {
	t.Helper()
	signature := ed25519.Sign(signingKey, []byte(external.HostOwnershipProofInput(purpose, hostID, subjectID, at.UnixMilli(), nonce)))
	return external.HostOwnershipProof{
		HostID: hostID, IssuedAtUnixMs: at.UnixMilli(), Nonce: nonce,
		Signature: base64.RawURLEncoding.EncodeToString(signature),
	}
}

func encodedHostProof(t *testing.T, proof external.HostOwnershipProof) string {
	t.Helper()
	data, err := json.Marshal(proof)
	if err != nil {
		t.Fatal(err)
	}
	return base64.RawURLEncoding.EncodeToString(data)
}

// TestEnrollmentCommitsOnceAndDeliversTheCredentialOnce proves that stock
// Access enrolls a Host against the independent provider, delivers the broker
// credential only on the committing call, and resumes an identical retry
// without a second credential.
func TestEnrollmentCommitsOnceAndDeliversTheCredentialOnce(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()

	status, response := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now, now.Add(5*time.Minute))
	if status != http.StatusOK {
		t.Fatalf("expected 200, got %d", status)
	}
	if response.GetCredential() == nil {
		t.Fatal("the committing call must deliver the broker credential")
	}
	if !strings.HasPrefix(response.GetCredential().GetPassword(), "") ||
		response.GetCredential().GetPassword() == "" {
		t.Fatal("the credential must be present")
	}
	if got := response.GetStatus().GetState(); got != whalereinv1.EnrollmentState_enrollment_pending {
		t.Fatalf("a new enrollment stays pending until readiness: %v", got)
	}

	// An identical retry resumes the recorded outcome and delivers no secret.
	status, repeated := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now, now.Add(5*time.Minute))
	if status != http.StatusOK {
		t.Fatalf("a retry must succeed, got %d", status)
	}
	if repeated.GetCredential() != nil {
		t.Fatal("a retry must not deliver a second credential")
	}
	if repeated.GetStatus().GetEnrollmentRevision() != response.GetStatus().GetEnrollmentRevision() {
		t.Fatal("a retry must report the recorded revision")
	}

	// Status reads never carry a credential.
	status, payload := h.get(t, "/v1/enrollments/hosts/host-1", adminToken)
	if status != http.StatusOK {
		t.Fatalf("expected 200 from the status read, got %d", status)
	}
	if strings.Contains(strings.ToLower(string(payload)), "password") ||
		strings.Contains(strings.ToLower(string(payload)), "mqtturl") ||
		strings.Contains(strings.ToLower(string(payload)), "username") {
		t.Fatalf("a status read must not expose a credential: %s", payload)
	}
	status, payload = h.get(t, "/v1/operations/op-1", adminToken)
	if status != http.StatusOK {
		t.Fatalf("expected 200 from the operation read, got %d", status)
	}
	var progress map[string]any
	if err := json.Unmarshal(payload, &progress); err != nil {
		t.Fatal(err)
	}
	if progress["state"] != "operation_committed" {
		t.Fatalf("unexpected operation state %v", progress["state"])
	}
}

// TestEnrollmentRejectsAnExpiredApproval proves that an expired approval
// enrolls nothing.
func TestEnrollmentRejectsAnExpiredApproval(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()
	status, _ := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now.Add(-time.Hour), now.Add(-time.Minute))
	if status != http.StatusGone {
		t.Fatalf("an expired approval must be refused with 410, got %d", status)
	}
}

// TestEnrollmentRejectsAMismatchedApproval proves that a Host proving a key
// other than the approved one is refused.
func TestEnrollmentRejectsAMismatchedApproval(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()

	// A Host that proves a key other than the approved one.
	signingPublic, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	authorization := h.provider.issue(t, &whalereinv1.ExternalAuthorization{
		AuthVersion:     external.AuthVersion,
		AuthorizationId: "auth-mismatch",
		Purpose:         external.PurposeHostEnrollment,
		WorkspaceId:     "ws-fixture-2",
		RegionId:        regionID,
		OperationId:     "op-mismatch",
		HostId:          "host-mismatch",
		HostPublicKeys:  [][]byte{signingPublic},
		IssuedAtUnixMs:  now.UnixMilli(),
		ExpiresAtUnixMs: now.Add(time.Minute).UnixMilli(),
		Nonce:           []byte("0123456789abcdef"),
	})
	other, otherPrivate, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	request := &whalereinv1.HostEnrollmentRequest{
		Authorization:        authorization,
		OperationId:          "op-mismatch",
		HostId:               "host-mismatch",
		HostSigningPublicKey: other,
		HostNoisePublicKey:   make([]byte, 32),
	}
	request.ProofSignature = ed25519.Sign(otherPrivate,
		[]byte(external.ProofInput(authorization, "op-mismatch", other)))
	body, err := protojson.Marshal(request)
	if err != nil {
		t.Fatal(err)
	}
	response, err := http.Post(h.server.URL+"/v1/enrollments/hosts", "application/json", strings.NewReader(string(body)))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusForbidden {
		t.Fatalf("a mismatched approval must be refused with 403, got %d", response.StatusCode)
	}
}

// TestStatusReadsRequireAnAuthorizedCaller proves that status reads need an
// authorized caller and report a missing enrollment without a secret.
func TestStatusReadsRequireAnAuthorizedCaller(t *testing.T) {
	h := newHarness(t)
	if status, _ := h.get(t, "/v1/enrollments/hosts/host-1", ""); status != http.StatusUnauthorized {
		t.Fatalf("expected 401 without a bearer token, got %d", status)
	}
	if status, _ := h.get(t, "/v1/operations/op-1", "wrong"); status != http.StatusUnauthorized {
		t.Fatalf("expected 401 for a wrong bearer token, got %d", status)
	}
	if status, _ := h.get(t, "/v1/enrollments/hosts/host-missing", adminToken); status != http.StatusNotFound {
		t.Fatalf("expected 404 for an unknown host, got %d", status)
	}
}

// TestConflictingPayloadIsRefused proves that a changed payload cannot reuse
// a recorded authorization.
func TestConflictingPayloadIsRefused(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()
	if status, _ := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now, now.Add(5*time.Minute)); status != http.StatusOK {
		t.Fatalf("the first enrollment failed with %d", status)
	}

	// The same authorization id and operation id, but a different approved
	// payload: the recorded intent must win and the change must be refused.
	original := h.prepared["host-1"]
	signingPublic, signingPrivate, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	noise := make([]byte, 32)
	if _, err := rand.Read(noise); err != nil {
		t.Fatal(err)
	}
	authorization := h.provider.issue(t, &whalereinv1.ExternalAuthorization{
		AuthVersion:     external.AuthVersion,
		AuthorizationId: original.GetAuthorization().GetAuthorizationId(),
		Purpose:         external.PurposeHostEnrollment,
		WorkspaceId:     "ws-fixture-1",
		RegionId:        regionID,
		OperationId:     "op-1",
		HostId:          "host-1",
		HostPublicKeys:  [][]byte{signingPublic, noise},
		IssuedAtUnixMs:  now.UnixMilli(),
		ExpiresAtUnixMs: now.Add(5 * time.Minute).UnixMilli(),
		Nonce:           []byte("0123456789abcdef"),
	})
	changed := &whalereinv1.HostEnrollmentRequest{
		Authorization:        authorization,
		OperationId:          "op-1",
		HostId:               "host-1",
		HostSigningPublicKey: signingPublic,
		HostNoisePublicKey:   noise,
	}
	changed.ProofSignature = ed25519.Sign(signingPrivate,
		[]byte(external.ProofInput(authorization, "op-1", signingPublic)))

	if status, _ := h.post(t, changed); status != http.StatusConflict {
		t.Fatalf("a changed payload must be refused with 409, got %d", status)
	}
}

// --- minimal collaborators -------------------------------------------------

type acceptAdmin struct{}

func (acceptAdmin) AuthenticateAdmin(token string) error {
	if token == adminToken {
		return nil
	}
	return auth.ErrUnauthorized
}
func (acceptAdmin) AuthenticateDeviceToken(context.Context, string) (auth.Identity, error) {
	return auth.Identity{}, auth.ErrUnauthorized
}
func (acceptAdmin) AuthorizeHost(context.Context, auth.Identity, string) error { return nil }
func (acceptAdmin) ListHosts(context.Context, auth.Identity, int, int) ([]auth.HostView, bool, error) {
	return nil, false, nil
}
func (acceptAdmin) RevokeDevice(context.Context, string) error                    { return nil }
func (acceptAdmin) AuthenticateHostRequest(context.Context, auth.HostProof) error { return nil }
func (acceptAdmin) RevokeHostGrant(context.Context, string, string) error         { return nil }
func (acceptAdmin) AuthenticateBroker(context.Context, string, string, string) (auth.BrokerIdentity, error) {
	return auth.BrokerIdentity{}, errors.New("unused")
}
func (acceptAdmin) IdentityFromCredentials(string, string) (auth.BrokerIdentity, error) {
	return auth.BrokerIdentity{}, errors.New("unused")
}
func (acceptAdmin) AuthorizeACL(context.Context, auth.BrokerIdentity, auth.ACLAction, string) (bool, error) {
	return false, nil
}
func (acceptAdmin) BrokerPolicy(context.Context, auth.BrokerIdentity) (policy.Connection, error) {
	return policy.Connection{}, nil
}

type stubPairing struct{}

func (stubPairing) CreateInvitation(context.Context, string) (*whalereinv1.PairingInvitation, error) {
	return nil, errors.New("unused")
}
func (stubPairing) Claim(context.Context, *whalereinv1.PairingClaimRequest) (*whalereinv1.PairingClaimResponse, error) {
	return nil, errors.New("unused")
}

type stubRequests struct{}

func (stubRequests) Submit(context.Context, requests.SubmitInput) (store.Request, bool, error) {
	return store.Request{}, false, errors.New("unused")
}

func (stubRequests) Get(context.Context, string, string) (store.Request, error) {
	return store.Request{}, errors.New("unused")
}

func (stubRequests) Release(context.Context, string, string, string, int64) (store.Request, error) {
	return store.Request{}, errors.New("unused")
}

func (stubRequests) Touch(context.Context, string, string) (bool, error) { return false, nil }

// TestHostProofStatusReadsTheEnrollmentWithoutACredential proves that the
// original Host can read state/revision with a scoped proof while a bare id,
// the admin bearer and a replayed proof are refused.
func TestHostProofStatusReadsTheEnrollmentWithoutACredential(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()
	status, enrolled := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now, now.Add(5*time.Minute))
	if status != http.StatusOK || enrolled.GetCredential() == nil {
		t.Fatalf("enrollment failed: %d", status)
	}
	proof := hostProof(t, h.signingKeys["host-1"], external.PurposeEnrollmentStatus,
		"host-1", "op-1", now, "c3RhdHVzLWludC1ub25jZQ")
	header := encodedHostProof(t, proof)

	code, payload := h.do(t, http.MethodGet, "/v1/hosts/host-1/enrollment", nil, nil)
	if code != http.StatusUnauthorized {
		t.Fatalf("a bare host id must be unauthorized, got %d", code)
	}
	code, _ = h.do(t, http.MethodGet, "/v1/hosts/host-1/enrollment", nil,
		map[string]string{"Authorization": "Bearer " + adminToken})
	if code != http.StatusUnauthorized {
		t.Fatalf("the admin bearer must not authorize the Host read, got %d", code)
	}
	code, payload = h.do(t, http.MethodGet, "/v1/hosts/host-1/enrollment", nil,
		map[string]string{"X-Whalerein-Host-Proof": header})
	if code != http.StatusOK {
		t.Fatalf("the proof read failed: %d %s", code, payload)
	}
	decoded := &whalereinv1.HostEnrollmentStatus{}
	if err := protojson.Unmarshal(payload, decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.GetState() != whalereinv1.EnrollmentState_enrollment_pending ||
		decoded.GetEnrollmentRevision() != enrolled.GetStatus().GetEnrollmentRevision() {
		t.Fatalf("unexpected status %+v", decoded)
	}
	if strings.Contains(strings.ToLower(string(payload)), "password") ||
		strings.Contains(strings.ToLower(string(payload)), "mqtturl") ||
		strings.Contains(strings.ToLower(string(payload)), "username") {
		t.Fatalf("a status read must not expose a credential: %s", payload)
	}
	// The nonce is single-use.
	code, _ = h.do(t, http.MethodGet, "/v1/hosts/host-1/enrollment", nil,
		map[string]string{"X-Whalerein-Host-Proof": header})
	if code != http.StatusForbidden {
		t.Fatalf("a replayed proof must be forbidden, got %d", code)
	}
}

// TestCredentialReplacementDeliversOneNewCredentialAndRejectsReplay proves
// the proof-bound replacement surface against stock Access and a real schema.
func TestCredentialReplacementDeliversOneNewCredentialAndRejectsReplay(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()
	status, enrolled := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now, now.Add(5*time.Minute))
	if status != http.StatusOK || enrolled.GetCredential() == nil {
		t.Fatalf("enrollment failed: %d", status)
	}
	authorization := h.provider.issue(t, &whalereinv1.ExternalAuthorization{
		AuthVersion:        external.AuthVersion,
		AuthorizationId:    "auth-recover-int",
		Purpose:            external.PurposeHostCredentialRecovery,
		WorkspaceId:        "ws-fixture-1",
		RegionId:           regionID,
		OperationId:        "op-recover-int",
		HostId:             "host-1",
		EnrollmentRevision: enrolled.GetStatus().GetEnrollmentRevision(),
		IssuedAtUnixMs:     now.UnixMilli(),
		ExpiresAtUnixMs:    now.Add(5 * time.Minute).UnixMilli(),
		Nonce:              []byte("0123456789abcdef"),
	})
	proof := hostProof(t, h.signingKeys["host-1"], external.PurposeCredentialReplacement,
		"host-1", authorization.GetAuthorizationId(), now, "cmVwbGFjZS1pbnQtbm9uY2U")
	body, err := json.Marshal(map[string]any{"authorization": authorization})
	if err != nil {
		t.Fatal(err)
	}
	headers := map[string]string{"X-Whalerein-Host-Proof": encodedHostProof(t, proof)}

	code, payload := h.do(t, http.MethodPost, "/v1/enrollments/hosts/host-1/credential", body, headers)
	if code != http.StatusOK {
		t.Fatalf("replacement failed: %d %s", code, payload)
	}
	var replaced struct {
		Credential struct {
			HostID   string `json:"hostId"`
			MQTTURL  string `json:"mqttUrl"`
			Username string `json:"username"`
			Password string `json:"password"`
		} `json:"credential"`
		State    string `json:"state"`
		Revision string `json:"revision"`
	}
	if err := json.Unmarshal(payload, &replaced); err != nil {
		t.Fatal(err)
	}
	if replaced.Credential.Password == "" || replaced.Credential.Username != "host:host-1" {
		t.Fatalf("unexpected credential %+v", replaced.Credential)
	}
	if replaced.Credential.Password == enrolled.GetCredential().GetPassword() {
		t.Fatal("the replacement must invalidate the previous credential")
	}
	// A fresh proof for the same authorization id cannot mint a second one.
	freshProof := hostProof(t, h.signingKeys["host-1"], external.PurposeCredentialReplacement,
		"host-1", authorization.GetAuthorizationId(), now, "cmVwbGFjZS1pbnQtbm9uY2UtMg")
	code, replayPayload := h.do(t, http.MethodPost, "/v1/enrollments/hosts/host-1/credential", body,
		map[string]string{"X-Whalerein-Host-Proof": encodedHostProof(t, freshProof)})
	if code != http.StatusOK {
		t.Fatalf("a replayed authorization must return its recorded receipt, got %d", code)
	}
	var replay struct {
		Credential struct {
			Password string `json:"password"`
		} `json:"credential"`
	}
	if err := json.Unmarshal(replayPayload, &replay); err != nil {
		t.Fatal(err)
	}
	if replay.Credential.Password != "" {
		t.Fatalf("a replay delivered a second credential: %s", payload)
	}
}

// TestDisableEndpointIsIdempotent proves the admin-only disable response and
// that a repeated call preserves the original disabledAt.
func TestDisableEndpointIsIdempotent(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()
	status, _ := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now, now.Add(5*time.Minute))
	if status != http.StatusOK {
		t.Fatalf("enrollment failed: %d", status)
	}
	headers := map[string]string{"Authorization": "Bearer " + adminToken}
	code, payload := h.do(t, http.MethodPost, "/v1/enrollments/hosts/host-1/disable", nil, headers)
	if code != http.StatusOK {
		t.Fatalf("disable failed: %d %s", code, payload)
	}
	var first struct {
		State            string `json:"state"`
		RemovalOperation string `json:"removal_operation_id"`
		DisabledAt       int64  `json:"disabled_at_unix_ms"`
		RevocationCount  int    `json:"revocation_count"`
	}
	if err := json.Unmarshal(payload, &first); err != nil {
		t.Fatal(err)
	}
	// The legacy bodyless call still returns an authoritative, enforced receipt
	// for the target Access applied.
	if (first.State != "enforced" && first.State != "disabled") || first.DisabledAt == 0 {
		t.Fatalf("unexpected disable response %+v", first)
	}
	code, payload = h.do(t, http.MethodPost, "/v1/enrollments/hosts/host-1/disable", nil, headers)
	if code != http.StatusOK {
		t.Fatalf("repeated disable failed: %d %s", code, payload)
	}
	var repeated struct {
		State           string `json:"state"`
		DisabledAt      int64  `json:"disabled_at_unix_ms"`
		RevocationCount int    `json:"revocation_count"`
	}
	if err := json.Unmarshal(payload, &repeated); err != nil {
		t.Fatal(err)
	}
	if repeated.DisabledAt != first.DisabledAt || repeated.State != first.State {
		t.Fatalf("repeated disable changed the receipt: %+v -> %+v", first, repeated)
	}
	// The enrollment is disabled for both retry and broker admission.
	code, payload = h.get(t, "/v1/enrollments/hosts/host-1", adminToken)
	if code != http.StatusOK || !strings.Contains(string(payload), "enrollment_disabled") {
		t.Fatalf("status after disable = %d %s", code, payload)
	}
}

// A lost replacement response must not become a second credential. Retrying the
// identical proof either receives a credential-free receipt or is refused; only
// a new recovery authorization can mint again.
func TestLostCredentialReplacementResponseNeverMintsASecondCredential(t *testing.T) {
	h := newHarness(t)
	now := time.Now().UTC()
	status, enrolled := h.enroll(t, "host-1", "ws-fixture-1", "op-1", now, now.Add(5*time.Minute))
	if status != http.StatusOK || enrolled.GetCredential() == nil {
		t.Fatalf("enrollment failed: %d", status)
	}
	authorization := h.provider.issue(t, &whalereinv1.ExternalAuthorization{
		AuthVersion:        external.AuthVersion,
		AuthorizationId:    "auth-lost-response",
		Purpose:            external.PurposeHostCredentialRecovery,
		WorkspaceId:        "ws-fixture-1",
		RegionId:           regionID,
		OperationId:        "op-lost-response",
		HostId:             "host-1",
		EnrollmentRevision: enrolled.GetStatus().GetEnrollmentRevision(),
		IssuedAtUnixMs:     now.UnixMilli(),
		ExpiresAtUnixMs:    now.Add(5 * time.Minute).UnixMilli(),
		Nonce:              []byte("lostresponseonce"),
	})
	proof := hostProof(t, h.signingKeys["host-1"], external.PurposeCredentialReplacement,
		"host-1", authorization.GetAuthorizationId(), now, "bG9zdC1yZXBsYWNlLW5vbmNl")
	body, err := json.Marshal(map[string]any{"authorization": authorization})
	if err != nil {
		t.Fatal(err)
	}
	headers := map[string]string{"X-Whalerein-Host-Proof": encodedHostProof(t, proof)}
	code, payload := h.do(t, http.MethodPost, "/v1/enrollments/hosts/host-1/credential", body, headers)
	if code != http.StatusOK {
		t.Fatalf("replacement failed: %d %s", code, payload)
	}

	// The client lost this response and retries the exact same proof.
	code, payload = h.do(t, http.MethodPost, "/v1/enrollments/hosts/host-1/credential", body, headers)
	var retry struct {
		Credential struct {
			Password string `json:"password"`
		} `json:"credential"`
	}
	_ = json.Unmarshal(payload, &retry)
	if retry.Credential.Password != "" {
		t.Fatalf("a lost-response retry minted a second credential: %s", payload)
	}
	if code == http.StatusOK {
		// Credential-free receipt is the intended replay answer.
	} else if code != http.StatusConflict && code != http.StatusUnauthorized && code != http.StatusForbidden {
		t.Fatalf("unexpected lost-response retry status: %d %s", code, payload)
	}

	// A fresh recovery authorization can mint one new credential.
	next := h.provider.issue(t, &whalereinv1.ExternalAuthorization{
		AuthVersion:        external.AuthVersion,
		AuthorizationId:    "auth-lost-response-next",
		Purpose:            external.PurposeHostCredentialRecovery,
		WorkspaceId:        "ws-fixture-1",
		RegionId:           regionID,
		OperationId:        "op-lost-response-next",
		HostId:             "host-1",
		EnrollmentRevision: enrolled.GetStatus().GetEnrollmentRevision(),
		IssuedAtUnixMs:     now.UnixMilli(),
		ExpiresAtUnixMs:    now.Add(5 * time.Minute).UnixMilli(),
		Nonce:              []byte("lostresponseagain"),
	})
	freshProof := hostProof(t, h.signingKeys["host-1"], external.PurposeCredentialReplacement,
		"host-1", next.GetAuthorizationId(), now, "bG9zdC1yZXBsYWNlLW5vbmNlLTI")
	freshBody, err := json.Marshal(map[string]any{"authorization": next})
	if err != nil {
		t.Fatal(err)
	}
	code, payload = h.do(t, http.MethodPost, "/v1/enrollments/hosts/host-1/credential", freshBody,
		map[string]string{"X-Whalerein-Host-Proof": encodedHostProof(t, freshProof)})
	if code != http.StatusOK {
		t.Fatalf("fresh recovery authorization failed: %d %s", code, payload)
	}
	var fresh struct {
		Credential struct {
			Password string `json:"password"`
		} `json:"credential"`
	}
	if err := json.Unmarshal(payload, &fresh); err != nil {
		t.Fatal(err)
	}
	if fresh.Credential.Password == "" {
		t.Fatal("a fresh recovery authorization did not mint a credential")
	}
}
