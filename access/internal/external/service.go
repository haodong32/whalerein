package external

import (
	"context"
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
}

// Service implements external account mode.
type Service struct {
	trust   Trust
	store   Store
	mqttURL string
	now     func() time.Time
}

// New constructs the service.
func New(trust Trust, data Store, mqttURL string) *Service {
	return &Service{
		trust:   trust,
		store:   data,
		mqttURL: mqttURL,
		now:     func() time.Time { return time.Now().UTC() },
	}
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
	if err == nil && existing.DisabledAt != nil {
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
	return &whalereinv1.HostEnrollmentStatus{
		HostId:               enrollment.HostID,
		WorkspaceId:          enrollment.WorkspaceID,
		EnrollmentRevision:   enrollment.EnrollmentRevision,
		State:                enrollmentState(enrollment.State),
		OperationId:          enrollment.OperationID,
		HostSigningPublicKey: enrollment.HostSigningPublicKey,
		HostNoisePublicKey:   enrollment.HostNoisePublicKey,
	}, nil
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
