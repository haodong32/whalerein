package auth

import (
	"context"
	"errors"

	"github.com/whalerein/whalerein/access/internal/store"
)

// grantScope freezes one grant authorization for enforcement. The revocation
// generation is advanced by one because the durable revoke commits before the
// Broker fence is installed; the frozen generation still names the exact
// authorization that was revoked.
func grantScope(grant store.AdmissionGrant, hostID, deviceID string) store.RevocationScope {
	return store.RevocationScope{
		HostID:               hostID,
		DeviceID:             deviceID,
		EnrollmentGeneration: grant.EnrollmentGeneration,
		Grants: []store.ScopeGrant{{
			HostID:                     hostID,
			DeviceID:                   deviceID,
			EnrollmentGeneration:       grant.EnrollmentGeneration,
			GrantGeneration:            grant.GrantGeneration,
			RevocationGeneration:       grant.RevocationGeneration + 1,
			DeviceRevocationGeneration: grant.DeviceRevocationGeneration,
		}},
	}
}

// freezeGrantConnections attaches the admitted device connections to every
// frozen grant of the scope. The list is deduplicated and sorted by the line
// manager, which drops references from a previous broker epoch.
func (s *Service) freezeGrantConnections(scope store.RevocationScope, deviceID string) store.RevocationScope {
	connectionPresence, ok := s.presence.(ConnectionPresence)
	if !ok || deviceID == "" {
		return scope
	}
	connections := connectionPresence.DeviceConnections(deviceID)
	for index := range scope.Grants {
		scope.Grants[index].Connections = append([]store.ScopeConnection(nil), connections...)
	}
	return scope
}

// freezeHostConnection attaches the admitted Host connection to a Host-disable
// scope.
func (s *Service) freezeHostConnection(scope store.RevocationScope) store.RevocationScope {
	connectionPresence, ok := s.presence.(ConnectionPresence)
	if !ok || scope.HostID == "" {
		return scope
	}
	connection, live := connectionPresence.HostScopeConnection(scope.HostID)
	if live {
		scope.HostConnection = &connection
	}
	return scope
}

// currentEnrollmentGeneration reads the current managed enrollment generation.
// Standalone Hosts and unknown enrollments report zero.
func (s *Service) currentEnrollmentGeneration(ctx context.Context, hostID string) (uint64, error) {
	enrollment, err := s.store.Enrollment(ctx, hostID)
	if errors.Is(err, store.ErrNotFound) {
		return 0, nil
	}
	if err != nil {
		return 0, err
	}
	if enrollment.Generation <= 0 {
		return 0, nil
	}
	return uint64(enrollment.Generation), nil
}

// enforceFrozenGrant applies one frozen grant scope. When the enrollment was
// already replaced, the old scope owns no current connection and its grant row
// is left alone; the Broker still receives the generation fence so a queued or
// delayed installation cannot restore the old authorization.
func (s *Service) enforceFrozenGrant(ctx context.Context, scope store.RevocationScope, revoke bool) (store.EnforcementReceipt, error) {
	frozen := scope.Frozen()
	current, err := s.currentEnrollmentGeneration(ctx, frozen.HostID)
	if err != nil {
		return store.EnforcementReceipt{Scope: frozen}, err
	}
	replaced := frozen.EnrollmentReplaced(current)
	if !replaced {
		frozen = s.freezeGrantConnections(frozen, frozen.DeviceID)
	}
	if revoke && !replaced {
		for _, grant := range frozen.Grants {
			revoked, err := s.store.RevokeGrantGeneration(ctx, grant.HostID, grant.DeviceID, grant.GrantGeneration, s.now())
			if err != nil {
				return store.EnforcementReceipt{Scope: frozen}, err
			}
			if !revoked {
				// A newer grant generation replaced the frozen one after it
				// was captured. Leave the newer authorization, its local
				// admission entry and its connection untouched.
				continue
			}
			s.admission.Deny(grant.HostID, grant.DeviceID)
		}
	}
	receipt, err := s.enforcer.RevokeGrant(ctx, frozen)
	if err != nil {
		return store.EnforcementReceipt{Scope: frozen}, err
	}
	if !replaced && s.presence.IsHostOnline(ctx, frozen.HostID) {
		if err := s.notifyRevokedGrant(ctx, frozen.HostID, frozen.DeviceID); err != nil {
			return receipt, err
		}
	}
	return receipt, nil
}
