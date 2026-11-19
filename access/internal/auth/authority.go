package auth

import (
	"context"
	"errors"

	"github.com/whalerein/whalerein/access/internal/store"
)

// Authority reasons for a denied long-lived observation. They are a stable
// wire contract: the client treats every one of them as terminal and never
// reconnects. The reason is carried in the ErrorResponse, so a denial is not
// inferred from a generic 403.
const (
	ReasonSessionRevoked       = "session_revoked"
	ReasonGrantRevoked         = "grant_revoked"
	ReasonHostRemoved          = "host_removed"
	ReasonHostDisabled         = "host_disabled"
	ReasonAuthorityUnavailable = "authority_unavailable"
)

// AuthorityDenial is an authoritative end of authority. It still matches
// ErrForbidden so existing admission and broker callers keep their 403
// behavior, while the HTTP boundary can expose the specific reason.
type AuthorityDenial struct {
	Reason     string
	StatusCode int
	// Cause keeps the underlying store failure visible to callers that
	// classify it (and to logs) without losing the specific reason.
	Cause error
}

func (d *AuthorityDenial) Unwrap() error { return d.Cause }

func (d *AuthorityDenial) Error() string {
	if d.Reason == "" {
		return "authority denied"
	}
	return "authority denied: " + d.Reason
}

// Is keeps a denial a forbidden/unauthorized condition for callers that only
// care that the request must stop.
func (d *AuthorityDenial) Is(target error) bool {
	return target == ErrForbidden
}

// enrollmentReader is the narrow capability used to explain why a Host is no
// longer authorized. It is optional so a test store without enrollments keeps
// the generic grant reason.
type enrollmentReader interface {
	Enrollment(ctx context.Context, hostID string) (store.ExternalEnrollment, error)
}

func (s *Service) hostDenial(ctx context.Context, hostID string) error {
	reader, ok := s.store.(enrollmentReader)
	if !ok {
		return &AuthorityDenial{Reason: ReasonGrantRevoked, StatusCode: 403}
	}
	enrollment, err := reader.Enrollment(ctx, hostID)
	if errors.Is(err, store.ErrNotFound) {
		return &AuthorityDenial{Reason: ReasonHostRemoved, StatusCode: 403}
	}
	if err != nil {
		return &AuthorityDenial{Reason: ReasonAuthorityUnavailable, StatusCode: 503, Cause: err}
	}
	if enrollment.DisabledAt != nil || enrollment.State == "enrollment_disabled" {
		return &AuthorityDenial{Reason: ReasonHostDisabled, StatusCode: 403}
	}
	return &AuthorityDenial{Reason: ReasonGrantRevoked, StatusCode: 403}
}
