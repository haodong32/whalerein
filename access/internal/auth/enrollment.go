package auth

import (
	"context"
	"fmt"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

// DisableHostScope applies the realtime side of a durable Host enrollment
// disable: local admission is fenced for every revoked grant and the line
// denies the current grant incarnation and closes the Host broker session.
//
// The durable disable and grant revocation already committed before this call.
// It is idempotent because the line control operations are keyed by the
// current grant generation and a missing broker session is not an error.
func (s *Service) DisableHostScope(ctx context.Context, hostID string, grantGenerations map[string]uint64) error {
	if hostID == "" {
		return ErrBadRequest
	}
	for deviceID := range grantGenerations {
		s.admission.Deny(hostID, deviceID)
	}
	if err := s.enforcer.RevokeHost(ctx, hostID, grantGenerations); err != nil {
		return fmt.Errorf("revoke host policy: %w", err)
	}
	s.logger.Info("host enrollment disabled", "source", "enrollment_admin",
		"trace_id", diagnostics.TraceID(ctx), "host", s.masker.Host(hostID),
		"revoked_grants", len(grantGenerations))
	return nil
}
