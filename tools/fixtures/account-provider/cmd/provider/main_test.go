package main

import (
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/tools/fixtures/account-provider/canonical"
	"github.com/whalerein/whalerein/tools/fixtures/account-provider/signer"
)

func newTestProvider(t *testing.T) (*provider, http.Handler) {
	t.Helper()
	now := time.Unix(1_700_000_000, 0).UTC()
	keys, err := signer.NewSet("fixture://issuer/a", now)
	if err != nil {
		t.Fatal(err)
	}
	instance := &provider{
		cfg: config{
			issuer:          "fixture://issuer/a",
			serviceIdentity: "https://access.fixture.test",
			mqttURL:         "mqtts://access.fixture.test:8883",
			region:          "fixture-a",
		},
		keys: keys,
		now:  func() time.Time { return now },
	}
	return instance, instance.routes()
}

func TestMetadataDescribesThePinnedService(t *testing.T) {
	_, handler := newTestProvider(t)
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/v1/.well-known/whalerein-account", nil))
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d", recorder.Code)
	}
	var body struct {
		ServiceIdentity  string            `json:"serviceIdentity"`
		ProtocolVersion  int               `json:"protocolVersion"`
		Issuer           string            `json:"issuer"`
		VerificationKeys []map[string]any  `json:"verificationKeys"`
		Operations       []string          `json:"operations"`
		Endpoints        map[string]string `json:"endpoints"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body.ServiceIdentity != "https://access.fixture.test" || body.Issuer != "fixture://issuer/a" {
		t.Fatalf("unexpected metadata identity: %+v", body)
	}
	if body.ProtocolVersion != 1 {
		t.Fatalf("expected protocol version 1, got %d", body.ProtocolVersion)
	}
	if len(body.VerificationKeys) == 0 {
		t.Fatal("metadata must publish verification keys")
	}
	if body.Endpoints["mqtt"] != "mqtts://access.fixture.test:8883" {
		t.Fatalf("metadata must advertise the MQTT endpoint: %+v", body.Endpoints)
	}
	for _, required := range []string{"host_enrollment", "device_pairing", "operation_status"} {
		found := false
		for _, operation := range body.Operations {
			if operation == required {
				found = true
				break
			}
		}
		if !found {
			t.Fatalf("metadata must advertise %q: %v", required, body.Operations)
		}
	}
}

func TestIssuedAuthorizationVerifiesWithThePublishedKey(t *testing.T) {
	instance, handler := newTestProvider(t)
	request := `{"purpose":"host_enrollment","workspaceId":"ws-fixture-1","hostId":"host-1",` +
		`"hostPubkeys":["` + base64.RawURLEncoding.EncodeToString([]byte("host-signing")) + `",` +
		`"` + base64.RawURLEncoding.EncodeToString([]byte("host-noise")) + `"]}`
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/fixture/authorizations", strings.NewReader(request)))
	if recorder.Code != http.StatusCreated {
		t.Fatalf("expected 201, got %d: %s", recorder.Code, recorder.Body.String())
	}
	var body struct {
		Authorization struct {
			AuthorizationID string `json:"authorizationId"`
			Purpose         string `json:"purpose"`
			HostID          string `json:"hostId"`
			IssuedAtUnixMS  int64  `json:"issuedAtUnixMs"`
			ExpiresAtUnixMS int64  `json:"expiresAtUnixMs"`
			KeyID           string `json:"keyId"`
			Signature       string `json:"signature"`
			Nonce           string `json:"nonce"`
		} `json:"authorization"`
		CanonicalInput string `json:"canonicalInput"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body.Authorization.AuthorizationID == "" || body.Authorization.KeyID != "issuer-1" {
		t.Fatalf("unexpected authorization: %+v", body.Authorization)
	}
	if body.Authorization.ExpiresAtUnixMS <= body.Authorization.IssuedAtUnixMS {
		t.Fatal("an authorization must expire after it is issued")
	}
	if !strings.Contains(body.CanonicalInput, "host_enrollment") ||
		!strings.Contains(body.CanonicalInput, "ws-fixture-1") {
		t.Fatalf("the canonical input must bind the purpose and workspace: %q", body.CanonicalInput)
	}

	signature, err := base64.RawURLEncoding.DecodeString(body.Authorization.Signature)
	if err != nil {
		t.Fatal(err)
	}
	key, err := instance.keys.Active(time.Unix(1_700_000_000, 0).UTC())
	if err != nil {
		t.Fatal(err)
	}
	if !signer.Verify(key.PublicKey, body.CanonicalInput, signature) {
		t.Fatal("the issued signature must verify with the published key")
	}

	// The canonical input the fixture returns is the one it signed, so a
	// verifier can rebuild it from the fields and compare.
	rebuilt := canonical.Input(canonical.Fields{
		AuthorizationID: body.Authorization.AuthorizationID,
		Issuer:          "fixture://issuer/a",
		Audience:        "https://access.fixture.test",
		Purpose:         "host_enrollment",
		WorkspaceID:     "ws-fixture-1",
		RegionID:        "fixture-a",
		HostID:          "host-1",
		HostPubkeys:     [][]byte{[]byte("host-signing"), []byte("host-noise")},
		IssuedAtUnixMS:  body.Authorization.IssuedAtUnixMS,
		ExpiresAtUnixMS: body.Authorization.ExpiresAtUnixMS,
		Nonce:           mustDecode(t, body.Authorization.Nonce),
	})
	if rebuilt != body.CanonicalInput {
		t.Fatalf("rebuilt input differs:\n%s\n%s", rebuilt, body.CanonicalInput)
	}
}

func TestIssuanceRejectsMissingScope(t *testing.T) {
	_, handler := newTestProvider(t)
	for name, body := range map[string]string{
		"no purpose":   `{"workspaceId":"ws-fixture-1"}`,
		"no workspace": `{"purpose":"host_enrollment"}`,
		"malformed":    `{`,
	} {
		t.Run(name, func(t *testing.T) {
			recorder := httptest.NewRecorder()
			handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/fixture/authorizations", strings.NewReader(body)))
			if recorder.Code != http.StatusBadRequest {
				t.Fatalf("expected 400, got %d", recorder.Code)
			}
		})
	}
}

func TestIssuanceRefusesToSignAsAnotherIssuer(t *testing.T) {
	_, handler := newTestProvider(t)
	body := `{"purpose":"host_enrollment","workspaceId":"ws-fixture-1","issuer":"fixture://issuer/unknown"}`
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/fixture/authorizations", strings.NewReader(body)))
	if recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400, got %d", recorder.Code)
	}
}

func TestIssuanceFailsWhenTheKeyRingHasExpired(t *testing.T) {
	instance, handler := newTestProvider(t)
	instance.now = func() time.Time { return time.Unix(1_700_000_000, 0).UTC().Add(96 * time.Hour) }
	body := `{"purpose":"host_enrollment","workspaceId":"ws-fixture-1"}`
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/fixture/authorizations", strings.NewReader(body)))
	if recorder.Code != http.StatusConflict {
		t.Fatalf("expected 409 from an expired ring, got %d", recorder.Code)
	}
	if !strings.Contains(recorder.Body.String(), "authorization_expired") {
		t.Fatalf("unexpected body %s", recorder.Body.String())
	}
}

func TestHealthAndKeyViews(t *testing.T) {
	_, handler := newTestProvider(t)
	for path, want := range map[string]int{
		"/healthz":      http.StatusOK,
		"/fixture/keys": http.StatusOK,
	} {
		recorder := httptest.NewRecorder()
		handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, path, nil))
		if recorder.Code != want {
			t.Fatalf("%s returned %d", path, recorder.Code)
		}
	}
}

func mustDecode(t *testing.T, value string) []byte {
	t.Helper()
	decoded, err := base64.RawURLEncoding.DecodeString(value)
	if err != nil {
		t.Fatal(err)
	}
	return decoded
}

func TestIssuanceHonoursExplicitIdentifiersAndTimes(t *testing.T) {
	_, handler := newTestProvider(t)
	body := `{"purpose":"device_pairing","workspaceId":"ws-fixture-1","authorizationId":"auth-fixed",` +
		`"hostId":"host-1","deviceId":"device-1","operationId":"op-fixed",` +
		`"issuedAtUnixMs":1700000000000,"expiresAtUnixMs":1700000060000,` +
		`"hostPubkeys":["not-base64!!"],"devicePubkeys":[]}`
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/fixture/authorizations", strings.NewReader(body)))
	if recorder.Code != http.StatusCreated {
		t.Fatalf("expected 201, got %d: %s", recorder.Code, recorder.Body.String())
	}
	var parsed struct {
		Authorization struct {
			AuthorizationID string `json:"authorizationId"`
			IssuedAtUnixMS  int64  `json:"issuedAtUnixMs"`
			ExpiresAtUnixMS int64  `json:"expiresAtUnixMs"`
		} `json:"authorization"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &parsed); err != nil {
		t.Fatal(err)
	}
	if parsed.Authorization.AuthorizationID != "auth-fixed" {
		t.Fatalf("explicit authorization id ignored: %+v", parsed.Authorization)
	}
	if parsed.Authorization.IssuedAtUnixMS != 1_700_000_000_000 ||
		parsed.Authorization.ExpiresAtUnixMS != 1_700_000_060_000 {
		t.Fatalf("explicit timestamps ignored: %+v", parsed.Authorization)
	}
}

func TestKeyViewPublishesTheRing(t *testing.T) {
	_, handler := newTestProvider(t)
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/fixture/keys", nil))
	var body struct {
		Issuer string           `json:"issuer"`
		Keys   []map[string]any `json:"keys"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body.Issuer != "fixture://issuer/a" || len(body.Keys) != 1 {
		t.Fatalf("unexpected key view: %+v", body)
	}
}
