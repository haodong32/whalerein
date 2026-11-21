package httpapi

import (
	"crypto/sha256"
	"net/http"
	"testing"
	"time"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/whalerein/whalerein/access/internal/requests"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestGetRequestReturnsStoredResultReferenceWithoutRenewal(t *testing.T) {
	f := newAPIFixture(t)
	digest := sha256.Sum256([]byte("result"))
	reference := &whalereinv1.PayloadReference{
		ObjectId:        "0123456789abcdef0123456789abcdef",
		OwnerNodeId:     "node-1",
		CiphertextBytes: 6,
		Sha256:          digest[:],
		ExpiresAtUnixMs: time.Now().Add(time.Hour).UnixMilli(),
		Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
	}
	encoded, err := proto.Marshal(reference)
	if err != nil {
		t.Fatal(err)
	}
	f.requests.row.ResultReference = encoded
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil,
		map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var state whalereinv1.RequestState
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &state); err != nil {
		t.Fatal(err)
	}
	if state.GetResultReference() == nil || state.GetResultReference().GetObjectId() != reference.GetObjectId() {
		t.Fatalf("result reference = %+v", state.GetResultReference())
	}
	if f.requests.touchRequest != "" {
		t.Fatal("status read renewed the execution lease")
	}
}

func TestHeartbeatReturnsTypedNonRenewalOutcomes(t *testing.T) {
	cases := []struct {
		name   string
		err    error
		status int
		code   whalereinv1.ErrorCode
	}{
		{"missing request", requests.ErrNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found},
		{"expired request", requests.ErrExpired, http.StatusGone, whalereinv1.ErrorCode_expired},
		{"terminal or elapsed lease", requests.ErrConflict, http.StatusConflict, whalereinv1.ErrorCode_conflict},
		{"foreign session", requests.ErrForbidden, http.StatusForbidden, whalereinv1.ErrorCode_forbidden},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newAPIFixture(t)
			f.requests.heartbeatErr = tc.err
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/requests/r1/heartbeat", nil,
				map[string]string{"Authorization": "Bearer device-token"})
			assertError(t, recorder, tc.status, tc.code)
		})
	}
}
