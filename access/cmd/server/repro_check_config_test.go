package main

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/whalerein/whalerein/access/internal/config"
)

// ACC-3 regression: -check-config verification key length.
//
// config.validateExternal must reject an external.verification_keys[].public_key
// that does not decode to a 32-byte Ed25519 public key, so the documented
// preflight agrees with runServer's externalTrust instead of passing and then
// letting the process crash-loop at startup.
func TestReproCheckConfigRejectsAWrongLengthExternalVerificationKey(t *testing.T) {
	raw, err := os.ReadFile(writeConfig(t, configValues{
		publicPort: 1, internalPort: 2,
		databaseURL: "postgres://access@db/access",
	}))
	if err != nil {
		t.Fatal(err)
	}
	writeExternal := func(t *testing.T, publicKey string) string {
		t.Helper()
		external := `
mode: "external"
external:
  service_identity: "https://access.fixture.test"
  issuer: "fixture://issuer/a"
  region: "fixture-a"
  mqtt_url: "mqtts://access.fixture.test:8883"
  verification_keys:
    - id: "issuer-1"
      public_key: "` + publicKey + `"
`
		path := filepath.Join(t.TempDir(), "external-config.yaml")
		if err := os.WriteFile(path, append(raw, []byte(external)...), 0o600); err != nil {
			t.Fatal(err)
		}
		return path
	}

	// 16 decoded bytes: valid base64, but not an Ed25519 public key.
	shortKey := base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{7}, 16))
	path := writeExternal(t, shortKey)
	if _, err := config.Load(path); err == nil || !strings.Contains(err.Error(), "verification_keys") {
		t.Fatalf("config load err = %v, want an error naming external.verification_keys", err)
	}

	var stdout, stderr bytes.Buffer
	code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr)
	if code == 0 {
		t.Fatalf("-check-config accepted a wrong-length verification key: exit=%d stdout=%q stderr=%q",
			code, stdout.String(), stderr.String())
	}
	if !strings.Contains(stderr.String(), "verification_keys") {
		t.Fatalf("-check-config exit=%d stderr=%q, want an error naming external.verification_keys", code, stderr.String())
	}

	// Positive control: the same preflight accepts a real Ed25519 key.
	validKey := base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{7}, ed25519.PublicKeySize))
	path = writeExternal(t, validKey)
	stdout.Reset()
	stderr.Reset()
	if code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr); code != 0 {
		t.Fatalf("-check-config rejected a valid Ed25519 key: exit=%d stdout=%q stderr=%q",
			code, stdout.String(), stderr.String())
	}
}
