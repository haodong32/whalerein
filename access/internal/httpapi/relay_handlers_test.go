package httpapi

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/whalerein/whalerein/access/internal/payload"
	"github.com/whalerein/whalerein/access/internal/relay"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

const relayTestObjectID = "0123456789abcdef0123456789abcdef"

// fakeRelay records the decoded policy input and returns canned outcomes so the
// HTTP tests exercise only routing, decoding, headers and error mapping.
type fakeRelay struct {
	reservation  relay.Reservation
	reserveErr   error
	target       relay.UploadTarget
	prepareErr   error
	uploadErr    error
	object       relay.Object
	finishErr    error
	fetchResult  relay.FetchResult
	fetchErr     error
	deleteResult relay.DeleteResult
	deleteErr    error

	lastReserve relay.ReserveRequest
	lastUpload  relay.UploadRequest
	lastTarget  relay.UploadTarget
	uploadBody  []byte
	lastFinish  relay.FinishRequest
	lastFetch   relay.ObjectRequest
	lastDelete  relay.ObjectRequest
}

func (f *fakeRelay) Reserve(_ context.Context, request relay.ReserveRequest) (relay.Reservation, error) {
	f.lastReserve = request
	return f.reservation, f.reserveErr
}

func (f *fakeRelay) PrepareUpload(_ context.Context, request relay.UploadRequest) (relay.UploadTarget, error) {
	f.lastUpload = request
	return f.target, f.prepareErr
}

func (f *fakeRelay) Upload(_ context.Context, target relay.UploadTarget, body io.Reader) error {
	f.lastTarget = target
	if f.uploadErr != nil {
		return f.uploadErr
	}
	data, err := io.ReadAll(body)
	if err != nil {
		return err
	}
	f.uploadBody = data
	return nil
}

func (f *fakeRelay) Finish(_ context.Context, request relay.FinishRequest) (relay.Object, error) {
	f.lastFinish = request
	return f.object, f.finishErr
}

func (f *fakeRelay) Fetch(_ context.Context, request relay.ObjectRequest) (relay.FetchResult, error) {
	f.lastFetch = request
	return f.fetchResult, f.fetchErr
}

func (f *fakeRelay) Delete(_ context.Context, request relay.ObjectRequest) (relay.DeleteResult, error) {
	f.lastDelete = request
	return f.deleteResult, f.deleteErr
}

func newRelayFixture(t *testing.T) (*apiFixture, *fakeRelay) {
	t.Helper()
	f := newAPIFixture(t)
	fake := &fakeRelay{
		reservation: relay.Reservation{
			ObjectID: relayTestObjectID, OwnerNodeID: "node-1",
			UploadPath: "/v1/relay/objects/" + relayTestObjectID,
			MaxBytes:   4, ExpiresAtUnixMs: 2, UploadTimeoutMS: 30000,
		},
		target: relay.UploadTarget{
			Ref:    payload.Ref{ObjectID: relayTestObjectID, HostID: "h1", Purpose: "PAYLOAD_PURPOSE_RESULT"},
			Length: 4,
		},
		object: relay.Object{
			ObjectID: relayTestObjectID, OwnerNodeID: "node-1",
			Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
			CiphertextBytes: 4, SHA256: []byte("digest"), ExpiresAtUnixMs: 2,
		},
		deleteResult: relay.DeleteResult{Cancelled: true},
	}
	f.server.deps.Relay = fake
	return f, fake
}

func relayReserveBody(epoch uint32) []byte {
	body := &whalereinv1.RelayObjectReserveRequest{
		RelayVersion: epoch, HostId: "h1", DeviceId: "d1", SessionId: "s1",
		ChannelId:       "c1",
		Purpose:         whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
		CiphertextBytes: 4, Sha256: []byte("digest"),
		ExpiresAtUnixMs: 2, SignedAtUnixMs: 1, Signature: []byte("signature"),
	}
	data, _ := protojson.Marshal(body)
	return data
}

func TestRelayReserveRouteDecodesAndReturnsReservation(t *testing.T) {
	f, fake := newRelayFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/relay/objects", relayReserveBody(4), nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var reservation whalereinv1.RelayObjectReservation
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &reservation); err != nil {
		t.Fatal(err)
	}
	if reservation.GetObjectId() != relayTestObjectID || reservation.GetUploadPath() != "/v1/relay/objects/"+relayTestObjectID ||
		reservation.GetOwnerNodeId() != "node-1" || reservation.GetMaxBytes() != 4 ||
		reservation.GetUploadTimeoutMs() != 30000 {
		t.Fatalf("reservation = %+v", &reservation)
	}
	got := fake.lastReserve
	if got.RelayVersion != 4 || got.HostID != "h1" || got.DeviceID != "d1" || got.SessionID != "s1" ||
		got.ChannelID != "c1" || got.Purpose != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT ||
		got.CiphertextBytes != 4 || !bytes.Equal(got.SHA256, []byte("digest")) ||
		got.ExpiresAtUnixMs != 2 || got.SignedAtUnixMs != 1 || !bytes.Equal(got.Signature, []byte("signature")) {
		t.Fatalf("decoded request = %+v", got)
	}
}

func TestRelayReserveRoutePassesBearerAndOwnerHeader(t *testing.T) {
	f, fake := newRelayFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/relay/objects", relayReserveBody(4), map[string]string{
		"Authorization":      "Bearer device-token",
		RelayOwnerNodeHeader: "node-2",
	})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if fake.lastReserve.BearerToken != "device-token" || fake.lastReserve.OwnerNode != "node-2" {
		t.Fatalf("headers = %+v", fake.lastReserve)
	}
}

func TestRelayReserveEpochAndBodyErrors(t *testing.T) {
	f, _ := newRelayFixture(t)
	for name, tc := range map[string]struct {
		body   []byte
		status int
		code   whalereinv1.ErrorCode
	}{
		"wrong epoch":   {relayReserveBody(2), http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible},
		"missing epoch": {[]byte(`{"hostId":"h1"}`), http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible},
		"bad json":      {[]byte(`{`), http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument},
	} {
		t.Run(name, func(t *testing.T) {
			fake := f.server.deps.Relay.(*fakeRelay)
			if tc.status == http.StatusBadRequest && strings.Contains(name, "epoch") {
				fake.reserveErr = relay.ErrEpoch
			} else {
				fake.reserveErr = nil
			}
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost, "/v1/relay/objects", tc.body, nil)
			assertError(t, recorder, tc.status, tc.code)
		})
	}
}

func TestRelayUploadRequiresEpochAndStreamsExactBody(t *testing.T) {
	f, fake := newRelayFixture(t)
	path := "/v1/relay/objects/" + relayTestObjectID

	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPut, path, []byte("body"), nil)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible)

	fake.prepareErr = relay.ErrEpoch
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPut, path, []byte("body"), map[string]string{RelayEpochHeader: "2"})
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible)
	fake.prepareErr = nil

	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPut, path, []byte("body"), map[string]string{
		RelayEpochHeader:         "4",
		RelayHostSignedAtHeader:  "1700000000000",
		RelayHostSignatureHeader: base64.RawURLEncoding.EncodeToString([]byte("sig")),
		RelayOwnerNodeHeader:     "",
	})
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if string(fake.uploadBody) != "body" || fake.lastTarget.Length != 4 {
		t.Fatalf("upload = %q target=%+v", fake.uploadBody, fake.lastTarget)
	}
	if fake.lastUpload.HasContentLength != true || fake.lastUpload.ContentLength != 4 {
		t.Fatalf("content length = %+v", fake.lastUpload)
	}
	if fake.lastUpload.SignedAtUnixMs != 1_700_000_000_000 || string(fake.lastUpload.Signature) != "sig" {
		t.Fatalf("host headers = %+v", fake.lastUpload)
	}
}

func TestRelayUploadCapsBodyAtReservedLength(t *testing.T) {
	f, fake := newRelayFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPut, "/v1/relay/objects/"+relayTestObjectID,
		[]byte("0123456789"), map[string]string{RelayEpochHeader: "4"})
	// The handler caps the stream at the reserved length plus one; the extra
	// byte is a conflict, never an internal error and never a complete upload.
	assertError(t, recorder, http.StatusConflict, whalereinv1.ErrorCode_conflict)
	if fake.uploadBody != nil {
		t.Fatalf("oversize body was accepted: %q", fake.uploadBody)
	}
}

func TestRelayUploadMissingContentLength(t *testing.T) {
	f, fake := newRelayFixture(t)
	req := httptest.NewRequest(http.MethodPut, "/v1/relay/objects/"+relayTestObjectID, bytes.NewReader(nil))
	req.ContentLength = -1
	req.Header.Set(RelayEpochHeader, "4")
	recorder := httptest.NewRecorder()
	f.server.PublicHandler().ServeHTTP(recorder, req)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("status = %d", recorder.Code)
	}
	if fake.lastUpload.HasContentLength {
		t.Fatal("missing Content-Length was reported present")
	}
}

func TestRelayFinishRoute(t *testing.T) {
	f, fake := newRelayFixture(t)
	body, _ := protojson.Marshal(&whalereinv1.RelayObjectFinishRequest{
		RelayVersion: 4, CiphertextBytes: 4, Sha256: []byte("digest"),
		SignedAtUnixMs: 1, Signature: []byte("signature"),
	})
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost,
		"/v1/relay/objects/"+relayTestObjectID+"/finish", body, nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var object whalereinv1.RelayObject
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &object); err != nil {
		t.Fatal(err)
	}
	if object.GetObjectId() != relayTestObjectID || object.GetCiphertextBytes() != 4 ||
		object.GetPurpose() != whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT {
		t.Fatalf("object = %+v", &object)
	}
	if fake.lastFinish.ObjectID != relayTestObjectID || fake.lastFinish.RelayVersion != 4 ||
		fake.lastFinish.CiphertextBytes != 4 || !bytes.Equal(fake.lastFinish.SHA256, []byte("digest")) {
		t.Fatalf("finish request = %+v", fake.lastFinish)
	}

	fake.finishErr = relay.ErrEpoch
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodPost,
		"/v1/relay/objects/"+relayTestObjectID+"/finish", []byte(`{"relayVersion":2}`), nil)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible)
}

func TestRelayFetchStreamsExactCiphertextHeaders(t *testing.T) {
	f, fake := newRelayFixture(t)
	digest := []byte{1, 2, 3, 4}
	fake.fetchResult = relay.FetchResult{
		Body: io.NopCloser(strings.NewReader("body")), Length: 4,
		Digest: digest, ExpiresAtUnixMs: 1_700_000_000_000,
	}
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet,
		"/v1/relay/objects/"+relayTestObjectID, nil, map[string]string{RelayEpochHeader: "4"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if recorder.Body.String() != "body" {
		t.Fatalf("body = %q", recorder.Body.String())
	}
	if recorder.Header().Get("Content-Length") != "4" {
		t.Fatalf("content length = %q", recorder.Header().Get("Content-Length"))
	}
	if recorder.Header().Get("Digest") != "sha-256="+base64.StdEncoding.EncodeToString(digest) {
		t.Fatalf("digest = %q", recorder.Header().Get("Digest"))
	}
	if recorder.Header().Get(ObjectExpiresHeader) != "1700000000000" {
		t.Fatalf("expires = %q", recorder.Header().Get(ObjectExpiresHeader))
	}
	if recorder.Header().Get("Cache-Control") != "no-store" {
		t.Fatalf("cache control = %q", recorder.Header().Get("Cache-Control"))
	}
}

func TestRelayFetchRequiresEpoch(t *testing.T) {
	f, _ := newRelayFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet,
		"/v1/relay/objects/"+relayTestObjectID, nil, nil)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible)
}

func TestRelayDeleteRoute(t *testing.T) {
	f, fake := newRelayFixture(t)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodDelete,
		"/v1/relay/objects/"+relayTestObjectID, nil, map[string]string{RelayEpochHeader: "4"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	var result whalereinv1.RelayObjectDeleteResult
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &result); err != nil {
		t.Fatal(err)
	}
	if !result.GetCancelled() || fake.lastDelete.ObjectID != relayTestObjectID {
		t.Fatalf("delete = %+v", &result)
	}

	fake.deleteResult = relay.DeleteResult{Cancelled: false}
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodDelete,
		"/v1/relay/objects/"+relayTestObjectID, nil, map[string]string{RelayEpochHeader: "4"})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d", recorder.Code)
	}
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &result); err != nil {
		t.Fatal(err)
	}
	if result.GetCancelled() {
		t.Fatal("repeated delete reported a release")
	}
}

func TestRelayWrongOwnerHeaderAndReason(t *testing.T) {
	f, fake := newRelayFixture(t)
	fake.fetchErr = &relay.WrongOwnerError{Requested: "node-2"}
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet,
		"/v1/relay/objects/"+relayTestObjectID, nil, map[string]string{RelayEpochHeader: "4"})
	assertError(t, recorder, http.StatusConflict, whalereinv1.ErrorCode_conflict)
	if recorder.Header().Get(RelayOwnerNodeHeader) != "node-2" {
		t.Fatalf("owner header = %q", recorder.Header().Get(RelayOwnerNodeHeader))
	}
	var response whalereinv1.ErrorResponse
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response.GetReason() != "wrong_owner_path" {
		t.Fatalf("reason = %q", response.GetReason())
	}
}

func TestRelayErrorMappingAndDisabledCapability(t *testing.T) {
	cases := []struct {
		name   string
		err    error
		status int
		code   whalereinv1.ErrorCode
		reason string
	}{
		{"invalid", relay.ErrInvalid, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, ""},
		{"unauthorized", relay.ErrUnauthorized, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized, ""},
		{"forbidden", relay.ErrForbidden, http.StatusForbidden, whalereinv1.ErrorCode_forbidden, ""},
		{"not found", relay.ErrNotFound, http.StatusNotFound, whalereinv1.ErrorCode_not_found, ""},
		{"digest", relay.ErrDigestMismatch, http.StatusConflict, whalereinv1.ErrorCode_payload_digest_mismatch, ""},
		{"conflict", relay.ErrConflict, http.StatusConflict, whalereinv1.ErrorCode_conflict, ""},
		{"expired", relay.ErrExpired, http.StatusGone, whalereinv1.ErrorCode_expired, ""},
		{"too large", relay.ErrTooLarge, http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large, ""},
		{"quota", relay.ErrQuota, http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited, "quota_exceeded"},
		{"pressure", relay.ErrPressure, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "storage_pressure"},
		{"unavailable", relay.ErrUnavailable, http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, ""},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			f, fake := newRelayFixture(t)
			fake.fetchErr = test.err
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet,
				"/v1/relay/objects/"+relayTestObjectID, nil, map[string]string{RelayEpochHeader: "4"})
			assertError(t, recorder, test.status, test.code)
			if test.reason != "" {
				var response whalereinv1.ErrorResponse
				if err := protojson.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
					t.Fatal(err)
				}
				if response.GetReason() != test.reason {
					t.Fatalf("reason = %q, want %q", response.GetReason(), test.reason)
				}
			}
		})
	}

	// The default service keeps the routes but reports a missing capability.
	f := newAPIFixture(t)
	f.server.deps.Relay = nil
	server, err := New(Dependencies{
		Auth: f.auth, Pairing: f.pairing, Requests: f.requests, Readiness: f.ready,
		InternalToken: "internal-token",
	})
	if err != nil {
		t.Fatal(err)
	}
	recorder := doRequest(t, server.PublicHandler(), http.MethodGet,
		"/v1/relay/objects/"+relayTestObjectID, nil, map[string]string{RelayEpochHeader: "4"})
	assertError(t, recorder, http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability)
}

func TestRelayErrorsDoNotEchoSecrets(t *testing.T) {
	f, fake := newRelayFixture(t)
	fake.fetchErr = errors.New("ciphertext=topsecret token=abc")
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet,
		"/v1/relay/objects/"+relayTestObjectID, nil, map[string]string{RelayEpochHeader: "4"})
	if recorder.Code != http.StatusInternalServerError {
		t.Fatalf("status = %d", recorder.Code)
	}
	if strings.Contains(recorder.Body.String(), "topsecret") || strings.Contains(recorder.Body.String(), "abc") {
		t.Fatalf("error body leaked internal text: %s", recorder.Body.String())
	}
	var response whalereinv1.ErrorResponse
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response.GetMessage() != "internal error" {
		t.Fatalf("message = %q", response.GetMessage())
	}
}
