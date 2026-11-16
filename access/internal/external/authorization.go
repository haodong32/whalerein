// Package external implements external account mode.
//
// An endpoint carries a portable authorization minted by the selected account
// service. Access verifies it locally against pinned issuer keys before it
// admits an enrollment, and it never trusts a login credential for this.
//
// The canonical signing input here is implemented independently of the
// acceptance fixture: the two must agree only through the public contract.
package external

import (
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"fmt"
	"sort"
	"strconv"
	"strings"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// AuthVersion is bound into every portable authorization.
const AuthVersion = "whalerein/v1/ext-auth-v1"

// ProofPrefix is the enrollment proof-of-possession domain separator.
const ProofPrefix = "whalerein/v1/enrollment"

// Purposes this package knows.
const (
	PurposeHostEnrollment = "host_enrollment"
	PurposeDevicePairing  = "device_pairing"
	PurposeDeviceRecovery = "device_recovery"
)

// VerificationKey is one pinned issuer key with its validity window.
type VerificationKey struct {
	ID        string
	PublicKey ed25519.PublicKey
	NotBefore time.Time
	NotAfter  time.Time
}

// Trust is the configured account service.
type Trust struct {
	Issuer          string
	ServiceIdentity string
	Region          string
	Keys            []VerificationKey
	// ClockSkew tolerates small differences when checking key windows.
	ClockSkew time.Duration
}

// key returns the pinned key with that id.
func (t Trust) key(id string) (VerificationKey, error) {
	for _, key := range t.Keys {
		if key.ID == id {
			return key, nil
		}
	}
	return VerificationKey{}, fmt.Errorf("unknown verification key %q", id)
}

// CanonicalInput renders the exact bytes an authorization signs.
func CanonicalInput(auth *whalereinv1.ExternalAuthorization) string {
	version := auth.GetAuthVersion()
	if version == "" {
		version = AuthVersion
	}
	lines := []string{
		version,
		"|" + auth.GetAuthorizationId(),
		"|" + auth.GetIssuer(),
		"|" + auth.GetAudience(),
		"|" + auth.GetPurpose(),
		"|" + auth.GetWorkspaceId(),
		"|" + auth.GetRegionId(),
		"|" + auth.GetOperationId(),
		"|" + auth.GetEnrollmentRevision(),
		"|" + auth.GetPrecondition(),
		"|" + auth.GetExpectedRevision(),
		"|" + auth.GetHostId(),
		"|" + HashKeys(auth.GetHostPublicKeys()),
		"|" + auth.GetDeviceId(),
		"|" + HashKeys(auth.GetDevicePublicKeys()),
		"|" + strconv.FormatInt(auth.GetIssuedAtUnixMs(), 10),
		"|" + strconv.FormatInt(auth.GetExpiresAtUnixMs(), 10),
		"|" + base64.RawURLEncoding.EncodeToString(auth.GetNonce()),
	}
	return strings.Join(lines, "\n")
}

// HashBytes returns the raw SHA-256 digest of a value.
func HashBytes(value []byte) []byte {
	sum := sha256.Sum256(value)
	return sum[:]
}

// HashKeys hashes a key set as sorted, length-prefixed values.
func HashKeys(keys [][]byte) string {
	sorted := make([][]byte, len(keys))
	copy(sorted, keys)
	sort.Slice(sorted, func(i, j int) bool { return string(sorted[i]) < string(sorted[j]) })
	hasher := sha256.New()
	var length [4]byte
	for _, key := range sorted {
		binary.BigEndian.PutUint32(length[:], uint32(len(key)))
		hasher.Write(length[:])
		hasher.Write(key)
	}
	return base64.RawURLEncoding.EncodeToString(hasher.Sum(nil))
}

// ProofInput renders the enrollment proof-of-possession input.
func ProofInput(auth *whalereinv1.ExternalAuthorization, operationID string, hostSigningKey []byte) string {
	return strings.Join([]string{
		ProofPrefix,
		"|" + auth.GetAuthorizationId(),
		"|" + base64.RawURLEncoding.EncodeToString(auth.GetNonce()),
		"|" + auth.GetHostId(),
		"|" + base64.RawURLEncoding.EncodeToString(hostSigningKey),
		"|" + operationID,
	}, "")
}

// Refusal codes an endpoint may observe.
var (
	ErrAuthorizationExpired   = fmt.Errorf("authorization expired")
	ErrAuthorizationMismatch  = fmt.Errorf("authorization does not match this service")
	ErrAuthorizationSignature = fmt.Errorf("authorization signature is invalid")
	ErrKeyNotValid            = fmt.Errorf("the signing key is not valid at the issue time")
	ErrProofInvalid           = fmt.Errorf("proof of possession is invalid")
	ErrEnrollmentDisabled     = fmt.Errorf("enrollment is disabled")
	ErrOperationConflict      = fmt.Errorf("the operation was already used for a different payload")
	ErrStaleRevision          = fmt.Errorf("the enrollment revision is stale")
	ErrNotFound               = fmt.Errorf("not found")
)

// Verify checks signature, scope, expiry and, when required, the key bindings.
// A missing or mismatched field refuses with ErrAuthorizationMismatch.
func (t Trust) Verify(auth *whalereinv1.ExternalAuthorization, purpose string, now time.Time) error {
	if auth == nil {
		return ErrAuthorizationMismatch
	}
	if auth.GetIssuer() != t.Issuer {
		return fmt.Errorf("%w: issuer %q is not trusted", ErrAuthorizationMismatch, auth.GetIssuer())
	}
	if auth.GetAudience() != t.ServiceIdentity {
		return fmt.Errorf("%w: audience %q is not this service", ErrAuthorizationMismatch, auth.GetAudience())
	}
	if auth.GetPurpose() != purpose {
		return fmt.Errorf("%w: purpose %q is not %q", ErrAuthorizationMismatch, auth.GetPurpose(), purpose)
	}
	if t.Region != "" && auth.GetRegionId() != t.Region {
		return fmt.Errorf("%w: region %q is not %q", ErrAuthorizationMismatch, auth.GetRegionId(), t.Region)
	}
	if now.UnixMilli() >= auth.GetExpiresAtUnixMs() {
		return fmt.Errorf("%w: expired at %d", ErrAuthorizationExpired, auth.GetExpiresAtUnixMs())
	}

	key, err := t.key(auth.GetKeyId())
	if err != nil {
		return fmt.Errorf("%w: %v", ErrAuthorizationSignature, err)
	}
	issuedAt := time.UnixMilli(auth.GetIssuedAtUnixMs())
	skew := t.ClockSkew
	// An omitted bound is open on that side. Reading a zero bound as an instant
	// turned "no not_after configured" into "ended in 1970", which refused every
	// authorization and looked like a bad signature.
	if !key.NotBefore.IsZero() && issuedAt.Add(-skew).Before(key.NotBefore) {
		return fmt.Errorf("%w: %s starts at %s", ErrKeyNotValid, key.ID, key.NotBefore.UTC())
	}
	if !key.NotAfter.IsZero() && !issuedAt.Add(-skew).Before(key.NotAfter) {
		return fmt.Errorf("%w: %s ended at %s", ErrKeyNotValid, key.ID, key.NotAfter.UTC())
	}
	if len(key.PublicKey) != ed25519.PublicKeySize || len(auth.GetSignature()) != ed25519.SignatureSize {
		return ErrAuthorizationSignature
	}
	if !ed25519.Verify(key.PublicKey, []byte(CanonicalInput(auth)), auth.GetSignature()) {
		return ErrAuthorizationSignature
	}
	return nil
}

// VerifyHostProof checks that the caller holds the approved Host signing key
// and that the asserted keys are the approved ones.
func VerifyHostProof(
	auth *whalereinv1.ExternalAuthorization,
	request *whalereinv1.HostEnrollmentRequest,
) error {
	if auth.GetHostId() == "" || auth.GetHostId() != request.GetHostId() {
		return fmt.Errorf("%w: host id %q does not match the authorization", ErrAuthorizationMismatch, request.GetHostId())
	}
	if request.GetOperationId() != "" && auth.GetOperationId() != "" &&
		request.GetOperationId() != auth.GetOperationId() {
		return fmt.Errorf("%w: operation id %q does not match the authorization", ErrAuthorizationMismatch, request.GetOperationId())
	}
	approved := auth.GetHostPublicKeys()
	signing := request.GetHostSigningPublicKey()
	noise := request.GetHostNoisePublicKey()
	if len(signing) != ed25519.PublicKeySize {
		return fmt.Errorf("%w: the signing key is not an Ed25519 public key", ErrAuthorizationMismatch)
	}
	if len(noise) != 32 {
		return fmt.Errorf("%w: the Noise key is not a 32-byte X25519 key", ErrAuthorizationMismatch)
	}
	if len(approved) != 2 || !contains(approved, signing) || !contains(approved, noise) {
		return fmt.Errorf("%w: the asserted keys are not the approved pair", ErrAuthorizationMismatch)
	}
	input := ProofInput(auth, request.GetOperationId(), signing)
	if len(request.GetProofSignature()) != ed25519.SignatureSize ||
		!ed25519.Verify(ed25519.PublicKey(signing), []byte(input), request.GetProofSignature()) {
		return ErrProofInvalid
	}
	return nil
}

// Digest is the immutable payload binding of an enrollment request.
func Digest(request *whalereinv1.HostEnrollmentRequest) []byte {
	hasher := sha256.New()
	for _, part := range [][]byte{
		request.GetHostSigningPublicKey(),
		request.GetHostNoisePublicKey(),
	} {
		var length [4]byte
		binary.BigEndian.PutUint32(length[:], uint32(len(part)))
		hasher.Write(length[:])
		hasher.Write(part)
	}
	return hasher.Sum(nil)
}

func contains(values [][]byte, target []byte) bool {
	for _, value := range values {
		if string(value) == string(target) {
			return true
		}
	}
	return false
}
