package httpapi

import (
	"net/http"
	"testing"

	"github.com/whalerein/whalerein/access/internal/auth"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

func TestHostAuthorityLeaseReturnsNoContentOrATypedReason(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.PublicHandler()
	headers := map[string]string{"Authorization": "Bearer device-token"}

	allowed := doRequest(t, handler, http.MethodGet, "/v1/hosts/h1/authority", nil, headers)
	if allowed.Code != http.StatusNoContent {
		t.Fatalf("live authority lease = %d %s", allowed.Code, allowed.Body.String())
	}

	// An ended grant is a terminal answer with a specific reason, not a generic
	// 403 that a client could mistake for a retryable transport failure.
	f.auth.authorizeErr = &auth.AuthorityDenial{
		Reason: auth.ReasonHostDisabled, StatusCode: http.StatusForbidden,
	}
	denied := doRequest(t, handler, http.MethodGet, "/v1/hosts/h1/authority", nil, headers)
	if denied.Code != http.StatusForbidden {
		t.Fatalf("ended authority lease = %d %s", denied.Code, denied.Body.String())
	}
	var response whalereinv1.ErrorResponse
	if err := protojson.Unmarshal(denied.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response.GetReason() != auth.ReasonHostDisabled {
		t.Fatalf("reason = %q, want %q", response.GetReason(), auth.ReasonHostDisabled)
	}
}
