package config

import (
	"strings"
	"testing"
)

// ACC-2 regression: broker.url validation.
//
// A malformed broker.url must return a validation error naming broker.url, not
// panic. url.Parse returns a nil *url.URL for several malformed inputs (a
// missing ']', a non-numeric port, an invalid escape), so validation must check
// err before reading any URL field. This affects both `-check-config` and
// normal startup.
func TestReproMalformedBrokerURLIsAValidationErrorNotAPanic(t *testing.T) {
	malformed := []string{
		"tls://[::1",             // missing ']' in host
		"tcp://mqtt.example:bad", // invalid port
		"://missing-scheme",      // missing protocol scheme
		"tcp://%zz",              // invalid URL escape
	}
	for _, raw := range malformed {
		t.Run(raw, func(t *testing.T) {
			cfg := validConfig(t)
			cfg.Broker.URL = raw

			var err error
			func() {
				defer func() {
					if recovered := recover(); recovered != nil {
						t.Fatalf("Validate panicked on broker.url %q instead of returning a validation error: %v", raw, recovered)
					}
				}()
				err = cfg.Validate()
			}()
			if err == nil || !strings.Contains(err.Error(), "broker.url") {
				t.Fatalf("broker.url %q: err = %v, want a broker.url validation error", raw, err)
			}
		})
	}

	// The same malformed value through the real load path (Parse) must not
	// panic either: this is what `-check-config` calls.
	t.Run("parse path", func(t *testing.T) {
		yaml := strings.Replace(minimalYAML, "tls://mqtt.example.com:8883", "tls://[::1", 1)
		var parseErr error
		func() {
			defer func() {
				if recovered := recover(); recovered != nil {
					t.Fatalf("Parse panicked on a malformed broker.url: %v", recovered)
				}
			}()
			_, parseErr = Parse([]byte(yaml), env(validEnv()))
		}()
		if parseErr == nil || !strings.Contains(parseErr.Error(), "broker.url") {
			t.Fatalf("Parse err = %v, want a broker.url validation error", parseErr)
		}
	})
}
