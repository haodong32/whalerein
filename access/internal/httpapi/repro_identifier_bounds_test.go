package httpapi

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"testing"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// ACC-1 regression: request identifier bounds.
//
// proto/v1/WIRE.md (identifier table) bounds channel_id at 128 bytes and
// trace_id at 64 bytes, requires printable ASCII and forbids the "|" AAD
// separator. It states: "A request whose identifier violates these bounds is
// invalid_argument and is not forwarded."
//
// POST /v1/hosts/{host_id}/requests must reject an unsafe or overlong
// channel_id/trace_id with 400 before the request service sees the frame.
func TestReproSubmitRejectsOutOfContractChannelAndTraceIdentifiers(t *testing.T) {
	cases := []struct {
		name  string
		field string
		value string
	}{
		{"channel id with AAD separator", "channelId", "chan|1"},
		{"channel id over 128 bytes", "channelId", strings.Repeat("c", 129)},
		{"channel id with control character", "channelId", "chan\x01nel"},
		{"trace id over 64 bytes", "traceId", strings.Repeat("t", 65)},
		{"trace id with AAD separator", "traceId", "trace|1"},
		{"trace id with control character", "traceId", "trace\x01id"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newAPIFixture(t)
			encoded, err := json.Marshal(tc.value)
			if err != nil {
				t.Fatal(err)
			}
			body := fmt.Sprintf(`{"requestId":"r1","kind":"REQUEST_KIND_COMMAND","commandId":"c1",`+
				`"payloadDigest":"ZGlnZXN0","expiresAtUnixMs":4102444800000,"protocolVersion":1,`+
				`"body":"Ym9keQ==",%q:%s}`, tc.field, encoded)
			recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost,
				"/v1/hosts/h1/requests", []byte(body),
				map[string]string{"Authorization": "Bearer device-token"})

			// The documented behavior is a typed 400 that is never forwarded.
			if recorder.Code != http.StatusBadRequest {
				t.Fatalf("%s %q was accepted: status=%d body=%s submitted=%d; "+
					"WIRE requires 400 invalid_argument and no forward",
					tc.field, tc.value, recorder.Code, recorder.Body.String(), len(f.requests.submitted))
			}
			assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
			if len(f.requests.submitted) != 0 {
				t.Fatalf("%s reached the request service: %+v", tc.field, f.requests.submitted[0])
			}
		})
	}

	// Positive control: the same envelope with in-contract identifiers is
	// admitted, so the repro fails only for the identifier bounds.
	t.Run("valid identifiers are accepted", func(t *testing.T) {
		f := newAPIFixture(t)
		body := `{"requestId":"r1","kind":"REQUEST_KIND_COMMAND","commandId":"c1",` +
			`"payloadDigest":"ZGlnZXN0","expiresAtUnixMs":4102444800000,"protocolVersion":1,` +
			`"body":"Ym9keQ==","channelId":"channel-1","traceId":"trace-1"}`
		recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost,
			"/v1/hosts/h1/requests", []byte(body),
			map[string]string{"Authorization": "Bearer device-token"})
		if recorder.Code != http.StatusOK {
			t.Fatalf("in-contract identifiers rejected: status=%d body=%s", recorder.Code, recorder.Body.String())
		}
	})
}

// TestReproTraceIdentifierIsPersistedAndLoggedVerbatim guards the second
// effect: an unbounded client trace_id must never reach the durable row or the
// request log verbatim, so it is rejected at admission.
func TestReproTraceIdentifierIsPersistedAndLoggedVerbatim(t *testing.T) {
	f := newAPIFixture(t)
	trace := strings.Repeat("x", 4096)
	encoded, err := json.Marshal(trace)
	if err != nil {
		t.Fatal(err)
	}
	body := fmt.Sprintf(`{"requestId":"r1","kind":"REQUEST_KIND_COMMAND","commandId":"c1",`+
		`"payloadDigest":"ZGlnZXN0","expiresAtUnixMs":4102444800000,"protocolVersion":1,`+
		`"body":"Ym9keQ==","traceId":%s}`, encoded)
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodPost,
		"/v1/hosts/h1/requests", []byte(body),
		map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusBadRequest {
		t.Fatalf("4096-byte trace id accepted: status=%d submitted=%d", recorder.Code, len(f.requests.submitted))
	}
}
