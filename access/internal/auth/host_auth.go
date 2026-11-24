package auth

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
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
	// Host authorization accepts a signature two minutes in the past through
	// one minute in the future. The Host runtime declares the identical window
	// in host/runtime/src/contracts/time.js; the shared vector file pins both.
	hostAuthPast   = 2 * time.Minute
	hostAuthFuture = time.Minute
)

// HostSignatureReplayStore is the optional durable replay guard. A store that
// implements it allows each verified Host signature exactly once across every
// Access replica sharing the database. A store without it falls back to the
// process-local guard, which is correct for one replica and covered by tests.
type HostSignatureReplayStore interface {
	ConsumeHostSignature(ctx context.Context, signatureHash []byte, hostID, purpose, deviceID string, signedAtUnixMs int64, at time.Time) (bool, error)
}

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
	if err := s.rememberSignatureOnce(ctx, proof); err != nil {
		return err
	}
	return nil
}

// rememberSignatureOnce consumes a verified signature exactly once. The
// durable guard is authoritative when the store supports it, so a captured
// signature cannot be replayed against another replica.
func (s *Service) rememberSignatureOnce(ctx context.Context, proof HostProof) error {
	if guard, ok := s.store.(HostSignatureReplayStore); ok {
		digest := sha256.Sum256(proof.Signature)
		consumed, err := guard.ConsumeHostSignature(ctx, digest[:], proof.HostID, proof.Purpose, proof.DeviceID, proof.SignedAtUnixMs, s.now())
		if err != nil {
			return fmt.Errorf("record host signature: %w", err)
		}
		if !consumed {
			return ErrUnauthorized
		}
		return nil
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

// RevokeHostGrant revokes one host's grant and enforces exactly that frozen
// grant scope on the Broker: the admitted connection instances are denied and
// closed only while they still carry the revoked generation. Other hosts'
// grants are left in place.
func (s *Service) RevokeHostGrant(ctx context.Context, hostID, deviceID string) error {
	if hostID == "" || deviceID == "" {
		return ErrBadRequest
	}
	at := s.now()
	// Freeze the authorization before the durable write. Enforcement must never
	// re-read a newer generation after a retry or a concurrent re-pair.
	grant, err := s.store.AdmissionGrantFor(ctx, hostID, deviceID)
	if err != nil {
		return err
	}
	scope := grantScope(grant, hostID, deviceID)
	// Fence realtime admission before the durable write so a concurrent
	// dispatch cannot use the grant being revoked.
	finishRevoke := s.admission.BeginGrantRevoke(hostID, deviceID)
	err = s.store.RevokeGrant(ctx, hostID, deviceID, at)
	finishRevoke()
	if err != nil {
		return err
	}
	if _, err := s.enforceFrozenGrant(ctx, scope, false); err != nil {
		return fmt.Errorf("revoke grant policy: %w", err)
	}
	s.logger.Info("revocation committed", "source", "host_settings", "trace_id", diagnostics.TraceID(ctx), "host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "revoked_at_unix_ms", at.UnixMilli())
	return nil
}
