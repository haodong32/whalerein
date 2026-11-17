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
	EnrollHost(ctx context.Context, operation store.ExternalOperation, enrollment store.ExternalEnrollment) (store.ExternalEnrollment, string, bool, error)
	Enrollment(ctx context.Context, hostID string) (store.ExternalEnrollment, error)
	Operation(ctx context.Context, operationID string) (store.ExternalOperation, error)
	NextEnrollmentRevision(ctx context.Context, workspaceID string) (string, error)
	MarkEnrollmentReady(ctx context.Context, hostID string, at time.Time) (store.ExternalEnrollment, bool, error)
	DisableEnrollment(ctx context.Context, hostID string, at time.Time) (store.DisabledEnrollment, error)
	ReplaceHostCredential(ctx context.Context, operation store.ExternalOperation) (store.ExternalEnrollment, string, error)
	ConsumeHostProofNonce(ctx context.Context, nonceHash []byte, purpose string, at time.Time) (bool, error)
}

// HostEnforcer applies the realtime side of a durable Host disable: local
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

// CredentialReplacement is the successful result of a scoped replacement.
type CredentialReplacement struct {
	Credential IssuedCredential
	State      string
	Revision   string
}

// ErrEnforcementUnavailable reports that the durable disable committed but the
// live broker/line enforcement could not be confirmed.
var ErrEnforcementUnavailable = errors.New("host enforcement is unavailable")

// Option configures the external service.
type Option func(*Service)

// WithHostEnforcer wires the realtime enforcement used by Host disable.
func WithHostEnforcer(enforcer HostEnforcer) Option {
	return func(service *Service) { service.enforcer = enforcer }
}

// Service implements external account mode.
type Service struct {
	trust    Trust
	store    Store
	mqttURL  string
	enforcer HostEnforcer
	now      func() time.Time
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
// It verifies the portable authorization and the Host's proof of possession
// before it commits anything. A retry with the same authorization and payload
// returns the recorded status and no credential; a different payload is a
// conflict. The broker credential is returned only by the committing call.
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
		operationID = store.NewOperationID()
	}
	existing, err := s.store.Enrollment(ctx, request.GetHostId())
	if err == nil && (existing.DisabledAt != nil || existing.State == "enrollment_disabled") {
		return nil, ErrEnrollmentDisabled
	}
	if err != nil && err != store.ErrNotFound {
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
		OperationID:        operationID,
		AuthorizationID:    auth.GetAuthorizationId(),
		Purpose:            PurposeHostEnrollment,
		WorkspaceID:        auth.GetWorkspaceId(),
		RegionID:           auth.GetRegionId(),
		HostID:             request.GetHostId(),
		PayloadDigest:      Digest(request),
		State:              "operation_committed",
		EnrollmentRevision: revision,
	}
	enrollment := store.ExternalEnrollment{
		HostID:               request.GetHostId(),
		WorkspaceID:          auth.GetWorkspaceId(),
		EnrollmentRevision:   revision,
		HostSigningPublicKey: request.GetHostSigningPublicKey(),
		HostNoisePublicKey:   request.GetHostNoisePublicKey(),
		State:                "enrollment_pending",
	}

	recorded, credential, committed, err := s.store.EnrollHost(ctx, operation, enrollment)
	if err != nil {
		return nil, err
	}

	response := &whalereinv1.HostEnrollmentResponse{
		Status: &whalereinv1.HostEnrollmentStatus{
			HostId:               recorded.HostID,
			WorkspaceId:          recorded.WorkspaceID,
			EnrollmentRevision:   recorded.EnrollmentRevision,
			State:                enrollmentState(recorded.State),
			OperationId:          recorded.OperationID,
			HostSigningPublicKey: recorded.HostSigningPublicKey,
			HostNoisePublicKey:   recorded.HostNoisePublicKey,
		},
	}
	if committed {
		response.Credential = &whalereinv1.HostCredential{
			HostId:   recorded.HostID,
			MqttUrl:  s.mqttURL,
			Username: "host:" + recorded.HostID,
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
	return &whalereinv1.HostEnrollmentStatus{
		HostId:               enrollment.HostID,
		WorkspaceId:          enrollment.WorkspaceID,
		EnrollmentRevision:   enrollment.EnrollmentRevision,
		State:                enrollmentState(enrollment.State),
		OperationId:          enrollment.OperationID,
		HostSigningPublicKey: enrollment.HostSigningPublicKey,
		HostNoisePublicKey:   enrollment.HostNoisePublicKey,
	}
}

// EnrollmentStatusForHost reports one enrollment to the original Host after it
// proves possession of the current enrollment signing key. The result never
// contains a credential or transport password.
func (s *Service) EnrollmentStatusForHost(ctx context.Context, hostID string, proof HostOwnershipProof) (*whalereinv1.HostEnrollmentStatus, error) {
	enrollment, err := s.store.Enrollment(ctx, hostID)
	if err != nil {
		return nil, err
	}
	if enrollment.DisabledAt != nil || enrollment.State == "enrollment_disabled" {
		return nil, ErrEnrollmentDisabled
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
// credential is returned exactly once; replays conflict.
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
	recorded, credential, err := s.store.ReplaceHostCredential(ctx, store.ExternalOperation{
		OperationID:        operationID,
		AuthorizationID:    auth.GetAuthorizationId(),
		Purpose:            PurposeHostCredentialRecovery,
		WorkspaceID:        auth.GetWorkspaceId(),
		RegionID:           auth.GetRegionId(),
		HostID:             hostID,
		PayloadDigest:      HashBytes([]byte(CanonicalInput(auth))),
		State:              "operation_committed",
		EnrollmentRevision: enrollment.EnrollmentRevision,
	})
	if err != nil {
		return nil, err
	}
	return &CredentialReplacement{
		Credential: IssuedCredential{
			HostID:   recorded.HostID,
			MQTTURL:  s.mqttURL,
			Username: "host:" + recorded.HostID,
			Password: credential,
		},
		State:    recorded.State,
		Revision: recorded.EnrollmentRevision,
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

// DisableHost durably disables one enrollment and revokes its grants before it
// closes the live broker scope. It is idempotent.
func (s *Service) DisableHost(ctx context.Context, hostID string) (time.Time, error) {
	disabled, err := s.store.DisableEnrollment(ctx, hostID, s.now())
	if err != nil {
		return time.Time{}, err
	}
	if s.enforcer == nil {
		return time.Time{}, fmt.Errorf("%w: no Host enforcement is configured", ErrEnforcementUnavailable)
	}
	if err := s.enforcer.DisableHostScope(ctx, hostID, disabled.Revocations); err != nil {
		return time.Time{}, fmt.Errorf("%w: %v", ErrEnforcementUnavailable, err)
	}
	return disabled.DisabledAt, nil
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

// OperationStatus reports one operation without any credential.
func (s *Service) OperationStatus(ctx context.Context, operationID string) (*whalereinv1.OperationStatus, error) {
	operation, err := s.store.Operation(ctx, operationID)
	if err != nil {
		return nil, err
	}
	status := &whalereinv1.OperationStatus{
		OperationId:        operation.OperationID,
		Purpose:            operation.Purpose,
		State:              whalereinv1.OperationState_operation_committed,
		WorkspaceId:        operation.WorkspaceID,
		HostId:             operation.HostID,
		EnrollmentRevision: operation.EnrollmentRevision,
		AuthorizationId:    operation.AuthorizationID,
		PayloadDigest:      operation.PayloadDigest,
	}
	if operation.CommittedAt != nil {
		status.CommittedAtUnixMs = operation.CommittedAt.UnixMilli()
	}
	return status, nil
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
