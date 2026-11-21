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
		packet, concurrency uint32
		envelope, reserved  uint64
		transfer            time.Duration
		httpBody            int64
	}{
		"default": {65536, 4, 16 << 20, 32 << 20, 120 * time.Second, 24 << 20},
		"compact": {32768, 2, 8 << 20, 8 << 20, 120 * time.Second, 13 << 20},
		"roomy":   {131072, 8, 32 << 20, 64 << 20, 300 * time.Second, 45 << 20},
	}
	for name, want := range cases {
		t.Run(name, func(t *testing.T) {
			effective, err := transportConfig(name).ResolveTransport()
			if err != nil {
				t.Fatalf("ResolveTransport: %v", err)
			}
			if effective.Profile != name || effective.PacketBytes != want.packet ||
				effective.EnvelopeBytes != want.envelope || effective.ReservedBytes != want.reserved ||
				effective.BulkConcurrency != want.concurrency ||
				effective.TransferTimeout != want.transfer || effective.HTTPBodyBytes != want.httpBody {
				t.Fatalf("resolved = %+v", effective)
			}
			if effective.ResultLimitBytes != int64(want.envelope) {
				t.Fatalf("result limit = %d", effective.ResultLimitBytes)
			}
			if effective.ReceiveMaximum != defaultReceiveMaximum || effective.MaxInFlight() != int(defaultSendInFlight) {
				t.Fatalf("ordered defaults = %+v", effective)
			}
			if effective.QueueDepth() != int(defaultSendQueueDepth) || effective.QueueBytes() != int64(defaultSendQueueBytes) {
				t.Fatalf("queue defaults = %d/%d", effective.QueueDepth(), effective.QueueBytes())
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
	cfg.Transport.EnvelopeBytes = ptr[uint64](8 << 20)
	cfg.Transport.BulkConcurrency = ptr[uint32](3)
	cfg.Transport.ReservedBytes = ptr[uint64](16 << 20)
	cfg.Transport.TransferTimeout = ptr(Duration(60 * time.Second))
	cfg.Transport.ResultLimitBytes = ptr[int64](4 << 20)
	cfg.Transport.ReceiveMaximum = ptr[uint16](8)
	cfg.Transport.SendInFlight = ptr[uint32](4)
	cfg.Transport.SendQueueDepth = ptr[uint32](64)
	cfg.Transport.SendQueueBytes = ptr[uint64](8 << 20)
	effective, err := cfg.ResolveTransport()
	if err != nil {
		t.Fatalf("ResolveTransport: %v", err)
	}
	if effective.EnvelopeBytes != 8<<20 ||
		effective.BulkConcurrency != 3 || effective.TransferTimeout != 60*time.Second ||
		effective.ResultLimitBytes != 4<<20 {
		t.Fatalf("overrides not applied: %+v", effective)
	}
	if effective.MaxReceive() != 8 || effective.MaxInFlight() != 4 ||
		effective.QueueDepth() != 64 || effective.QueueBytes() != 8<<20 {
		t.Fatalf("ordered overrides not applied: %+v", effective)
	}
	if effective.HTTPBodyBytes < int64(Base64EncodedBytes(4<<20)) {
		t.Fatalf("derived body = %d", effective.HTTPBodyBytes)
	}
}

func TestResolveTransportRejectsInvalidRelations(t *testing.T) {
	base := func() Config { return transportConfig("default") }
	cases := map[string]func(*Config){
		"unknown profile": func(c *Config) { c.Transport.Profile = "enormous" },
		"zero packet":     func(c *Config) { c.Transport.PacketBytes = ptr[uint32](0) },
		"packet above broker": func(c *Config) {
			c.Transport.PacketBytes = ptr[uint32](uint32(whalereinv1.BrokerPacketLimitBytes) + 1)
			c.Transport.EnvelopeBytes = ptr[uint64](32 << 20)
			c.Transport.ReservedBytes = ptr[uint64](64 << 20)
		},
		"overhead at packet": func(c *Config) {
			c.Transport.PacketBytes = ptr[uint32](65536)
			c.Transport.PacketOverheadBytes = ptr[uint32](65536)
		},
		"envelope below packet": func(c *Config) {
			c.Transport.PacketBytes = ptr[uint32](65536)
			c.Transport.EnvelopeBytes = ptr[uint64](32 << 10)
		},
		"zero concurrency":        func(c *Config) { c.Transport.BulkConcurrency = ptr[uint32](0) },
		"reserved below envelope": func(c *Config) { c.Transport.ReservedBytes = ptr[uint64](1 << 20) },
		"zero transfer":           func(c *Config) { c.Transport.TransferTimeout = ptr(Duration(0)) },
		"sub-millisecond time":    func(c *Config) { c.Transport.TransferTimeout = ptr(Duration(1500 * time.Microsecond)) },
		"zero receive maximum":    func(c *Config) { c.Transport.ReceiveMaximum = ptr[uint16](0) },
		"zero in flight":          func(c *Config) { c.Transport.SendInFlight = ptr[uint32](0) },
		"zero queue depth":        func(c *Config) { c.Transport.SendQueueDepth = ptr[uint32](0) },
		"queue below packet":      func(c *Config) { c.Transport.SendQueueBytes = ptr[uint64](1024) },
		"result above envelope":   func(c *Config) { c.Requests.MaxResultBytes = 32 << 20 },
		"body below derived":      func(c *Config) { c.HTTP.MaxBodyBytes = 1 << 20 },
		"negative body":           func(c *Config) { c.HTTP.MaxBodyBytes = -1 },
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
	cfg.Transport.EnvelopeBytes = ptr[uint64](1 << 63)
	cfg.Transport.ReservedBytes = ptr[uint64](1 << 63)
	if _, err := cfg.ResolveTransport(); err == nil || !strings.Contains(err.Error(), "representable") {
		t.Fatalf("unrepresentable envelope accepted: %v", err)
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
	garbage = strings.Replace(minimalYAML, "internal:", "transport:\n  packet_bytes: 999999999999\ninternal:", 1)
	if _, err := Parse([]byte(garbage), env(validEnv())); err == nil {
		t.Fatal("overflowing byte override accepted")
	}
	garbage = strings.Replace(minimalYAML, "internal:", "transport:\n  receive_maximum: 0\ninternal:", 1)
	if _, err := Parse([]byte(garbage), env(validEnv())); err == nil {
		t.Fatal("zero receive maximum accepted")
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
