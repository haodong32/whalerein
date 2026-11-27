package httpapi

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"log/slog"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

type fakeDirectory struct {
	directory store.HostDirectory
	err       error
	calls     int
	hostID    string
	tokenHash []byte
	now       time.Time
}

func (f *fakeDirectory) HostDirectory(_ context.Context, hostID string, tokenHash []byte, now time.Time) (store.HostDirectory, error) {
	f.calls++
	f.hostID = hostID
	f.tokenHash = append([]byte(nil), tokenHash...)
	f.now = now
	if f.err != nil {
		return store.HostDirectory{}, f.err
	}
	return f.directory, nil
}

func TestHostDirectoryRouteReturnsDisplayMetadataOnly(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/directory", nil,
		map[string]string{"Authorization": "Bearer invitation-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var directory whalereinv1.HostDirectory
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &directory); err != nil {
		t.Fatal(err)
	}
	if directory.GetHostId() != "h1" || directory.GetDisplayName() != "Studio Mac" || directory.GetOsFamily() != "macos" {
		t.Fatalf("directory = %+v", &directory)
	}
	if recorder.Header().Get("Cache-Control") != "no-store" {
		t.Fatalf("cache-control = %q", recorder.Header().Get("Cache-Control"))
	}
	wantHash := sha256.Sum256([]byte("invitation-token"))
	if f.directory.hostID != "h1" || !bytes.Equal(f.directory.tokenHash, wantHash[:]) {
		t.Fatalf("store call host=%q hash=%x", f.directory.hostID, f.directory.tokenHash)
	}
}

func TestHostDirectoryRouteHidesEveryInvalidInvitation(t *testing.T) {
	for name, tc := range map[string]struct {
		path  string
		token string
		err   error
	}{
		"missing token":    {path: "/v1/hosts/h1/directory", token: ""},
		"malformed token":  {path: "/v1/hosts/h1/directory", token: "Basic abc"},
		"unknown token":    {path: "/v1/hosts/h1/directory", token: "Bearer unknown", err: store.ErrNotFound},
		"used token":       {path: "/v1/hosts/h1/directory", token: "Bearer used", err: store.ErrNotFound},
		"expired token":    {path: "/v1/hosts/h1/directory", token: "Bearer expired", err: store.ErrNotFound},
		"wrong host token": {path: "/v1/hosts/h2/directory", token: "Bearer other-host", err: store.ErrNotFound},
	} {
		t.Run(name, func(t *testing.T) {
			f := newAPIFixture(t)
			f.directory.err = tc.err
			headers := map[string]string{}
			if tc.token != "" {
				headers["Authorization"] = tc.token
			}
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, tc.path, nil, headers)
			assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
			if tc.token == "" && f.directory.calls != 0 {
				t.Fatal("missing token reached the store")
			}
		})
	}
}

func TestHostDirectoryRouteRejectsBadHostID(t *testing.T) {
	f := newAPIFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/bad!/directory", nil,
		map[string]string{"Authorization": "Bearer invitation-token"})
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
	if f.directory.calls != 0 {
		t.Fatal("invalid host id reached the store")
	}
}

func TestHostDirectoryRouteMapsStoreFailureToUnavailable(t *testing.T) {
	f := newAPIFixture(t)
	f.directory.err = errors.New("database unavailable")
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/directory", nil,
		map[string]string{"Authorization": "Bearer invitation-token"})
	assertError(t, recorder, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable)
}

func TestHostDirectoryRouteNeverLogsTokenOrDisplayValues(t *testing.T) {
	f := newAPIFixture(t)
	var logs bytes.Buffer
	f.server.deps.Logger = slog.New(slog.NewTextHandler(&logs, nil))
	token := "secret-invitation-token-value"

	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/directory", nil,
		map[string]string{"Authorization": "Bearer " + token})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	f.directory.err = store.ErrNotFound
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/directory", nil,
		map[string]string{"Authorization": "Bearer " + token})
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
	f.directory.err = errors.New("database unavailable")
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/directory", nil,
		map[string]string{"Authorization": "Bearer " + token})
	assertError(t, recorder, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable)

	logged := logs.String()
	for _, secret := range []string{token, "Studio Mac", "macos"} {
		if strings.Contains(logged, secret) {
			t.Fatalf("log leaked %q: %s", secret, logged)
		}
	}
}

func TestHostDirectoryRouteWithoutServiceIsUnavailable(t *testing.T) {
	f := newAPIFixture(t)
	f.server.deps.Directory = nil
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts/h1/directory", nil,
		map[string]string{"Authorization": "Bearer invitation-token"})
	assertError(t, recorder, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable)
}
