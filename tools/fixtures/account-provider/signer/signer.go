// Package signer holds the independent provider's Ed25519 issuer keys.
//
// Rotation is explicit: every key carries a validity window, and an unknown
// issuer exists so negative cases can prove that an untrusted identity is
// rejected rather than silently accepted.
package signer

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"fmt"
	"sort"
	"time"
)

// Key is one issuer signing key with its published validity window.
type Key struct {
	ID        string
	PublicKey ed25519.PublicKey
	private   ed25519.PrivateKey
	NotBefore time.Time
	NotAfter  time.Time
}

// PublicBase64 returns the published key material.
func (k Key) PublicBase64() string { return base64.RawURLEncoding.EncodeToString(k.PublicKey) }

// Sign signs the canonical authorization input.
func (k Key) Sign(input string) []byte { return ed25519.Sign(k.private, []byte(input)) }

// Set is the provider's key ring for one issuer.
type Set struct {
	Issuer string
	keys   []Key
}

// NewSet creates an issuer with one active key and returns it.
func NewSet(issuer string, now time.Time) (*Set, error) {
	key, err := newKey("issuer-1", now.Add(-time.Hour), now.Add(24*time.Hour))
	if err != nil {
		return nil, err
	}
	return &Set{Issuer: issuer, keys: []Key{key}}, nil
}

// AddRotationKey publishes a second key that becomes valid after the first
// key expires, so a verifier must overlap them.
func (s *Set) AddRotationKey(notBefore, notAfter time.Time) (Key, error) {
	key, err := newKey(fmt.Sprintf("issuer-%d", len(s.keys)+1), notBefore, notAfter)
	if err != nil {
		return Key{}, err
	}
	s.keys = append(s.keys, key)
	return key, nil
}

// Active returns the key that may sign at the given instant.
func (s *Set) Active(now time.Time) (Key, error) {
	for _, key := range s.keys {
		if !now.Before(key.NotBefore) && now.Before(key.NotAfter) {
			return key, nil
		}
	}
	return Key{}, fmt.Errorf("no issuer key is valid at %s", now.UTC().Format(time.RFC3339))
}

// VerificationKeys renders the published key list with overlapping windows.
func (s *Set) VerificationKeys() []map[string]any {
	published := make([]map[string]any, 0, len(s.keys))
	for _, key := range s.keys {
		published = append(published, map[string]any{
			"keyId":     key.ID,
			"key":       key.PublicBase64(),
			"notBefore": key.NotBefore.UnixMilli(),
			"notAfter":  key.NotAfter.UnixMilli(),
		})
	}
	sort.Slice(published, func(i, j int) bool {
		return published[i]["keyId"].(string) < published[j]["keyId"].(string)
	})
	return published
}

// Verify checks a signature with the key the verifier actually trusts. An
// unknown issuer cannot verify anything.
func Verify(publicKey ed25519.PublicKey, input string, signature []byte) bool {
	if len(publicKey) != ed25519.PublicKeySize || len(signature) != ed25519.SignatureSize {
		return false
	}
	return ed25519.Verify(publicKey, []byte(input), signature)
}

// generateKey is a seam so tests can exercise the entropy-failure path.
var generateKey = ed25519.GenerateKey

func newKey(id string, notBefore, notAfter time.Time) (Key, error) {
	publicKey, privateKey, err := generateKey(rand.Reader)
	if err != nil {
		return Key{}, fmt.Errorf("generate issuer key: %w", err)
	}
	return Key{ID: id, PublicKey: publicKey, private: privateKey, NotBefore: notBefore, NotAfter: notAfter}, nil
}
