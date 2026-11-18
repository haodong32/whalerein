//go:build integration

package external_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/httpapi"
	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

type regionalPairingPeer struct {
	hostKey, deviceKey []byte
	commitUnavailable  bool
	commits            int
}

func (p *regionalPairingPeer) SupportsPairing(string) bool               { return true }
func (p *regionalPairingPeer) IsHostOnline(context.Context, string) bool { return true }
func (p *regionalPairingPeer) PublishAndAwait(_ context.Context, _ string, req *whalereinv1.HostRequest, _ time.Duration) (*whalereinv1.HostResult, error) {
	claim := &whalereinv1.PairingClaim{}
	if err := proto.Unmarshal(req.Body, claim); err != nil {
		return nil, err
	}
	if !bytes.Equal(claim.DevicePublicKey, p.deviceKey) {
		return nil, errors.New("the relayed Device key was not the authorized key")
	}
	result := &whalereinv1.PairingClaimResult{DeviceId: claim.DeviceId, HostPublicKey: p.hostKey}
	switch claim.Phase {
	case whalereinv1.PairingPhase_PAIRING_PHASE_HELLO:
		result.NoiseMessage = []byte("scripted-noise-hello")
	case whalereinv1.PairingPhase_PAIRING_PHASE_FINISH:
		result.DevicePublicKey = p.deviceKey
		result.DeviceAuthHash = claim.DeviceAuthHash
	case whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE:
		result.Authorized = true
	case whalereinv1.PairingPhase_PAIRING_PHASE_COMMIT:
		if p.commitUnavailable {
			return nil, errors.New("simulated interruption before Host activation")
		}
		p.commits++
		result.Authorized = true
	default:
		return nil, errors.New("unexpected pairing phase")
	}
	body, err := proto.Marshal(result)
	if err != nil {
		return nil, err
	}
	return &whalereinv1.HostResult{
		RequestId: req.RequestId, HostId: req.HostId, DeviceId: req.DeviceId,
		Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body,
	}, nil
}

type regionalGrantEnforcement struct{ data *store.Store }

func (e regionalGrantEnforcement) ApplyPairing(ctx context.Context, host, device string) error {
	active, err := e.data.HasActiveGrant(ctx, host, device)
	if err == nil && !active {
		return errors.New("policy was applied before the Host confirmed activation")
	}
	return err
}

func (e regionalGrantEnforcement) RevokeHostGrant(ctx context.Context, host, device string) error {
	return e.data.RevokeGrant(ctx, host, device, time.Now().UTC())
}

// A Cloud-signed pairing survives a lost activation response, and Access denies
// credentials before Host confirmation and after a scoped revocation.
func TestCloudDevicePairingActivationRecoveryAndRevocation(t *testing.T) {
	ctx := context.Background()
	database, _ := storetest.New(t)
	provider := newProvider(t)
	peer := &regionalPairingPeer{
		hostKey: bytes.Repeat([]byte{1}, 32), deviceKey: bytes.Repeat([]byte{2}, 32),
		commitUnavailable: true,
	}
	enforcement := regionalGrantEnforcement{data: database}
	claims, err := pairing.New(pairing.Dependencies{
		Presence: peer, Transport: peer, Store: database, Policy: enforcement,
		Config: pairing.Config{ClaimTTL: time.Minute, ClaimTimeout: time.Second, ProtocolVersion: 1},
	})
	if err != nil {
		t.Fatal(err)
	}
	service := external.New(external.Trust{
		Issuer: issuerID, ServiceIdentity: serviceURL, Region: regionID,
		Keys: []external.VerificationKey{{ID: provider.key.id, PublicKey: provider.key.public}},
	}, database, "mqtts://access.fixture.test:443", external.WithDeviceGrants(claims, database, enforcement))
	api, err := httpapi.New(httpapi.Dependencies{
		Auth: &acceptAdmin{}, Pairing: claims, Requests: stubRequests{}, External: service,
		Readiness: diagnostics.NewReadiness(),
	})
	if err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(api.PublicHandler())
	t.Cleanup(server.Close)
	now := time.Now().UTC()
	operation := store.ExternalOperation{
		OperationID: "host-enrollment", AuthorizationID: "host-authorization",
		Purpose: external.PurposeHostEnrollment, WorkspaceID: "workspace", RegionID: regionID,
		HostID: "host", PayloadDigest: []byte{1}, EnrollmentRevision: "1", State: "operation_committed",
	}
	signingKey := bytes.Repeat([]byte{3}, 32)
	_, _, _, err = database.EnrollHost(ctx, operation, store.ExternalEnrollment{
		HostID: "host", WorkspaceID: "workspace", EnrollmentRevision: "1", State: "enrollment_pending",
		HostSigningPublicKey: signingKey, HostNoisePublicKey: peer.hostKey,
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := database.EnsureHost(ctx, "host"); err != nil {
		t.Fatal(err)
	}
	if _, advanced, err := database.MarkEnrollmentReady(ctx, "host", now); err != nil || !advanced {
		t.Fatalf("enrollment did not become ready: %v", err)
	}
	auth := func(device, operation string) *whalereinv1.ExternalAuthorization {
		return provider.issue(t, &whalereinv1.ExternalAuthorization{
			AuthVersion: external.AuthVersion, AuthorizationId: "auth-" + operation, Purpose: external.PurposeDevicePairing,
			WorkspaceId: "workspace", RegionId: regionID, EnrollmentRevision: "1", HostId: "host",
			OperationId: operation, DeviceId: device, DevicePublicKeys: [][]byte{peer.deviceKey},
			HostPublicKeys: [][]byte{signingKey, peer.hostKey}, IssuedAtUnixMs: now.UnixMilli(),
			ExpiresAtUnixMs: now.Add(time.Minute).UnixMilli(), Nonce: bytes.Repeat([]byte{4}, 16),
		})
	}
	wireClaim := func(a *whalereinv1.ExternalAuthorization, claim *whalereinv1.PairingClaimRequest, expected int) {
		t.Helper()
		aJSON, err := protojson.Marshal(a)
		if err != nil {
			t.Fatal(err)
		}
		cJSON, err := protojson.Marshal(claim)
		if err != nil {
			t.Fatal(err)
		}
		body, err := json.Marshal(map[string]json.RawMessage{"authorization": aJSON, "claim": cJSON})
		if err != nil {
			t.Fatal(err)
		}
		webRequest(t, server.URL, http.MethodPost, "/v1/enrollments/devices/pairing", "provider-bearer", body, expected)
	}
	checkGrant := func(device string, expectedState string, usable bool) {
		t.Helper()
		state, err := database.CloudDeviceGrantStatus(ctx, "workspace", "host", device)
		if err != nil || state != expectedState {
			t.Fatalf("grant status %q, %v, want %q", state, err, expectedState)
		}
		active, err := database.HasActiveGrant(ctx, "host", device)
		if err != nil || active != usable {
			t.Fatalf("grant active %v, %v, want %v", active, err, usable)
		}
	}
	hello := &whalereinv1.PairingClaimRequest{
		ClaimId: "pairing", DeviceId: "device", NoiseMessage: []byte("hello"),
		Phase: whalereinv1.PairingPhase_PAIRING_PHASE_HELLO, ProtocolVersion: 1,
	}
	finish := proto.Clone(hello).(*whalereinv1.PairingClaimRequest)
	finish.Phase = whalereinv1.PairingPhase_PAIRING_PHASE_FINISH
	finish.NoiseMessage = []byte("finish")
	finish.Confirmed = true
	finish.DeviceToken = base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{5}, 32))
	proof := auth("device", "pairing")
	wireClaim(proof, hello, http.StatusOK)
	wireClaim(proof, finish, http.StatusServiceUnavailable)
	checkGrant("device", "pending", false)
	credentialHash := sha256.Sum256([]byte(finish.DeviceToken))
	if _, err := database.ActiveDeviceByAuthHash(ctx, credentialHash[:]); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("unconfirmed credential lookup returned %v", err)
	}

	peer.commitUnavailable = false
	wireClaim(proof, finish, http.StatusOK)
	checkGrant("device", "ready", true)
	refreshed := proto.Clone(proof).(*whalereinv1.ExternalAuthorization)
	refreshed.AuthorizationId = "refreshed-pairing-authorization"
	wireClaim(provider.issue(t, refreshed), finish, http.StatusOK)
	if peer.commits != 1 {
		t.Fatalf("Host activation ran %d times", peer.commits)
	}

	path := "/v1/enrollments/hosts/host/devices/device"
	webRequest(t, server.URL, http.MethodGet, path+"?workspaceId=workspace", "", nil, http.StatusUnauthorized)
	webRequest(t, server.URL, http.MethodGet, path+"?workspaceId=another-workspace", adminToken, nil, http.StatusNotFound)
	webRequest(t, server.URL, http.MethodPost, path+"/revocation?workspaceId=workspace", adminToken, nil, http.StatusNoContent)
	checkGrant("device", "revoked", false)
	wireClaim(proof, finish, http.StatusConflict)

	hello.ClaimId, hello.DeviceId = "before-commit", "another-device"
	finish.ClaimId, finish.DeviceId = hello.ClaimId, hello.DeviceId
	otherProof := auth(hello.DeviceId, hello.ClaimId)
	wireClaim(otherProof, hello, http.StatusOK)
	webRequest(t, server.URL, http.MethodPost, "/v1/enrollments/hosts/host/devices/another-device/revocation?workspaceId=workspace", adminToken, nil, http.StatusNoContent)
	wireClaim(otherProof, finish, http.StatusConflict)
	checkGrant("another-device", "revoked", false)
}

func webRequest(t *testing.T, base, method, path, bearer string, body []byte, expected int) {
	t.Helper()
	req, err := http.NewRequest(method, base+path, bytes.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	if bearer != "" {
		req.Header.Set("Authorization", "Bearer "+bearer)
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = resp.Body.Close() }()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != expected {
		t.Fatalf("%s %s returned HTTP %d, want %d", method, path, resp.StatusCode, expected)
	}
}
