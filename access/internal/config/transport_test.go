package config

import (
	"math"
	"strings"
	"testing"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func ptr[T any](value T) *T { return &value }

func transportConfig(profile string) Config {
	cfg := Default()
	cfg.Transport = TransportConfig{Profile: profile}
	return cfg
}

func TestResolveTransportPackagedProfiles(t *testing.T) {
	cases := map[string]struct {
		packet, payload, count uint32
		envelope, reserved     uint64
		concurrency            uint32
		idle, transfer         time.Duration
		httpBody               int64
	}{
		"default": {65536, 61440, 274, 16 << 20, 32 << 20, 4, 30 * time.Second, 120 * time.Second, 24 << 20},
		"compact": {32768, 28672, 293, 8 << 20, 8 << 20, 2, 30 * time.Second, 120 * time.Second, 13 << 20},
		"roomy":   {131072, 122880, 274, 32 << 20, 64 << 20, 8, 30 * time.Second, 300 * time.Second, 45 << 20},
	}
	for name, want := range cases {
		t.Run(name, func(t *testing.T) {
			effective, err := transportConfig(name).ResolveTransport()
			if err != nil {
				t.Fatalf("ResolveTransport: %v", err)
			}
			if effective.Profile != name || effective.PacketBytes != want.packet ||
				effective.FragmentPayloadBytes != want.payload || effective.FragmentCount != want.count ||
				effective.EnvelopeBytes != want.envelope || effective.ReservedBytes != want.reserved ||
				effective.BulkConcurrency != want.concurrency || effective.FragmentIdleTimeout != want.idle ||
				effective.TransferTimeout != want.transfer || effective.HTTPBodyBytes != want.httpBody {
				t.Fatalf("resolved = %+v", effective)
			}
			if effective.ResultLimitBytes != int64(want.envelope) {
				t.Fatalf("result limit = %d", effective.ResultLimitBytes)
			}
			if effective.WireProfile == whalereinv1.TransportProfile_TRANSPORT_PROFILE_UNSPECIFIED {
				t.Fatal("wire profile not resolved")
			}
		})
	}
}

func TestResolveTransportExplicitOverrides(t *testing.T) {
	cfg := transportConfig("default")
	cfg.Transport.PacketBytes = ptr[uint32](65536)
	cfg.Transport.PacketOverheadBytes = ptr[uint32](2048)
	cfg.Transport.FragmentPayloadBytes = ptr[uint32](32768)
	cfg.Transport.FragmentCount = ptr[uint32](600)
	cfg.Transport.EnvelopeBytes = ptr[uint64](8 << 20)
	cfg.Transport.BulkConcurrency = ptr[uint32](3)
	cfg.Transport.ReservedBytes = ptr[uint64](16 << 20)
	cfg.Transport.FragmentIdleTimeout = ptr(Duration(10 * time.Second))
	cfg.Transport.TransferTimeout = ptr(Duration(60 * time.Second))
	cfg.Transport.ResultLimitBytes = ptr[int64](4 << 20)
	effective, err := cfg.ResolveTransport()
	if err != nil {
		t.Fatalf("ResolveTransport: %v", err)
	}
	if effective.FragmentPayloadBytes != 32768 || effective.EnvelopeBytes != 8<<20 ||
		effective.BulkConcurrency != 3 || effective.TransferTimeout != 60*time.Second ||
		effective.ResultLimitBytes != 4<<20 {
		t.Fatalf("overrides not applied: %+v", effective)
	}
	if effective.HTTPBodyBytes < int64(Base64EncodedBytes(4<<20)) {
		t.Fatalf("derived body = %d", effective.HTTPBodyBytes)
	}
}

func TestResolveTransportRejectsInvalidRelations(t *testing.T) {
	base := func() Config { return transportConfig("default") }
	cases := map[string]func(*Config){
		"unknown profile":       func(c *Config) { c.Transport.Profile = "enormous" },
		"zero packet":           func(c *Config) { c.Transport.PacketBytes = ptr[uint32](0) },
		"zero fragment payload": func(c *Config) { c.Transport.FragmentPayloadBytes = ptr[uint32](0) },
		"packet below payload":  func(c *Config) { c.Transport.PacketBytes = ptr[uint32](32768) },
		"overhead overflow": func(c *Config) {
			c.Transport.PacketBytes = ptr[uint32](65536)
			c.Transport.FragmentPayloadBytes = ptr[uint32](61440)
			c.Transport.PacketOverheadBytes = ptr[uint32](8192)
		},
		"too few fragments": func(c *Config) { c.Transport.FragmentCount = ptr[uint32](10) },
		"zero concurrency":  func(c *Config) { c.Transport.BulkConcurrency = ptr[uint32](0) },
		"reserved below envelope": func(c *Config) {
			c.Transport.ReservedBytes = ptr[uint64](1 << 20)
		},
		"reserved below in-flight": func(c *Config) {
			c.Transport.ReservedBytes = ptr[uint64](16 << 20)
			c.Transport.EnvelopeBytes = ptr[uint64](16 << 20)
			c.Transport.BulkConcurrency = ptr[uint32](300)
			c.Transport.FragmentPayloadBytes = ptr[uint32](61440)
			c.Transport.FragmentCount = ptr[uint32](274)
		},
		"zero idle timeout":    func(c *Config) { c.Transport.FragmentIdleTimeout = ptr(Duration(0)) },
		"negative idle":        func(c *Config) { c.Transport.FragmentIdleTimeout = ptr(Duration(-time.Second)) },
		"idle at transfer":     func(c *Config) { c.Transport.FragmentIdleTimeout = ptr(Duration(120 * time.Second)) },
		"zero transfer":        func(c *Config) { c.Transport.TransferTimeout = ptr(Duration(0)) },
		"sub-millisecond time": func(c *Config) { c.Transport.TransferTimeout = ptr(Duration(1500 * time.Microsecond)) },
		"result above envelope": func(c *Config) {
			c.Requests.MaxResultBytes = 32 << 20
		},
		"body below derived": func(c *Config) { c.HTTP.MaxBodyBytes = 1 << 20 },
		"negative body":      func(c *Config) { c.HTTP.MaxBodyBytes = -1 },
	}
	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			cfg := base()
			mutate(&cfg)
			if _, err := cfg.ResolveTransport(); err == nil {
				t.Fatal("invalid transport configuration accepted")
			}
			if err := cfg.Validate(); err == nil {
				t.Fatal("invalid transport configuration passed Validate")
			}
		})
	}
}

func TestResolveTransportOverflowAndRepresentability(t *testing.T) {
	cfg := transportConfig("default")
	cfg.Transport.FragmentCount = ptr[uint32](100)
	cfg.Transport.EnvelopeBytes = ptr[uint64](1 << 40)
	if _, err := cfg.ResolveTransport(); err == nil || !strings.Contains(err.Error(), "fragment") {
		t.Fatalf("uncovered huge envelope accepted: %v", err)
	}

	cfg = transportConfig("default")
	cfg.Transport.TransferTimeout = ptr(Duration(1 << 32 * time.Millisecond))
	if _, err := cfg.ResolveTransport(); err == nil {
		t.Fatal("unrepresentable timeout accepted")
	}
}

func TestParseTransportRejectsGarbageAndDerivesBounds(t *testing.T) {
	raw := strings.Replace(minimalYAML, "internal:", "transport:\n  profile: roomy\ninternal:", 1)
	cfg, err := Parse([]byte(raw), env(validEnv()))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if cfg.Requests.MaxResultBytes != 32<<20 || cfg.HTTP.MaxBodyBytes != 45<<20 {
		t.Fatalf("derived bounds not materialized: result=%d body=%d", cfg.Requests.MaxResultBytes, cfg.HTTP.MaxBodyBytes)
	}

	garbage := strings.Replace(minimalYAML, "internal:", "transport:\n  packet_bytes: -1\ninternal:", 1)
	if _, err := Parse([]byte(garbage), env(validEnv())); err == nil {
		t.Fatal("negative byte override accepted")
	}
	garbage = strings.Replace(minimalYAML, "internal:", "transport:\n  profile: not-a-profile\ninternal:", 1)
	if _, err := Parse([]byte(garbage), env(validEnv())); err == nil {
		t.Fatal("unknown profile accepted")
	}
	garbage = strings.Replace(minimalYAML, "internal:", "transport:\n  fragment_payload_bytes: 999999999999\ninternal:", 1)
	if _, err := Parse([]byte(garbage), env(validEnv())); err == nil {
		t.Fatal("overflowing byte override accepted")
	}
}

func TestDerivedHTTPBodyCoversBase64AndFraming(t *testing.T) {
	body, err := DerivedHTTPBodyBytes(16 << 20)
	if err != nil || body != 24<<20 {
		t.Fatalf("default body = %d (%v)", body, err)
	}
	body, err = DerivedHTTPBodyBytes(32 << 20)
	if err != nil || body != 45<<20 {
		t.Fatalf("roomy body = %d (%v)", body, err)
	}
	if _, err := DerivedHTTPBodyBytes(math.MaxUint64); err == nil {
		t.Fatal("overflow accepted")
	}
}
