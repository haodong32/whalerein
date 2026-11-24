package config

import (
	"encoding/base64"
	"strings"
	"testing"
)

// ACC-7 regression: external-only settings in Standalone mode.
//
// In Standalone mode external.require_device_session must be rejected, not
// accepted and then silently neutralized: cmd/server computes
// RequireDeviceSession only in external mode, so accepting the block in
// Standalone leaves legacy Device credentials working while the operator
// believes the cutover is enforced. Any setting only External mode implements
// is rejected like external.issuer.
func TestReproStandaloneRejectsExternalDeviceSessionEnforcement(t *testing.T) {
	standalone := minimalYAML + `
external:
  require_device_session: true
  session_environment: "env-a"
  session_authority_revision: "rev-1"
`
	cfg, err := Parse([]byte(standalone), env(validEnv()))
	if err == nil {
		t.Fatalf("Standalone accepted external.require_device_session=true "+
			"(mode=%q, require=%v) and then ignores it; want the same "+
			"\"only valid in external mode\" rejection as external.issuer",
			cfg.Mode, cfg.External.RequireDeviceSession)
	}
	if !strings.Contains(err.Error(), "only valid in external mode") {
		t.Fatalf("Standalone session enforcement: err = %v, want an "+
			"\"only valid in external mode\" validation error", err)
	}

	// Positive control: the identical block is valid in the mode that
	// implements it, so the setting itself is not malformed.
	external := strings.Replace(minimalYAML, `broker:
  url:`, `mode: "external"
external:
  service_identity: "https://access.fixture.test"
  issuer: "fixture://issuer/a"
  region: "fixture-a"
  mqtt_url: "mqtts://access.fixture.test:8883"
  require_device_session: true
  session_environment: "env-a"
  session_authority_revision: "rev-1"
  verification_keys:
    - id: "issuer-1"
      public_key: "`+base64.RawURLEncoding.EncodeToString(make([]byte, 32))+`"
broker:
  url:`, 1)
	cfg, err = Parse([]byte(external), env(validEnv()))
	if err != nil {
		t.Fatalf("external mode rejected the same session settings: %v", err)
	}
	if cfg.Mode != "external" || !cfg.External.RequireDeviceSession {
		t.Fatalf("external control config = mode %q require %v", cfg.Mode, cfg.External.RequireDeviceSession)
	}
}
