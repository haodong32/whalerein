package httpapi

import (
	"context"
	"encoding/json"
	"net/http"
	"slices"
	"testing"

	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type fakeExternalDevice struct {
	fakeExternal
	state    string
	err      error
	received []string
}

func (f *fakeExternalDevice) PairDevice(_ context.Context, a *whalereinv1.ExternalAuthorization, c *whalereinv1.PairingClaimRequest) (*whalereinv1.PairingClaimResponse, error) {
	f.received = []string{a.GetPurpose(), c.GetDeviceId(), c.GetClaimId()}
	return &whalereinv1.PairingClaimResponse{ClaimId: c.GetClaimId()}, f.err
}

func (f *fakeExternalDevice) DeviceGrantStatus(_ context.Context, workspace, host, device string) (string, error) {
	f.received = []string{workspace, host, device}
	return f.state, f.err
}

func (f *fakeExternalDevice) RevokeDeviceGrant(_ context.Context, workspace, host, device string) error {
	f.received = []string{workspace, host, device}
	return f.err
}

func TestCloudPairingHTTPRequiresThePortableAuthorizationAndClaim(t *testing.T) {
	const path = "/v1/enrollments/devices/pairing"
	const body = `{"authorization":{"purpose":"device_pairing"},"claim":{"deviceId":"device","claimId":"operation"}}`
	fake := &fakeExternalDevice{}
	server := newExternalFixture(t, fake)
	for _, tc := range []struct {
		name   string
		body   string
		err    error
		status int
	}{
		{"bearer alone", "", nil, http.StatusBadRequest},
		{"malformed", "{", nil, http.StatusBadRequest},
		{"missing claim", `{"authorization":{}}`, nil, http.StatusBadRequest},
		{"bad claim", `{"authorization":{},"claim":[]}`, nil, http.StatusBadRequest},
		{"mismatch", body, external.ErrAuthorizationMismatch, http.StatusForbidden},
		{"expired", body, external.ErrAuthorizationExpired, http.StatusGone},
		{"scope present", body, nil, http.StatusOK},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fake.err = tc.err
			response := doRequest(t, server.PublicHandler(), http.MethodPost, path, []byte(tc.body),
				map[string]string{adminHeaderName: "Bearer account"})
			if response.Code != tc.status {
				t.Fatalf("HTTP status %d, want %d", response.Code, tc.status)
			}
		})
	}
	if !slices.Equal(fake.received, []string{"device_pairing", "device", "operation"}) {
		t.Fatal("body did not preserve the claim scope")
	}
	standalone := newAPIFixture(t)
	if got := doRequest(t, standalone.server.PublicHandler(), http.MethodPost, path, []byte(body), nil).Code; got != http.StatusNotImplemented {
		t.Fatalf("Standalone pairing response %d", got)
	}
}

func TestCloudDeviceAdminScopeAndStatus(t *testing.T) {
	const path = "/v1/enrollments/hosts/host/devices/device"
	const scope = "?workspaceId=workspace"
	fake := &fakeExternalDevice{state: "pending"}
	server := newExternalFixture(t, fake)
	admin := map[string]string{adminHeaderName: "Bearer admin-token"}
	for _, tc := range []struct {
		name, method, path string
		headers            map[string]string
		err                error
		status             int
		state              string
	}{
		{"scoped", http.MethodGet, path + scope, admin, nil, http.StatusOK, "pending"},
		{"missing", http.MethodGet, path + scope, admin, store.ErrCloudGrantMissing, http.StatusNotFound, "missing_grant"},
		{"other scope", http.MethodGet, path + scope, admin, store.ErrNotFound, http.StatusNotFound, ""},
		{"missing auth", http.MethodGet, path + scope, nil, nil, http.StatusUnauthorized, ""},
		{"no workspace", http.MethodGet, path, admin, nil, http.StatusBadRequest, ""},
		{"ambiguous workspace", http.MethodGet, path + scope + "&workspaceId=another", admin, nil, http.StatusBadRequest, ""},
		{"pending revocation", http.MethodPost, path + "/revocation" + scope, admin, external.ErrEnforcementUnavailable, http.StatusServiceUnavailable, ""},
		{"confirmed revocation", http.MethodPost, path + "/revocation" + scope, admin, nil, http.StatusNoContent, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fake.err = tc.err
			response := doRequest(t, server.PublicHandler(), tc.method, tc.path, nil, tc.headers)
			if response.Code != tc.status {
				t.Fatalf("HTTP status %d, want %d", response.Code, tc.status)
			}
			if tc.state != "" {
				var result map[string]string
				if json.Unmarshal(response.Body.Bytes(), &result) != nil || result["state"] != tc.state {
					t.Fatalf("device state %q, want %q", result["state"], tc.state)
				}
			}
		})
	}
	if !slices.Equal(fake.received, []string{"workspace", "host", "device"}) {
		t.Fatal("management request lost its scope")
	}
	standalone := newAPIFixture(t)
	for _, req := range []struct{ method, path string }{
		{http.MethodGet, path + scope},
		{http.MethodPost, path + "/revocation" + scope},
	} {
		if got := doRequest(t, standalone.server.PublicHandler(), req.method, req.path, nil, admin).Code; got != http.StatusNotImplemented {
			t.Fatalf("Standalone grant route response %d", got)
		}
	}
}

func TestAccountBearerDoesNotAuthenticateAsARegionalDevice(t *testing.T) {
	fixture := newAPIFixture(t)
	account := map[string]string{"Authorization": "Bearer cloud-account-fixture"}
	device := map[string]string{"Authorization": "Bearer device-token"}
	for _, check := range []struct {
		headers map[string]string
		status  int
	}{
		{account, http.StatusUnauthorized},
		{device, http.StatusOK},
	} {
		reply := doRequest(t, fixture.server.PublicHandler(), http.MethodGet, "/v1/hosts", nil, check.headers)
		if reply.Code != check.status {
			t.Fatalf("regional directory returned %d, want %d", reply.Code, check.status)
		}
	}
}
