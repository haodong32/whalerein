package config

import (
	"math"
	"strings"
	"testing"
	"time"

	"gopkg.in/yaml.v3"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// A resolved budget keeps its packaged wire identity so the peer validates the
// exact profile rather than a caller-supplied name.
func TestTransportProfileWireMappingRejectsUnknownNames(t *testing.T) {
	for _, name := range whalereinv1.TransportProfileNames() {
		wire, ok := WireNameForProfile(name)
		if !ok || wire == "" {
			t.Fatalf("profile %q has no wire name", name)
		}
		resolved, ok := ProfileForWireName(wire)
		if !ok || resolved != name {
			t.Fatalf("wire %q resolved to %q/%v, want %q", wire, resolved, ok, name)
		}
	}
	if _, ok := WireNameForProfile("not-a-profile"); ok {
		t.Fatal("unknown profile name resolved")
	}
	if _, ok := ProfileForWireName("not.a.wire.Name"); ok {
		t.Fatal("unknown wire name resolved")
	}
	if got := wireProfileFor("not.a.wire.Name"); got != whalereinv1.TransportProfile_TRANSPORT_PROFILE_UNSPECIFIED {
		t.Fatalf("unknown wire profile = %v", got)
	}
}

// An empty profile selects the packaged default, while a negative result
// override must fail instead of silently falling back.
func TestResolveTransportDefaultsAndRejectsNegativeResultOverride(t *testing.T) {
	cfg := transportConfig("")
	effective, err := cfg.ResolveTransport()
	if err != nil {
		t.Fatalf("empty profile: %v", err)
	}
	if effective.Profile != whalereinv1.DefaultTransportProfile || effective.EnvelopeBytes == 0 {
		t.Fatalf("default profile not resolved: %+v", effective)
	}

	cfg = transportConfig("default")
	cfg.Transport.ResultLimitBytes = ptr[int64](-1)
	if _, err := cfg.ResolveTransport(); err == nil || !strings.Contains(err.Error(), "result_limit_bytes") {
		t.Fatalf("negative result override = %v", err)
	}
}

func TestTransportBoundsRejectUnrepresentableValues(t *testing.T) {
	if Base64EncodedBytes(0) != 0 {
		t.Fatal("empty payload has a nonzero encoding")
	}
	if got := Base64EncodedBytes(math.MaxUint64); got != math.MaxUint64 {
		t.Fatalf("overflowing base64 bound = %d", got)
	}
	if _, err := resolveResultLimit(1<<63, nil, 0); err == nil {
		t.Fatal("unrepresentable envelope accepted as a result bound")
	}
	if _, err := resolveHTTPBodyBytes(-1, 0); err == nil {
		t.Fatal("negative result bound produced an HTTP body bound")
	}
	if _, err := resolveHTTPBodyBytes(math.MaxInt64, 0); err == nil {
		t.Fatal("unrepresentable HTTP body bound accepted")
	}
}

// Timestamps round-trip as RFC 3339 and an open bound is the empty string.
func TestTimestampRendersOpenAndClosedBounds(t *testing.T) {
	var open Timestamp
	marshaled, err := open.MarshalYAML()
	if err != nil || marshaled != "" {
		t.Fatalf("open bound = %v/%v", marshaled, err)
	}
	if err := open.UnmarshalYAML(&yaml.Node{Kind: yaml.ScalarNode, Value: "  "}); err != nil {
		t.Fatal(err)
	}
	if !open.Time().IsZero() {
		t.Fatalf("blank timestamp = %v", open.Time())
	}
	var closed Timestamp
	if err := closed.UnmarshalYAML(&yaml.Node{Kind: yaml.ScalarNode, Value: "2030-01-02T03:04:05+02:00"}); err != nil {
		t.Fatal(err)
	}
	rendered, err := closed.MarshalYAML()
	if err != nil || rendered != "2030-01-02T01:04:05Z" {
		t.Fatalf("closed bound = %v/%v", rendered, err)
	}
	for name, node := range map[string]*yaml.Node{
		"timestamp mapping": {Kind: yaml.MappingNode},
		"duration sequence": {Kind: yaml.SequenceNode},
		"timestamp garbage": {Kind: yaml.ScalarNode, Value: "tomorrow"},
		"duration garbage":  {Kind: yaml.ScalarNode, Value: "soon"},
	} {
		t.Run(name, func(t *testing.T) {
			var duration Duration
			var timestamp Timestamp
			var err error
			if strings.HasPrefix(name, "duration") {
				err = duration.UnmarshalYAML(node)
			} else {
				err = timestamp.UnmarshalYAML(node)
			}
			if err == nil {
				t.Fatal("invalid YAML value accepted")
			}
		})
	}
}

func TestPublicMQTTURLDerivationDefaultsThePort(t *testing.T) {
	for _, tc := range []struct{ public, want string }{
		{"http://access.example.com", "mqtt://access.example.com:1883"},
		{"https://access.example.com", "mqtts://access.example.com:443"},
	} {
		got, derived := (Config{PublicURL: tc.public}).ResolveMQTTPublicURL()
		if !derived || got != tc.want {
			t.Fatalf("%s = %q/%v, want %q", tc.public, got, derived, tc.want)
		}
	}
}

func TestValidationRejectsNonAbsoluteBrokerURLs(t *testing.T) {
	cfg := validConfig(t)
	cfg.Broker.PublicURL = ":::"
	cfg.Broker.URL = "tcp://"
	err := cfg.Validate()
	if err == nil {
		t.Fatal("non-absolute broker URLs accepted")
	}
	for _, want := range []string{"broker.public_url must be an absolute URL", "broker.url must be an absolute URL"} {
		if !strings.Contains(err.Error(), want) {
			t.Fatalf("missing %q in %v", want, err)
		}
	}
}

func TestLoopbackHostRecognizesNamesAndAddresses(t *testing.T) {
	for host, want := range map[string]bool{
		"localhost": true, "LOCALHOST": true, "127.0.0.1": true, "::1": true,
		"mqtt.example.com": false, "10.0.0.1": false, "": false,
	} {
		if got := loopbackHost(host); got != want {
			t.Fatalf("loopbackHost(%q) = %v", host, got)
		}
	}
}

func TestParseWithoutLookupKeepsLiteralEnvironmentReference(t *testing.T) {
	// A nil lookup must fail closed rather than panic or read the process
	// environment, preserving the documented missing-variable error.
	_, err := Parse([]byte(minimalYAML), nil)
	if err == nil || !strings.Contains(err.Error(), "environment") {
		t.Fatalf("nil lookup = %v", err)
	}
}

func TestRuntimeSeparatedFromTransportTiming(t *testing.T) {
	// The packaged duration fields must stay representable whole milliseconds;
	// a sub-millisecond value is not a valid transport budget.
	if _, err := durationMillis(1500 * time.Microsecond); err == nil {
		t.Fatal("sub-millisecond duration accepted")
	}
	if _, err := durationMillis(time.Duration(math.MaxUint32+1) * time.Millisecond); err == nil {
		t.Fatal("unrepresentable duration accepted")
	}
}
