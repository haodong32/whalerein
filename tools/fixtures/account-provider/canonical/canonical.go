// Package canonical builds the exact bytes a portable authorization signs.
//
// Ratification R1 fixes this profile: Ed25519 over a UTF-8 text form with no
// JSON serialization anywhere in the signed bytes. Every field is written as
// its exact string value, and byte sets are hashed as sorted, length-prefixed
// values so two implementations cannot disagree about ordering.
package canonical

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"fmt"
	"sort"
	"strings"
)

// Version is the authorization version string bound into the input.
const Version = "whalerein/v1/ext-auth-v1"

// Fields is one portable authorization.
type Fields struct {
	AuthVersion        string
	AuthorizationID    string
	Issuer             string
	Audience           string
	Purpose            string
	WorkspaceID        string
	RegionID           string
	OperationID        string
	EnrollmentRevision string
	Precondition       string
	ExpectedRevision   string
	HostID             string
	HostPubkeys        [][]byte
	DeviceID           string
	DevicePubkeys      [][]byte
	IssuedAtUnixMS     int64
	ExpiresAtUnixMS    int64
	Nonce              []byte
}

// Input renders the canonical signing input. It is deterministic: the same
// fields always produce byte-identical output.
func Input(fields Fields) string {
	version := fields.AuthVersion
	if version == "" {
		version = Version
	}
	lines := []string{
		version,
		"|" + fields.AuthorizationID,
		"|" + fields.Issuer,
		"|" + fields.Audience,
		"|" + fields.Purpose,
		"|" + fields.WorkspaceID,
		"|" + fields.RegionID,
		"|" + fields.OperationID,
		"|" + fields.EnrollmentRevision,
		"|" + fields.Precondition,
		"|" + fields.ExpectedRevision,
		"|" + fields.HostID,
		"|" + HashKeys(fields.HostPubkeys),
		"|" + fields.DeviceID,
		"|" + HashKeys(fields.DevicePubkeys),
		"|" + formatInt(fields.IssuedAtUnixMS),
		"|" + formatInt(fields.ExpiresAtUnixMS),
		"|" + base64.RawURLEncoding.EncodeToString(fields.Nonce),
	}
	return strings.Join(lines, "\n")
}

// HashKeys hashes a key set as sorted, length-prefixed values so the digest
// does not depend on the caller's ordering.
func HashKeys(keys [][]byte) string {
	if len(keys) == 0 {
		return base64.RawURLEncoding.EncodeToString(sha256Of(nil))
	}
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

func sha256Of(value []byte) []byte {
	sum := sha256.Sum256(value)
	return sum[:]
}

func formatInt(value int64) string {
	return fmt.Sprintf("%d", value)
}
