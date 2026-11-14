package signer

import (
	"crypto/ed25519"
	"encoding/base64"
	"errors"
	"io"
	"testing"
	"time"
)

func TestActiveKeySignsAndVerifies(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	set, err := NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	key, err := set.Active(now)
	if err != nil {
		t.Fatal(err)
	}
	input := "whalerein/v1/ext-auth-v1\n|auth-1"
	signature := key.Sign(input)
	if !Verify(key.PublicKey, input, signature) {
		t.Fatal("a signature from the active key must verify")
	}
	if Verify(key.PublicKey, input+"x", signature) {
		t.Fatal("a changed input must not verify")
	}
	if Verify(key.PublicKey, input, signature[:len(signature)-1]) {
		t.Fatal("a truncated signature must not verify")
	}
}

func TestRotationWindowIsExclusiveAtHandover(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	set, err := NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	handover := now.Add(24 * time.Hour)
	if _, err := set.AddRotationKey(handover, handover.Add(24*time.Hour)); err != nil {
		t.Fatal(err)
	}

	first, err := set.Active(now)
	if err != nil {
		t.Fatal(err)
	}
	if first.ID != "issuer-1" {
		t.Fatalf("expected issuer-1 before the handover, got %s", first.ID)
	}
	second, err := set.Active(handover)
	if err != nil {
		t.Fatal(err)
	}
	if second.ID != "issuer-2" {
		t.Fatalf("expected issuer-2 at the handover, got %s", second.ID)
	}
	if first.Sign("input") != nil && Verify(second.PublicKey, "input", first.Sign("input")) {
		t.Fatal("a signature from the retired key must not verify against the next key")
	}
}

func TestNoActiveKeyIsAnError(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	set, err := NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := set.Active(now.Add(72 * time.Hour)); err == nil {
		t.Fatal("an expired key ring must not sign")
	}
}

func TestPublishedKeysCoverTheWholeRing(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	set, err := NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := set.AddRotationKey(now.Add(24*time.Hour), now.Add(48*time.Hour)); err != nil {
		t.Fatal(err)
	}
	published := set.VerificationKeys()
	if len(published) != 2 {
		t.Fatalf("expected both keys published for overlap, got %d", len(published))
	}
	if published[0]["keyId"] != "issuer-1" || published[1]["keyId"] != "issuer-2" {
		t.Fatalf("published keys must be ordered by id: %v", published)
	}
	for _, entry := range published {
		raw, err := base64.RawURLEncoding.DecodeString(entry["key"].(string))
		if err != nil {
			t.Fatal(err)
		}
		if len(raw) != 32 {
			t.Fatalf("expected a 32-byte Ed25519 public key, got %d", len(raw))
		}
	}
}

func TestUnknownKeyDoesNotVerify(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	known, err := NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	unknown, err := NewSet("fixture://issuer/unknown", now)
	if err != nil {
		t.Fatal(err)
	}
	knownKey, _ := known.Active(now)
	unknownKey, _ := unknown.Active(now)
	signature := unknownKey.Sign("input")
	if Verify(knownKey.PublicKey, "input", signature) {
		t.Fatal("a signature from an untrusted issuer must not verify")
	}
}

func TestVerifyRejectsWrongSizedMaterial(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	set, err := NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	key, err := set.Active(now)
	if err != nil {
		t.Fatal(err)
	}
	signature := key.Sign("input")
	if Verify(nil, "input", signature) {
		t.Fatal("a missing public key must not verify")
	}
	if Verify(key.PublicKey, "input", nil) {
		t.Fatal("a missing signature must not verify")
	}
	if Verify(key.PublicKey[:16], "input", signature) {
		t.Fatal("a short public key must not verify")
	}
}

func TestPublicBase64MatchesThePublishedKey(t *testing.T) {
	now := time.Unix(1_700_000_000, 0).UTC()
	set, err := NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	key, err := set.Active(now)
	if err != nil {
		t.Fatal(err)
	}
	published := set.VerificationKeys()[0]["key"].(string)
	if key.PublicBase64() != published {
		t.Fatalf("published key %q does not match the active key %q", published, key.PublicBase64())
	}
}

func TestKeyGenerationFailureIsReported(t *testing.T) {
	original := generateKey
	generateKey = func(_ io.Reader) (ed25519.PublicKey, ed25519.PrivateKey, error) {
		return nil, nil, errors.New("no entropy")
	}
	defer func() { generateKey = original }()

	if _, err := NewSet("fixture://issuer/a", time.Unix(1_700_000_000, 0).UTC()); err == nil {
		t.Fatal("a key generation failure must be reported")
	}
	set, err := NewSet("fixture://issuer/a", time.Unix(1_700_000_000, 0).UTC())
	if err == nil {
		_, rotateErr := set.AddRotationKey(time.Time{}, time.Time{})
		if rotateErr == nil {
			t.Fatal("a rotation key generation failure must be reported")
		}
	}
}
