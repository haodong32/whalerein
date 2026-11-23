package external

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"strconv"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func testHigherRevision(candidate, current string) bool {
	c, ce := strconv.ParseUint(candidate, 10, 64)
	o, oe := strconv.ParseUint(current, 10, 64)
	return ce == nil && oe == nil && c > o
}

type fakeStore struct {
	enrollments map[string]store.ExternalEnrollment
	operations  map[string]store.ExternalOperation
	byPayload   map[string]store.ExternalOperation
	err         error
	commits     int
	credential  string
	request     *whalereinv1.HostEnrollmentRequest
	signingKey  ed25519.PrivateKey
	nonces      map[string]bool
	readyCalls  int
	disabled    map[string]time.Time
	revocations map[string]uint64
	removals    map[string]store.DisabledEnrollment
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		enrollments: map[string]store.ExternalEnrollment{},
		operations:  map[string]store.ExternalOperation{},
		byPayload:   map[string]store.ExternalOperation{},
		credential:  "credential-once",
		nonces:      map[string]bool{},
		disabled:    map[string]time.Time{},
		removals:    map[string]store.DisabledEnrollment{},
	}
}

func (f *fakeStore) AdmitOperation(_ context.Context, operation store.ExternalOperation) (store.ExternalOperation, bool, error) {
	if f.err != nil {
		return store.ExternalOperation{}, false, f.err
	}
	if recorded, ok := f.operations[operation.OperationID]; ok {
		if string(recorded.PayloadDigest) != string(operation.PayloadDigest) {
			return store.ExternalOperation{}, false, store.ErrOperationConflict
		}
		return recorded, false, nil
	}
	if recorded, ok := f.byPayload[operation.AuthorizationID]; ok {
		if string(recorded.PayloadDigest) != string(operation.PayloadDigest) {
			return store.ExternalOperation{}, false, store.ErrOperationConflict
		}
		return recorded, false, nil
	}
	operation.Outcome = "admitted"
	operation.State = "operation_admitted"
	f.operations[operation.OperationID] = operation
	f.byPayload[operation.AuthorizationID] = operation
	return operation, true, nil
}

func (f *fakeStore) EnrollHost(_ context.Context, operation store.ExternalOperation, enrollment store.ExternalEnrollment) (store.ExternalEnrollment, string, bool, error) {
	if f.err != nil {
		return store.ExternalEnrollment{}, "", false, f.err
	}
	if recorded, ok := f.byPayload[operation.AuthorizationID]; ok && recorded.Outcome == "committed" {
		if string(recorded.PayloadDigest) != string(operation.PayloadDigest) {
			return store.ExternalEnrollment{}, "", false, store.ErrOperationConflict
		}
		return f.enrollments[operation.HostID], "", false, nil
	}
	if existing, exists := f.enrollments[enrollment.HostID]; exists {
		disabled := existing.DisabledAt != nil || existing.State == "enrollment_disabled"
		if !disabled {
			return store.ExternalEnrollment{}, "", false, store.ErrConflict
		}
		if !testHigherRevision(operation.EnrollmentRevision, existing.EnrollmentRevision) {
			return store.ExternalEnrollment{}, "", false, ErrEnrollmentDisabled
		}
		existing.EnrollmentRevision = operation.EnrollmentRevision
		existing.HostSigningPublicKey = enrollment.HostSigningPublicKey
		existing.HostNoisePublicKey = enrollment.HostNoisePublicKey
		existing.OperationID = operation.OperationID
		existing.State = "enrollment_pending"
		existing.DisabledAt = nil
		operation.Outcome = "committed"
		operation.State = "operation_committed"
		f.operations[operation.OperationID] = operation
		f.byPayload[operation.AuthorizationID] = operation
		f.enrollments[existing.HostID] = existing
		f.commits++
		return existing, f.credential, true, nil
	}
	f.commits++
	operation.Outcome = "committed"
	operation.State = "operation_committed"
	f.operations[operation.OperationID] = operation
	f.byPayload[operation.AuthorizationID] = operation
	enrollment.OperationID = operation.OperationID
	enrollment.State = "enrollment_pending"
	f.enrollments[enrollment.HostID] = enrollment
	return enrollment, f.credential, true, nil
}

func (f *fakeStore) Enrollment(_ context.Context, hostID string) (store.ExternalEnrollment, error) {
	enrollment, ok := f.enrollments[hostID]
	if !ok {
		return store.ExternalEnrollment{}, store.ErrNotFound
	}
	return enrollment, nil
}

func (f *fakeStore) Operation(_ context.Context, operationID string) (store.ExternalOperation, error) {
	operation, ok := f.operations[operationID]
	if !ok {
		return store.ExternalOperation{}, store.ErrNotFound
	}
	return operation, nil
}

func (f *fakeStore) MarkEnrollmentReady(_ context.Context, hostID string, at time.Time) (store.ExternalEnrollment, bool, error) {
	f.readyCalls++
	enrollment, ok := f.enrollments[hostID]
	if !ok {
		return store.ExternalEnrollment{}, false, store.ErrNotFound
	}
	if enrollment.DisabledAt != nil || enrollment.State == "enrollment_disabled" || enrollment.State == "enrollment_ready" {
		return enrollment, false, nil
	}
	enrollment.State = "enrollment_ready"
	f.enrollments[hostID] = enrollment
	if operation, ok := f.operations[enrollment.OperationID]; ok {
		operation.State = "operation_committed"
		f.operations[enrollment.OperationID] = operation
	}
	return enrollment, true, nil
}

func (f *fakeStore) DisableEnrollment(_ context.Context, hostID string, at time.Time) (store.DisabledEnrollment, error) {
	enrollment, ok := f.enrollments[hostID]
	if !ok {
		return store.DisabledEnrollment{}, store.ErrNotFound
	}
	disabledAt, repeated := f.disabled[hostID]
	if !repeated {
		disabledAt = at
		f.disabled[hostID] = at
	}
	enrollment.DisabledAt = &disabledAt
	enrollment.State = "enrollment_disabled"
	f.enrollments[hostID] = enrollment
	revocations := map[string]uint64{}
	if !repeated {
		for deviceID, generation := range f.revocations {
			revocations[deviceID] = generation
		}
	}
	return store.DisabledEnrollment{HostID: hostID, DisabledAt: disabledAt, Revocations: revocations}, nil
}

func (f *fakeStore) ReplaceHostCredential(_ context.Context, operation store.ExternalOperation) (store.ExternalEnrollment, string, error) {
	if recorded, ok := f.byPayload[operation.AuthorizationID]; ok && recorded.Outcome == "committed" {
		return store.ExternalEnrollment{}, "", store.ErrOperationConflict
	}
	enrollment, ok := f.enrollments[operation.HostID]
	if !ok {
		return store.ExternalEnrollment{}, "", store.ErrNotFound
	}
	if enrollment.DisabledAt != nil || enrollment.EnrollmentRevision != operation.EnrollmentRevision {
		return store.ExternalEnrollment{}, "", store.ErrConflict
	}
	operation.Outcome = "committed"
	operation.State = "operation_committed"
	f.byPayload[operation.AuthorizationID] = operation
	f.operations[operation.OperationID] = operation
	f.commits++
	enrollment.CurrentCredentialOperationID = operation.OperationID
	enrollment.CredentialGeneration++
	f.enrollments[operation.HostID] = enrollment
	return enrollment, f.credential + "-replacement", nil
}

func (f *fakeStore) DisableEnrollmentWithTarget(_ context.Context, hostID, removalOperationID string, expected *store.RemovalTarget, at time.Time) (store.DisabledEnrollment, error) {
	if removalOperationID == "" {
		removalOperationID = "legacy:" + hostID
	}
	if recorded, ok := f.removals[removalOperationID]; ok {
		return recorded, nil
	}
	enrollment, ok := f.enrollments[hostID]
	if !ok {
		return store.DisabledEnrollment{}, store.ErrNotFound
	}
	target := store.RemovalTarget{WorkspaceID: enrollment.WorkspaceID, EnrollmentOperationID: enrollment.OperationID, EnrollmentRevision: enrollment.EnrollmentRevision}
	if expected != nil && (expected.WorkspaceID != target.WorkspaceID || expected.EnrollmentOperationID != target.EnrollmentOperationID || expected.EnrollmentRevision != target.EnrollmentRevision) {
		return store.DisabledEnrollment{}, store.ErrConflict
	}
	disabledAt, repeated := f.disabled[hostID]
	if !repeated {
		disabledAt = at
		f.disabled[hostID] = at
	}
	enrollment.DisabledAt = &disabledAt
	enrollment.State = "enrollment_disabled"
	f.enrollments[hostID] = enrollment
	revocations := map[string]uint64{}
	if !repeated {
		for deviceID, generation := range f.revocations {
			revocations[deviceID] = generation
		}
	}
	recorded := store.DisabledEnrollment{HostID: hostID, RemovalOperationID: removalOperationID, Target: target, DisabledAt: disabledAt, Revocations: revocations}
	f.removals[removalOperationID] = recorded
	return recorded, nil
}

func (f *fakeStore) MarkRemovalEnforced(_ context.Context, removalOperationID string, at time.Time) error {
	recorded, ok := f.removals[removalOperationID]
	if !ok {
		return store.ErrNotFound
	}
	recorded.EnforcedAt = &at
	f.removals[removalOperationID] = recorded
	return nil
}

func (f *fakeStore) ConsumeHostProofNonce(_ context.Context, nonceHash []byte, purpose string, _ time.Time) (bool, error) {
	key := purpose + ":" + string(nonceHash)
	if f.nonces[key] {
		return false, nil
	}
	f.nonces[key] = true
	return true, nil
}

type harness struct {
	service  *Service
	data     *fakeStore
	signer   issuer
	enforcer *fakeEnforcer
}

type fakeEnforcer struct {
	calls       []string
	generations map[string]uint64
	err         error
}

func (f *fakeEnforcer) DisableHostScope(_ context.Context, hostID string, grantGenerations map[string]uint64) error {
	f.calls = append(f.calls, hostID)
	f.generations = grantGenerations
	return f.err
}

func newService(t *testing.T) (*Service, *fakeStore, issuer) {
	t.Helper()
	h := newHarness(t)
	return h.service, h.data, h.signer
}

func (h harness) request(t *testing.T) *whalereinv1.HostEnrollmentRequest {
	t.Helper()
	return h.data.request
}

func newHarness(t *testing.T) harness {
	t.Helper()
	signer, _, request, signingKey := enrollmentFixtureWithKey(t)
	data := newFakeStore()
	enforcer := &fakeEnforcer{}
	service := New(trustFor(signer), data, "mqtts://access.fixture.test:8883",
		WithHostEnforcer(enforcer))
	service.now = func() time.Time { return issued }
	// Keep the signed request with the harness so every test starts from a
	// request the service actually trusts.
	data.request = request
	data.signingKey = signingKey
	return harness{service: service, data: data, signer: signer, enforcer: enforcer}
}

func freshRequest(t *testing.T) *whalereinv1.HostEnrollmentRequest {
	t.Helper()
	return newHarness(t).data.request
}

func TestEnrollHostCommitsOnceAndReturnsTheCredential(t *testing.T) {
	h := newHarness(t)
	service, data, request := h.service, h.data, h.data.request

	response, err := service.EnrollHost(context.Background(), request)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if response.GetCredential() == nil {
		t.Fatal("the committing call must deliver the credential")
	}
	if got := response.GetCredential().GetUsername(); got != "host:host-1" {
		t.Fatalf("unexpected broker username %q", got)
	}
	if got := response.GetCredential().GetMqttUrl(); got != "mqtts://access.fixture.test:8883" {
		t.Fatalf("unexpected MQTT URL %q", got)
	}
	if response.GetStatus().GetEnrollmentRevision() != "1" {
		t.Fatalf("unexpected revision %q", response.GetStatus().GetEnrollmentRevision())
	}
	if response.GetStatus().GetState() != whalereinv1.EnrollmentState_enrollment_pending {
		t.Fatalf("a new enrollment must stay pending until readiness: %v", response.GetStatus().GetState())
	}
	if data.commits != 1 {
		t.Fatalf("expected one commit, got %d", data.commits)
	}
}

func TestEnrollHostRetryReturnsTheRecordedResultWithoutACredential(t *testing.T) {
	h := newHarness(t)
	service, data, request := h.service, h.data, h.data.request

	if _, err := service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	repeated, err := service.EnrollHost(context.Background(), request)
	if err != nil {
		t.Fatalf("an identical retry must succeed: %v", err)
	}
	if repeated.GetCredential() != nil {
		t.Fatal("a retry must not deliver a second credential")
	}
	if repeated.GetStatus().GetEnrollmentRevision() != "1" {
		t.Fatalf("a retry must return the recorded revision, got %q",
			repeated.GetStatus().GetEnrollmentRevision())
	}
	if data.commits != 1 {
		t.Fatalf("a retry must not commit again, got %d commits", data.commits)
	}
}

func TestEnrollHostRejectsAConflictingPayload(t *testing.T) {
	h := newHarness(t)
	service, signer, request := h.service, h.signer, h.data.request
	if _, err := service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}

	// Re-sign a changed payload: same authorization id, different Noise key,
	// with a proof that still comes from the approved signing key.
	replacement, _, err := enrollmentKeyPair(t)
	if err != nil {
		t.Fatal(err)
	}
	auth := request.GetAuthorization()
	auth.HostPublicKeys = [][]byte{request.GetHostSigningPublicKey(), replacement}
	signer.authorize(t, auth)
	changed := &whalereinv1.HostEnrollmentRequest{
		Authorization:        auth,
		OperationId:          request.GetOperationId(),
		HostId:               request.GetHostId(),
		HostSigningPublicKey: request.GetHostSigningPublicKey(),
		HostNoisePublicKey:   replacement,
	}
	changed.ProofSignature = ed25519.Sign(h.data.signingKey,
		[]byte(ProofInput(auth, changed.GetOperationId(), changed.GetHostSigningPublicKey())))

	if _, err := service.EnrollHost(context.Background(), changed); !errors.Is(err, ErrOperationConflict) {
		t.Fatalf("expected a conflict, got %v", err)
	}
}

func TestEnrollHostRefusesAnExpiredAuthorizationBeforeWriting(t *testing.T) {
	h := newHarness(t)
	service, data, request := h.service, h.data, h.data.request
	service.now = func() time.Time { return expires }

	if _, err := service.EnrollHost(context.Background(), request); !errors.Is(err, ErrAuthorizationExpired) {
		t.Fatalf("expected an expiry refusal, got %v", err)
	}
	if data.commits != 0 {
		t.Fatal("an expired authorization must not commit")
	}
}

func TestEnrollHostRefusesAMismatchedService(t *testing.T) {
	h := newHarness(t)
	service, data, request := h.service, h.data, h.data.request
	request.Authorization.Audience = "https://other.fixture.test"

	if _, err := service.EnrollHost(context.Background(), request); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("expected an audience refusal, got %v", err)
	}
	if data.commits != 0 {
		t.Fatal("a mismatched authorization must not commit")
	}
}

func TestEnrollHostRefusesADisabledEnrollmentWithoutAHigherRevision(t *testing.T) {
	h := newHarness(t)
	service, data, request := h.service, h.data, h.data.request
	disabled := time.Now().UTC()
	data.enrollments[request.GetHostId()] = store.ExternalEnrollment{
		HostID:             request.GetHostId(),
		EnrollmentRevision: "1",
		DisabledAt:         &disabled,
	}
	if _, err := service.EnrollHost(context.Background(), request); !errors.Is(err, ErrEnrollmentDisabled) {
		t.Fatalf("expected a disabled refusal, got %v", err)
	}
	if data.commits != 0 {
		t.Fatal("a disabled enrollment must not be re-created")
	}
}

func TestEnrollHostReenablesWithAHigherRevision(t *testing.T) {
	h := newHarness(t)
	service, data, request := h.service, h.data, h.data.request
	disabled := time.Now().UTC()
	data.enrollments[request.GetHostId()] = store.ExternalEnrollment{
		HostID:             request.GetHostId(),
		EnrollmentRevision: "0",
		DisabledAt:         &disabled,
	}
	if response, err := service.EnrollHost(context.Background(), request); err != nil {
		t.Fatalf("unexpected re-enable error: %v", err)
	} else if response.GetStatus().GetEnrollmentRevision() != "1" {
		t.Fatalf("unexpected revision %q", response.GetStatus().GetEnrollmentRevision())
	}
	if data.commits != 1 {
		t.Fatalf("expected one re-enable commit, got %d", data.commits)
	}
	if data.enrollments[request.GetHostId()].DisabledAt != nil {
		t.Fatal("the new generation must clear disabled_at")
	}
}

func TestEnrollHostRefusesAStaleRevision(t *testing.T) {
	service, data, signer := newService(t)
	request := freshRequest(t)
	data.enrollments[request.GetHostId()] = store.ExternalEnrollment{
		HostID:             request.GetHostId(),
		EnrollmentRevision: "7",
	}
	auth := request.GetAuthorization()
	auth.ExpectedRevision = "3"
	signer.authorize(t, auth)

	if _, err := service.EnrollHost(context.Background(), request); !errors.Is(err, ErrStaleRevision) {
		t.Fatalf("expected a stale-revision refusal, got %v", err)
	}
	if data.commits != 0 {
		t.Fatal("a stale revision must not commit")
	}
}

func TestEnrollmentStatusNeverCarriesACredential(t *testing.T) {
	h := newHarness(t)
	service, request := h.service, h.data.request
	if _, err := service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	status, err := service.EnrollmentStatus(context.Background(), request.GetHostId())
	if err != nil {
		t.Fatal(err)
	}
	if status.GetHostId() != "host-1" || status.GetWorkspaceId() != "ws-fixture-1" {
		t.Fatalf("unexpected status %+v", status)
	}
}

func TestEnrollmentStatusReportsMissingRows(t *testing.T) {
	h := newHarness(t)
	service := h.service
	if _, err := service.EnrollmentStatus(context.Background(), "host-missing"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("expected not found, got %v", err)
	}
}

func TestOperationStatusReportsTheRecordedBindings(t *testing.T) {
	h := newHarness(t)
	service, request := h.service, h.data.request
	created, err := service.EnrollHost(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	status, err := service.OperationStatus(context.Background(), created.GetStatus().GetOperationId())
	if err != nil {
		t.Fatal(err)
	}
	if status.GetPurpose() != PurposeHostEnrollment || status.GetWorkspaceId() != "ws-fixture-1" {
		t.Fatalf("unexpected operation status %+v", status)
	}
	if status.GetState() != whalereinv1.OperationState_operation_committed {
		t.Fatalf("expected a committed operation, got %v", status.GetState())
	}
	if len(status.GetPayloadDigest()) != 32 {
		t.Fatalf("expected a payload digest, got %d bytes", len(status.GetPayloadDigest()))
	}
}

func TestOperationStatusReportsMissingRows(t *testing.T) {
	h := newHarness(t)
	service := h.service
	if _, err := service.OperationStatus(context.Background(), "op-missing"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("expected not found, got %v", err)
	}
}

func TestStoreFailureIsSurfaced(t *testing.T) {
	h := newHarness(t)
	service, data := h.service, h.data
	data.err = errors.New("database unavailable")
	if _, err := service.EnrollHost(context.Background(), h.data.request); err == nil {
		t.Fatal("a store failure must be surfaced")
	}
}

func TestEnrollmentStatusReportsADisabledEnrollment(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	disabled := time.Now().UTC()
	enrollment := h.data.enrollments[request.GetHostId()]
	enrollment.State = "enrollment_disabled"
	enrollment.DisabledAt = &disabled
	h.data.enrollments[request.GetHostId()] = enrollment

	status, err := h.service.EnrollmentStatus(context.Background(), request.GetHostId())
	if err != nil {
		t.Fatal(err)
	}
	if status.GetState() != whalereinv1.EnrollmentState_enrollment_disabled {
		t.Fatalf("expected a disabled state, got %v", status.GetState())
	}
}

func TestOperationStatusCarriesTheCommitTimeWhenRecorded(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	created, err := h.service.EnrollHost(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	committedAt := time.Now().UTC()
	operation := h.data.operations[created.GetStatus().GetOperationId()]
	operation.CommittedAt = &committedAt
	h.data.operations[created.GetStatus().GetOperationId()] = operation

	status, err := h.service.OperationStatus(context.Background(), created.GetStatus().GetOperationId())
	if err != nil {
		t.Fatal(err)
	}
	if status.GetCommittedAtUnixMs() != committedAt.UnixMilli() {
		t.Fatalf("expected the commit time, got %d", status.GetCommittedAtUnixMs())
	}
}

func TestEnrollHostRefusesAProofForAnotherHost(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	request.HostId = "host-other"
	if _, err := h.service.EnrollHost(context.Background(), request); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("expected a host mismatch, got %v", err)
	}
	if h.data.commits != 0 {
		t.Fatal("a mismatched host must not commit")
	}
}

// signedHostProof builds the exact signed JSON document the Host sends in the
// X-Whalerein-Host-Proof header.
func signedHostProof(t *testing.T, signingKey ed25519.PrivateKey, purpose, hostID, subjectID string, at time.Time, nonce string) HostOwnershipProof {
	t.Helper()
	input := HostOwnershipProofInput(purpose, hostID, subjectID, at.UnixMilli(), nonce)
	signature := ed25519.Sign(signingKey, []byte(input))
	return HostOwnershipProof{
		HostID:         hostID,
		IssuedAtUnixMs: at.UnixMilli(),
		Nonce:          nonce,
		Signature:      base64.RawURLEncoding.EncodeToString(signature),
	}
}

func recoveryAuthorization(signer issuer, hostID, workspace, revision, authorizationID string) *whalereinv1.ExternalAuthorization {
	auth := baseAuth("fixture://issuer/a")
	auth.AuthorizationId = authorizationID
	auth.Purpose = PurposeHostCredentialRecovery
	auth.OperationId = "op-recover-1"
	auth.WorkspaceId = workspace
	auth.EnrollmentRevision = revision
	auth.HostId = hostID
	auth.Precondition = "credential_recovery"
	return auth
}

func TestHandleHostReadyAdvancesOnlyPendingEnrollments(t *testing.T) {
	h := newHarness(t)
	// A standalone Host has no external enrollment and is ignored.
	if err := h.service.HandleHostReady(context.Background(), "host-standalone"); err != nil {
		t.Fatalf("an unenrolled Host must be ignored: %v", err)
	}
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	if err := h.service.HandleHostReady(context.Background(), request.GetHostId()); err != nil {
		t.Fatal(err)
	}
	enrollment := h.data.enrollments[request.GetHostId()]
	if enrollment.State != "enrollment_ready" {
		t.Fatalf("HostReady must advance the enrollment, got %q", enrollment.State)
	}
	if operation := h.data.operations[enrollment.OperationID]; operation.State != "operation_committed" {
		t.Fatalf("the operation outcome was not recorded: %+v", operation)
	}
	// A repeated HostReady is a no-op.
	if err := h.service.HandleHostReady(context.Background(), request.GetHostId()); err != nil {
		t.Fatal(err)
	}
	if got := h.data.enrollments[request.GetHostId()].State; got != "enrollment_ready" {
		t.Fatalf("repeated readiness changed state to %q", got)
	}
}

func TestHandleHostReadyDoesNotAdvanceDisabledEnrollment(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	if _, err := h.service.DisableHost(context.Background(), request.GetHostId()); err != nil {
		t.Fatal(err)
	}
	if err := h.service.HandleHostReady(context.Background(), request.GetHostId()); err != nil {
		t.Fatal(err)
	}
	if got := h.data.enrollments[request.GetHostId()].State; got != "enrollment_disabled" {
		t.Fatalf("a disabled enrollment became %q after HostReady", got)
	}
}

func TestDisableHostIsIdempotentAndClosesTheLiveScope(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	h.data.revocations = map[string]uint64{"device-1": 4, "device-2": 7}
	first, err := h.service.DisableHost(context.Background(), request.GetHostId())
	if err != nil {
		t.Fatal(err)
	}
	if len(h.enforcer.calls) != 1 || h.enforcer.calls[0] != request.GetHostId() {
		t.Fatalf("expected one enforcement call, got %v", h.enforcer.calls)
	}
	if len(h.enforcer.generations) != 2 {
		t.Fatalf("expected both revoked grants to reach the line, got %v", h.enforcer.generations)
	}
	second, err := h.service.DisableHost(context.Background(), request.GetHostId())
	if err != nil {
		t.Fatal(err)
	}
	if !second.Equal(first) {
		t.Fatalf("repeated disable changed disabled_at from %s to %s", first, second)
	}
	// A retry repeats exactly the stored suppression for the original target;
	// it never changes the receipt or invents a second revocation set.
	if h.enforcer.calls[len(h.enforcer.calls)-1] != request.GetHostId() {
		t.Fatalf("repeated disable did not repeat the original scope: %v", h.enforcer.calls)
	}
	if _, ok := h.enforcer.generations["device-1"]; !ok {
		t.Fatalf("repeated disable did not repeat the stored revocations: %v", h.enforcer.generations)
	}
	if _, err := h.service.DisableHost(context.Background(), "host-missing"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("expected not found for an unknown Host, got %v", err)
	}
}

func TestDisableHostSurfacesEnforcementUnavailability(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	h.enforcer.err = errors.New("line unavailable")
	if _, err := h.service.DisableHost(context.Background(), request.GetHostId()); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("expected an enforcement refusal, got %v", err)
	}
}

func TestEnrollmentStatusForHostRequiresTheCurrentSigningKey(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	proof := signedHostProof(t, h.data.signingKey, PurposeEnrollmentStatus,
		request.GetHostId(), "op-1", issued, "c3RhdHVzLW5vbmNl")
	status, err := h.service.EnrollmentStatusForHost(context.Background(), request.GetHostId(), proof)
	if err != nil {
		t.Fatal(err)
	}
	if status.GetState() != whalereinv1.EnrollmentState_enrollment_pending {
		t.Fatalf("unexpected state %v", status.GetState())
	}
	// The same proof is single-use.
	if _, err := h.service.EnrollmentStatusForHost(context.Background(), request.GetHostId(), proof); !errors.Is(err, ErrProofReplay) {
		t.Fatalf("expected a replay refusal, got %v", err)
	}
	// The Host may sign an empty subject when it lost the operation id.
	other := signedHostProof(t, h.data.signingKey, PurposeEnrollmentStatus,
		request.GetHostId(), "", issued, "c3RhdHVzLW5vbmNlLTI")
	if _, err := h.service.EnrollmentStatusForHost(context.Background(), request.GetHostId(), other); err != nil {
		t.Fatalf("an empty subject must be accepted: %v", err)
	}
	// A proof from another key is refused.
	_, otherKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	foreign := signedHostProof(t, otherKey, PurposeEnrollmentStatus,
		request.GetHostId(), "op-1", issued, "c3RhdHVzLW5vbmNlLTM")
	if _, err := h.service.EnrollmentStatusForHost(context.Background(), request.GetHostId(), foreign); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a proof refusal, got %v", err)
	}
	// A bare host id carries no proof.
	if _, err := h.service.EnrollmentStatusForHost(context.Background(), request.GetHostId(), HostOwnershipProof{HostID: request.GetHostId()}); err == nil {
		t.Fatal("a bare host id must not read enrollment status")
	}
}

func TestEnrollmentStatusForHostRefusesDisabledEnrollments(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	if _, err := h.service.DisableHost(context.Background(), request.GetHostId()); err != nil {
		t.Fatal(err)
	}
	proof := signedHostProof(t, h.data.signingKey, PurposeEnrollmentStatus,
		request.GetHostId(), "op-1", issued, "c3RhdHVzLW5vbmNlLTQ")
	status, err := h.service.EnrollmentStatusForHost(context.Background(), request.GetHostId(), proof)
	if err != nil {
		t.Fatalf("disabled status must be an authoritative read, got %v", err)
	}
	if status.GetRemovalState() == "" || status.GetRemovalState() == "none" {
		t.Fatalf("disabled status must report the removal lifecycle, got %+v", status)
	}
}

func TestRecoverCredentialReplacesOnceAndRejectsReplays(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	created, err := h.service.EnrollHost(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	auth := recoveryAuthorization(h.signer, request.GetHostId(), "ws-fixture-1", "1", "auth-recover-1")
	h.signer.authorize(t, auth)
	proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
		request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1ub25jZQ")
	replacement, err := h.service.RecoverCredential(context.Background(), request.GetHostId(), auth, proof)
	if err != nil {
		t.Fatalf("unexpected replacement error: %v", err)
	}
	if replacement.Credential.Password == "" || replacement.Credential.Username != "host:host-1" {
		t.Fatalf("unexpected one-time credential %+v", replacement.Credential)
	}
	if replacement.Revision != created.GetStatus().GetEnrollmentRevision() {
		t.Fatalf("unexpected revision %q", replacement.Revision)
	}
	// The same proof nonce is single-use and fails before the store.
	if _, err := h.service.RecoverCredential(context.Background(), request.GetHostId(), auth, proof); !errors.Is(err, ErrProofReplay) {
		t.Fatalf("expected a proof replay refusal, got %v", err)
	}
	// A fresh proof for the same authorization id still cannot mint a second
	// credential: the durable operation records one intent only.
	replayProof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
		request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1ub25jZS00")
	replayed, err := h.service.RecoverCredential(context.Background(), request.GetHostId(), auth, replayProof)
	if err != nil {
		t.Fatalf("a replay must return the recorded credential-free outcome, got %v", err)
	}
	if replayed.Credential.Password != "" || replayed.CredentialIssued {
		t.Fatalf("a replay issued a second credential: %+v", replayed)
	}
}

func TestRecoverCredentialRefusesExpiredStaleDisabledAndForeignProofs(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	if _, err := h.service.EnrollHost(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	auth := recoveryAuthorization(h.signer, request.GetHostId(), "ws-fixture-1", "1", "auth-recover-2")
	h.signer.authorize(t, auth)
	proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
		request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1ub25jZS0y")

	// A stale enrollment revision is refused before the credential changes.
	stale := recoveryAuthorization(h.signer, request.GetHostId(), "ws-fixture-1", "9", "auth-recover-3")
	h.signer.authorize(t, stale)
	if _, err := h.service.RecoverCredential(context.Background(), request.GetHostId(), stale, proof); !errors.Is(err, ErrStaleRevision) {
		t.Fatalf("expected a stale-revision refusal, got %v", err)
	}
	// An expired authorization is refused.
	expired := recoveryAuthorization(h.signer, request.GetHostId(), "ws-fixture-1", "1", "auth-recover-4")
	expired.ExpiresAtUnixMs = issued.Add(-time.Minute).UnixMilli()
	h.signer.authorize(t, expired)
	if _, err := h.service.RecoverCredential(context.Background(), request.GetHostId(), expired, proof); !errors.Is(err, ErrAuthorizationExpired) {
		t.Fatalf("expected an expiry refusal, got %v", err)
	}
	// A proof from another key is refused.
	_, otherKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	foreign := signedHostProof(t, otherKey, PurposeCredentialReplacement,
		request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1ub25jZS0z")
	if _, err := h.service.RecoverCredential(context.Background(), request.GetHostId(), auth, foreign); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a proof refusal, got %v", err)
	}
	// A disabled enrollment cannot be recovered.
	if _, err := h.service.DisableHost(context.Background(), request.GetHostId()); err != nil {
		t.Fatal(err)
	}
	if _, err := h.service.RecoverCredential(context.Background(), request.GetHostId(), auth, proof); !errors.Is(err, ErrEnrollmentDisabled) {
		t.Fatalf("expected a disabled refusal, got %v", err)
	}
}
