package config

import (
	"strings"
	"testing"
)

// ACC-5 regression: advertised URL credentials.
//
// public_url and broker.public_url are copied into client-visible pairing URIs,
// so an embedded URL userinfo credential must be rejected at validation. The
// process already strips userinfo before logging the same values, which shows
// the project treats them as secrets.
func TestReproAdvertisedURLsRejectCredentials(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*Config)
		want   string
	}{
		{
			"public_url with userinfo",
			func(c *Config) { c.PublicURL = "https://operator:secret@access.example.com" },
			"public_url must not embed credentials",
		},
		{
			"broker.public_url with userinfo",
			func(c *Config) { c.Broker.PublicURL = "mqtts://access:secret@mqtt.example.com:8883" },
			"broker.public_url must not embed credentials",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			cfg := validConfig(t)
			tc.mutate(&cfg)
			err := cfg.Validate()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("err = %v, want a validation error containing %q", err, tc.want)
			}
		})
	}

	// Positive control: the same endpoints without userinfo stay valid.
	cfg := validConfig(t)
	cfg.PublicURL = "https://access.example.com"
	cfg.Broker.PublicURL = "mqtts://mqtt.example.com:8883"
	if err := cfg.Validate(); err != nil {
		t.Fatalf("credential-free advertised URLs rejected: %v", err)
	}
}
