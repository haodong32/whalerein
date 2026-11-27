package observation

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"strings"
)

// Domain separates the identity namespaces a record may carry. A request id
// and an object id with the same bytes never produce the same token.
//
// The vocabulary is fixed by contracts/latency/observation.json and shared
// with every producer: session, stream, frame, request, watch, connection,
// object and ordinal.
type Domain string

const (
	DomainSession    Domain = "session"
	DomainStream     Domain = "stream"
	DomainFrame      Domain = "frame"
	DomainRequest    Domain = "request"
	DomainWatch      Domain = "watch"
	DomainConnection Domain = "connection"
	DomainObject     Domain = "object"
	DomainOrdinal    Domain = "ordinal"
)

const (
	// tokenVersion binds tokens to the shared derivation contract. All
	// producers hash this prefix before the domain and raw identity.
	tokenVersion = "whalerein/latency-token/v1"
	// tokenBytes is the truncated HMAC length. 128 bits keeps joins
	// collision-safe inside one run while remaining short enough for NDJSON.
	tokenBytes = 16
)

// Tokenizer derives run-local opaque identity tokens from the ephemeral
// capture key. The key never leaves the process; raw identifiers are never
// exported.
type Tokenizer struct {
	key []byte
}

// NewTokenizer validates and decodes the capture key. The fixture exports a
// 64-character lowercase hex key, which decodes to the 32 key bytes every
// producer hashes. Tests may supply raw key material of at least 16 bytes when
// it is not valid hex.
func NewTokenizer(key []byte) (*Tokenizer, error) {
	material, err := decodeCaptureKey(key)
	if err != nil {
		return nil, err
	}
	return &Tokenizer{key: material}, nil
}

func decodeCaptureKey(key []byte) ([]byte, error) {
	trimmed := strings.TrimSpace(string(key))
	if len(trimmed) >= 32 && len(trimmed)%2 == 0 {
		if decoded, err := hex.DecodeString(trimmed); err == nil && len(decoded) >= 16 {
			return decoded, nil
		}
	}
	if len(key) < 16 {
		return nil, errors.New("observation: capture key must be 64 hex characters or at least 16 bytes")
	}
	return append([]byte(nil), key...), nil
}

// Token returns lowercase hex HMAC-SHA256(capture key,
// "whalerein/latency-token/v1" || 0x00 || domain || 0x00 || raw) truncated to
// 16 bytes. An unknown domain is a programming error and returns an empty
// token, never a token in a different namespace.
func (t *Tokenizer) Token(domain Domain, raw string) string {
	if t == nil {
		return ""
	}
	normalized, ok := normalizeDomain(domain)
	if !ok {
		return ""
	}
	mac := hmac.New(sha256.New, t.key)
	mac.Write([]byte(tokenVersion))
	mac.Write([]byte{0})
	mac.Write([]byte(normalized))
	mac.Write([]byte{0})
	mac.Write([]byte(raw))
	sum := mac.Sum(nil)
	return hex.EncodeToString(sum[:tokenBytes])
}

func normalizeDomain(domain Domain) (string, bool) {
	switch domain {
	case DomainSession, DomainStream, DomainFrame, DomainRequest, DomainWatch, DomainConnection, DomainObject, DomainOrdinal:
		return string(domain), true
	default:
		return "", false
	}
}
