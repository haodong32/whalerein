package config

import (
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"gopkg.in/yaml.v3"
)

const minimalYAML = `
listen: "127.0.0.1:18080"
internal_listen: "127.0.0.1:18081"
public_url: "https://access.example.com"
database_url: "${DB_URL}"
admin_token: "${ADMIN_TOKEN}"
host_provisioning_token: "${HOST_TOKEN}"
broker:
  url: "tls://mqtt.example.com:8883"
  username: "access"
  password: "${BROKER_PASSWORD}"
internal:
  callback_token: "${INTERNAL_TOKEN}"
`

func env(lookup map[string]string) func(string) (string, bool) {
	return func(name string) (string, bool) {
		value, ok := lookup[name]
		return value, ok
	}
}

func validEnv() map[string]string {
	return map[string]string{
		"DB_URL":          "postgres://access@db/access",
		"ADMIN_TOKEN":     "admin-secret",
		"HOST_TOKEN":      "host-secret",
		"BROKER_PASSWORD": "broker-secret",
		"INTERNAL_TOKEN":  "internal-secret",
	}
}

func TestParseMinimalConfigurationAppliesDefaults(t *testing.T) {
	cfg, err := Parse([]byte(minimalYAML), env(validEnv()))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if cfg.Listen != "127.0.0.1:18080" || cfg.InternalListen != "127.0.0.1:18081" {
		t.Fatalf("listeners = %s / %s", cfg.Listen, cfg.InternalListen)
	}
	if cfg.Pairing.InvitationTTL.D() != 10*time.Minute || cfg.Requests.Lease.D() != 30*time.Second {
		t.Fatalf("defaults not applied: %+v", cfg)
	}
	if cfg.HTTP.MaxBodyBytes != 24<<20 || cfg.Protocol.Version != 1 || cfg.Log.Level != "info" {
		t.Fatalf("defaults not applied: %+v", cfg)
	}
	if cfg.Line.PolicyTTL.D() != 10*time.Second || cfg.Line.HeartbeatInterval.D() != 5*time.Second {
		t.Fatalf("line defaults not applied: %+v", cfg.Line)
	}
	if cfg.Admission.MaxEntries != 10000 || cfg.Admission.TTL.D() != 5*time.Minute || cfg.Admission.MaxBackgroundLoads != 64 {
		t.Fatalf("admission defaults not applied: %+v", cfg.Admission)
	}
}

func TestParseEnvironmentDefaultsAndEscapedDollarSigns(t *testing.T) {
	yaml := strings.Replace(minimalYAML, "${DB_URL}", "${DB_URL}", 1)
	yaml = strings.Replace(yaml, "${ADMIN_TOKEN}", "${MISSING:-fallback-admin}", 1)
	yaml = strings.Replace(yaml, "access.example.com", "access.example.com", 1)
	yaml += "\nlog:\n  level: \"${LOG_LEVEL:-debug}\"\n"
	cfg, err := Parse([]byte(yaml), env(validEnv()))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if cfg.AdminToken != "fallback-admin" || cfg.Log.Level != "debug" {
		t.Fatalf("env defaults not applied: admin=%q log=%q", cfg.AdminToken, cfg.Log.Level)
	}
	escaped, err := expandEnv([]byte("password: $$literal"), env(nil))
	if err != nil {
		t.Fatal(err)
	}
	if string(escaped) != "password: $literal" {
		t.Fatalf("escaped = %q", escaped)
	}
}

func TestParseRejectsMissingEnvironmentVariable(t *testing.T) {
	_, err := Parse([]byte(minimalYAML), env(map[string]string{}))
	if err == nil || !strings.Contains(err.Error(), "missing required environment") {
		t.Fatalf("err = %v", err)
	}
}

func TestParseRejectsUnknownConfigurationField(t *testing.T) {
	_, err := Parse([]byte(strings.Replace(minimalYAML, "listen:", "unknown_field: 1\nlisten:", 1)), env(validEnv()))
	if err == nil || !strings.Contains(err.Error(), "field unknown_field not found") {
		t.Fatalf("err = %v", err)
	}
}

func TestParseRejectsInvalidDuration(t *testing.T) {
	_, err := Parse([]byte(minimalYAML+"\nrequests:\n  lease: nonsense\n"), env(validEnv()))
	if err == nil || !strings.Contains(err.Error(), "invalid duration") {
		t.Fatalf("err = %v", err)
	}
}

func TestLoadResolvesEnvironmentAndReadsConfiguration(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.yaml")
	if err := os.WriteFile(path, []byte(minimalYAML), 0o600); err != nil {
		t.Fatal(err)
	}
	for name, value := range validEnv() {
		t.Setenv(name, value)
	}
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if cfg.DatabaseURL == "" {
		t.Fatal("database URL empty")
	}
	if _, err := LoadWithEnv(filepath.Join(dir, "missing.yaml"), env(validEnv())); err == nil {
		t.Fatal("expected missing file error")
	}
	if _, err := LoadWithEnv(path, env(validEnv())); err != nil {
		t.Fatalf("LoadWithEnv: %v", err)
	}
}

func validConfig(t *testing.T) Config {
	t.Helper()
	cfg, err := Parse([]byte(minimalYAML), env(validEnv()))
	if err != nil {
		t.Fatal(err)
	}
	return cfg
}

func TestValidateRejectsInvalidConfigurationValues(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*Config)
		want   string
	}{
		{"rejects identical public and internal listeners", func(c *Config) { c.InternalListen = c.Listen }, "internal_listen must differ"},
		{"rejects unsupported public URL scheme", func(c *Config) { c.PublicURL = "ftp://x" }, "public_url must be"},
		{"rejects unsupported broker URL scheme", func(c *Config) { c.Broker.URL = "http://x" }, "broker.url must use"},
		{"rejects unsupported advertised broker URL scheme", func(c *Config) { c.Broker.PublicURL = "http://x" }, "broker.public_url must use"},
		{"rejects missing broker password", func(c *Config) { c.Broker.Password = "" }, "broker.password is required"},
		{"rejects missing internal callback token", func(c *Config) { c.Internal.CallbackToken = "" }, "internal.callback_token is required"},
		{"rejects callback token matching admin token", func(c *Config) { c.Internal.CallbackToken = c.AdminToken }, "must differ from admin_token"},
		{"rejects nonpositive HTTP body limit", func(c *Config) { c.HTTP.MaxBodyBytes = 0 }, "max_body_bytes"},
		{"rejects nonpositive HTTP timeout", func(c *Config) { c.HTTP.ReadTimeout = 0 }, "http timeouts"},
		{"rejects nonpositive line policy TTL", func(c *Config) { c.Line.PolicyTTL = 0 }, "line durations"},
		{"rejects nonpositive line reconcile interval", func(c *Config) { c.Line.ReconcileInterval = 0 }, "line durations"},
		{"rejects heartbeat not shorter than policy TTL", func(c *Config) { c.Line.HeartbeatInterval = c.Line.PolicyTTL }, "heartbeat_interval must be shorter"},
		{"rejects nonpositive admission entries", func(c *Config) { c.Admission.MaxEntries = 0 }, "admission tunables"},
		{"rejects nonpositive admission TTL", func(c *Config) { c.Admission.TTL = 0 }, "admission tunables"},
		{"rejects nonpositive admission load timeout", func(c *Config) { c.Admission.LoadTimeout = 0 }, "admission tunables"},
		{"rejects nonpositive background loads", func(c *Config) { c.Admission.MaxBackgroundLoads = 0 }, "admission tunables"},
		{"rejects nonpositive pairing failure limit", func(c *Config) { c.Pairing.MaxFailures = 0 }, "max_failures"},
		{"rejects nonpositive request batch size", func(c *Config) { c.Requests.BatchSize = 0 }, "requests tunables"},
		{"rejects nonpositive revocation acknowledgement timeout", func(c *Config) { c.Revocation.AckTimeout = 0 }, "ack_timeout"},
		{"rejects nonpositive presence cache TTL", func(c *Config) { c.Presence.CacheTTL = 0 }, "presence.cache_ttl"},
		{"rejects nonpositive cleanup interval", func(c *Config) { c.Cleanup.Interval = 0 }, "cleanup durations"},
		{"rejects unsupported protocol version", func(c *Config) { c.Protocol.Version = 0 }, "protocol.version"},
		{"rejects nonpositive broker keepalive", func(c *Config) { c.Broker.KeepAlive = 0 }, "broker durations"},
		{"rejects unsupported log level", func(c *Config) { c.Log.Level = "trace" }, "log.level"},
		{"rejects missing database URL", func(c *Config) { c.DatabaseURL = "" }, "database_url is required"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			cfg := validConfig(t)
			tc.mutate(&cfg)
			err := cfg.Validate()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("err = %v, want substring %q", err, tc.want)
			}
		})
	}
}

func TestRedactedConfigurationOmitsCredentials(t *testing.T) {
	cfg := validConfig(t)
	cfg.Broker.URL = "tls://user:url-password@mqtt.example.com:8883"
	summary := cfg.Redacted()
	text := summaryString(summary)
	for _, secret := range []string{cfg.AdminToken, cfg.HostProvisioningToken, cfg.Broker.Password, cfg.Internal.CallbackToken, cfg.DatabaseURL, "url-password"} {
		if secret != "" && strings.Contains(text, secret) {
			t.Fatalf("redacted summary contains secret %q: %s", secret, text)
		}
	}
	if !strings.Contains(text, "tls://mqtt.example.com:8883") {
		t.Fatalf("summary missing sanitized broker URL: %s", text)
	}
}

func summaryString(value map[string]any) string {
	var builder strings.Builder
	for key, item := range value {
		builder.WriteString(key)
		builder.WriteString("=")
		builder.WriteString(toString(item))
		builder.WriteString(";")
	}
	return builder.String()
}

func toString(value any) string {
	switch typed := value.(type) {
	case string:
		return typed
	default:
		return "other"
	}
}

func TestResolveMQTTPublicURLDerivesEndpointFromAccessScheme(t *testing.T) {
	cases := []struct {
		name    string
		cfg     Config
		want    string
		derived bool
	}{
		{"preserves explicitly configured MQTT public URL", Config{Broker: BrokerConfig{PublicURL: "mqtts://mqtt.example.com:8883/"}}, "mqtts://mqtt.example.com:8883", false},
		{"derives MQTT TLS URL from HTTPS default port", Config{PublicURL: "https://access.example.com"}, "mqtts://access.example.com:443", true},
		{"preserves custom HTTPS port in derived MQTT URL", Config{PublicURL: "https://access.example.com:8443"}, "mqtts://access.example.com:8443", true},
		{"derives plaintext MQTT URL from HTTP endpoint", Config{PublicURL: "http://127.0.0.1:8080"}, "mqtt://127.0.0.1:8080", true},
		{"returns no MQTT URL for invalid Access URL", Config{PublicURL: ":::"}, "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, derived := tc.cfg.ResolveMQTTPublicURL()
			if got != tc.want || derived != tc.derived {
				t.Fatalf("got %q derived=%v, want %q derived=%v", got, derived, tc.want, tc.derived)
			}
		})
	}
}

func yamlNode(value string) *yaml.Node {
	return &yaml.Node{Kind: yaml.ScalarNode, Value: value}
}

func TestDurationMarshalSerializesConfiguredDuration(t *testing.T) {
	value := Duration(90 * time.Second)
	marshaled, err := value.MarshalYAML()
	if err != nil || marshaled != "1m30s" {
		t.Fatalf("marshaled = %v, err = %v", marshaled, err)
	}
	var parsed Duration
	if err := parsed.UnmarshalYAML(yamlNode("2s")); err != nil || parsed.D() != 2*time.Second {
		t.Fatalf("parsed = %v, err = %v", parsed, err)
	}
	if err := parsed.UnmarshalYAML(yamlNode("")); err == nil {
		t.Fatal("expected invalid duration error")
	}
}

func TestExternalAccountModeRequiresExplicitTrust(t *testing.T) {
	// Selecting external mode without trust settings must fail: Access may not
	// guess an account service or silently stay in Standalone.
	c := validConfig(t)
	c.Mode = "external"
	err := c.Validate()
	if err == nil {
		t.Fatal("external mode without settings was accepted")
	}
	for _, required := range []string{
		"external.service_identity", "external.issuer", "external.region",
		"external.mqtt_url", "external.verification_keys",
	} {
		if !strings.Contains(err.Error(), required) {
			t.Fatalf("error should name %s: %v", required, err)
		}
	}
}

func TestUnknownModeIsRejected(t *testing.T) {
	c := Default()
	c.Mode = "managed"
	if err := c.Validate(); err == nil || !strings.Contains(err.Error(), `mode must be "standalone" or "external"`) {
		t.Fatalf("an unknown mode was accepted: %v", err)
	}
}

func TestExternalSettingsAreInvalidInStandaloneMode(t *testing.T) {
	c := Default()
	c.External.Issuer = "fixture://issuer/a"
	if err := c.Validate(); err == nil || !strings.Contains(err.Error(), "only valid in external mode") {
		t.Fatalf("standalone accepted external settings: %v", err)
	}
}

func TestFullyConfiguredExternalModeIsAccepted(t *testing.T) {
	c := validConfig(t)
	c.Mode = "external"
	c.External = ExternalConfig{
		ServiceIdentity: "https://access.fixture.test",
		Issuer:          "fixture://issuer/a",
		Region:          "fixture-a",
		MQTTURL:         "mqtts://access.fixture.test:8883",
		ClockSkew:       Duration(30 * time.Second),
		VerificationKeys: []ExternalVerificationKey{{
			ID:        "issuer-1",
			PublicKey: base64.RawURLEncoding.EncodeToString(make([]byte, 32)),
			NotBefore: Timestamp(time.Time{}),
			NotAfter:  Timestamp(time.Time{}),
		}},
	}
	if err := c.Validate(); err != nil {
		t.Fatalf("a complete external configuration was refused: %v", err)
	}
}

func TestExternalVerificationKeysAreValidated(t *testing.T) {
	base := func() Config {
		c := validConfig(t)
		c.Mode = "external"
		c.External = ExternalConfig{
			ServiceIdentity: "https://access.fixture.test",
			Issuer:          "fixture://issuer/a",
			Region:          "fixture-a",
			MQTTURL:         "mqtts://access.fixture.test:8883",
			VerificationKeys: []ExternalVerificationKey{{
				ID:        "issuer-1",
				PublicKey: base64.RawURLEncoding.EncodeToString(make([]byte, 32)),
			}},
		}
		return c
	}

	for name, mutate := range map[string]func(*Config){
		"duplicate id": func(c *Config) {
			key := c.External.VerificationKeys[0]
			c.External.VerificationKeys = append(c.External.VerificationKeys, key)
		},
		"missing id": func(c *Config) { c.External.VerificationKeys[0].ID = "" },
		"not base64": func(c *Config) { c.External.VerificationKeys[0].PublicKey = "not base64!" },
		"inverted window": func(c *Config) {
			c.External.VerificationKeys[0].NotBefore = Timestamp(time.Date(2040, 1, 1, 0, 0, 0, 0, time.UTC))
			c.External.VerificationKeys[0].NotAfter = Timestamp(time.Date(2030, 1, 1, 0, 0, 0, 0, time.UTC))
		},
		"service identity": func(c *Config) { c.External.ServiceIdentity = "not-a-url" },
	} {
		t.Run(name, func(t *testing.T) {
			c := base()
			mutate(&c)
			if err := c.Validate(); err == nil {
				t.Fatal("expected a validation error")
			}
		})
	}
}

func TestExternalKeyWindowUsesTimestamps(t *testing.T) {
	base := validConfig(t)
	base.Mode = "external"
	base.External = ExternalConfig{
		ServiceIdentity: "https://access.fixture.test",
		Issuer:          "fixture://issuer/a",
		Region:          "fixture-a",
		MQTTURL:         "mqtts://access.fixture.test:8883",
		VerificationKeys: []ExternalVerificationKey{{
			ID:        "issuer-1",
			PublicKey: base64.RawURLEncoding.EncodeToString(make([]byte, 32)),
		}},
	}

	// An omitted window is open on both sides: the operator pins the key, not
	// an expiry that was never written down.
	if err := base.Validate(); err != nil {
		t.Fatalf("an open window was refused: %v", err)
	}

	yaml := minimalYAML + `
mode: external
external:
  service_identity: "https://access.fixture.test"
  issuer: "fixture://issuer/a"
  region: "fixture-a"
  mqtt_url: "mqtts://access.fixture.test:8883"
  verification_keys:
    - id: k
      public_key: ` + base64.RawURLEncoding.EncodeToString(make([]byte, 32)) + `
      not_after: 2040-01-01T00:00:00Z
`
	parsed, err := Parse([]byte(yaml), env(validEnv()))
	if err != nil {
		t.Fatal(err)
	}
	key := parsed.External.VerificationKeys[0]
	if got := key.NotAfter.Time().UTC().Format(time.RFC3339); got != "2040-01-01T00:00:00Z" {
		t.Fatalf("not_after = %q", got)
	}
	if !key.NotBefore.Time().IsZero() {
		t.Fatalf("an omitted not_before must stay open: %v", key.NotBefore.Time())
	}
}

func TestExternalKeyWindowRejectsDurationsAndInvertedWindows(t *testing.T) {
	t.Setenv("WA_ACCEPTANCE", "1")
	for name, window := range map[string]string{
		"duration": "      not_after: 613620h\n",
		"inverted": "      not_before: 2040-01-01T00:00:00Z\n      not_after: 2030-01-01T00:00:00Z\n",
	} {
		t.Run(name, func(t *testing.T) {
			yaml := minimalYAML + `
mode: external
external:
  service_identity: "https://access.fixture.test"
  issuer: "fixture://issuer/a"
  region: "fixture-a"
  mqtt_url: "mqtts://access.fixture.test:8883"
  verification_keys:
    - id: k
      public_key: ` + base64.RawURLEncoding.EncodeToString(make([]byte, 32)) + `
` + window
			cfg, err := Parse([]byte(yaml), env(validEnv()))
			if err != nil {
				return // a duration fails at unmarshal time, which is correct
			}
			if err := cfg.Validate(); err == nil {
				t.Fatal("an invalid window was accepted")
			}
		})
	}
}
