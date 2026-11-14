// Command provider runs the independent account-provider fixture.
//
// It is a separate Go program that shares no package with Access: it imports
// only the public protobuf bindings and its own canonical/signer helpers, so a
// passing integration case proves the contract rather than a shared code path
// (ratification R10).
//
// It serves the O1 service metadata and a fixture-only issuance endpoint that
// mints portable authorizations so Access can be driven without Cloud.
package main

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	"github.com/whalerein/whalerein/tools/fixtures/account-provider/canonical"
	"github.com/whalerein/whalerein/tools/fixtures/account-provider/signer"
)

type config struct {
	listen          string
	issuer          string
	unknownIssuer   string
	serviceIdentity string
	mqttURL         string
	region          string
}

func main() {
	cfg := config{}
	flag.StringVar(&cfg.listen, "listen", "127.0.0.1:9099", "HTTP listen address")
	flag.StringVar(&cfg.issuer, "issuer", "fixture://issuer/a", "issuer identifier")
	flag.StringVar(&cfg.unknownIssuer, "unknown-issuer", "fixture://issuer/unknown", "untrusted issuer used by negative cases")
	flag.StringVar(&cfg.serviceIdentity, "service-identity", "https://access.fixture.test", "public service identity")
	flag.StringVar(&cfg.mqttURL, "mqtt-url", "mqtts://access.fixture.test:8883", "regional MQTT endpoint")
	flag.StringVar(&cfg.region, "region", "fixture-a", "region identifier")
	flag.Parse()

	now := time.Now().UTC()
	keys, err := signer.NewSet(cfg.issuer, now)
	if err != nil {
		log.Fatalf("create issuer keys: %v", err)
	}
	// A rotation key becomes valid only after the first key expires.
	if _, err := keys.AddRotationKey(now.Add(24*time.Hour), now.Add(48*time.Hour)); err != nil {
		log.Fatalf("create rotation key: %v", err)
	}

	provider := &provider{cfg: cfg, keys: keys, now: func() time.Time { return time.Now().UTC() }}

	server := &http.Server{Addr: cfg.listen, Handler: provider.routes(), ReadHeaderTimeout: 5 * time.Second}
	log.Printf("account-provider fixture listening on %s as %s", cfg.listen, cfg.issuer)
	if err := server.ListenAndServe(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

type provider struct {
	cfg  config
	keys *signer.Set
	now  func() time.Time
}

// routes returns the fixture's route table.
func (p *provider) routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
	})
	mux.HandleFunc("GET /v1/.well-known/whalerein-account", p.metadata)
	mux.HandleFunc("POST /fixture/authorizations", p.issue)
	mux.HandleFunc("GET /fixture/keys", p.publishKeys)
	return mux
}

// metadata serves O1: the public service description a client pins.
func (p *provider) metadata(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{
		"serviceIdentity":  p.cfg.serviceIdentity,
		"protocolVersion":  1,
		"issuer":           p.cfg.issuer,
		"region":           p.cfg.region,
		"maxAgeSeconds":    300,
		"verificationKeys": p.keys.VerificationKeys(),
		"operations": []string{
			"host_enrollment", "host_discovery", "host_enrollment_status",
			"device_pairing", "operation_status", "device_recovery",
			"host_recovery", "management_apply", "management_read",
			"regional_authority",
		},
		"endpoints": map[string]string{
			"enrollments": p.cfg.serviceIdentity + "/v1/enrollments/hosts",
			"discovery":   p.cfg.serviceIdentity + "/v1/account/hosts",
			"operations":  p.cfg.serviceIdentity + "/v1/operations",
			"mqtt":        p.cfg.mqttURL,
		},
	})
}

// publishKeys is a fixture-only view of the issuer key ring.
func (p *provider) publishKeys(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{
		"issuer": p.cfg.issuer,
		"keys":   p.keys.VerificationKeys(),
	})
}

// issuanceRequest is what a test asks the fixture to authorize.
type issuanceRequest struct {
	Purpose            string   `json:"purpose"`
	AuthorizationID    string   `json:"authorizationId"`
	WorkspaceID        string   `json:"workspaceId"`
	HostID             string   `json:"hostId"`
	HostPubkeys        []string `json:"hostPubkeys"`
	DeviceID           string   `json:"deviceId"`
	DevicePubkeys      []string `json:"devicePubkeys"`
	OperationID        string   `json:"operationId"`
	EnrollmentRevision string   `json:"enrollmentRevision"`
	Precondition       string   `json:"precondition"`
	ExpectedRevision   string   `json:"expectedRevision"`
	IssuedAtUnixMS     *int64   `json:"issuedAtUnixMs"`
	ExpiresAtUnixMS    *int64   `json:"expiresAtUnixMs"`
	// IssuerOverride lets a negative case sign with the unknown issuer.
	IssuerOverride string `json:"issuer"`
}

// issue mints one portable authorization. It is fixture-only: the official
// service produces the same artifacts from real account authorization.
func (p *provider) issue(w http.ResponseWriter, r *http.Request) {
	var request issuanceRequest
	if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "invalid_argument", "malformed issuance request")
		return
	}
	if request.Purpose == "" || request.WorkspaceID == "" {
		writeError(w, http.StatusBadRequest, "invalid_argument", "purpose and workspaceId are required")
		return
	}
	if request.IssuerOverride != "" {
		writeError(w, http.StatusBadRequest, "invalid_argument",
			"the fixture cannot sign as another issuer; use its own keys")
		return
	}

	now := p.now()
	issuedAt := now.UnixMilli()
	if request.IssuedAtUnixMS != nil {
		issuedAt = *request.IssuedAtUnixMS
	}
	expiresAt := now.Add(5 * time.Minute).UnixMilli()
	if request.ExpiresAtUnixMS != nil {
		expiresAt = *request.ExpiresAtUnixMS
	}
	authorizationID := request.AuthorizationID
	if authorizationID == "" {
		authorizationID = "auth-" + randomHex(8)
	}
	nonce := randomBytes(16)

	fields := canonical.Fields{
		AuthorizationID:    authorizationID,
		Issuer:             p.cfg.issuer,
		Audience:           p.cfg.serviceIdentity,
		Purpose:            request.Purpose,
		WorkspaceID:        request.WorkspaceID,
		RegionID:           p.cfg.region,
		OperationID:        request.OperationID,
		EnrollmentRevision: request.EnrollmentRevision,
		Precondition:       request.Precondition,
		ExpectedRevision:   request.ExpectedRevision,
		HostID:             request.HostID,
		HostPubkeys:        decodeKeys(request.HostPubkeys),
		DeviceID:           request.DeviceID,
		DevicePubkeys:      decodeKeys(request.DevicePubkeys),
		IssuedAtUnixMS:     issuedAt,
		ExpiresAtUnixMS:    expiresAt,
		Nonce:              nonce,
	}
	key, err := p.keys.Active(time.UnixMilli(issuedAt))
	if err != nil {
		writeError(w, http.StatusConflict, "authorization_expired", err.Error())
		return
	}
	signature := key.Sign(canonical.Input(fields))

	writeJSON(w, http.StatusCreated, map[string]any{
		"authorization": map[string]any{
			"authVersion":        canonical.Version,
			"authorizationId":    fields.AuthorizationID,
			"issuer":             fields.Issuer,
			"audience":           fields.Audience,
			"purpose":            fields.Purpose,
			"workspaceId":        fields.WorkspaceID,
			"regionId":           fields.RegionID,
			"operationId":        fields.OperationID,
			"enrollmentRevision": fields.EnrollmentRevision,
			"precondition":       fields.Precondition,
			"expectedRevision":   fields.ExpectedRevision,
			"hostId":             fields.HostID,
			"hostPubkeys":        request.HostPubkeys,
			"deviceId":           fields.DeviceID,
			"devicePubkeys":      request.DevicePubkeys,
			"issuedAtUnixMs":     fields.IssuedAtUnixMS,
			"expiresAtUnixMs":    fields.ExpiresAtUnixMS,
			"nonce":              base64.RawURLEncoding.EncodeToString(nonce),
			"keyId":              key.ID,
			"signature":          base64.RawURLEncoding.EncodeToString(signature),
		},
		"canonicalInput": canonical.Input(fields),
	})
}

func decodeKeys(values []string) [][]byte {
	keys := make([][]byte, 0, len(values))
	for _, value := range values {
		decoded, err := base64.RawURLEncoding.DecodeString(value)
		if err != nil {
			continue
		}
		keys = append(keys, decoded)
	}
	return keys
}

func randomHex(bytes int) string {
	raw := randomBytes(bytes)
	return base64.RawURLEncoding.EncodeToString(raw)
}

func randomBytes(count int) []byte {
	raw := make([]byte, count)
	if _, err := rand.Read(raw); err != nil {
		panic(fmt.Sprintf("generate random bytes: %v", err))
	}
	return raw
}

func writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(body)
}

func writeError(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, map[string]string{"code": code, "message": message})
}
