package external

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"regexp"
	"strconv"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

const PurposeDeviceSession = "device_session"

// MaxDeviceSessionLifetime bounds regional use when Cloud is unreachable.
const MaxDeviceSessionLifetime = 5 * time.Minute

var sessionField = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,128}$`)
var sessionIDField = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,96}$`)

type DeviceSessionStore interface {
	InstallDeviceSession(context.Context, store.DeviceSession) error
	RevokeDeviceSession(context.Context, string, int64) error
	DeviceSessionByCredential(context.Context, []byte, time.Time) (store.DeviceSession, error)
	DeviceSessionPairingStatus(context.Context, store.DeviceSession, string, string) (string, error)
}

type DeviceSessionEnforcer interface {
	RenewDeviceSession(context.Context, store.DeviceSession) error
	EndDeviceSession(context.Context, string) error
}

func WithDeviceSessions(data DeviceSessionStore, enforcer DeviceSessionEnforcer, required bool) Option {
	return func(s *Service) {
		s.deviceSessions = data
		s.sessionEnforcer = enforcer
		s.requireDeviceSession = required
	}
}

func (s *Service) InstallDeviceSession(ctx context.Context, id string, a *whalereinv1.ExternalAuthorization) error {
	if s.deviceSessions == nil {
		return ErrEnforcementUnavailable
	}
	now := s.now()
	if err := s.trust.Verify(a, PurposeDeviceSession, now); err != nil {
		return err
	}
	if !sessionIDField.MatchString(id) {
		return ErrAuthorizationMismatch
	}
	for _, field := range []string{a.AuthorizationId, a.WorkspaceId, a.RegionId,
		a.DeviceId, a.EnrollmentRevision, a.Precondition} {
		if !sessionField.MatchString(field) {
			return ErrAuthorizationMismatch
		}
	}
	revision, err := strconv.ParseInt(a.ExpectedRevision, 10, 64)
	issuedAt := time.UnixMilli(a.IssuedAtUnixMs)
	expiresAt := time.UnixMilli(a.ExpiresAtUnixMs)
	if err != nil || revision <= 0 || a.OperationId != id || a.AuthVersion != AuthVersion ||
		a.Precondition != s.trust.SessionEnvironment || a.EnrollmentRevision != s.trust.SessionAuthorityRevision ||
		a.IssuedAtUnixMs <= 0 || issuedAt.After(now.Add(s.trust.ClockSkew)) ||
		!expiresAt.After(issuedAt) || expiresAt.Sub(issuedAt) > MaxDeviceSessionLifetime ||
		len(a.DevicePublicKeys) != 1 || len(a.DevicePublicKeys[0]) != 32 ||
		len(a.Nonce) != sha256.Size || a.HostId != "" || len(a.HostPublicKeys) != 0 {
		return ErrAuthorizationMismatch
	}

	fingerprint := sha256.Sum256(a.DevicePublicKeys[0])
	permission := store.DeviceSession{
		ID: id, WorkspaceID: a.WorkspaceId, RegionID: a.RegionId, DeviceID: a.DeviceId,
		KeyFingerprint: hex.EncodeToString(fingerprint[:]), AuthorityRevision: a.EnrollmentRevision,
		CredentialHash: a.Nonce, Revision: revision, ExpiresAt: expiresAt,
	}
	if err := s.deviceSessions.InstallDeviceSession(ctx, permission); err != nil {
		return err
	}
	if s.sessionEnforcer == nil {
		return ErrEnforcementUnavailable
	}
	if err := s.sessionEnforcer.RenewDeviceSession(ctx, permission); err != nil {
		return fmt.Errorf("%w: %v", ErrEnforcementUnavailable, err)
	}
	return nil
}

func (s *Service) RevokeDeviceSession(ctx context.Context, id string, revision int64) error {
	if s.deviceSessions == nil || s.sessionEnforcer == nil {
		return ErrEnforcementUnavailable
	}
	if !sessionIDField.MatchString(id) || revision <= 0 {
		return ErrAuthorizationMismatch
	}
	if err := s.deviceSessions.RevokeDeviceSession(ctx, id, revision); err != nil {
		return err
	}
	if err := s.sessionEnforcer.EndDeviceSession(ctx, id); err != nil {
		return fmt.Errorf("%w: %v", ErrEnforcementUnavailable, err)
	}
	return nil
}

type deviceSessionCredentialKey struct{}

// WithDeviceSessionCredential passes the bearer digest to portable pairing.
func WithDeviceSessionCredential(ctx context.Context, credential string) context.Context {
	return context.WithValue(ctx, deviceSessionCredentialKey{}, HashBytes([]byte(credential)))
}

func (s *Service) DeviceSessionPairingStatus(ctx context.Context, credential, hostID, claimID string) (string, error) {
	if s.deviceSessions == nil {
		return "", ErrEnforcementUnavailable
	}
	permission, err := s.deviceSessions.DeviceSessionByCredential(ctx, HashBytes([]byte(credential)), s.now())
	if errors.Is(err, store.ErrNotFound) {
		return "", ErrAuthorizationMismatch
	}
	if err != nil {
		return "", err
	}
	if permission.AuthorityRevision != s.trust.SessionAuthorityRevision {
		return "", ErrAuthorizationMismatch
	}
	return s.deviceSessions.DeviceSessionPairingStatus(ctx, permission, hostID, claimID)
}
