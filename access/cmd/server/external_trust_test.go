package main

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/base64"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/store"
)

func externalConfig(keys ...config.ExternalVerificationKey) config.Config {
	cfg := config.Default()
	cfg.Mode = "external"
	cfg.External = config.ExternalConfig{
		ServiceIdentity:  "https://access.fixture.test",
		Issuer:           "fixture://issuer/a",
		Region:           "fixture-a",
		MQTTURL:          "mqtts://access.fixture.test:8883",
		ClockSkew:        config.Duration(15 * time.Second),
		VerificationKeys: keys,
	}
	return cfg
}

func validKey(t *testing.T) config.ExternalVerificationKey {
	t.Helper()
	publicKey, _, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatal(err)
	}
	return config.ExternalVerificationKey{
		ID:        "issuer-1",
		PublicKey: base64.RawURLEncoding.EncodeToString(publicKey),
		NotBefore: config.Timestamp(time.Now().Add(-time.Hour).UTC()),
		NotAfter:  config.Timestamp(time.Now().Add(2 * time.Hour).UTC()),
	}
}

func TestExternalTrustConvertsTheConfiguredKeys(t *testing.T) {
	trust, err := externalTrust(externalConfig(validKey(t)))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if trust.Issuer != "fixture://issuer/a" || trust.Region != "fixture-a" {
		t.Fatalf("unexpected trust %+v", trust)
	}
	if len(trust.Keys) != 1 || trust.Keys[0].ID != "issuer-1" {
		t.Fatalf("unexpected keys %+v", trust.Keys)
	}
	if !trust.Keys[0].NotAfter.After(trust.Keys[0].NotBefore) {
		t.Fatal("the key window must survive the conversion")
	}
}

func TestExternalTrustRejectsUnusableKeys(t *testing.T) {
	for name, key := range map[string]config.ExternalVerificationKey{
		"not base64": func() config.ExternalVerificationKey {
			value := validKey(t)
			value.PublicKey = "not base64!"
			return value
		}(),
		"wrong length": func() config.ExternalVerificationKey {
			value := validKey(t)
			value.PublicKey = base64.RawURLEncoding.EncodeToString([]byte("short"))
			return value
		}(),
	} {
		t.Run(name, func(t *testing.T) {
			_, err := externalTrust(externalConfig(key))
			if err == nil {
				t.Fatal("expected a conversion error")
			}
			if !strings.Contains(err.Error(), "issuer-1") {
				t.Fatalf("the error should name the key: %v", err)
			}
		})
	}
}

func TestExternalModeStartupFailsOnUnusableKeys(t *testing.T) {
	// A misconfigured key must stop startup instead of serving an
	// unverifiable authority.
	broken := validKey(t)
	broken.PublicKey = "not base64!"
	cfg := externalConfig(broken)
	cfg.DatabaseURL = "fixture"
	cfg.Broker.URL = "tcp://127.0.0.1:1"
	cfg.Listen = "127.0.0.1:0"
	cfg.InternalListen = "127.0.0.1:0"

	ops := startupStoreOps{
		open: func(context.Context, string) (*store.Store, error) {
			return store.NewWithPool(nil), nil
		},
		migrate: func(context.Context, *store.Store) error { return nil },
		close:   func(*store.Store) {},
	}
	var logs bytes.Buffer
	err := runServerWithStore(context.Background(), cfg, slog.New(slog.NewTextHandler(&logs, nil)), ops)
	if err == nil {
		t.Fatal("external mode started with an unusable key")
	}
	if !strings.Contains(err.Error(), "external key issuer-1") {
		t.Fatalf("unexpected error: %v", err)
	}
}
