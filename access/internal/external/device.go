package external

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// DevicePairing runs the common Noise handshake after portable authorization.
type DevicePairing interface {
	ClaimCloud(context.Context, *whalereinv1.PairingClaimRequest, pairing.CloudScope) (*whalereinv1.PairingClaimResponse, error)
}

// DeviceGrantStore observes and revokes one Cloud-scoped regional grant.
type DeviceGrantStore interface {
	CloudDeviceGrantStatus(context.Context, string, string, string) (string, error)
	BeginCloudDeviceRevocation(context.Context, string, string, string, time.Time) (bool, error)
	CompleteCloudDeviceRevocation(context.Context, string, string, time.Time) error
}

// DeviceGrantEnforcer applies the durable grant revocation to live clients.
type DeviceGrantEnforcer interface {
	RevokeHostGrant(context.Context, string, string) error
}

// WithDeviceGrants wires the pairing and grant lifecycle services.
func WithDeviceGrants(claims DevicePairing, grants DeviceGrantStore, enforcer DeviceGrantEnforcer) Option {
	return func(s *Service) {
		s.devicePairing = claims
		s.deviceGrants = grants
		s.deviceEnforcer = enforcer
	}
}

// PairDevice verifies the account authorization before forwarding a Noise claim.
func (s *Service) PairDevice(ctx context.Context, authorization *whalereinv1.ExternalAuthorization, claim *whalereinv1.PairingClaimRequest) (*whalereinv1.PairingClaimResponse, error) {
	if s.devicePairing == nil || s.deviceGrants == nil {
		return nil, ErrEnforcementUnavailable
	}
	now := s.now()
	var permission store.DeviceSession
	if s.requireDeviceSession || (s.trust.SessionAuthorityRevision != "" && authorization.GetPrecondition() != "") {
		digest, _ := ctx.Value(deviceSessionCredentialKey{}).([]byte)
		if len(digest) != sha256.Size || s.deviceSessions == nil {
			return nil, ErrAuthorizationMismatch
		}
		var err error
		permission, err = s.deviceSessions.DeviceSessionByCredential(ctx, digest, now)
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrAuthorizationMismatch
		}
		if err != nil {
			return nil, err
		}
	}

	if err := s.trust.Verify(authorization, PurposeDevicePairing, now); err != nil {
		return nil, err
	}
	a := authorization
	if a.AuthVersion != AuthVersion || a.IssuedAtUnixMs <= 0 ||
		time.UnixMilli(a.IssuedAtUnixMs).After(now.Add(s.trust.ClockSkew)) ||
		a.ExpiresAtUnixMs <= a.IssuedAtUnixMs || len(a.Nonce) < 16 || len(a.DevicePublicKeys) != 1 ||
		len(a.DevicePublicKeys[0]) != 32 || len(a.HostPublicKeys) != 2 ||
		a.DeviceId != claim.GetDeviceId() || a.OperationId != claim.GetClaimId() {
		return nil, ErrAuthorizationMismatch
	}
	for _, value := range []string{a.AuthorizationId, a.Issuer, a.Audience, a.WorkspaceId, a.RegionId,
		a.EnrollmentRevision, a.HostId, a.DeviceId, a.OperationId} {
		if value == "" || strings.ContainsAny(value, "\r\n") {
			return nil, ErrAuthorizationMismatch
		}
	}
	if permission.ID != "" {
		fingerprint := sha256.Sum256(a.DevicePublicKeys[0])
		if permission.ID != a.Precondition || permission.DeviceID != a.DeviceId ||
			permission.WorkspaceID != a.WorkspaceId || permission.RegionID != a.RegionId ||
			permission.KeyFingerprint != hex.EncodeToString(fingerprint[:]) ||
			permission.AuthorityRevision != s.trust.SessionAuthorityRevision {
			return nil, ErrAuthorizationMismatch
		}
	}
	host, err := s.store.Enrollment(ctx, a.HostId)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrAuthorizationMismatch
		}
		return nil, err
	}
	if host.DisabledAt != nil || host.State != "enrollment_ready" {
		return nil, ErrEnrollmentDisabled
	}
	if host.WorkspaceID != a.WorkspaceId || !contains(a.HostPublicKeys, host.HostSigningPublicKey) ||
		!contains(a.HostPublicKeys, host.HostNoisePublicKey) {
		return nil, ErrAuthorizationMismatch
	}
	if host.EnrollmentRevision != a.EnrollmentRevision {
		return nil, ErrStaleRevision
	}
	if permission.ID != "" {
		ctx = store.WithDeviceSessionAuthority(ctx, s.trust.SessionAuthorityRevision)
	}
	return s.devicePairing.ClaimCloud(ctx, claim, pairing.CloudScope{
		Issuer: a.Issuer, Audience: a.Audience, WorkspaceID: a.WorkspaceId, RegionID: a.RegionId,
		Revision: a.EnrollmentRevision, HostID: a.HostId, DeviceID: a.DeviceId,
		OperationID: a.OperationId, SessionID: permission.ID, HostKey: host.HostNoisePublicKey, DeviceKey: a.DevicePublicKeys[0],
		ExpiresAt: time.UnixMilli(a.ExpiresAtUnixMs),
	})
}

func (s *Service) DeviceGrantStatus(ctx context.Context, workspaceID, hostID, deviceID string) (string, error) {
	if s.deviceGrants == nil {
		return "", ErrEnforcementUnavailable
	}
	return s.deviceGrants.CloudDeviceGrantStatus(ctx, workspaceID, hostID, deviceID)
}

// RevokeDeviceGrant leaves a pending fence if live enforcement is unavailable.
func (s *Service) RevokeDeviceGrant(ctx context.Context, workspaceID, hostID, deviceID string) error {
	if s.deviceGrants == nil || s.deviceEnforcer == nil {
		return ErrEnforcementUnavailable
	}
	hasGrant, err := s.deviceGrants.BeginCloudDeviceRevocation(ctx, workspaceID, hostID, deviceID, s.now())
	if err != nil {
		return err
	}
	if hasGrant {
		if err := s.deviceEnforcer.RevokeHostGrant(ctx, hostID, deviceID); err != nil {
			return fmt.Errorf("%w: %v", ErrEnforcementUnavailable, err)
		}
	}
	return s.deviceGrants.CompleteCloudDeviceRevocation(ctx, hostID, deviceID, s.now())
}
