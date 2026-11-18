package httpapi

import (
	"context"
	"net/http"
	"testing"

	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type sessionEndpointFixture struct {
	fakeExternal
	installed, revoked, read string
	revision                 int64
	err                      error
}

func (s *sessionEndpointFixture) InstallDeviceSession(_ context.Context, id string, a *whalereinv1.ExternalAuthorization) error {
	s.installed = id + "/" + a.OperationId
	return s.err
}

func (s *sessionEndpointFixture) RevokeDeviceSession(_ context.Context, id string, rev int64) error {
	s.revoked, s.revision = id, rev
	return s.err
}

func (s *sessionEndpointFixture) DeviceSessionPairingStatus(_ context.Context, token, hostID, operationID string) (string, error) {
	if token != "regional-token" {
		return "", external.ErrAuthorizationMismatch
	}
	s.read = hostID + "/" + operationID
	return "ready", s.err
}

// Managed endpoints keep administrative mutation separate from the scoped
// pairing outcome read and classify a lost or pending enforcement response.
func TestManagedDeviceSessionHTTPContract(t *testing.T) {
	const revoke = "/v1/external/sessions/a/revocation"
	const permit = "/v1/external/sessions/a/permission"
	const status = "/v1/external/hosts/host-a/pairings/operation-a"
	admin := map[string]string{"Authorization": "Bearer admin-token"}
	device := map[string]string{"Authorization": "Bearer regional-token"}
	fixture := &sessionEndpointFixture{}
	server := newExternalFixture(t, fixture)
	for _, tc := range []struct {
		method, route, payload string
		headers                map[string]string
		want                   int
	}{
		{http.MethodPost, permit, `{"operationId":"a"}`, nil, http.StatusUnauthorized},
		{http.MethodPost, permit, `{"operationId":"a"}`, device, http.StatusUnauthorized},
		{http.MethodPost, permit, `{`, admin, http.StatusBadRequest},
		{http.MethodPost, permit, `{"operationId":"a"}`, admin, http.StatusNoContent},
		{http.MethodPost, revoke, `{"revision":3}`, admin, http.StatusNoContent},
		{http.MethodPost, revoke, `{"revision":"wrong"}`, admin, http.StatusBadRequest},
		{http.MethodGet, status, "", nil, http.StatusUnauthorized},
		{http.MethodGet, status, "", admin, http.StatusForbidden},
		{http.MethodGet, status, "", device, http.StatusOK},
		{http.MethodGet, "/v1/external/hosts/%2B/pairings/operation-a", "", device, http.StatusBadRequest},
	} {
		got := doRequest(t, server.PublicHandler(), tc.method, tc.route, []byte(tc.payload), tc.headers)
		if got.Code != tc.want {
			t.Fatalf("%s %s returned %d, expected %d", tc.method, tc.route, got.Code, tc.want)
		}
	}
	if fixture.installed != "a/a" || fixture.revoked != "a" || fixture.revision != 3 ||
		fixture.read != "host-a/operation-a" {
		t.Fatalf("wrong operation scopes: %+v", fixture)
	}
	if got := doRequest(t, newExternalFixture(t, nil).PublicHandler(), http.MethodPost, permit, nil, nil); got.Code != http.StatusNotImplemented {
		t.Fatalf("standalone managed mutation returned %d", got.Code)
	}
	if got := doRequest(t, newExternalFixture(t, nil).PublicHandler(), http.MethodGet, status, nil, nil); got.Code != http.StatusNotImplemented {
		t.Fatalf("standalone managed read returned %d", got.Code)
	}
	fixture.err = external.ErrEnforcementUnavailable
	for _, route := range []string{permit, revoke} {
		if got := doRequest(t, server.PublicHandler(), http.MethodPost, route, []byte(`{}`), admin); got.Code != http.StatusServiceUnavailable {
			t.Fatalf("outage of %s returned %d", route, got.Code)
		}
	}
	fixture.err = store.ErrNotFound
	if got := doRequest(t, server.PublicHandler(), http.MethodGet, status, nil, device); got.Code != http.StatusNotFound {
		t.Fatalf("unresolved outcome returned %d", got.Code)
	}
}
