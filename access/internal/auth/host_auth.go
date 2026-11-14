package auth

import (
	"context"
	"crypto/ed25519"
	"errors"
	"fmt"
	"strconv"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

const (
	// HostAuthInvitation and HostAuthRevoke are the purpose strings covered by
	// a host Ed25519 signature. The message is
	// whalerein/v1/host-auth|{purpose}|{host_id}|{device_id}|{signed_at_unix_ms}.
	// Invitation leaves device_id empty.
	HostAuthInvitation = "invitation"
	HostAuthRevoke     = "revoke"

	hostAuthPrefix = "whalerein/v1/host-auth"
	hostAuthPast   = 2 * time.Minute
	hostAuthFuture = time.Minute
)

// HostProof is a host-signed request. HostID is the only host the signature
// can authorize.
type HostProof struct {
	Purpose        string
	HostID         string
	DeviceID       string
	SignedAtUnixMs int64
	Signature      []byte
}

func hostAuthMessage(purpose, hostID, deviceID string, signedAt int64) []byte {
	return []byte(fmt.Sprintf("%s|%s|%s|%s|%d", hostAuthPrefix, purpose, hostID, deviceID, signedAt))
}

// RememberHostAuthorizationKey stores the Ed25519 public key from an
// authenticated HostReady. The first key for a host is kept.
func (s *Service) RememberHostAuthorizationKey(ctx context.Context, hostID string, publicKey []byte) error {
	if hostID == "" || len(publicKey) != ed25519.PublicKeySize {
		return ErrBadRequest
	}
	key := append([]byte(nil), publicKey...)
	if err := s.store.SetHostAuthorizationKey(ctx, hostID, key); err != nil {
		return fmt.Errorf("remember host authorization key: %w", err)
	}
	return nil
}

// AuthenticateHostRequest checks a host signature against the key registered
// for proof.HostID. A signature outside the acceptance window is expired. A
// signature already seen inside that window is rejected.
func (s *Service) AuthenticateHostRequest(ctx context.Context, proof HostProof) error {
	if proof.HostID == "" || (proof.Purpose != HostAuthInvitation && proof.Purpose != HostAuthRevoke) {
		return ErrUnauthorized
	}
	if proof.Purpose == HostAuthRevoke && proof.DeviceID == "" {
		return ErrUnauthorized
	}
	if proof.Purpose == HostAuthInvitation && proof.DeviceID != "" {
		return ErrUnauthorized
	}
	signedAt := time.UnixMilli(proof.SignedAtUnixMs)
	now := s.now()
	if signedAt.Before(now.Add(-hostAuthPast)) || signedAt.After(now.Add(hostAuthFuture)) {
		return ErrSignatureExpired
	}
	if len(proof.Signature) != ed25519.SignatureSize {
		return ErrUnauthorized
	}
	publicKey, err := s.store.HostAuthorizationKey(ctx, proof.HostID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrUnauthorized
		}
		return fmt.Errorf("authenticate host request: %w", err)
	}
	message := hostAuthMessage(proof.Purpose, proof.HostID, proof.DeviceID, proof.SignedAtUnixMs)
	if !ed25519.Verify(ed25519.PublicKey(publicKey), message, proof.Signature) {
		return ErrUnauthorized
	}
	if !s.rememberSignature(proof) {
		return ErrUnauthorized
	}
	return nil
}

func (s *Service) rememberSignature(proof HostProof) bool {
	key := proof.Purpose + "\n" + proof.HostID + "\n" + proof.DeviceID + "\n" + strconv.FormatInt(proof.SignedAtUnixMs, 10)
	cutoff := s.now().Add(-hostAuthPast).UnixMilli()
	s.seenMu.Lock()
	defer s.seenMu.Unlock()
	for seenKey, signedAt := range s.seen {
		if signedAt < cutoff {
			delete(s.seen, seenKey)
		}
	}
	if _, ok := s.seen[key]; ok {
		return false
	}
	s.seen[key] = proof.SignedAtUnixMs
	return true
}

// RevokeHostGrant revokes one host's grant and disconnects the device so a
// cached broker ACL drops. Other hosts' grants are left in place.
func (s *Service) RevokeHostGrant(ctx context.Context, hostID, deviceID string) error {
	at := s.now()
	// Fence realtime admission before the durable write so a concurrent
	// dispatch cannot use the grant being revoked.
	s.admission.Deny(hostID, deviceID)
	if err := s.store.RevokeGrant(ctx, hostID, deviceID, at); err != nil {
		return err
	}
	if err := s.enforcer.RevokeGrant(ctx, hostID, deviceID); err != nil {
		return fmt.Errorf("revoke grant policy: %w", err)
	}
	s.logger.Info("revocation committed", "source", "host_settings", "trace_id", diagnostics.TraceID(ctx), "host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "revoked_at_unix_ms", at.UnixMilli())
	if err := s.transport.DisconnectDevice(ctx, deviceID); err != nil {
		s.logger.Warn("host grant disconnect failed",
			"host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "error", err.Error())
	}
	if s.presence.IsHostOnline(ctx, hostID) {
		return s.notifyRevokedGrant(ctx, hostID, deviceID)
	}
	return nil
}
