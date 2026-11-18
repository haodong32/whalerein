package pairing

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// CloudScope is a verified account authorization for one exact Noise exchange.
type CloudScope struct {
	Issuer, Audience, WorkspaceID, RegionID, Revision string
	HostID, DeviceID, OperationID                     string
	HostKey, DeviceKey                                []byte
	ExpiresAt                                         time.Time
}

func (s CloudScope) tokenHash() []byte {
	digest := sha256.Sum256([]byte(strings.Join([]string{
		"whalerein/v1/cloud-pairing", s.Issuer, s.Audience, s.WorkspaceID,
		s.RegionID, s.Revision, s.HostID, s.DeviceID, s.OperationID,
		hex.EncodeToString(s.HostKey), hex.EncodeToString(s.DeviceKey),
	}, "\n")))
	// The database namespace is disjoint from all one-time invitation hashes.
	// The Host receives the untagged hash as its Noise prologue input.
	return append([]byte{store.CloudPairingHashTag}, digest[:]...)
}

func pairingPrologueHash(tokenHash []byte) string {
	if store.IsCloudPairingHash(tokenHash) {
		tokenHash = tokenHash[1:]
	}
	return hex.EncodeToString(tokenHash)
}

// ClaimCloud reuses the invitation proof and activation machinery after the
// external account service verifies the portable authorization independently.
func (s *Service) ClaimCloud(ctx context.Context, request *whalereinv1.PairingClaimRequest, scope CloudScope) (*whalereinv1.PairingClaimResponse, error) {
	if request.GetToken() != "" || request.GetClaimId() != scope.OperationID || request.GetDeviceId() != scope.DeviceID {
		return nil, ErrInvalidClaim
	}
	return s.claim(ctx, request, &scope)
}

func (s *Service) prepareCloudClaim(ctx context.Context, scope *CloudScope, hash []byte, now time.Time) error {
	if err := s.store.CreatePairingToken(ctx, hash, scope.HostID, scope.ExpiresAt, now); err != nil && !errors.Is(err, store.ErrConflict) {
		return ErrInternal
	}
	return nil
}

func (scope *CloudScope) matchesHost(key []byte) bool {
	return scope == nil || bytes.Equal(scope.HostKey, key)
}

func (scope *CloudScope) matchesDevice(key []byte) bool {
	return scope == nil || bytes.Equal(scope.DeviceKey, key)
}
