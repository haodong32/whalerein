// Package config loads and validates the WhaleRein Access configuration.
//
// Secrets are always read from environment variables through ${NAME}
// substitution. Configuration values are never logged; callers can use
// Redacted for a safe startup summary.
package config

import (
	"bytes"
	"encoding/base64"
	"errors"
	"fmt"
	"net"
	"net/url"
	"os"
	"regexp"
	"strings"
	"time"

	"gopkg.in/yaml.v3"
)

// Duration is a YAML duration such as "10m" or "500ms".
type Duration time.Duration

// UnmarshalYAML implements yaml.Unmarshaler.
func (d *Duration) UnmarshalYAML(value *yaml.Node) error {
	if value.Kind != yaml.ScalarNode {
		return fmt.Errorf("duration must be a string")
	}
	parsed, err := time.ParseDuration(value.Value)
	if err != nil {
		return fmt.Errorf("invalid duration %q", value.Value)
	}
	*d = Duration(parsed)
	return nil
}

// MarshalYAML implements yaml.Marshaler.
func (d Duration) MarshalYAML() (any, error) { return time.Duration(d).String(), nil }

// D returns the standard library duration.
func (d Duration) D() time.Duration { return time.Duration(d) }

// Config is the complete Access configuration.
type Config struct {
	Mode                  string           `yaml:"mode"`
	Listen                string           `yaml:"listen"`
	InternalListen        string           `yaml:"internal_listen"`
	PublicURL             string           `yaml:"public_url"`
	DatabaseURL           string           `yaml:"database_url"`
	AdminToken            string           `yaml:"admin_token"`
	HostProvisioningToken string           `yaml:"host_provisioning_token"`
	Broker                BrokerConfig     `yaml:"broker"`
	Line                  LineConfig       `yaml:"line"`
	Admission             AdmissionConfig  `yaml:"admission"`
	Internal              InternalConfig   `yaml:"internal"`
	HTTP                  HTTPConfig       `yaml:"http"`
	External              ExternalConfig   `yaml:"external"`
	Pairing               PairingConfig    `yaml:"pairing"`
	Requests              RequestConfig    `yaml:"requests"`
	Revocation            RevocationConfig `yaml:"revocation"`
	Presence              PresenceConfig   `yaml:"presence"`
	Cleanup               CleanupConfig    `yaml:"cleanup"`
	Protocol              ProtocolConfig   `yaml:"protocol"`
	Log                   LogConfig        `yaml:"log"`
}

// ExternalConfig describes external account mode.
//
// External mode is explicit: an operator selects the trusted account service
// and its issuer keys, and Access never falls back to Standalone or to an
// unconfigured authority.
type ExternalConfig struct {
	ServiceIdentity  string                    `yaml:"service_identity"`
	Issuer           string                    `yaml:"issuer"`
	Region           string                    `yaml:"region"`
	MQTTURL          string                    `yaml:"mqtt_url"`
	ClockSkew        Duration                  `yaml:"clock_skew"`
	VerificationKeys []ExternalVerificationKey `yaml:"verification_keys"`
}

// ExternalVerificationKey is one pinned issuer key with its validity window.
type ExternalVerificationKey struct {
	ID        string   `yaml:"id"`
	PublicKey string   `yaml:"public_key"`
	NotBefore Duration `yaml:"not_before"`
	NotAfter  Duration `yaml:"not_after"`
}

// BrokerConfig describes the MQTT 5 broker connection.
type BrokerConfig struct {
	URL                string   `yaml:"url"`
	PublicURL          string   `yaml:"public_url"`
	ClientID           string   `yaml:"client_id"`
	Username           string   `yaml:"username"`
	Password           string   `yaml:"password"`
	CAFile             string   `yaml:"ca_file"`
	ServerName         string   `yaml:"server_name"`
	InsecureSkipVerify bool     `yaml:"insecure_skip_verify"`
	ConnectTimeout     Duration `yaml:"connect_timeout"`
	PublishTimeout     Duration `yaml:"publish_timeout"`
	KeepAlive          Duration `yaml:"keep_alive"`
	ReconnectMin       Duration `yaml:"reconnect_min"`
	ReconnectMax       Duration `yaml:"reconnect_max"`
}

// LineConfig bounds Mosquitto policy reconciliation.
type LineConfig struct {
	PolicyTTL         Duration `yaml:"policy_ttl"`
	ReconcileInterval Duration `yaml:"reconcile_interval"`
	HeartbeatInterval Duration `yaml:"heartbeat_interval"`
	OperationTimeout  Duration `yaml:"operation_timeout"`
}

// AdmissionConfig bounds the local grant cache.
type AdmissionConfig struct {
	MaxEntries         int      `yaml:"max_entries"`
	TTL                Duration `yaml:"ttl"`
	LoadTimeout        Duration `yaml:"load_timeout"`
	MaxBackgroundLoads int      `yaml:"max_background_loads"`
}

// InternalConfig guards the broker callback endpoints.
type InternalConfig struct {
	CallbackToken string `yaml:"callback_token"`
}

// HTTPConfig holds public/internal HTTP server tunables.
type HTTPConfig struct {
	ReadHeaderTimeout Duration `yaml:"read_header_timeout"`
	ReadTimeout       Duration `yaml:"read_timeout"`
	WriteTimeout      Duration `yaml:"write_timeout"`
	IdleTimeout       Duration `yaml:"idle_timeout"`
	MaxBodyBytes      int64    `yaml:"max_body_bytes"`
}

// PairingConfig holds one-time invitation and claim tunables.
type PairingConfig struct {
	InvitationTTL Duration `yaml:"invitation_ttl"`
	ClaimTimeout  Duration `yaml:"claim_timeout"`
	ClaimTTL      Duration `yaml:"claim_ttl"`
	MaxFailures   int      `yaml:"max_failures"`
}

// RequestConfig holds forwarding and retention tunables.
type RequestConfig struct {
	MaxTTL          Duration `yaml:"max_ttl"`
	ForwardInterval Duration `yaml:"forward_interval"`
	Lease           Duration `yaml:"lease"`
	RetryInterval   Duration `yaml:"retry_interval"`
	MaxAttempts     int      `yaml:"max_attempts"`
	BatchSize       int      `yaml:"batch_size"`
}

// RevocationConfig holds revocation acknowledgement wait time.
type RevocationConfig struct {
	AckTimeout Duration `yaml:"ack_timeout"`
}

// PresenceConfig controls how long broker presence is cached.
type PresenceConfig struct {
	CacheTTL Duration `yaml:"cache_ttl"`
}

// CleanupConfig controls periodic retention work.
type CleanupConfig struct {
	Interval              Duration `yaml:"interval"`
	PairingTokenRetention Duration `yaml:"pairing_token_retention"`
}

// ProtocolConfig pins the supported wire protocol version.
type ProtocolConfig struct {
	Version uint32 `yaml:"version"`
}

// LogConfig configures structured logging.
type LogConfig struct {
	Level string `yaml:"level"`
}

var envPattern = regexp.MustCompile(`\$\{([A-Za-z_][A-Za-z0-9_]*)(:-([^}]*))?\}`)

// Default returns the baseline configuration used with a YAML overlay.
func Default() Config {
	return Config{
		Mode:           "standalone",
		Listen:         ":8080",
		InternalListen: "127.0.0.1:8081",
		Broker: BrokerConfig{
			ClientID:       "whalerein-access",
			Username:       "access",
			ConnectTimeout: Duration(10 * time.Second),
			PublishTimeout: Duration(5 * time.Second),
			KeepAlive:      Duration(30 * time.Second),
			ReconnectMin:   Duration(500 * time.Millisecond),
			ReconnectMax:   Duration(30 * time.Second),
		},
		Line: LineConfig{
			PolicyTTL:         Duration(10 * time.Second),
			ReconcileInterval: Duration(30 * time.Second),
			HeartbeatInterval: Duration(5 * time.Second),
			OperationTimeout:  Duration(5 * time.Second),
		},
		Admission: AdmissionConfig{
			MaxEntries:         10000,
			TTL:                Duration(5 * time.Minute),
			LoadTimeout:        Duration(250 * time.Millisecond),
			MaxBackgroundLoads: 64,
		},
		HTTP: HTTPConfig{
			ReadHeaderTimeout: Duration(5 * time.Second),
			ReadTimeout:       Duration(30 * time.Second),
			WriteTimeout:      Duration(30 * time.Second),
			IdleTimeout:       Duration(60 * time.Second),
			MaxBodyBytes:      24 << 20,
		},
		Pairing: PairingConfig{
			InvitationTTL: Duration(10 * time.Minute),
			ClaimTimeout:  Duration(15 * time.Second),
			ClaimTTL:      Duration(5 * time.Minute),
			MaxFailures:   5,
		},
		Requests: RequestConfig{
			MaxTTL:          Duration(24 * time.Hour),
			ForwardInterval: Duration(500 * time.Millisecond),
			Lease:           Duration(30 * time.Second),
			RetryInterval:   Duration(10 * time.Second),
			MaxAttempts:     5,
			BatchSize:       50,
		},
		Revocation: RevocationConfig{AckTimeout: Duration(10 * time.Second)},
		Presence:   PresenceConfig{CacheTTL: Duration(15 * time.Second)},
		Cleanup: CleanupConfig{
			Interval:              Duration(time.Minute),
			PairingTokenRetention: Duration(24 * time.Hour),
		},
		Protocol: ProtocolConfig{Version: 1},
		Log:      LogConfig{Level: "info"},
	}
}

// Load reads a YAML file, substitutes ${ENV} references, and validates it.
func Load(path string) (Config, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return Config{}, fmt.Errorf("read config: %w", err)
	}
	return Parse(raw, os.LookupEnv)
}

// LoadWithEnv is Load with an explicit environment lookup, for tests.
func LoadWithEnv(path string, lookup func(string) (string, bool)) (Config, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return Config{}, fmt.Errorf("read config: %w", err)
	}
	return Parse(raw, lookup)
}

// Parse expands environment references, decodes the YAML overlay, applies
// defaults and validates the result.
func Parse(raw []byte, lookup func(string) (string, bool)) (Config, error) {
	if lookup == nil {
		lookup = func(string) (string, bool) { return "", false }
	}
	expanded, err := expandEnv(raw, lookup)
	if err != nil {
		return Config{}, err
	}
	cfg := Default()
	decoder := yaml.NewDecoder(bytes.NewReader(expanded))
	decoder.KnownFields(true)
	if err := decoder.Decode(&cfg); err != nil {
		return Config{}, fmt.Errorf("decode config: %w", err)
	}
	if err := cfg.Validate(); err != nil {
		return Config{}, err
	}
	return cfg, nil
}

func expandEnv(raw []byte, lookup func(string) (string, bool)) ([]byte, error) {
	// "$$" escapes a literal dollar sign.
	const placeholder = "\x00DOLLAR\x00"
	replaced := strings.ReplaceAll(string(raw), "$$", placeholder)
	var missing []string
	out := envPattern.ReplaceAllStringFunc(replaced, func(match string) string {
		groups := envPattern.FindStringSubmatch(match)
		name := groups[1]
		value, ok := lookup(name)
		if !ok {
			if groups[2] != "" {
				return groups[3]
			}
			missing = append(missing, name)
			return ""
		}
		return value
	})
	if len(missing) > 0 {
		return nil, fmt.Errorf("missing required environment variable(s): %s", strings.Join(missing, ", "))
	}
	return []byte(strings.ReplaceAll(out, placeholder, "$")), nil
}

// validateExternal checks the external account mode settings.
func (c Config) validateExternal() []string {
	var problems []string
	require := func(name, value string) {
		if strings.TrimSpace(value) == "" {
			problems = append(problems, name+" is required")
		}
	}
	require("external.service_identity", c.External.ServiceIdentity)
	require("external.issuer", c.External.Issuer)
	require("external.region", c.External.Region)
	require("external.mqtt_url", c.External.MQTTURL)
	if len(c.External.VerificationKeys) == 0 {
		problems = append(problems, "external.verification_keys needs at least one key")
	}
	if identity, err := url.Parse(c.External.ServiceIdentity); err != nil || identity.Host == "" {
		problems = append(problems, "external.service_identity must be an absolute URL")
	}
	seen := map[string]bool{}
	for index, key := range c.External.VerificationKeys {
		label := fmt.Sprintf("external.verification_keys[%d]", index)
		if strings.TrimSpace(key.ID) == "" {
			problems = append(problems, label+".id is required")
		} else if seen[key.ID] {
			problems = append(problems, label+".id is duplicated")
		} else {
			seen[key.ID] = true
		}
		if _, err := base64.RawURLEncoding.DecodeString(key.PublicKey); err != nil {
			problems = append(problems, label+".public_key must be raw URL base64")
		}
		if key.NotBefore != 0 && key.NotAfter != 0 &&
			time.Duration(key.NotBefore) >= time.Duration(key.NotAfter) {
			problems = append(problems, label+".not_before must precede not_after")
		}
	}
	return problems
}

// Validate checks required values and bounded tunables without echoing
// secret values.
func (c Config) Validate() error {
	var problems []string
	switch c.Mode {
	case "standalone":
		if len(c.External.VerificationKeys) > 0 || c.External.Issuer != "" {
			problems = append(problems, "external settings are only valid in external mode")
		}
	case "external":
		problems = append(problems, c.validateExternal()...)
	default:
		problems = append(problems, `mode must be "standalone" or "external"`)
	}
	require := func(name, value string) {
		if strings.TrimSpace(value) == "" {
			problems = append(problems, name+" is required")
		}
	}
	require("listen", c.Listen)
	require("internal_listen", c.InternalListen)
	if c.Listen == c.InternalListen {
		problems = append(problems, "internal_listen must differ from listen")
	}
	require("public_url", c.PublicURL)
	if c.PublicURL != "" {
		u, err := url.Parse(c.PublicURL)
		if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
			problems = append(problems, "public_url must be an absolute http(s) URL")
		}
	}
	require("database_url", c.DatabaseURL)
	require("admin_token", c.AdminToken)
	require("host_provisioning_token", c.HostProvisioningToken)
	require("broker.url", c.Broker.URL)
	if c.Broker.PublicURL != "" {
		u, err := url.Parse(c.Broker.PublicURL)
		if err != nil || u.Host == "" {
			problems = append(problems, "broker.public_url must be an absolute URL")
		} else {
			switch u.Scheme {
			case "mqtt", "mqtts", "tcp", "tls", "ssl":
			default:
				problems = append(problems, "broker.public_url must use mqtt, mqtts, tcp, tls or ssl")
			}
		}
	}
	if c.Broker.URL != "" {
		u, err := url.Parse(c.Broker.URL)
		switch u.Scheme {
		case "tcp", "tls", "mqtt", "mqtts", "ssl":
		default:
			problems = append(problems, "broker.url must use tcp, tls, mqtt, mqtts or ssl")
		}
		if err != nil || u.Host == "" {
			problems = append(problems, "broker.url must be an absolute URL")
		}
	}
	require("broker.username", c.Broker.Username)
	require("broker.password", c.Broker.Password)
	require("internal.callback_token", c.Internal.CallbackToken)
	if c.Internal.CallbackToken != "" && c.Internal.CallbackToken == c.AdminToken {
		problems = append(problems, "internal.callback_token must differ from admin_token")
	}
	if c.HTTP.MaxBodyBytes <= 0 {
		problems = append(problems, "http.max_body_bytes must be positive")
	}
	if c.HTTP.ReadHeaderTimeout.D() <= 0 || c.HTTP.ReadTimeout.D() <= 0 ||
		c.HTTP.WriteTimeout.D() <= 0 || c.HTTP.IdleTimeout.D() <= 0 {
		problems = append(problems, "http timeouts must be positive")
	}
	if c.Line.PolicyTTL.D() <= 0 || c.Line.ReconcileInterval.D() <= 0 ||
		c.Line.HeartbeatInterval.D() <= 0 || c.Line.OperationTimeout.D() <= 0 {
		problems = append(problems, "line durations must be positive")
	}
	if c.Line.HeartbeatInterval.D() >= c.Line.PolicyTTL.D() {
		problems = append(problems, "line.heartbeat_interval must be shorter than line.policy_ttl")
	}
	if c.Admission.MaxEntries <= 0 || c.Admission.TTL.D() <= 0 ||
		c.Admission.LoadTimeout.D() <= 0 || c.Admission.MaxBackgroundLoads <= 0 {
		problems = append(problems, "admission tunables must be positive")
	}
	if c.Pairing.InvitationTTL <= 0 || c.Pairing.ClaimTimeout <= 0 || c.Pairing.ClaimTTL <= 0 {
		problems = append(problems, "pairing durations must be positive")
	}
	if c.Pairing.MaxFailures <= 0 {
		problems = append(problems, "pairing.max_failures must be positive")
	}
	if c.Requests.MaxTTL <= 0 || c.Requests.ForwardInterval <= 0 || c.Requests.Lease <= 0 ||
		c.Requests.RetryInterval <= 0 || c.Requests.BatchSize <= 0 || c.Requests.MaxAttempts <= 0 {
		problems = append(problems, "requests tunables must be positive")
	}
	if c.Revocation.AckTimeout <= 0 {
		problems = append(problems, "revocation.ack_timeout must be positive")
	}
	if c.Presence.CacheTTL <= 0 {
		problems = append(problems, "presence.cache_ttl must be positive")
	}
	if c.Cleanup.Interval <= 0 || c.Cleanup.PairingTokenRetention <= 0 {
		problems = append(problems, "cleanup durations must be positive")
	}
	if c.Protocol.Version == 0 {
		problems = append(problems, "protocol.version must be positive")
	}
	switch c.Broker.ConnectTimeout.D() <= 0 || c.Broker.PublishTimeout.D() <= 0 || c.Broker.KeepAlive.D() <= 0 ||
		c.Broker.ReconnectMin.D() <= 0 || c.Broker.ReconnectMax.D() <= 0 {
	case true:
		problems = append(problems, "broker durations must be positive")
	}
	switch c.Log.Level {
	case "debug", "info", "warn", "error":
	default:
		problems = append(problems, "log.level must be debug, info, warn or error")
	}
	if len(problems) > 0 {
		return errors.New("invalid config: " + strings.Join(problems, "; "))
	}
	return nil
}

// ResolveMQTTPublicURL returns the broker URL advertised in pairing URIs. When
// broker.public_url is omitted it is derived from public_url and the returned
// derived flag lets the caller warn about the fallback.
func (c Config) ResolveMQTTPublicURL() (string, bool) {
	if strings.TrimSpace(c.Broker.PublicURL) != "" {
		return strings.TrimRight(c.Broker.PublicURL, "/"), false
	}
	u, err := url.Parse(c.PublicURL)
	if err != nil || u.Hostname() == "" {
		return "", false
	}
	host := u.Hostname()
	port := u.Port()
	scheme := "mqtts"
	if u.Scheme == "http" {
		scheme = "mqtt"
	}
	if port == "" {
		if u.Scheme == "https" {
			port = "443"
		} else {
			port = "1883"
		}
	}
	return scheme + "://" + net.JoinHostPort(host, port), true
}

func sanitizeURL(raw string) string {
	parsed, err := url.Parse(raw)
	if err != nil || parsed.User == nil {
		return raw
	}
	parsed.User = nil
	return parsed.String()
}

// Redacted returns a log-safe summary. No token, password, secret or API key
// value is included.
func (c Config) Redacted() map[string]any {
	return map[string]any{
		"listen":            c.Listen,
		"internal_listen":   c.InternalListen,
		"mode":              c.Mode,
		"public_url":        sanitizeURL(c.PublicURL),
		"broker_url":        sanitizeURL(c.Broker.URL),
		"broker_public_url": sanitizeURL(c.Broker.PublicURL),
		"broker_username":   c.Broker.Username,
		"line_policy_ttl":   c.Line.PolicyTTL.D().String(),
		"admission_entries": c.Admission.MaxEntries,
		"protocol_version":  c.Protocol.Version,
		"log_level":         c.Log.Level,
	}
}
