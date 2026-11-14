package external

import (
	"context"
	"crypto/ed25519"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type fakeStore struct {
	enrollments map[string]store.ExternalEnrollment
	operations  map[string]store.ExternalOperation
	byPayload   map[string]store.ExternalOperation
	revision    int
	err         error
	commits     int
	credential  string
	request     *whalereinv1.HostEnrollmentRequest
	signingKey  ed25519.PrivateKey
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		enrollments: map[string]store.ExternalEnrollment{},
		operations:  map[string]store.ExternalOperation{},
		byPayload:   map[string]store.ExternalOperation{},
		credential:  "credential-once",
	}
}

func (f *fakeStore) EnrollHost(_ context.Context, operation store.ExternalOperation, enrollment store.ExternalEnrollment) (store.ExternalEnrollment, string, bool, error) {
	if f.err != nil {
		return store.ExternalEnrollment{}, "", false, f.err
	}
	if recorded, ok := f.byPayload[operation.AuthorizationID]; ok {
		if string(recorded.PayloadDigest) != string(operation.PayloadDigest) {
			return store.ExternalEnrollment{}, "", false, store.ErrOperationConflict
		}
		return f.enrollments[operation.HostID], "", false, nil
	}
	f.commits++
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

func (f *fakeStore) NextEnrollmentRevision(context.Context, string) (string, error) {
	f.revision++
	return "1", nil
}

type harness struct {
	service *Service
	data    *fakeStore
	signer  issuer
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
	service := New(trustFor(signer), data, "mqtts://access.fixture.test:8883")
	service.now = func() time.Time { return issued }
	// Keep the signed request with the harness so every test starts from a
	// request the service actually trusts.
	data.request = request
	data.signingKey = signingKey
	return harness{service: service, data: data, signer: signer}
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

	if _, err := service.EnrollHost(context.Background(), changed); !errors.Is(err, store.ErrOperationConflict) {
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

func TestEnrollHostRefusesADisabledEnrollment(t *testing.T) {
	h := newHarness(t)
	service, data, request := h.service, h.data, h.data.request
	disabled := time.Now().UTC()
	data.enrollments[request.GetHostId()] = store.ExternalEnrollment{
		HostID:     request.GetHostId(),
		DisabledAt: &disabled,
	}
	if _, err := service.EnrollHost(context.Background(), request); !errors.Is(err, ErrEnrollmentDisabled) {
		t.Fatalf("expected a disabled refusal, got %v", err)
	}
	if data.commits != 0 {
		t.Fatal("a disabled enrollment must not be re-created")
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
