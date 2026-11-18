package auth

import (
	"context"
	"errors"
	"regexp"
	"time"

	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/store"
)

var managedSessionID = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,96}$`)

type DeviceSessionStore interface {
	DeviceSessionByCredential(context.Context, []byte, time.Time) (store.DeviceSession, error)
	ActiveDeviceSession(context.Context, string, time.Time) (store.DeviceSession, error)
	DeviceSessionHosts(context.Context, store.DeviceSession) (map[string]bool, error)
}

func (s *Service) sessionHosts(ctx context.Context, identity Identity) (store.DeviceSession, map[string]bool, error) {
	permission, err := s.sessions.ActiveDeviceSession(ctx, identity.SessionID, s.now())
	if errors.Is(err, store.ErrNotFound) || (err == nil && (permission.DeviceID != identity.DeviceID ||
		permission.AuthorityRevision != s.cfg.SessionAuthorityRevision)) {
		return store.DeviceSession{}, nil, ErrUnauthorized
	}
	if err != nil {
		return store.DeviceSession{}, nil, err
	}
	hosts, err := s.sessions.DeviceSessionHosts(ctx, permission)
	return permission, hosts, err
}

// ManagedBrokerPolicy refreshes existing broker sessions from durable authority.
func (s *Service) ManagedBrokerPolicy(ctx context.Context, sessionID, deviceID string) (policy.Connection, error) {
	connection, err := s.BrokerPolicy(ctx, BrokerIdentity{Kind: KindDevice, DeviceID: deviceID, SessionID: sessionID})
	if errors.Is(err, ErrUnauthorized) {
		return policy.Connection{}, nil
	}
	return connection, err
}
