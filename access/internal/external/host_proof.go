package external

import (
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"
)

// HostOwnershipProofPrefix is the domain separator for the scoped Host
// ownership proof used by the status and credential-replacement endpoints.
const HostOwnershipProofPrefix = "whalerein/v1/host-proof-v1"

// Host ownership proof purposes. The canonical text binds the purpose, so a
// status proof can never authorize a credential replacement.
const (
	PurposeEnrollmentStatus      = "enrollment_status"
	PurposeCredentialReplacement = "credential_replacement"
)

const hostOwnershipProofWindow = 5 * time.Minute

// HostProof refusals.
var (
	ErrProofExpired = errors.New("host proof expired")
	ErrProofReplay  = errors.New("host proof nonce was already used")
)

// HostOwnershipProof is the signed JSON document carried in the
// X-Whalerein-Host-Proof header. Signature and nonce are base64url strings.
type HostOwnershipProof struct {
	HostID         string `json:"hostId"`
	IssuedAtUnixMs int64  `json:"issuedAt"`
	Nonce          string `json:"nonce"`
	Signature      string `json:"signature"`
}

// ParseHostOwnershipProof decodes and validates the wire shape of one proof
// header value. Signature verification is separate because it needs the
// current enrollment key.
func ParseHostOwnershipProof(encoded string) (HostOwnershipProof, error) {
	raw, err := base64.RawURLEncoding.DecodeString(strings.TrimSpace(encoded))
	if err != nil {
		return HostOwnershipProof{}, fmt.Errorf("%w: proof is not base64url", ErrProofInvalid)
	}
	var proof HostOwnershipProof
	if err := json.Unmarshal(raw, &proof); err != nil {
		return HostOwnershipProof{}, fmt.Errorf("%w: proof is not JSON", ErrProofInvalid)
	}
	if proof.HostID == "" || proof.IssuedAtUnixMs == 0 || proof.Nonce == "" || proof.Signature == "" {
		return HostOwnershipProof{}, fmt.Errorf("%w: proof is missing a required field", ErrProofInvalid)
	}
	if nonce, err := base64.RawURLEncoding.DecodeString(proof.Nonce); err != nil || len(nonce) == 0 || len(nonce) > 64 {
		return HostOwnershipProof{}, fmt.Errorf("%w: proof nonce is invalid", ErrProofInvalid)
	}
	if signature, err := base64.RawURLEncoding.DecodeString(proof.Signature); err != nil || len(signature) != ed25519.SignatureSize {
		return HostOwnershipProof{}, fmt.Errorf("%w: proof signature is invalid", ErrProofInvalid)
	}
	return proof, nil
}

// HostOwnershipProofInput renders the exact newline-joined UTF-8 text the Host
// signs for a status read or credential replacement.
func HostOwnershipProofInput(purpose, hostID, subjectID string, issuedAtUnixMs int64, nonce string) string {
	return strings.Join([]string{
		HostOwnershipProofPrefix,
		"|" + purpose,
		"|" + hostID,
		"|" + subjectID,
		"|" + strconv.FormatInt(issuedAtUnixMs, 10),
		"|" + nonce,
	}, "\n")
}

// VerifyHostOwnershipProof checks the issue window and the Ed25519 signature
// against the current enrollment signing key.
func VerifyHostOwnershipProof(publicKey ed25519.PublicKey, proof HostOwnershipProof, purpose, subjectID string, now time.Time) error {
	if proof.HostID == "" {
		return fmt.Errorf("%w: host id is required", ErrAuthorizationMismatch)
	}
	issuedAt := time.UnixMilli(proof.IssuedAtUnixMs)
	if issuedAt.Before(now.Add(-hostOwnershipProofWindow)) || issuedAt.After(now.Add(hostOwnershipProofWindow)) {
		return fmt.Errorf("%w: issued at %d", ErrProofExpired, proof.IssuedAtUnixMs)
	}
	signature, err := base64.RawURLEncoding.DecodeString(proof.Signature)
	if err != nil {
		return ErrProofInvalid
	}
	if len(publicKey) != ed25519.PublicKeySize || len(signature) != ed25519.SignatureSize {
		return ErrProofInvalid
	}
	input := HostOwnershipProofInput(purpose, proof.HostID, subjectID, proof.IssuedAtUnixMs, proof.Nonce)
	if !ed25519.Verify(publicKey, []byte(input), signature) {
		return ErrProofInvalid
	}
	return nil
}

// HostProofNonceHash is the durable single-use guard value for one nonce.
func HostProofNonceHash(nonce string) []byte {
	sum := sha256.Sum256([]byte(nonce))
	return sum[:]
}
