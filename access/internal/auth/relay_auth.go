package auth

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Relay directory signature purposes. The canonical signed texts are fixed by
// proto/v1/WIRE.md and are deliberately separate from HostAuthInvitation and
// HostAuthRevoke: a relay signature never authorizes pairing or revocation and
// a pairing signature never authorizes a ciphertext directory operation.
const (
	RelayAuthReserve = "reserve"
	RelayAuthUpload  = "upload"
	RelayAuthFinish  = "finish"
	RelayAuthObject  = "object"

	relayAuthPrefix = "whalerein/v1/relay"
)

// relayReplayPurpose namespaces a relay signature in the durable replay guard
// so it can never collide with an invitation or revocation signature.
func relayReplayPurpose(purpose string) string {
	return relayAuthPrefix + "|" + purpose
}

var relayObjectIDPattern = regexp.MustCompile(`^[0-9a-f]{32}$`)

// RelayFields is every value bound into a relay directory signature. The
// operation selects which subset participates in the canonical text:
//
//	reserve: version, host, device, session, channel, purpose, part, bytes,
//	         digest, expiry, signed-at
//	upload:  version, object id, bytes, digest, signed-at
//	finish:  version, object id, bytes, digest, signed-at
//	object:  version, object id, signed-at
//
// The digest is the SHA-256 of the exact ciphertext bytes, never plaintext.
type RelayFields struct {
	RelayVersion    uint32
	HostID          string
	DeviceID        string
	SessionID       string
	ChannelID       string
	Purpose         whalereinv1.PayloadPurpose
	PartID          string
	CiphertextBytes uint64
	SHA256          []byte
	ExpiresAtUnixMs int64
	ObjectID        string
	SignedAtUnixMs  int64
}

// RelaySignedText returns the exact UTF-8 text a Host signs for one relay
// operation. Components are validated so a separator cannot be smuggled into
// an identifier and change the meaning of the signed text.
func RelaySignedText(purpose string, fields RelayFields) ([]byte, error) {
	if fields.HostID == "" {
		return nil, fmt.Errorf("%w: relay host id is required", ErrBadRequest)
	}
	if err := safeRelayComponent("host id", fields.HostID, 200); err != nil {
		return nil, err
	}
	switch purpose {
	case RelayAuthReserve:
		if err := safeRelayComponent("device id", fields.DeviceID, 200); err != nil {
			return nil, err
		}
		if err := safeRelayComponent("session id", fields.SessionID, 512); err != nil {
			return nil, err
		}
		if err := safeRelayComponent("channel id", fields.ChannelID, 128); err != nil {
			return nil, err
		}
		if err := safeRelayComponent("part id", fields.PartID, 512); err != nil {
			return nil, err
		}
		if fields.DeviceID == "" {
			return nil, fmt.Errorf("%w: relay device id is required", ErrBadRequest)
		}
		purposeName, ok := whalereinv1.PayloadPurpose_name[int32(fields.Purpose)]
		if !ok || fields.Purpose == whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_UNSPECIFIED {
			return nil, fmt.Errorf("%w: unsupported payload purpose", ErrBadRequest)
		}
		if len(fields.SHA256) != sha256.Size {
			return nil, fmt.Errorf("%w: relay digest must be SHA-256", ErrBadRequest)
		}
		return []byte(fmt.Sprintf("%s|reserve|%d|%s|%s|%s|%s|%s|%s|%d|%s|%d|%d",
			relayAuthPrefix, fields.RelayVersion, fields.HostID, fields.DeviceID,
			fields.SessionID, fields.ChannelID, purposeName, fields.PartID,
			fields.CiphertextBytes, hex.EncodeToString(fields.SHA256),
			fields.ExpiresAtUnixMs, fields.SignedAtUnixMs)), nil
	case RelayAuthUpload, RelayAuthFinish:
		if err := validateRelayObjectID(fields.ObjectID); err != nil {
			return nil, err
		}
		if len(fields.SHA256) != sha256.Size {
			return nil, fmt.Errorf("%w: relay digest must be SHA-256", ErrBadRequest)
		}
		return []byte(fmt.Sprintf("%s|%s|%d|%s|%d|%s|%d",
			relayAuthPrefix, purpose, fields.RelayVersion, fields.ObjectID,
			fields.CiphertextBytes, hex.EncodeToString(fields.SHA256),
			fields.SignedAtUnixMs)), nil
	case RelayAuthObject:
		if err := validateRelayObjectID(fields.ObjectID); err != nil {
			return nil, err
		}
		return []byte(fmt.Sprintf("%s|object|%d|%s|%d",
			relayAuthPrefix, fields.RelayVersion, fields.ObjectID,
			fields.SignedAtUnixMs)), nil
	default:
		return nil, fmt.Errorf("%w: unknown relay signature purpose", ErrBadRequest)
	}
}

func validateRelayObjectID(objectID string) error {
	if !relayObjectIDPattern.MatchString(objectID) {
		return fmt.Errorf("%w: object id must be 32 lowercase hex digits", ErrBadRequest)
	}
	return nil
}

// safeRelayComponent rejects a value that is too long, carries control bytes or
// contains the canonical-text separator. Empty values are allowed because the
// schema leaves session, channel and part ids optional for non-part purposes.
func safeRelayComponent(name, value string, max int) error {
	if len(value) > max {
		return fmt.Errorf("%w: %s is too long", ErrBadRequest, name)
	}
	if strings.ContainsAny(value, "|\x00\r\n") {
		return fmt.Errorf("%w: %s contains an unsafe character", ErrBadRequest, name)
	}
	for _, r := range value {
		if r < 0x20 || r == 0x7f {
			return fmt.Errorf("%w: %s contains a control character", ErrBadRequest, name)
		}
	}
	return nil
}

// AuthenticateRelayRequest verifies an Ed25519 relay signature against the key
// registered for the owning Host, enforces the pairing clock window (two
// minutes past through one minute future) and consumes the signature exactly
// once through the same durable replay guard as pairing and revocation.
func (s *Service) AuthenticateRelayRequest(ctx context.Context, purpose string, fields RelayFields, signature []byte) error {
	if fields.HostID == "" || signature == nil {
		return ErrUnauthorized
	}
	message, err := RelaySignedText(purpose, fields)
	if err != nil {
		return err
	}
	signedAt := time.UnixMilli(fields.SignedAtUnixMs)
	now := s.now()
	if signedAt.Before(now.Add(-hostAuthPast)) || signedAt.After(now.Add(hostAuthFuture)) {
		return ErrSignatureExpired
	}
	if len(signature) != ed25519.SignatureSize {
		return ErrUnauthorized
	}
	publicKey, err := s.store.HostAuthorizationKey(ctx, fields.HostID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrUnauthorized
		}
		return fmt.Errorf("authenticate relay request: %w", err)
	}
	if !ed25519.Verify(ed25519.PublicKey(publicKey), message, signature) {
		return ErrUnauthorized
	}
	return s.rememberSignatureOnce(ctx, HostProof{
		Purpose:        relayReplayPurpose(purpose),
		HostID:         fields.HostID,
		DeviceID:       relayReplayBinding(purpose, fields),
		SignedAtUnixMs: fields.SignedAtUnixMs,
		Signature:      signature,
	})
}

// relayReplayBinding scopes a consumed signature to the exact object or device
// it authorized, so the durable row is useful evidence during an audit.
func relayReplayBinding(purpose string, fields RelayFields) string {
	switch purpose {
	case RelayAuthReserve:
		return fields.DeviceID + ":" + strconv.FormatUint(fields.CiphertextBytes, 10)
	default:
		return fields.ObjectID
	}
}
