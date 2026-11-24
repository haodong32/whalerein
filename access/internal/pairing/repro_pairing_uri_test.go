package pairing

import (
	"bytes"
	"context"
	"io"
	"log/slog"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
)

// ACC-5 regression: pairing URI credentials.
//
// The pairing URI is a client-visible artifact: a device or operator copies it
// into a client. Config validation rejects URL userinfo in public_url and
// broker.public_url, and CreateInvitation strips it defensively, so the
// advertised passwords never appear in the `access=` or `mqtt=` parameters.
// Logs already strip URL userinfo (Config.Redacted/sanitizeURL), so the
// credential is treated as a secret everywhere.
func TestReproPairingURIDoesNotLeakAdvertisedURLCredentials(t *testing.T) {
	fs := newFakeStore()
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	transport := &fakeTransport{}
	transport.awaitFn = defaultPairingAnswers
	presence := &fakePresence{online: map[string]bool{"h1": true}}
	const (
		accessPassword = "access-admin-password"
		brokerPassword = "broker-password"
	)
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	service, err := New(Dependencies{
		Store: fs, Policy: &fakePolicy{}, Transport: transport, Presence: presence,
		Config: Config{
			PublicURL:     "https://operator:" + accessPassword + "@access.example.com",
			MQTTURL:       "mqtts://access:" + brokerPassword + "@mqtt.example.com:8883",
			InvitationTTL: 10 * time.Minute, ClaimTimeout: time.Second, ClaimTTL: time.Minute,
			MaxFailures: 3, ProtocolVersion: 1,
		},
		Metrics: diagnostics.NewMetrics(),
		Masker:  diagnostics.NewMaskerWithKey([]byte("pair")),
		Logger:  slog.New(slog.NewTextHandler(io.Discard, nil)),
		Now:     func() time.Time { return now },
		Random:  bytes.NewReader(bytes.Repeat([]byte{0x42}, 4096)),
	})
	if err != nil {
		t.Fatal(err)
	}

	invitation, err := service.CreateInvitation(context.Background(), "h1")
	if err != nil {
		t.Fatal(err)
	}
	uri := invitation.GetPairingUri()
	if strings.Contains(uri, accessPassword) || strings.Contains(uri, brokerPassword) {
		t.Fatalf("pairing URI exposes advertised URL credentials: %s", uri)
	}
	parsed, err := url.Parse(uri)
	if err != nil {
		t.Fatal(err)
	}
	for _, parameter := range []string{"access", "mqtt"} {
		value := parsed.Query().Get(parameter)
		if value == "" {
			continue
		}
		endpoint, err := url.Parse(value)
		if err != nil {
			t.Fatalf("%s parameter is not a URL: %q", parameter, value)
		}
		if endpoint.User != nil {
			t.Fatalf("%s parameter carries userinfo: %q", parameter, value)
		}
	}
}
