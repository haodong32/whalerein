package routeproof

import (
	"crypto/ed25519"
	"crypto/rand"
	"strings"
	"testing"
	"time"
)

func TestCanonicalAndVerify(t *testing.T) {
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	noise := make([]byte, 32)
	now := time.UnixMilli(1760000000000).UTC()
	canonical := Canonical("svc", "env", "host", "node", "inc", "challenge", 9, noise, now)
	if !strings.Contains(canonical, "|9|") || !strings.HasSuffix(canonical, "|1760000000000") {
		t.Fatalf("canonical = %q", canonical)
	}
	signature := ed25519.Sign(private, []byte(canonical))
	if !Verify(public, "svc", "env", "host", "node", "inc", "challenge", 9, noise, now, signature) {
		t.Fatal("valid proof rejected")
	}
	if Verify(public, "svc", "env", "host", "other", "inc", "challenge", 9, noise, now, signature) {
		t.Fatal("proof did not bind the node")
	}
	if Verify(nil, "svc", "env", "host", "node", "inc", "challenge", 9, noise, now, signature) {
		t.Fatal("empty key accepted")
	}
	if Verify(public, "svc", "env", "host", "node", "inc", "challenge", 0, noise, now, signature) {
		t.Fatal("zero generation accepted")
	}
}
