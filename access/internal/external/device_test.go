package external

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type capturedClaim struct {
	scope *pairing.CloudScope
}

func (c *capturedClaim) ClaimCloud(_ context.Context, _ *whalereinv1.PairingClaimRequest, scope pairing.CloudScope) (*whalereinv1.PairingClaimResponse, error) {
	c.scope = &scope
	return &whalereinv1.PairingClaimResponse{}, nil
}

type testDeviceGrants struct {
	events []string
	exists bool
	begin  error
	fail   error
}

func (s *testDeviceGrants) CloudDeviceGrantStatus(context.Context, string, string, string) (string, error) {
	return "pending", nil
}

func (s *testDeviceGrants) BeginCloudDeviceRevocation(context.Context, string, string, string, time.Time) (bool, error) {
	s.events = append(s.events, "record")
	return s.exists, s.begin
}

func (s *testDeviceGrants) CompleteCloudDeviceRevocation(context.Context, string, string, time.Time) error {
	s.events = append(s.events, "complete")
	return nil
}

func (s *testDeviceGrants) RevokeHostGrant(context.Context, string, string) error {
	s.events = append(s.events, "enforce")
	return s.fail
}

func TestCloudDevicePairingValidatesScopeBeforeNoise(t *testing.T) {
	h := newHarness(t)
	ctx := context.Background()
	req := h.request(t)
	if _, err := h.service.PairDevice(ctx, nil, nil); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("unconfigured Device pairing returned %v", err)
	}
	if _, err := h.service.EnrollHost(ctx, req); err != nil {
		t.Fatal(err)
	}
	if _, _, err := h.data.MarkEnrollmentReady(ctx, req.HostId, issued); err != nil {
		t.Fatal(err)
	}
	data := &testDeviceGrants{}
	deviceClaim := &capturedClaim{}
	WithDeviceGrants(deviceClaim, data, data)(h.service)
	a := proto.Clone(req.Authorization).(*whalereinv1.ExternalAuthorization)
	a.Purpose = PurposeDevicePairing
	a.AuthorizationId = "authorize-device"
	a.OperationId = "device-pairing-operation"
	a.EnrollmentRevision = "1"
	a.DeviceId = "new-device"
	a.DevicePublicKeys = [][]byte{bytes.Repeat([]byte{0x23}, 32)}
	claim := &whalereinv1.PairingClaimRequest{ClaimId: a.OperationId, DeviceId: a.DeviceId}
	for _, tc := range []struct {
		name string
		edit func(*whalereinv1.ExternalAuthorization)
		err  error
	}{
		{"issuer", func(a *whalereinv1.ExternalAuthorization) { a.Issuer = "other-issuer" }, ErrAuthorizationMismatch},
		{"audience", func(a *whalereinv1.ExternalAuthorization) { a.Audience = "https://other.fixture.test" }, ErrAuthorizationMismatch},
		{"region", func(a *whalereinv1.ExternalAuthorization) { a.RegionId = "other-region" }, ErrAuthorizationMismatch},
		{"purpose", func(a *whalereinv1.ExternalAuthorization) { a.Purpose = PurposeHostEnrollment }, ErrAuthorizationMismatch},
		{"workspace", func(a *whalereinv1.ExternalAuthorization) { a.WorkspaceId = "other-workspace" }, ErrAuthorizationMismatch},
		{"unknown Host", func(a *whalereinv1.ExternalAuthorization) { a.HostId = "other-host" }, ErrAuthorizationMismatch},
		{"ambiguous workspace", func(a *whalereinv1.ExternalAuthorization) { a.WorkspaceId = "workspace\n|other" }, ErrAuthorizationMismatch},
		{"operation", func(a *whalereinv1.ExternalAuthorization) { a.OperationId = "other-operation" }, ErrAuthorizationMismatch},
		{"revision", func(a *whalereinv1.ExternalAuthorization) { a.EnrollmentRevision = "2" }, ErrStaleRevision},
		{"host key", func(a *whalereinv1.ExternalAuthorization) { a.HostPublicKeys[0] = make([]byte, 32) }, ErrAuthorizationMismatch},
		{"device key", func(a *whalereinv1.ExternalAuthorization) { a.DevicePublicKeys[0] = []byte{1} }, ErrAuthorizationMismatch},
		{"future", func(a *whalereinv1.ExternalAuthorization) { a.IssuedAtUnixMs++ }, ErrAuthorizationMismatch},
		{"expiry", func(a *whalereinv1.ExternalAuthorization) { a.ExpiresAtUnixMs = issued.UnixMilli() }, ErrAuthorizationExpired},
	} {
		t.Run(tc.name, func(t *testing.T) {
			edited := proto.Clone(a).(*whalereinv1.ExternalAuthorization)
			tc.edit(edited)
			if _, err := h.service.PairDevice(ctx, h.signer.authorize(t, edited), claim); !errors.Is(err, tc.err) {
				t.Fatalf("got %v, want %v", err, tc.err)
			}
			if deviceClaim.scope != nil {
				t.Fatal("rejected scope reached the Noise transport")
			}
		})
	}
	if _, err := h.service.PairDevice(ctx, h.signer.authorize(t, a), claim); err != nil {
		t.Fatal(err)
	}
	scope := deviceClaim.scope
	if scope == nil || scope.OperationID != a.OperationId || !bytes.Equal(scope.HostKey, req.HostNoisePublicKey) ||
		!bytes.Equal(scope.DeviceKey, a.DevicePublicKeys[0]) {
		t.Fatal("Noise scope lost identity bindings")
	}
	if _, err := h.data.DisableEnrollment(ctx, req.HostId, issued); err != nil {
		t.Fatal(err)
	}
	if _, err := h.service.PairDevice(ctx, a, claim); !errors.Is(err, ErrEnrollmentDisabled) {
		t.Fatalf("disabled Host paired a Device: %v", err)
	}
}

func TestCloudDeviceRevocationCompletesOnlyAfterEnforcement(t *testing.T) {
	h := newHarness(t)
	data := &testDeviceGrants{exists: true, fail: errors.New("line unavailable")}
	ctx := context.Background()
	if _, err := h.service.DeviceGrantStatus(ctx, "workspace", "host", "device"); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("unconfigured status returned %v", err)
	}
	if err := h.service.RevokeDeviceGrant(ctx, "workspace", "host", "device"); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("unconfigured revocation returned %v", err)
	}
	WithDeviceGrants(nil, data, data)(h.service)
	if state, err := h.service.DeviceGrantStatus(ctx, "workspace", "host", "device"); err != nil || state != "pending" {
		t.Fatalf("expected pending status, got %q, %v", state, err)
	}
	data.begin = store.ErrNotFound
	if err := h.service.RevokeDeviceGrant(ctx, "workspace", "unknown-host", "device"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("unexpected unknown host result: %v", err)
	}
	if got := len(data.events); got != 1 || data.events[0] != "record" {
		t.Fatalf("scope failure should stop enforcement, got %v", data.events)
	}
	data.begin, data.events = nil, nil
	if err := h.service.RevokeDeviceGrant(ctx, "workspace", "host", "device"); !errors.Is(err, ErrEnforcementUnavailable) {
		t.Fatalf("expected pending enforcement, got %v", err)
	}
	if got := len(data.events); got != 2 || data.events[0] != "record" || data.events[1] != "enforce" {
		t.Fatalf("unexpected event order %v", data.events)
	}
	data.fail = nil
	if err := h.service.RevokeDeviceGrant(ctx, "workspace", "host", "device"); err != nil {
		t.Fatal(err)
	}
	if got := len(data.events); got != 5 || data.events[4] != "complete" {
		t.Fatalf("unexpected event order %v", data.events)
	}
	data.events = nil
	data.exists = false
	if err := h.service.RevokeDeviceGrant(ctx, "workspace", "host", "unpaired"); err != nil {
		t.Fatal(err)
	}
	if got := len(data.events); got != 2 || data.events[0] != "record" || data.events[1] != "complete" {
		t.Fatalf("pre-grant revocation should record a fence without a live line, got %v", data.events)
	}
}

// Both phases carry the regional bearer and a signed pairing scoped to that
// same session, workspace, region, Device and pinned key.
func TestManagedPairingRequiresMatchingSessionPermission(t *testing.T) {
	h := newHarness(t)
	ctx := context.Background()
	req := h.request(t)
	if _, err := h.service.EnrollHost(ctx, req); err != nil {
		t.Fatal(err)
	}
	if _, _, err := h.data.MarkEnrollmentReady(ctx, req.HostId, issued); err != nil {
		t.Fatal(err)
	}
	grants := &testDeviceGrants{}
	noise := &capturedClaim{}
	WithDeviceGrants(noise, grants, grants)(h.service)
	a := proto.Clone(req.Authorization).(*whalereinv1.ExternalAuthorization)
	a.Purpose, a.AuthorizationId, a.OperationId = PurposeDevicePairing, "auth", "operation"
	a.EnrollmentRevision, a.Precondition, a.DeviceId = "1", "original-session", "device"
	a.DevicePublicKeys = [][]byte{make([]byte, 32)}
	claim := &whalereinv1.PairingClaimRequest{ClaimId: a.OperationId, DeviceId: a.DeviceId}
	a = h.signer.authorize(t, a)
	const token = "regional fixture token"
	fingerprint := sha256.Sum256(a.DevicePublicKeys[0])
	permission := store.DeviceSession{
		ID: a.Precondition, DeviceID: a.DeviceId, AuthorityRevision: "serving",
		WorkspaceID: a.WorkspaceId, RegionID: a.RegionId, KeyFingerprint: hex.EncodeToString(fingerprint[:]),
		CredentialHash: HashBytes([]byte(token)),
	}
	h.service.trust.SessionAuthorityRevision = "serving"
	data := &sessionFixture{current: permission}
	WithDeviceSessions(data, &sessionEnforcerFixture{}, true)(h.service)
	if _, err := h.service.PairDevice(ctx, a, claim); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("missing bearer accepted: %v", err)
	}
	if _, err := h.service.PairDevice(WithDeviceSessionCredential(ctx, "wrong"), a, claim); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("unequal bearer accepted: %v", err)
	}
	authenticated := WithDeviceSessionCredential(ctx, token)
	for _, mutation := range []func(*store.DeviceSession){
		func(s *store.DeviceSession) { s.ID = "new-session" },
		func(s *store.DeviceSession) { s.DeviceID = "different-device" },
		func(s *store.DeviceSession) { s.WorkspaceID = "foreign" },
		func(s *store.DeviceSession) { s.RegionID = "foreign" },
		func(s *store.DeviceSession) { s.KeyFingerprint = "different-key" },
		func(s *store.DeviceSession) { s.AuthorityRevision = "previous" },
	} {
		data.current = permission
		mutation(&data.current)
		if _, err := h.service.PairDevice(authenticated, a, claim); !errors.Is(err, ErrAuthorizationMismatch) {
			t.Fatalf("mismatched session scope accepted: %v", err)
		}
	}
	if noise.scope != nil {
		t.Fatal("invalid permission reached Noise")
	}
	data.current = permission
	data.refusal = errors.New("unavailable")
	if _, err := h.service.PairDevice(authenticated, a, claim); !errors.Is(err, data.refusal) {
		t.Fatalf("store failure classification: %v", err)
	}
	data.refusal = nil
	if _, err := h.service.PairDevice(authenticated, a, claim); err != nil || noise.scope == nil || noise.scope.SessionID != permission.ID {
		t.Fatalf("matched permission scope=%+v err=%v", noise.scope, err)
	}
}
