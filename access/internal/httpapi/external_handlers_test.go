package httpapi

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"testing"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

// fakeExternal scripts the external account service boundary.
type fakeExternal struct {
	enrollResponse *whalereinv1.HostEnrollmentResponse
	enrollErr      error
	statusResponse *whalereinv1.HostEnrollmentStatus
	statusErr      error
	operation      *whalereinv1.OperationStatus
	operationErr   error
	lastHost       string
}

func (f *fakeExternal) EnrollHost(context.Context, *whalereinv1.HostEnrollmentRequest) (*whalereinv1.HostEnrollmentResponse, error) {
	return f.enrollResponse, f.enrollErr
}

func (f *fakeExternal) EnrollmentStatus(_ context.Context, hostID string) (*whalereinv1.HostEnrollmentStatus, error) {
	f.lastHost = hostID
	return f.statusResponse, f.statusErr
}

func (f *fakeExternal) OperationStatus(context.Context, string) (*whalereinv1.OperationStatus, error) {
	return f.operation, f.operationErr
}

// newExternalFixture builds a server with external mode enabled.
func newExternalFixture(t *testing.T, fake *fakeExternal) *Server {
	t.Helper()
	server, err := New(Dependencies{
		Auth:          &fakeAuth{adminToken: "admin-token"},
		Pairing:       &fakePairing{},
		Requests:      &fakeRequests{},
		Readiness:     diagnostics.NewReadiness(),
		Logger:        slog.New(slog.NewTextHandler(io.Discard, nil)),
		InternalToken: "internal",
		MaxBodyBytes:  1 << 20,
		External:      fake,
	})
	if err != nil {
		t.Fatal(err)
	}
	return server
}

const adminHeaderName = "Authorization"

func TestEnrollHostEndpointReturnsTheStatusAndCredential(t *testing.T) {
	fake := &fakeExternal{enrollResponse: &whalereinv1.HostEnrollmentResponse{
		Status:     &whalereinv1.HostEnrollmentStatus{HostId: "host-1", State: whalereinv1.EnrollmentState_enrollment_pending},
		Credential: &whalereinv1.HostCredential{HostId: "host-1", Username: "host:host-1", Password: "secret"},
	}}
	server := newExternalFixture(t, fake)
	body, err := protojson.Marshal(&whalereinv1.HostEnrollmentRequest{HostId: "host-1"})
	if err != nil {
		t.Fatal(err)
	}
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", recorder.Code, recorder.Body.String())
	}
	decoded := &whalereinv1.HostEnrollmentResponse{}
	if err := protojson.Unmarshal(recorder.Body.Bytes(), decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.GetCredential().GetPassword() != "secret" {
		t.Fatalf("the committing response must carry the credential: %v", decoded)
	}
}

func TestEnrollHostEndpointRejectsMalformedBody(t *testing.T) {
	server := newExternalFixture(t, &fakeExternal{})
	recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts", []byte("{"), nil)
	if recorder.Code != http.StatusBadRequest {
		t.Fatalf("expected 400, got %d", recorder.Code)
	}
}

func TestEnrollHostEndpointMapsRefusals(t *testing.T) {
	for name, testCase := range map[string]struct {
		err  error
		want int
	}{
		"expired":   {external.ErrAuthorizationExpired, http.StatusGone},
		"mismatch":  {external.ErrAuthorizationMismatch, http.StatusForbidden},
		"signature": {external.ErrAuthorizationSignature, http.StatusForbidden},
		"proof":     {external.ErrProofInvalid, http.StatusForbidden},
		"key":       {external.ErrKeyNotValid, http.StatusForbidden},
		"disabled":  {external.ErrEnrollmentDisabled, http.StatusForbidden},
		"conflict":  {store.ErrOperationConflict, http.StatusConflict},
		"stale":     {external.ErrStaleRevision, http.StatusConflict},
		"missing":   {store.ErrNotFound, http.StatusNotFound},
		"internal":  {errors.New("boom"), http.StatusInternalServerError},
	} {
		t.Run(name, func(t *testing.T) {
			server := newExternalFixture(t, &fakeExternal{enrollErr: testCase.err})
			body, err := protojson.Marshal(&whalereinv1.HostEnrollmentRequest{HostId: "host-1"})
			if err != nil {
				t.Fatal(err)
			}
			recorder := doRequest(t, server.PublicHandler(), http.MethodPost, "/v1/enrollments/hosts", body, nil)
			if recorder.Code != testCase.want {
				t.Fatalf("expected %d, got %d: %s", testCase.want, recorder.Code, recorder.Body.String())
			}
		})
	}
}

func TestExternalStatusEndpointsRequireTheAdminBearer(t *testing.T) {
	fake := &fakeExternal{statusResponse: &whalereinv1.HostEnrollmentStatus{HostId: "host-1"}}
	server := newExternalFixture(t, fake)

	recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil, nil)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 without a token, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil,
		map[string]string{adminHeaderName: "Bearer wrong"})
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 for a wrong token, got %d", recorder.Code)
	}
	recorder = doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/enrollments/hosts/host-1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("expected 200 with the admin token, got %d", recorder.Code)
	}
	if fake.lastHost != "host-1" {
		t.Fatalf("the path parameter was not passed through: %q", fake.lastHost)
	}
}

func TestOperationStatusEndpointReportsRefusals(t *testing.T) {
	fake := &fakeExternal{operationErr: store.ErrNotFound}
	server := newExternalFixture(t, fake)
	recorder := doRequest(t, server.PublicHandler(), http.MethodGet, "/v1/operations/op-1", nil,
		map[string]string{adminHeaderName: "Bearer admin-token"})
	if recorder.Code != http.StatusNotFound {
		t.Fatalf("expected 404 for an unknown operation, got %d", recorder.Code)
	}
}

func TestExternalRoutesAreAbsentWithoutTheMode(t *testing.T) {
	server := newAPIFixture(t).server
	for _, path := range []string{"/v1/enrollments/hosts", "/v1/operations/op-1"} {
		recorder := doRequest(t, server.PublicHandler(), http.MethodGet, path, nil,
			map[string]string{adminHeaderName: "Bearer admin-token"})
		if recorder.Code != http.StatusNotFound && recorder.Code != http.StatusMethodNotAllowed {
			t.Fatalf("%s should not exist in standalone mode, got %d", path, recorder.Code)
		}
	}
}
