package external

import (
	"context"
	"crypto/ed25519"
	"errors"
	"fmt"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Store is the persistence boundary the service needs.
type Store interface {
	AdmitOperation(ctx context.Context, operation store.ExternalOperation) (store.ExternalOperation, bool, error)
	EnrollHost(ctx context.Context, operation store.ExternalOperation, enrollment store.ExternalEnrollment) (store.ExternalEnrollment, string, bool, error)
	Enrollment(ctx context.Context, hostID string) (store.ExternalEnrollment, error)
	Operation(ctx context.Context, operationID string) (store.ExternalOperation, error)
	NextEnrollmentRevision(ctx context.Context, workspaceID string) (string, error)
	MarkEnrollmentReady(ctx context.Context, hostID string, at time.Time) (store.ExternalEnrollment, bool, error)
	DisableEnrollment(ctx context.Context, hostID string, at time.Time) (store.DisabledEnrollment, error)
	DisableEnrollmentWithTarget(ctx context.Context, hostID, removalOperationID string, expected *store.RemovalTarget, at time.Time) (store.DisabledEnrollment, error)
	MarkRemovalEnforced(ctx context.Context, removalOperationID string, at time.Time) error
	ReplaceHostCredential(ctx context.Context, operation store.ExternalOperation) (store.ExternalEnrollment, string, error)
	ConsumeHostProofNonce(ctx context.Context, nonceHash []byte, purpose string, at time.Time) (bool, error)
}

// HostEnforcer applies the realtime side of a durable Host removal: local
// admission and the live broker line.
type HostEnforcer interface {
	DisableHostScope(ctx context.Context, hostID string, grantGenerations map[string]uint64) error
}

// IssuedCredential is the one-time broker credential for a Host. It is never
// persisted in plaintext and is returned only by a committing call.
type IssuedCredential struct {
	HostID   string
	MQTTURL  string
	Username string
	Password string
}

// CredentialReplacement is the result of a scoped replacement. A committed
// replay returns the receipt without a credential.
type CredentialReplacement struct {
	Credential           IssuedCredential
	OperationID          string
	CredentialGeneration uint64
	CredentialIssued     bool
	State                string
	Revision             string
}

// RemovalRequest is the optional durable removal target. Its zero value keeps
// the legacy bodyless behavior: Access applies the effective binding.
type RemovalRequest struct {
	RemovalOperationID string
	Target             *store.RemovalTarget
}

// RemovalReceipt is the credential-free outcome of one Host removal.
type RemovalReceipt struct {
	State              string
	RemovalOperationID string
	Target             store.RemovalTarget
	DisabledAt         time.Time
	EnforcedAt         *time.Time
	Revocations        map[string]uint64
}

// RevocationCount reports the number of exact grant denials the removal owns.
func (r RemovalReceipt) RevocationCount() int { return len(r.Revocations) }

// ErrEnforcementUnavailable reports that the durable removal committed but the
// live broker/line enforcement could not be confirmed.
var ErrEnforcementUnavailable = errors.New("host enforcement is unavailable")

// Stable external refusals that are also exposed as typed wire reasons.
var (
	ErrDuplicateHost    = store.ErrDuplicateHost
	ErrOperationFenced  = store.ErrOperationFenced
	ErrOperationPending = store.ErrOperationPending
	ErrTargetMismatch   = store.ErrTargetMismatch
)

// Option configures the external service.
type Option func(*Service)

// WithHostEnforcer wires the realtime enforcement used by Host removal.
func WithHostEnforcer(enforcer HostEnforcer) Option {
	return func(service *Service) { service.enforcer = enforcer }
}

// Service implements external account mode.
type Service struct {
	deviceSessions       DeviceSessionStore
	sessionEnforcer      DeviceSessionEnforcer
	requireDeviceSession bool

	trust            Trust
	store            Store
	mqttURL          string
	enforcer         HostEnforcer
	now              func() time.Time
	devicePairing    DevicePairing
	deviceGrants     DeviceGrantStore
	deviceEnforcer   DeviceGrantEnforcer
	hostClientGrants HostClientGrants
	hostClientProbe  HostClientProbe
	hostPresence     HostPresence
}

// New constructs the service.
func New(trust Trust, data Store, mqttURL string, options ...Option) *Service {
	service := &Service{
		trust:   trust,
		store:   data,
		mqttURL: mqttURL,
		now:     func() time.Time { return time.Now().UTC() },
	}
	for _, option := range options {
		if option != nil {
			option(service)
		}
	}
	return service
}

// EnrollHost admits one Host enrollment.
//
// It verifies the portable authorization and the Host's proof of possession,
// then admits the operation in its own committed transaction. A retry with the
// same payload returns the recorded status and no credential; a changed payload
// is a conflict. The broker credential is returned only by the committing call.
func (s *Service) EnrollHost(ctx context.Context, request *whalereinv1.HostEnrollmentRequest) (*whalereinv1.HostEnrollmentResponse, error) {
	auth := request.GetAuthorization()
	if err := s.trust.Verify(auth, PurposeHostEnrollment, s.now()); err != nil {
		return nil, err
	}
	if err := VerifyHostProof(auth, request); err != nil {
		return nil, err
	}

	operationID := auth.GetOperationId()
	if operationID == "" {
		// Legacy compatibility: an authorization minted before operation ids
		// still receives one so the attempt can be reconciled.
		operationID = store.NewOperationID()
	}
	existing, err := s.store.Enrollment(ctx, request.GetHostId())
	if err == nil && (existing.DisabledAt != nil || existing.State == "enrollment_disabled") {
		return nil, ErrEnrollmentDisabled
	}
	if err != nil && !errors.Is(err, store.ErrNotFound) {
		return nil, fmt.Errorf("load enrollment: %w", err)
	}

	revision := auth.GetEnrollmentRevision()
	if revision == "" {
		revision, err = s.store.NextEnrollmentRevision(ctx, auth.GetWorkspaceId())
		if err != nil {
			return nil, err
		}
	}
	if expected := auth.GetExpectedRevision(); expected != "" && existing.EnrollmentRevision != "" &&
		existing.EnrollmentRevision != expected {
		return nil, fmt.Errorf("%w: enrollment is at revision %s, not %s",
			ErrStaleRevision, existing.EnrollmentRevision, expected)
	}

	operation := store.ExternalOperation{
		OperationID:                operationID,
		AuthorizationID:            auth.GetAuthorizationId(),
		Purpose:                    PurposeHostEnrollment,
		WorkspaceID:                auth.GetWorkspaceId(),
		RegionID:                   auth.GetRegionId(),
		HostID:                     request.GetHostId(),
		PayloadDigest:              Digest(request),
		State:                      "operation_admitted",
		EnrollmentRevision:         revision,
		AuthorizationExpiresAt:     authorizationExpiresAt(auth),
		ExpectedWorkspaceID:        auth.GetWorkspaceId(),
		ExpectedEnrollmentRevision: auth.GetExpectedRevision(),
	}
	recorded, admitted, err := s.store.AdmitOperation(ctx, operation)
	if err != nil {
		return nil, mapStoreError(err)
	}
	if !admitted {
		switch recorded.Outcome {
		case store.OperationCommitted:
			enrollment, loadErr := s.store.Enrollment(ctx, request.GetHostId())
			if loadErr != nil {
				return nil, loadErr
			}
			return &whalereinv1.HostEnrollmentResponse{Status: enrollmentStatus(enrollment)}, nil
		case store.OperationRefused:
			return nil, refusedStoreError(recorded)
		case store.OperationExpired:
			return nil, ErrAuthorizationExpired
		case store.OperationFenced:
			return nil, ErrOperationFenced
		default:
			return nil, ErrOperationPending
		}
	}

	enrollment := store.ExternalEnrollment{
		HostID:               request.GetHostId(),
		WorkspaceID:          auth.GetWorkspaceId(),
		EnrollmentRevision:   revision,
		HostSigningPublicKey: request.GetHostSigningPublicKey(),
		HostNoisePublicKey:   request.GetHostNoisePublicKey(),
		State:                "enrollment_pending",
	}
	recording, credential, committed, err := s.store.EnrollHost(ctx, operation, enrollment)
	if err != nil {
		return nil, mapStoreError(err)
	}

	response := &whalereinv1.HostEnrollmentResponse{Status: enrollmentStatus(recording)}
	if committed {
		response.Credential = &whalereinv1.HostCredential{
			HostId:   recording.HostID,
			MqttUrl:  s.mqttURL,
			Username: "host:" + recording.HostID,
			Password: credential,
		}
	}
	return response, nil
}

// EnrollmentStatus reports one Host enrollment without any credential.
func (s *Service) EnrollmentStatus(ctx context.Context, hostID string) (*whalereinv1.HostEnrollmentStatus, error) {
	enrollment, err := s.store.Enrollment(ctx, hostID)
	if err != nil {
		return nil, err
	}
	return enrollmentStatus(enrollment), nil
}

func enrollmentStatus(enrollment store.ExternalEnrollment) *whalereinv1.HostEnrollmentStatus {
	status := &whalereinv1.HostEnrollmentStatus{
		HostId:                       enrollment.HostID,
		WorkspaceId:                  enrollment.WorkspaceID,
		EnrollmentRevision:           enrollment.EnrollmentRevision,
		State:                        enrollmentState(enrollment.State),
		OperationId:                  enrollment.OperationID,
		HostSigningPublicKey:         enrollment.HostSigningPublicKey,
		HostNoisePublicKey:           enrollment.HostNoisePublicKey,
		CurrentCredentialOperationId: enrollment.CurrentCredentialOperationID,
		CredentialGeneration:         enrollment.CredentialGeneration,
		RemovalState:                 removalState(enrollment),
		RemovalOperationId:           enrollment.RemovalOperationID,
	}
	if enrollment.RemovalEnforcedAt != nil {
		status.RemovalEnforcedAtUnixMs = enrollment.RemovalEnforcedAt.UnixMilli()
	}
	return status
}

func removalState(enrollment store.ExternalEnrollment) string {
	if enrollment.RemovalState != "" {
		return enrollment.RemovalState
	}
	if enrollment.DisabledAt != nil || enrollment.State == "enrollment_disabled" {
		return "disabled"
	}
	return "none"
}

// EnrollmentStatusForHost reports one enrollment to the original Host after it
// proves possession of the current enrollment signing key. A disabled or
// enforced binding still returns authoritative removal fields; only a failed
// proof is refused. The result never contains a credential or transport
// password.
func (s *Service) EnrollmentStatusForHost(ctx context.Context, hostID string, proof HostOwnershipProof) (*whalereinv1.HostEnrollmentStatus, error) {
	enrollment, err := s.store.Enrollment(ctx, hostID)
	if err != nil {
		return nil, err
	}
	// The Host may sign the recorded operation id or an empty subject when it
	// lost the original reply. Both are bound to this enrollment.
	if err := s.verifyHostOwnership(ctx, enrollment, proof, PurposeEnrollmentStatus,
		[]string{enrollment.OperationID, ""}); err != nil {
		return nil, err
	}
	return enrollmentStatus(enrollment), nil
}

// RecoverCredential replaces one enrollment credential after the Host proves
// possession and presents a fresh, purpose-bound Cloud authorization. The new
// credential is returned exactly once; a same-payload replay returns a
// credential-free receipt, and a changed payload is a conflict.
func (s *Service) RecoverCredential(ctx context.Context, hostID string, auth *whalereinv1.ExternalAuthorization, proof HostOwnershipProof) (*CredentialReplacement, error) {
	if err := s.trust.Verify(auth, PurposeHostCredentialRecovery, s.now()); err != nil {
		return nil, err
	}
	enrollment, err := s.store.Enrollment(ctx, hostID)
	if err != nil {
		return nil, err
	}
	if enrollment.DisabledAt != nil || enrollment.State == "enrollment_disabled" {
		return nil, ErrEnrollmentDisabled
	}
	if auth.GetHostId() != hostID || auth.GetHostId() != enrollment.HostID {
		return nil, fmt.Errorf("%w: host id does not match the enrollment", ErrAuthorizationMismatch)
	}
	if auth.GetWorkspaceId() != enrollment.WorkspaceID {
		return nil, fmt.Errorf("%w: workspace does not match the enrollment", ErrAuthorizationMismatch)
	}
	if auth.GetEnrollmentRevision() != enrollment.EnrollmentRevision {
		return nil, fmt.Errorf("%w: enrollment is at revision %s, not %s",
			ErrStaleRevision, enrollment.EnrollmentRevision, auth.GetEnrollmentRevision())
	}
	if auth.GetAuthorizationId() == "" {
		return nil, fmt.Errorf("%w: authorization id is required", ErrAuthorizationMismatch)
	}
	if err := s.verifyHostOwnership(ctx, enrollment, proof, PurposeCredentialReplacement,
		[]string{auth.GetAuthorizationId()}); err != nil {
		return nil, err
	}

	operationID := auth.GetOperationId()
	if operationID == "" {
		operationID = store.NewOperationID()
	}
	operation := store.ExternalOperation{
		OperationID:                operationID,
		AuthorizationID:            auth.GetAuthorizationId(),
		Purpose:                    PurposeHostCredentialRecovery,
		WorkspaceID:                auth.GetWorkspaceId(),
		RegionID:                   auth.GetRegionId(),
		HostID:                     hostID,
		PayloadDigest:              HashBytes([]byte(CanonicalInput(auth))),
		State:                      "operation_admitted",
		EnrollmentRevision:         enrollment.EnrollmentRevision,
		AuthorizationExpiresAt:     authorizationExpiresAt(auth),
		ExpectedWorkspaceID:        auth.GetWorkspaceId(),
		ExpectedEnrollmentRevision: enrollment.EnrollmentRevision,
	}
	recorded, admitted, err := s.store.AdmitOperation(ctx, operation)
	if err != nil {
		return nil, mapStoreError(err)
	}
	if !admitted {
		switch recorded.Outcome {
		case store.OperationCommitted:
			return &CredentialReplacement{
				OperationID:          recorded.OperationID,
				CredentialGeneration: recorded.CredentialGeneration,
				CredentialIssued:     recorded.CredentialIssued,
				State:                enrollment.State,
				Revision:             enrollment.EnrollmentRevision,
			}, nil
		case store.OperationRefused:
			return nil, refusedStoreError(recorded)
		case store.OperationExpired:
			return nil, ErrAuthorizationExpired
		case store.OperationFenced:
			return nil, ErrOperationFenced
		default:
			return nil, ErrOperationPending
		}
	}

	recording, credential, err := s.store.ReplaceHostCredential(ctx, operation)
	if err != nil {
		return nil, mapStoreError(err)
	}
	return &CredentialReplacement{
		Credential: IssuedCredential{
			HostID:   recording.HostID,
			MQTTURL:  s.mqttURL,
			Username: "host:" + recording.HostID,
			Password: credential,
		},
		OperationID:          recorded.OperationID,
		CredentialGeneration: recording.CredentialGeneration,
		CredentialIssued:     credential != "",
		State:                recording.State,
		Revision:             recording.EnrollmentRevision,
	}, nil
}

// HandleHostReady advances one pending enrollment after a real HostReady. A
// Host that has no external enrollment (Standalone) is ignored, and a probe
// connection never reaches this path because probes cannot publish a result.
func (s *Service) HandleHostReady(ctx context.Context, hostID string) error {
	if hostID == "" {
		return fmt.Errorf("host id is required")
	}
	_, _, err := s.store.MarkEnrollmentReady(ctx, hostID, s.now())
	if errors.Is(err, store.ErrNotFound) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("mark enrollment ready: %w", err)
	}
	return nil
}

// DisableHost durably disables one enrollment through the legacy bodyless
// path. It returns the applied disabled_at.
func (s *Service) DisableHost(ctx context.Context, hostID string) (time.Time, error) {
	receipt, err := s.RemoveHost(ctx, hostID, RemovalRequest{})
	if err != nil {
		return time.Time{}, err
	}
	return receipt.DisabledAt, nil
}

// RemoveHost durably disables one enrollment, validates the optional frozen
// target, revokes its grants, and returns the pending or enforced receipt. A
// retry with the same removal operation id re-issues exactly the stored
// denials, and enforced_at is recorded only after a matching acknowledgement.
func (s *Service) RemoveHost(ctx context.Context, hostID string, request RemovalRequest) (RemovalReceipt, error) {
	disabled, err := s.store.DisableEnrollmentWithTarget(ctx, hostID, request.RemovalOperationID, request.Target, s.now())
	if err != nil {
		return RemovalReceipt{}, mapStoreError(err)
	}
	receipt := RemovalReceipt{
		State:              "pending",
		RemovalOperationID: disabled.RemovalOperationID,
		Target:             disabled.Target,
		DisabledAt:         disabled.DisabledAt,
		EnforcedAt:         disabled.EnforcedAt,
		Revocations:        disabled.Revocations,
	}
	if disabled.EnforcedAt != nil {
		receipt.State = "enforced"
		return receipt, nil
	}
	if s.enforcer == nil {
		return receipt, fmt.Errorf("%w: no Host enforcement is configured", ErrEnforcementUnavailable)
	}
	if err := s.enforcer.DisableHostScope(ctx, disabled.HostID, disabled.Revocations); err != nil {
		return receipt, fmt.Errorf("%w: %v", ErrEnforcementUnavailable, err)
	}
	enforcedAt := s.now()
	if err := s.store.MarkRemovalEnforced(ctx, disabled.RemovalOperationID, enforcedAt); err != nil {
		return receipt, fmt.Errorf("record removal enforcement: %w", err)
	}
	receipt.State = "enforced"
	receipt.EnforcedAt = &enforcedAt
	return receipt, nil
}

// verifyHostOwnership checks the scoped proof against the current enrollment
// signing key and consumes its nonce once. subjects holds the acceptable
// subject bindings; the first one that verifies wins.
func (s *Service) verifyHostOwnership(ctx context.Context, enrollment store.ExternalEnrollment, proof HostOwnershipProof, purpose string, subjects []string) error {
	if proof.HostID == "" || proof.HostID != enrollment.HostID {
		return fmt.Errorf("%w: host id does not match the enrollment", ErrAuthorizationMismatch)
	}
	if len(enrollment.HostSigningPublicKey) != ed25519.PublicKeySize {
		return fmt.Errorf("%w: the enrollment has no signing key", ErrAuthorizationMismatch)
	}
	now := s.now()
	seen := map[string]bool{}
	var lastErr error
	for _, subjectID := range subjects {
		if seen[subjectID] {
			continue
		}
		seen[subjectID] = true
		if err := VerifyHostOwnershipProof(ed25519.PublicKey(enrollment.HostSigningPublicKey), proof, purpose, subjectID, now); err != nil {
			lastErr = err
			continue
		}
		consumed, err := s.store.ConsumeHostProofNonce(ctx, HostProofNonceHash(proof.Nonce), purpose, now)
		if err != nil {
			return fmt.Errorf("record host proof nonce: %w", err)
		}
		if !consumed {
			return ErrProofReplay
		}
		return nil
	}
	if lastErr == nil {
		lastErr = ErrProofInvalid
	}
	return lastErr
}

// OperationStatus reports one operation without any credential. This is the
// administrative read and is scoped by operation id only.
func (s *Service) OperationStatus(ctx context.Context, operationID string) (*whalereinv1.OperationStatus, error) {
	operation, err := s.store.Operation(ctx, operationID)
	if err != nil {
		return nil, err
	}
	return operationStatus(operation), nil
}

// OperationStatusForHost reports one operation to the original Host after it
// proves possession of the current enrollment signing key and binds the proof
// to the operation id. A foreign operation is refused without authoritative
// state. A missing operation is definitive only when the caller proves the
// authorization deadline has already passed; otherwise it stays pending.
func (s *Service) OperationStatusForHost(ctx context.Context, hostID, operationID string, authorizationExpiresAtUnixMs int64, proof HostOwnershipProof) (*whalereinv1.OperationStatus, error) {
	enrollment, err := s.store.Enrollment(ctx, hostID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrAuthorizationMismatch
		}
		return nil, err
	}
	if err := s.verifyHostOwnership(ctx, enrollment, proof, PurposeOperationStatus, []string{operationID}); err != nil {
		return nil, err
	}
	operation, err := s.store.Operation(ctx, operationID)
	if errors.Is(err, store.ErrNotFound) {
		status := &whalereinv1.OperationStatus{OperationId: operationID}
		if authorizationExpiresAtUnixMs > 0 && s.now().UnixMilli() >= authorizationExpiresAtUnixMs {
			status.Outcome = store.OperationAbsent
			status.Reason = "no_admission_record"
		} else {
			status.Outcome = store.OperationPending
		}
		return status, nil
	}
	if err != nil {
		return nil, err
	}
	if operation.HostID != hostID {
		return nil, ErrAuthorizationMismatch
	}
	return operationStatus(operation), nil
}

func operationStatus(operation store.ExternalOperation) *whalereinv1.OperationStatus {
	status := &whalereinv1.OperationStatus{
		OperationId:           operation.OperationID,
		Purpose:               operation.Purpose,
		State:                 operationState(operation),
		WorkspaceId:           operation.WorkspaceID,
		HostId:                operation.HostID,
		EnrollmentRevision:    operation.EnrollmentRevision,
		AuthorizationId:       operation.AuthorizationID,
		PayloadDigest:         operation.PayloadDigest,
		Outcome:               operation.Outcome,
		Reason:                operation.Reason,
		CredentialGeneration:  operation.CredentialGeneration,
		CredentialIssued:      operation.CredentialIssued,
		CredentialOperationId: operation.CredentialOperationID,
	}
	if operation.Outcome == "" {
		// A legacy row before admission outcomes: keep the last known stored
		// state and report unknown rather than claiming committed.
		status.Outcome = store.OperationPending
	}
	if operation.CommittedAt != nil {
		status.CommittedAtUnixMs = operation.CommittedAt.UnixMilli()
	}
	if operation.UpdatedAt != nil {
		status.UpdatedAtUnixMs = operation.UpdatedAt.UnixMilli()
	}
	return status
}

func operationState(operation store.ExternalOperation) whalereinv1.OperationState {
	switch operation.Outcome {
	case store.OperationCommitted:
		return whalereinv1.OperationState_operation_committed
	case store.OperationRefused, store.OperationExpired, store.OperationFenced:
		return whalereinv1.OperationState_operation_refused
	default:
		return whalereinv1.OperationState_operation_pending
	}
}

func authorizationExpiresAt(auth *whalereinv1.ExternalAuthorization) *time.Time {
	if auth == nil || auth.GetExpiresAtUnixMs() <= 0 {
		return nil
	}
	value := time.UnixMilli(auth.GetExpiresAtUnixMs()).UTC()
	return &value
}

// mapStoreError maps the store's typed operation refusals onto the external
// contract errors while preserving the wire reason.
func mapStoreError(err error) error {
	switch {
	case err == nil:
		return nil
	case errors.Is(err, store.ErrAuthorizationExpired):
		return ErrAuthorizationExpired
	case errors.Is(err, store.ErrOperationFenced):
		return ErrOperationFenced
	case errors.Is(err, store.ErrOperationPending):
		return ErrOperationPending
	case errors.Is(err, store.ErrDuplicateHost):
		return ErrDuplicateHost
	case errors.Is(err, store.ErrEnrollmentDisabled):
		return ErrEnrollmentDisabled
	case errors.Is(err, store.ErrTargetMismatch):
		return ErrTargetMismatch
	case errors.Is(err, store.ErrOperationRefused):
		return ErrOperationConflict
	case errors.Is(err, store.ErrOperationConflict):
		return ErrOperationConflict
	default:
		return err
	}
}

func refusedStoreError(operation store.ExternalOperation) error {
	switch operation.Reason {
	case "duplicate_host":
		return ErrDuplicateHost
	case "enrollment_disabled":
		return ErrEnrollmentDisabled
	default:
		return ErrOperationConflict
	}
}

func enrollmentState(value string) whalereinv1.EnrollmentState {
	switch value {
	case "enrollment_ready":
		return whalereinv1.EnrollmentState_enrollment_ready
	case "enrollment_disabled":
		return whalereinv1.EnrollmentState_enrollment_disabled
	default:
		return whalereinv1.EnrollmentState_enrollment_pending
	}
}
