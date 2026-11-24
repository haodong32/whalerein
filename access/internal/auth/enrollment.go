package auth

import (
	"context"
	"errors"
	"fmt"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

// DisableHostScope applies the realtime side of a durable Host enrollment
// disable: local admission is fenced for every revoked grant and the Broker
// denies exactly the frozen grant generations and closes the admitted Host
// connection.
//
// The durable disable and grant revocation already committed before this call.
// It is idempotent because every control operation is keyed by the frozen
// generation and an absent connection counts as enforced. When the enrollment
// was already replaced by a re-add, the old scope owns no current connection.
func (s *Service) DisableHostScope(ctx context.Context, scope store.RevocationScope) (store.EnforcementReceipt, error) {
	if scope.HostID == "" {
		return store.EnforcementReceipt{}, ErrBadRequest
	}
	frozen := scope.Frozen()
	current, err := s.currentEnrollmentGeneration(ctx, frozen.HostID)
	if err != nil {
		return store.EnforcementReceipt{Scope: frozen}, fmt.Errorf("disable host scope: %w", err)
	}
	replaced := frozen.EnrollmentReplaced(current)
	if !replaced {
		// The durable removal only stores the grant generation. Read the
		// post-commit revocation generations so the Broker fence covers the
		// ACLs that were installed for the revoked authorization.
		for index := range frozen.Grants {
			grant, err := s.store.AdmissionGrantFor(ctx, frozen.HostID, frozen.Grants[index].DeviceID)
			if errors.Is(err, store.ErrNotFound) {
				continue
			}
			if err != nil {
				return store.EnforcementReceipt{Scope: frozen}, fmt.Errorf("disable host scope: %w", err)
			}
			if grant.GrantGeneration != frozen.Grants[index].GrantGeneration {
				continue
			}
			frozen.Grants[index].RevocationGeneration = grant.RevocationGeneration
			frozen.Grants[index].DeviceRevocationGeneration = grant.DeviceRevocationGeneration
			frozen.Grants[index].EnrollmentGeneration = grant.EnrollmentGeneration
		}
		for _, grant := range frozen.Grants {
			s.admission.Deny(frozen.HostID, grant.DeviceID)
			frozen.Grants = s.freezeGrantConnectionsFor(frozen.Grants, grant.DeviceID)
		}
		frozen = s.freezeHostConnection(frozen)
	}
	receipt, err := s.enforcer.RevokeHost(ctx, frozen)
	if err != nil {
		return receipt, fmt.Errorf("revoke host policy: %w", err)
	}
	s.logger.Info("host enrollment disabled", "source", "enrollment_admin",
		"trace_id", diagnostics.TraceID(ctx), "host", s.masker.Host(frozen.HostID),
		"revoked_grants", len(frozen.Grants), "replaced", replaced)
	return receipt, nil
}

// freezeGrantConnectionsFor attaches the admitted connections of one device to
// every frozen grant for that device without duplicating work between grants.
func (s *Service) freezeGrantConnectionsFor(grants []store.ScopeGrant, deviceID string) []store.ScopeGrant {
	connectionPresence, ok := s.presence.(ConnectionPresence)
	if !ok || deviceID == "" {
		return grants
	}
	connections := connectionPresence.DeviceConnections(deviceID)
	for index := range grants {
		if grants[index].DeviceID != deviceID {
			continue
		}
		grants[index].Connections = append([]store.ScopeConnection(nil), connections...)
	}
	return grants
}

// EnforceRevocationScope applies one already-frozen grant scope, for example a
// pending console revocation whose durable fence was committed earlier. It
// revokes only the frozen grant generation and leaves a newer authorization
// untouched.
func (s *Service) EnforceRevocationScope(ctx context.Context, scope store.RevocationScope) (store.EnforcementReceipt, error) {
	if scope.HostID == "" || scope.DeviceID == "" || len(scope.Grants) == 0 {
		return store.EnforcementReceipt{Scope: scope}, ErrBadRequest
	}
	return s.enforceFrozenGrant(ctx, scope, true)
}
