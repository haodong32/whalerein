package httpapi

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type proofExternal struct {
	*fakeExternal
	got        *whalereinv1.SessionProof
	credential string
}

func (p *proofExternal) RegisterSessionProof(_ context.Context, _ string, credential string, proof *whalereinv1.SessionProof) error {
	p.got = proof
	p.credential = credential
	return nil
}

func TestSessionProofEndpointRelaysTheExactBody(t *testing.T) {
	external := &proofExternal{fakeExternal: &fakeExternal{}}
	server := newExternalFixture(t, external)
	hash := make([]byte, 32)
	signature := make([]byte, 64)
	body, err := json.Marshal(map[string]any{
		"hostId": "host-1", "deviceId": "device-1", "channelId": "channel-1",
		"transcriptHash": base64.RawURLEncoding.EncodeToString(hash),
		"issuedAtUnixMs": 1_700_000_000_000, "expiresAtUnixMs": 1_700_000_030_000,
		"signature": base64.RawURLEncoding.EncodeToString(signature),
	})
	if err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequest(http.MethodPost, "/v1/sessions/session-1/proof", bytes.NewReader(body))
	request.Header.Set("Authorization", "Bearer device-credential")
	request.Header.Set("Content-Type", "application/json")
	response := httptest.NewRecorder()
	server.PublicHandler().ServeHTTP(response, request)
	if response.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", response.Code, response.Body.String())
	}
	if external.credential != "device-credential" || external.got == nil {
		t.Fatalf("relay = %q %v", external.credential, external.got)
	}
	if external.got.GetSessionId() != "session-1" || external.got.GetHostId() != "host-1" ||
		external.got.GetDeviceId() != "device-1" || external.got.GetChannelId() != "channel-1" ||
		len(external.got.GetTranscriptHash()) != 32 || len(external.got.GetSignature()) != 64 {
		t.Fatalf("proof = %v", external.got)
	}
}
