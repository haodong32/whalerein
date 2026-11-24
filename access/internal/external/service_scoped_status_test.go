package external

import (
	"context"
	"crypto/ed25519"
	"encoding/base64"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// hookStore substitutes individual store calls so a test can exercise a
// failure branch without a real database.
type hookStore struct {
	*fakeStore
	enrollmentHook   func(context.Context, string) (store.ExternalEnrollment, error)
	operationHook    func(context.Context, string) (store.ExternalOperation, error)
	admitHook        func(context.Context, store.ExternalOperation) (store.ExternalOperation, bool, error)
	enrollHostHook   func(context.Context, store.ExternalOperation, store.ExternalEnrollment) (store.ExternalEnrollment, string, bool, error)
	replaceHook      func(context.Context, store.ExternalOperation) (store.ExternalEnrollment, string, error)
	readyHook        func(context.Context, string, time.Time) (store.ExternalEnrollment, bool, error)
	markEnforcedHook func(context.Context, string, time.Time) error
	nonceHook        func(context.Context, []byte, string, time.Time) (bool, error)
	nextRevisionHook func(context.Context, string) (string, error)
}

func (h *hookStore) Enrollment(ctx context.Context, hostID string) (store.ExternalEnrollment, error) {
	if h.enrollmentHook != nil {
		return h.enrollmentHook(ctx, hostID)
	}
	return h.fakeStore.Enrollment(ctx, hostID)
}

func (h *hookStore) Operation(ctx context.Context, operationID string) (store.ExternalOperation, error) {
	if h.operationHook != nil {
		return h.operationHook(ctx, operationID)
	}
	return h.fakeStore.Operation(ctx, operationID)
}

func (h *hookStore) AdmitOperation(ctx context.Context, operation store.ExternalOperation) (store.ExternalOperation, bool, error) {
	if h.admitHook != nil {
		return h.admitHook(ctx, operation)
	}
	return h.fakeStore.AdmitOperation(ctx, operation)
}

func (h *hookStore) EnrollHost(ctx context.Context, operation store.ExternalOperation, enrollment store.ExternalEnrollment) (store.ExternalEnrollment, string, bool, error) {
	if h.enrollHostHook != nil {
		return h.enrollHostHook(ctx, operation, enrollment)
	}
	return h.fakeStore.EnrollHost(ctx, operation, enrollment)
}

func (h *hookStore) ReplaceHostCredential(ctx context.Context, operation store.ExternalOperation) (store.ExternalEnrollment, string, error) {
	if h.replaceHook != nil {
		return h.replaceHook(ctx, operation)
	}
	return h.fakeStore.ReplaceHostCredential(ctx, operation)
}

func (h *hookStore) MarkEnrollmentReady(ctx context.Context, hostID string, at time.Time) (store.ExternalEnrollment, bool, error) {
	if h.readyHook != nil {
		return h.readyHook(ctx, hostID, at)
	}
	return h.fakeStore.MarkEnrollmentReady(ctx, hostID, at)
}

func (h *hookStore) MarkRemovalEnforced(ctx context.Context, removalOperationID string, at time.Time) error {
	if h.markEnforcedHook != nil {
		return h.markEnforcedHook(ctx, removalOperationID, at)
	}
	return h.fakeStore.MarkRemovalEnforced(ctx, removalOperationID, at)
}

func (h *hookStore) ConsumeHostProofNonce(ctx context.Context, nonceHash []byte, purpose string, at time.Time) (bool, error) {
	if h.nonceHook != nil {
		return h.nonceHook(ctx, nonceHash, purpose, at)
	}
	return h.fakeStore.ConsumeHostProofNonce(ctx, nonceHash, purpose, at)
}

func enrollReadyHost(t *testing.T) harness {
	t.Helper()
	h := newHarness(t)
	if _, err := h.service.EnrollHost(context.Background(), h.data.request); err != nil {
		t.Fatal(err)
	}
	return h
}

// TestOperationStatusForHostReportsTheScopedOperation covers the happy path:
// the Host proves possession of the current signing key and binds the proof to
// the exact operation id before Access reveals its recorded state.
func TestOperationStatusForHostReportsTheScopedOperation(t *testing.T) {
	h := enrollReadyHost(t)
	hostID := h.data.request.GetHostId()
	created, err := h.service.EnrollmentStatus(context.Background(), hostID)
	if err != nil {
		t.Fatal(err)
	}
	operationID := created.GetOperationId()
	proof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
		hostID, operationID, issued, "b3Atc3RhdHVzLWhhcHB5")

	status, err := h.service.OperationStatusForHost(context.Background(), hostID, operationID, 0, proof)
	if err != nil {
		t.Fatalf("unexpected status error: %v", err)
	}
	if status.GetOperationId() != operationID || status.GetHostId() != hostID {
		t.Fatalf("unexpected operation scope %+v", status)
	}
	if status.GetPurpose() != PurposeHostEnrollment {
		t.Fatalf("unexpected purpose %q", status.GetPurpose())
	}
	if status.GetState() != whalereinv1.OperationState_operation_committed {
		t.Fatalf("expected a committed operation, got %v", status.GetState())
	}
	if len(status.GetPayloadDigest()) != 32 {
		t.Fatalf("expected the recorded payload digest, got %d bytes", len(status.GetPayloadDigest()))
	}
}

// TestOperationStatusForHostRefusesUnknownOrForeignScopes proves the read is
// scoped to both the Host enrollment and the operation owner.
func TestOperationStatusForHostRefusesUnknownOrForeignScopes(t *testing.T) {
	t.Run("unknown enrollment", func(t *testing.T) {
		h := newHarness(t)
		if _, err := h.service.OperationStatusForHost(context.Background(), "host-missing",
			"op-1", 0, HostOwnershipProof{}); !errors.Is(err, ErrAuthorizationMismatch) {
			t.Fatalf("expected an authorization mismatch, got %v", err)
		}
	})

	t.Run("foreign operation", func(t *testing.T) {
		h := enrollReadyHost(t)
		hostID := h.data.request.GetHostId()
		h.data.operations["op-foreign"] = store.ExternalOperation{
			OperationID: "op-foreign",
			HostID:      "host-other",
			Outcome:     store.OperationCommitted,
		}
		proof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
			hostID, "op-foreign", issued, "b3Atc3RhdHVzLWZvcmVpZ24")
		if _, err := h.service.OperationStatusForHost(context.Background(), hostID,
			"op-foreign", 0, proof); !errors.Is(err, ErrAuthorizationMismatch) {
			t.Fatalf("expected a foreign-operation refusal, got %v", err)
		}
	})

	t.Run("proof for another Host", func(t *testing.T) {
		h := enrollReadyHost(t)
		hostID := h.data.request.GetHostId()
		proof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
			"host-other", "op-1", issued, "b3Atc3RhdHVzLW90aGVy")
		if _, err := h.service.OperationStatusForHost(context.Background(), hostID,
			"op-1", 0, proof); !errors.Is(err, ErrAuthorizationMismatch) {
			t.Fatalf("expected a proof mismatch, got %v", err)
		}
	})

	t.Run("enrollment without a signing key", func(t *testing.T) {
		h := newHarness(t)
		hostID := h.data.request.GetHostId()
		h.data.enrollments[hostID] = store.ExternalEnrollment{HostID: hostID}
		proof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
			hostID, "op-1", issued, "b3Atc3RhdHVzLW5va2V5")
		if _, err := h.service.OperationStatusForHost(context.Background(), hostID,
			"op-1", 0, proof); !errors.Is(err, ErrAuthorizationMismatch) {
			t.Fatalf("expected a missing-key refusal, got %v", err)
		}
	})
}

// TestOperationStatusForHostDistinguishesMissingOperations covers the
// definitive-absence rule: a missing row is only absent when the caller proves
// the authorization deadline has passed, otherwise it stays pending.
func TestOperationStatusForHostDistinguishesMissingOperations(t *testing.T) {
	h := enrollReadyHost(t)
	hostID := h.data.request.GetHostId()
	ctx := context.Background()

	pendingProof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
		hostID, "op-not-recorded", issued, "b3AtbWlzc2luZy1wZW5kaW5n")
	pending, err := h.service.OperationStatusForHost(ctx, hostID, "op-not-recorded", 0, pendingProof)
	if err != nil {
		t.Fatal(err)
	}
	if pending.GetOutcome() != store.OperationPending || pending.GetOperationId() != "op-not-recorded" {
		t.Fatalf("an unexpired missing operation must stay pending: %+v", pending)
	}

	absentProof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
		hostID, "op-not-recorded", issued, "b3AtbWlzc2luZy1hYnNlbnQ")
	absent, err := h.service.OperationStatusForHost(ctx, hostID, "op-not-recorded",
		issued.UnixMilli(), absentProof)
	if err != nil {
		t.Fatal(err)
	}
	if absent.GetOutcome() != store.OperationAbsent || absent.GetReason() != "no_admission_record" {
		t.Fatalf("an expired missing operation must be definitive: %+v", absent)
	}

	futureProof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
		hostID, "op-not-recorded", issued, "b3AtbWlzc2luZy1mdXR1cmU")
	future, err := h.service.OperationStatusForHost(ctx, hostID, "op-not-recorded",
		issued.Add(time.Minute).UnixMilli(), futureProof)
	if err != nil {
		t.Fatal(err)
	}
	if future.GetOutcome() != store.OperationPending {
		t.Fatalf("a future authorization deadline must stay pending: %+v", future)
	}
}

// TestOperationStatusForHostSurfacesStoreFailures keeps transport failures
// distinguishable from authorization refusals.
func TestOperationStatusForHostSurfacesStoreFailures(t *testing.T) {
	t.Run("enrollment store", func(t *testing.T) {
		h := newHarness(t)
		h.service.store = &hookStore{fakeStore: h.data,
			enrollmentHook: func(context.Context, string) (store.ExternalEnrollment, error) {
				return store.ExternalEnrollment{}, errors.New("enrollment store unavailable")
			}}
		_, err := h.service.OperationStatusForHost(context.Background(), "host-1", "op-1", 0, HostOwnershipProof{})
		if err == nil || errors.Is(err, ErrAuthorizationMismatch) {
			t.Fatalf("expected a surfaced store failure, got %v", err)
		}
	})

	t.Run("operation store", func(t *testing.T) {
		h := enrollReadyHost(t)
		hostID := h.data.request.GetHostId()
		h.service.store = &hookStore{fakeStore: h.data,
			operationHook: func(context.Context, string) (store.ExternalOperation, error) {
				return store.ExternalOperation{}, errors.New("operation store unavailable")
			}}
		proof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
			hostID, "op-1", issued, "b3Atc3RhdHVzLXN0b3Jl")
		_, err := h.service.OperationStatusForHost(context.Background(), hostID, "op-1", 0, proof)
		if err == nil || errors.Is(err, ErrAuthorizationMismatch) {
			t.Fatalf("expected a surfaced operation failure, got %v", err)
		}
	})
}

// TestMapStoreErrorMapsEveryTypedRefusal checks the store-to-wire translation
// independently of the service flow.
func TestMapStoreErrorMapsEveryTypedRefusal(t *testing.T) {
	for name, testCase := range map[string]struct {
		in   error
		want error
	}{
		"nil":             {nil, nil},
		"expired":         {store.ErrAuthorizationExpired, ErrAuthorizationExpired},
		"fenced":          {store.ErrOperationFenced, ErrOperationFenced},
		"pending":         {store.ErrOperationPending, ErrOperationPending},
		"duplicate host":  {store.ErrDuplicateHost, ErrDuplicateHost},
		"disabled":        {store.ErrEnrollmentDisabled, ErrEnrollmentDisabled},
		"target mismatch": {store.ErrTargetMismatch, ErrTargetMismatch},
		"refused":         {store.ErrOperationRefused, ErrOperationConflict},
		"conflict":        {store.ErrOperationConflict, ErrOperationConflict},
		"unmapped":        {store.ErrNotFound, store.ErrNotFound},
	} {
		t.Run(name, func(t *testing.T) {
			got := mapStoreError(testCase.in)
			if testCase.want == nil {
				if got != nil {
					t.Fatalf("expected nil, got %v", got)
				}
				return
			}
			if !errors.Is(got, testCase.want) {
				t.Fatalf("mapStoreError(%v) = %v, want %v", testCase.in, got, testCase.want)
			}
		})
	}

	wrapped := fmt.Errorf("wrapped: %w", store.ErrOperationFenced)
	if got := mapStoreError(wrapped); !errors.Is(got, ErrOperationFenced) {
		t.Fatalf("a wrapped sentinel must map through errors.Is: %v", got)
	}
}

func TestRefusedStoreErrorSelectsTheWireReason(t *testing.T) {
	for reason, want := range map[string]error{
		"duplicate_host":      ErrDuplicateHost,
		"enrollment_disabled": ErrEnrollmentDisabled,
		"":                    ErrOperationConflict,
		"other":               ErrOperationConflict,
	} {
		if got := refusedStoreError(store.ExternalOperation{Reason: reason}); !errors.Is(got, want) {
			t.Fatalf("reason %q mapped to %v, want %v", reason, got, want)
		}
	}
}

func TestRemovalReceiptCountsItsRevocations(t *testing.T) {
	if got := (RemovalReceipt{}).RevocationCount(); got != 0 {
		t.Fatalf("empty receipt count = %d", got)
	}
	receipt := RemovalReceipt{Revocations: map[string]uint64{"device-1": 2, "device-2": 3}}
	if got := receipt.RevocationCount(); got != 2 {
		t.Fatalf("receipt count = %d, want 2", got)
	}
}

func TestRemovalAndEnrollmentStateProjection(t *testing.T) {
	disabledAt := issued
	for name, testCase := range map[string]struct {
		enrollment store.ExternalEnrollment
		want       string
	}{
		"recorded removal state": {store.ExternalEnrollment{RemovalState: "pending"}, "pending"},
		"disabled at":            {store.ExternalEnrollment{DisabledAt: &disabledAt}, "disabled"},
		"disabled state":         {store.ExternalEnrollment{State: "enrollment_disabled"}, "disabled"},
		"active":                 {store.ExternalEnrollment{State: "enrollment_ready"}, "none"},
	} {
		t.Run(name, func(t *testing.T) {
			if got := removalState(testCase.enrollment); got != testCase.want {
				t.Fatalf("removalState = %q, want %q", got, testCase.want)
			}
		})
	}

	for state, want := range map[string]whalereinv1.EnrollmentState{
		"enrollment_ready":    whalereinv1.EnrollmentState_enrollment_ready,
		"enrollment_disabled": whalereinv1.EnrollmentState_enrollment_disabled,
		"enrollment_pending":  whalereinv1.EnrollmentState_enrollment_pending,
		"unknown":             whalereinv1.EnrollmentState_enrollment_pending,
	} {
		if got := enrollmentState(state); got != want {
			t.Fatalf("enrollmentState(%q) = %v, want %v", state, got, want)
		}
	}
}

// TestEnrollmentStatusProjectsTheRemovalLifecycle covers the removal fields a
// disabled binding still exposes to the original Host.
func TestEnrollmentStatusProjectsTheRemovalLifecycle(t *testing.T) {
	enforcedAt := issued.Add(time.Minute)
	enrollment := store.ExternalEnrollment{
		HostID:                       "host-1",
		WorkspaceID:                  "ws-1",
		EnrollmentRevision:           "2",
		State:                        "enrollment_disabled",
		RemovalState:                 "enforced",
		RemovalOperationID:           "rem-1",
		RemovalEnforcedAt:            &enforcedAt,
		CurrentCredentialOperationID: "op-cred-1",
		CredentialGeneration:         3,
	}
	status := enrollmentStatus(enrollment)
	if status.GetRemovalState() != "enforced" || status.GetRemovalOperationId() != "rem-1" {
		t.Fatalf("unexpected removal projection %+v", status)
	}
	if status.GetRemovalEnforcedAtUnixMs() != enforcedAt.UnixMilli() {
		t.Fatalf("unexpected enforced time %d", status.GetRemovalEnforcedAtUnixMs())
	}
	if status.GetState() != whalereinv1.EnrollmentState_enrollment_disabled ||
		status.GetCurrentCredentialOperationId() != "op-cred-1" || status.GetCredentialGeneration() != 3 {
		t.Fatalf("unexpected enrollment projection %+v", status)
	}
}

func TestAuthorizationExpiryProjection(t *testing.T) {
	if got := authorizationExpiresAt(nil); got != nil {
		t.Fatalf("nil authorization expiry = %v", got)
	}
	unset := &whalereinv1.ExternalAuthorization{}
	if got := authorizationExpiresAt(unset); got != nil {
		t.Fatalf("omitted expiry = %v", got)
	}
	auth := &whalereinv1.ExternalAuthorization{ExpiresAtUnixMs: expires.UnixMilli()}
	got := authorizationExpiresAt(auth)
	if got == nil || !got.Equal(expires) {
		t.Fatalf("expiry = %v, want %v", got, expires)
	}
}

func TestOperationStateAndStatusProjection(t *testing.T) {
	for outcome, want := range map[string]whalereinv1.OperationState{
		store.OperationCommitted: whalereinv1.OperationState_operation_committed,
		store.OperationRefused:   whalereinv1.OperationState_operation_refused,
		store.OperationExpired:   whalereinv1.OperationState_operation_refused,
		store.OperationFenced:    whalereinv1.OperationState_operation_refused,
		store.OperationPending:   whalereinv1.OperationState_operation_pending,
		"":                       whalereinv1.OperationState_operation_pending,
	} {
		if got := operationState(store.ExternalOperation{Outcome: outcome}); got != want {
			t.Fatalf("operationState(%q) = %v, want %v", outcome, got, want)
		}
	}

	updatedAt := issued.Add(time.Minute)
	legacy := operationStatus(store.ExternalOperation{
		OperationID: "op-legacy",
		HostID:      "host-1",
		UpdatedAt:   &updatedAt,
	})
	if legacy.GetOutcome() != store.OperationPending {
		t.Fatalf("a legacy row must report pending, got %q", legacy.GetOutcome())
	}
	if legacy.GetState() != whalereinv1.OperationState_operation_pending {
		t.Fatalf("a legacy row must report a pending state, got %v", legacy.GetState())
	}
	if legacy.GetUpdatedAtUnixMs() != updatedAt.UnixMilli() {
		t.Fatalf("unexpected updated time %d", legacy.GetUpdatedAtUnixMs())
	}
}

func TestCanonicalInputDefaultsTheAuthorizationVersion(t *testing.T) {
	explicit := baseAuth("fixture://issuer/a")
	implicit := baseAuth("fixture://issuer/a")
	implicit.AuthVersion = ""
	if CanonicalInput(implicit) != CanonicalInput(explicit) {
		t.Fatal("an omitted auth version must sign as the current version")
	}
}

func TestVerifyRejectsAnInvalidKeyOrSignatureLength(t *testing.T) {
	auth := baseAuth("fixture://issuer/a")
	auth.KeyId = "short-key"
	auth.Signature = make([]byte, ed25519.SignatureSize)
	trust := Trust{
		Issuer:          "fixture://issuer/a",
		ServiceIdentity: "https://access.fixture.test",
		Keys:            []VerificationKey{{ID: "short-key", PublicKey: make([]byte, 8)}},
	}
	if err := trust.Verify(auth, PurposeHostEnrollment, issued); !errors.Is(err, ErrAuthorizationSignature) {
		t.Fatalf("expected a signature-length refusal, got %v", err)
	}
}

func TestVerifyHostOwnershipProofRejectsMalformedProofs(t *testing.T) {
	publicKey, _, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatal(err)
	}
	base := HostOwnershipProof{
		HostID:         "host-1",
		IssuedAtUnixMs: issued.UnixMilli(),
		Nonce:          "bm9uY2U",
		Signature:      "%%%",
	}
	if err := VerifyHostOwnershipProof(publicKey, base, PurposeOperationStatus, "op-1", issued); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a malformed-signature refusal, got %v", err)
	}
	base.Signature = base64.RawURLEncoding.EncodeToString(make([]byte, ed25519.SignatureSize))
	if err := VerifyHostOwnershipProof(make(ed25519.PublicKey, 8), base, PurposeOperationStatus, "op-1", issued); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a short-key refusal, got %v", err)
	}
	withoutHost := base
	withoutHost.HostID = ""
	if err := VerifyHostOwnershipProof(publicKey, withoutHost, PurposeOperationStatus, "op-1", issued); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("expected a missing-host refusal, got %v", err)
	}
}

func TestEnrollHostReplaysTheRecordedAdmissionOutcome(t *testing.T) {
	for name, testCase := range map[string]struct {
		outcome string
		reason  string
		wantErr error
	}{
		"committed":         {store.OperationCommitted, "", nil},
		"refused duplicate": {store.OperationRefused, "duplicate_host", ErrDuplicateHost},
		"refused disabled":  {store.OperationRefused, "enrollment_disabled", ErrEnrollmentDisabled},
		"refused conflict":  {store.OperationRefused, "enrollment_revision_stale", ErrOperationConflict},
		"expired":           {store.OperationExpired, "", ErrAuthorizationExpired},
		"fenced":            {store.OperationFenced, "", ErrOperationFenced},
		"pending":           {store.OperationPending, "", ErrOperationPending},
		"unknown outcome":   {"", "", ErrOperationPending},
	} {
		t.Run(name, func(t *testing.T) {
			h := newHarness(t)
			request := h.data.request
			h.data.operations["op-1"] = store.ExternalOperation{
				OperationID:     "op-1",
				AuthorizationID: request.GetAuthorization().GetAuthorizationId(),
				PayloadDigest:   Digest(request),
				Outcome:         testCase.outcome,
				Reason:          testCase.reason,
			}
			h.data.enrollments[request.GetHostId()] = store.ExternalEnrollment{
				HostID:               request.GetHostId(),
				WorkspaceID:          "ws-fixture-1",
				EnrollmentRevision:   "1",
				OperationID:          "op-1",
				State:                "enrollment_ready",
				HostSigningPublicKey: request.GetHostSigningPublicKey(),
			}

			response, err := h.service.EnrollHost(context.Background(), request)
			if testCase.wantErr == nil {
				if err != nil {
					t.Fatalf("unexpected replay error: %v", err)
				}
				if response.GetStatus().GetState() != whalereinv1.EnrollmentState_enrollment_ready {
					t.Fatalf("replay did not return the recorded status: %+v", response)
				}
				if response.GetCredential() != nil {
					t.Fatal("a committed replay must not issue a credential")
				}
				return
			}
			if !errors.Is(err, testCase.wantErr) {
				t.Fatalf("replay error = %v, want %v", err, testCase.wantErr)
			}
		})
	}
}

func TestEnrollHostAssignsAnOperationIDForALegacyAuthorization(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	auth := request.GetAuthorization()
	auth.OperationId = ""
	request.OperationId = ""
	h.signer.authorize(t, auth)
	request.ProofSignature = ed25519.Sign(h.data.signingKey,
		[]byte(ProofInput(auth, "", request.GetHostSigningPublicKey())))

	response, err := h.service.EnrollHost(context.Background(), request)
	if err != nil {
		t.Fatalf("unexpected enrollment error: %v", err)
	}
	if response.GetStatus().GetOperationId() == "" {
		t.Fatal("a legacy authorization must receive a reconcilable operation id")
	}
}

func TestEnrollHostRefusesAStateOnlyDisabledEnrollmentWithoutAHigherRevision(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	h.data.enrollments[request.GetHostId()] = store.ExternalEnrollment{
		HostID:             request.GetHostId(),
		EnrollmentRevision: "1",
		State:              "enrollment_disabled",
	}
	if _, err := h.service.EnrollHost(context.Background(), request); !errors.Is(err, ErrEnrollmentDisabled) {
		t.Fatalf("expected a disabled refusal, got %v", err)
	}
	if h.data.commits != 0 {
		t.Fatal("a disabled enrollment must not commit")
	}
}

func TestEnrollHostSurfacesStoreFailures(t *testing.T) {
	boom := errors.New("store unavailable")
	for name, install := range map[string]func(*hookStore){
		"admit": func(hooks *hookStore) {
			hooks.admitHook = func(context.Context, store.ExternalOperation) (store.ExternalOperation, bool, error) {
				return store.ExternalOperation{}, false, store.ErrOperationFenced
			}
		},
		"enroll": func(hooks *hookStore) {
			hooks.enrollHostHook = func(context.Context, store.ExternalOperation, store.ExternalEnrollment) (store.ExternalEnrollment, string, bool, error) {
				return store.ExternalEnrollment{}, "", false, store.ErrTargetMismatch
			}
		},
		"load": func(hooks *hookStore) {
			hooks.enrollmentHook = func(context.Context, string) (store.ExternalEnrollment, error) {
				return store.ExternalEnrollment{}, boom
			}
		},
	} {
		t.Run(name, func(t *testing.T) {
			h := newHarness(t)
			h.service.store = &hookStore{fakeStore: h.data}
			install(h.service.store.(*hookStore))
			_, err := h.service.EnrollHost(context.Background(), h.data.request)
			if name == "admit" {
				if !errors.Is(err, ErrOperationFenced) {
					t.Fatalf("expected the mapped fenced refusal, got %v", err)
				}
				return
			}
			if name == "enroll" {
				if !errors.Is(err, ErrTargetMismatch) {
					t.Fatalf("expected the mapped target refusal, got %v", err)
				}
				return
			}
			if err == nil {
				t.Fatal("expected the store failure to surface")
			}
		})
	}
}

func TestEnrollHostSurfacesACommittedStatusReloadFailure(t *testing.T) {
	h := newHarness(t)
	request := h.data.request
	h.data.operations["op-1"] = store.ExternalOperation{
		OperationID:     "op-1",
		AuthorizationID: request.GetAuthorization().GetAuthorizationId(),
		PayloadDigest:   Digest(request),
		Outcome:         store.OperationCommitted,
	}
	loads := 0
	h.service.store = &hookStore{fakeStore: h.data,
		enrollmentHook: func(ctx context.Context, hostID string) (store.ExternalEnrollment, error) {
			loads++
			if loads > 1 {
				return store.ExternalEnrollment{}, errors.New("reload failed")
			}
			return h.data.Enrollment(ctx, hostID)
		}}
	if _, err := h.service.EnrollHost(context.Background(), request); err == nil {
		t.Fatal("expected the committed status reload failure to surface")
	}
}

func TestRecoverCredentialRejectsMismatchedScopes(t *testing.T) {
	for name, mutate := range map[string]func(*whalereinv1.ExternalAuthorization){
		"host":             func(a *whalereinv1.ExternalAuthorization) { a.HostId = "host-other" },
		"workspace":        func(a *whalereinv1.ExternalAuthorization) { a.WorkspaceId = "ws-other" },
		"authorization id": func(a *whalereinv1.ExternalAuthorization) { a.AuthorizationId = "" },
	} {
		t.Run(name, func(t *testing.T) {
			h := enrollReadyHost(t)
			auth := recoveryAuthorization(h.signer, h.data.request.GetHostId(), "ws-fixture-1", "1", "auth-recover-scope")
			mutate(auth)
			h.signer.authorize(t, auth)
			proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
				h.data.request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1zY29wZQ")
			_, err := h.service.RecoverCredential(context.Background(), h.data.request.GetHostId(), auth, proof)
			if !errors.Is(err, ErrAuthorizationMismatch) {
				t.Fatalf("expected an authorization mismatch, got %v", err)
			}
		})
	}

	t.Run("unknown enrollment", func(t *testing.T) {
		h := newHarness(t)
		auth := recoveryAuthorization(h.signer, "host-missing", "ws-fixture-1", "1", "auth-recover-missing")
		h.signer.authorize(t, auth)
		proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
			"host-missing", auth.GetAuthorizationId(), issued, "cmVjb3Zlci1taXNzaW5n")
		if _, err := h.service.RecoverCredential(context.Background(), "host-missing", auth, proof); !errors.Is(err, store.ErrNotFound) {
			t.Fatalf("expected not found, got %v", err)
		}
	})
}

func TestRecoverCredentialReplaysTheRecordedAdmissionOutcome(t *testing.T) {
	for name, testCase := range map[string]struct {
		outcome string
		reason  string
		wantErr error
	}{
		"committed":         {store.OperationCommitted, "", nil},
		"refused duplicate": {store.OperationRefused, "duplicate_host", ErrDuplicateHost},
		"refused disabled":  {store.OperationRefused, "enrollment_disabled", ErrEnrollmentDisabled},
		"expired":           {store.OperationExpired, "", ErrAuthorizationExpired},
		"fenced":            {store.OperationFenced, "", ErrOperationFenced},
		"pending":           {store.OperationPending, "", ErrOperationPending},
		"unknown outcome":   {"", "", ErrOperationPending},
	} {
		t.Run(name, func(t *testing.T) {
			h := enrollReadyHost(t)
			auth := recoveryAuthorization(h.signer, h.data.request.GetHostId(), "ws-fixture-1", "1", "auth-recover-replay")
			h.signer.authorize(t, auth)
			proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
				h.data.request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1yZXBsYXk")
			h.data.operations[auth.GetOperationId()] = store.ExternalOperation{
				OperationID:          auth.GetOperationId(),
				AuthorizationID:      auth.GetAuthorizationId(),
				PayloadDigest:        HashBytes([]byte(CanonicalInput(auth))),
				Outcome:              testCase.outcome,
				Reason:               testCase.reason,
				CredentialGeneration: 4,
				CredentialIssued:     true,
			}

			replacement, err := h.service.RecoverCredential(context.Background(), h.data.request.GetHostId(), auth, proof)
			if testCase.wantErr != nil {
				if !errors.Is(err, testCase.wantErr) {
					t.Fatalf("replay error = %v, want %v", err, testCase.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected replay error: %v", err)
			}
			if replacement.OperationID != auth.GetOperationId() || replacement.CredentialGeneration != 4 ||
				!replacement.CredentialIssued || replacement.Credential.Password != "" {
				t.Fatalf("unexpected credential-free replay %+v", replacement)
			}
		})
	}
}

func TestRecoverCredentialSurfacesStoreFailures(t *testing.T) {
	t.Run("admission", func(t *testing.T) {
		h := enrollReadyHost(t)
		auth := recoveryAuthorization(h.signer, h.data.request.GetHostId(), "ws-fixture-1", "1", "auth-recover-admit")
		h.signer.authorize(t, auth)
		proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
			h.data.request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1hZG1pdA")
		h.service.store = &hookStore{fakeStore: h.data,
			admitHook: func(context.Context, store.ExternalOperation) (store.ExternalOperation, bool, error) {
				return store.ExternalOperation{}, false, errors.New("admission store unavailable")
			}}
		if _, err := h.service.RecoverCredential(context.Background(), h.data.request.GetHostId(), auth, proof); err == nil {
			t.Fatal("expected the admission failure to surface")
		}
	})

	t.Run("replacement", func(t *testing.T) {
		h := enrollReadyHost(t)
		auth := recoveryAuthorization(h.signer, h.data.request.GetHostId(), "ws-fixture-1", "1", "auth-recover-replace")
		h.signer.authorize(t, auth)
		proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
			h.data.request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1yZXBsYWNl")
		h.service.store = &hookStore{fakeStore: h.data,
			replaceHook: func(context.Context, store.ExternalOperation) (store.ExternalEnrollment, string, error) {
				return store.ExternalEnrollment{}, "", store.ErrConflict
			}}
		if _, err := h.service.RecoverCredential(context.Background(), h.data.request.GetHostId(), auth, proof); err == nil {
			t.Fatal("expected the replacement failure to surface")
		}
	})
}

func TestRecoverCredentialAssignsAnOperationIDForALegacyAuthorization(t *testing.T) {
	h := enrollReadyHost(t)
	auth := recoveryAuthorization(h.signer, h.data.request.GetHostId(), "ws-fixture-1", "1", "auth-recover-legacy")
	auth.OperationId = ""
	h.signer.authorize(t, auth)
	proof := signedHostProof(t, h.data.signingKey, PurposeCredentialReplacement,
		h.data.request.GetHostId(), auth.GetAuthorizationId(), issued, "cmVjb3Zlci1sZWdhY3k")

	replacement, err := h.service.RecoverCredential(context.Background(), h.data.request.GetHostId(), auth, proof)
	if err != nil {
		t.Fatalf("unexpected recovery error: %v", err)
	}
	if replacement.OperationID == "" || replacement.Credential.Password == "" {
		t.Fatalf("a legacy authorization must receive an operation id and credential: %+v", replacement)
	}
}

// TestVerifyHostOwnershipConsumesTheNonceOnce covers the durable replay guard
// and the store failure that precedes it.
func TestVerifyHostOwnershipConsumesTheNonceOnce(t *testing.T) {
	t.Run("nonce store failure", func(t *testing.T) {
		h := enrollReadyHost(t)
		h.service.store = &hookStore{fakeStore: h.data,
			nonceHook: func(context.Context, []byte, string, time.Time) (bool, error) {
				return false, errors.New("nonce store unavailable")
			}}
		hostID := h.data.request.GetHostId()
		proof := signedHostProof(t, h.data.signingKey, PurposeOperationStatus,
			hostID, "op-1", issued, "b3Atbm9uY2UtZmFpbHVyZQ")
		_, err := h.service.OperationStatusForHost(context.Background(), hostID, "op-1", 0, proof)
		if err == nil || !strings.Contains(err.Error(), "host proof nonce") {
			t.Fatalf("expected a nonce store failure, got %v", err)
		}
	})

	t.Run("repeated subject binding", func(t *testing.T) {
		h := enrollReadyHost(t)
		hostID := h.data.request.GetHostId()
		enrollment := h.data.enrollments[hostID]
		enrollment.OperationID = ""
		h.data.enrollments[hostID] = enrollment
		// The two acceptable subjects collapse to one empty subject, and a
		// proof for another subject stays refused.
		proof := signedHostProof(t, h.data.signingKey, PurposeEnrollmentStatus,
			hostID, "op-1", issued, "c3RhdHVzLWR1cGxpY2F0ZQ")
		if _, err := h.service.EnrollmentStatusForHost(context.Background(), hostID, proof); !errors.Is(err, ErrProofInvalid) {
			t.Fatalf("expected a proof refusal, got %v", err)
		}
	})
}

func TestHandleHostReadyRejectsAnEmptyHostAndSurfacesStoreFailure(t *testing.T) {
	h := newHarness(t)
	if err := h.service.HandleHostReady(context.Background(), ""); err == nil {
		t.Fatal("an empty host id must be refused")
	}
	h.service.store = &hookStore{fakeStore: h.data,
		readyHook: func(context.Context, string, time.Time) (store.ExternalEnrollment, bool, error) {
			return store.ExternalEnrollment{}, false, errors.New("ready store unavailable")
		}}
	if err := h.service.HandleHostReady(context.Background(), "host-1"); err == nil {
		t.Fatal("a readiness store failure must be surfaced")
	}
}

func TestRemoveHostRequiresEnforcementAndRecordsIt(t *testing.T) {
	t.Run("without an enforcer", func(t *testing.T) {
		h := enrollReadyHost(t)
		h.service.enforcer = nil
		receipt, err := h.service.RemoveHost(context.Background(), h.data.request.GetHostId(), RemovalRequest{})
		if !errors.Is(err, ErrEnforcementUnavailable) {
			t.Fatalf("expected an enforcement refusal, got %v", err)
		}
		if receipt.State != "pending" {
			t.Fatalf("unexpected receipt state %q", receipt.State)
		}
	})

	t.Run("enforcement record failure", func(t *testing.T) {
		h := enrollReadyHost(t)
		h.service.store = &hookStore{fakeStore: h.data,
			markEnforcedHook: func(context.Context, string, time.Time) error {
				return errors.New("enforcement record unavailable")
			}}
		receipt, err := h.service.RemoveHost(context.Background(), h.data.request.GetHostId(), RemovalRequest{})
		if err == nil || receipt.State != "pending" {
			t.Fatalf("expected a pending receipt with an error, got %+v, %v", receipt, err)
		}
	})
}
