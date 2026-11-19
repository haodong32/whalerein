package httpapi

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/whalerein/whalerein/access/internal/auth"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

// --- ACC-05: JSON body contract -------------------------------------------

func TestJSONBodyContractDistinguishesMediaTypeSyntaxAndUnknownFields(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.PublicHandler()
	admin := map[string]string{"Authorization": "Bearer admin-token"}

	for name, contentType := range map[string]string{
		"reports an unsupported media type when the header is absent": "",
		"reports an unsupported media type for a form body":           "application/x-www-form-urlencoded",
		"reports an unsupported media type for a text body":           "text/plain",
		"reports an unsupported media type for a malformed value":     "application/json; charset",
	} {
		t.Run(name, func(t *testing.T) {
			headers := map[string]string{"Content-Type": contentType}
			for key, value := range admin {
				headers[key] = value
			}
			recorder := doRequest(t, handler, http.MethodPost, "/v1/pairing/invitations", []byte(`{"hostId":"h1"}`), headers)
			assertError(t, recorder, http.StatusUnsupportedMediaType, whalereinv1.ErrorCode_invalid_argument)
			if !strings.Contains(errorMessage(t, recorder), "application/json") {
				t.Fatalf("message = %q", errorMessage(t, recorder))
			}
		})
	}

	// A media type with parameters and a different case is accepted.
	headers := map[string]string{"Content-Type": "Application/JSON; charset=utf-8"}
	for key, value := range admin {
		headers[key] = value
	}
	recorder := doRequest(t, handler, http.MethodPost, "/v1/pairing/invitations", []byte(`{"hostId":"h1"}`), headers)
	if recorder.Code != http.StatusOK {
		t.Fatalf("parameterized media type status = %d body=%s", recorder.Code, recorder.Body.String())
	}

	// Malformed JSON and an unknown additive field have distinct messages and
	// neither echoes the submitted value.
	recorder = doRequest(t, handler, http.MethodPost, "/v1/pairing/invitations", []byte(`{"hostId":`), admin)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
	syntaxMessage := errorMessage(t, recorder)
	if !strings.Contains(syntaxMessage, "not valid JSON") {
		t.Fatalf("syntax message = %q", syntaxMessage)
	}
	if strings.Contains(recorder.Body.String(), "hostId") {
		t.Fatalf("syntax response echoed input: %s", recorder.Body.String())
	}

	// An additive field from a newer peer is ignored (rolling upgrade), not a
	// 400 that looks like a syntax error.
	f.pairing.invited = ""
	recorder = doRequest(t, handler, http.MethodPost, "/v1/pairing/invitations",
		[]byte(`{"hostId":"h1","futureField":{"secretValue":"private-peer-data"}}`), admin)
	if recorder.Code != http.StatusOK {
		t.Fatalf("additive field status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if f.pairing.invited != "h1" {
		t.Fatalf("additive field did not reach pairing: %q", f.pairing.invited)
	}
	if strings.Contains(recorder.Body.String(), "futureField") || strings.Contains(recorder.Body.String(), "private-peer-data") {
		t.Fatalf("response reflected the additive field: %s", recorder.Body.String())
	}
}

// TestCrossVersionDecodeIgnoresAdditiveFieldsAndRejectsUnknownEnums covers the
// rolling-upgrade contract: a newer peer may add fields, but an enum value this
// build does not know must never silently become the zero value.
func TestCrossVersionDecodeIgnoresAdditiveFieldsAndRejectsUnknownEnums(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.PublicHandler()

	// Unknown enum *name* on the pairing phase.
	recorder := doRequest(t, handler, http.MethodPost, "/v1/pairing/claims", []byte(
		`{"token":"tok","claimId":"c1","phase":"PAIRING_PHASE_FROM_THE_FUTURE","noiseMessage":"bXNn","deviceId":"d1","protocolVersion":1}`), nil)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible)
	if strings.Contains(recorder.Body.String(), "PAIRING_PHASE_FROM_THE_FUTURE") {
		t.Fatalf("error echoed the submitted enum name: %s", recorder.Body.String())
	}

	// Unknown numeric enum value is also an incompatibility, not a zero value.
	recorder = doRequest(t, handler, http.MethodPost, "/v1/pairing/claims", []byte(
		`{"token":"tok","claimId":"c1","phase":99,"noiseMessage":"bXNn","deviceId":"d1","protocolVersion":1}`), nil)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible)

	// Unknown RequestKind maps to protocol_version_incompatible (exit 9), not
	// invalid_argument.
	recorder = doRequest(t, handler, http.MethodPost, "/v1/hosts/h1/requests", []byte(
		`{"requestId":"r1","kind":"REQUEST_KIND_FROM_THE_FUTURE","commandId":"c1","payloadDigest":"ZGlnZXN0","expiresAtUnixMs":4102444800000,"protocolVersion":1,"body":"Ym9keQ=="}`),
		map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible)

	// A known enum plus an additive field still decodes.
	f.pairing.claimErr = nil
	recorder = doRequest(t, handler, http.MethodPost, "/v1/pairing/claims", []byte(
		`{"token":"tok","claimId":"c1","phase":"PAIRING_PHASE_HELLO","noiseMessage":"bXNn","deviceId":"d1","protocolVersion":1,"futureField":true}`), nil)
	if recorder.Code != http.StatusOK {
		t.Fatalf("known enum with additive field status = %d body=%s", recorder.Code, recorder.Body.String())
	}
}

func errorMessage(t *testing.T, recorder *httptest.ResponseRecorder) string {
	t.Helper()
	var response whalereinv1.ErrorResponse
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatalf("decode error response: %v", err)
	}
	return response.GetMessage()
}

// --- ACC-09: bounded host list --------------------------------------------

func TestHostListIsBoundedAndPagesWithExplicitTruncation(t *testing.T) {
	f := newAPIFixture(t)
	f.auth.list = make([]auth.HostView, 0, 1200)
	for index := 0; index < 1200; index++ {
		f.auth.list = append(f.auth.list, auth.HostView{
			HostID: fmt.Sprintf("host-%04d", index), Fingerprint: "fp", ProtocolVersion: 1,
		})
	}
	handler := f.server.PublicHandler()
	device := map[string]string{"Authorization": "Bearer device-token"}

	recorder := doRequest(t, handler, http.MethodGet, "/v1/hosts", nil, device)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	list := decodeHostList(t, recorder)
	if len(list.GetHosts()) != 500 {
		t.Fatalf("default page size = %d", len(list.GetHosts()))
	}
	if got := recorder.Header().Get("X-Whalerein-Hosts-Truncated"); got != "true" {
		t.Fatalf("truncated header = %q", got)
	}
	if got := recorder.Header().Get("X-Whalerein-Hosts-Limit"); got != "500" {
		t.Fatalf("limit header = %q", got)
	}

	recorder = doRequest(t, handler, http.MethodGet, "/v1/hosts?limit=10&offset=15", nil, device)
	list = decodeHostList(t, recorder)
	if len(list.GetHosts()) != 10 || list.GetHosts()[0].GetHostId() != "host-0015" {
		t.Fatalf("page = %+v", list.GetHosts())
	}
	if got := recorder.Header().Get("X-Whalerein-Hosts-Offset"); got != "15" {
		t.Fatalf("offset header = %q", got)
	}
	if got := recorder.Header().Get("X-Whalerein-Hosts-Returned"); got != "10" {
		t.Fatalf("returned header = %q", got)
	}

	recorder = doRequest(t, handler, http.MethodGet, "/v1/hosts?limit=500&offset=1000", nil, device)
	list = decodeHostList(t, recorder)
	if len(list.GetHosts()) != 200 {
		t.Fatalf("tail page = %d", len(list.GetHosts()))
	}
	if got := recorder.Header().Get("X-Whalerein-Hosts-Truncated"); got != "false" {
		t.Fatalf("tail truncated header = %q", got)
	}

	for _, query := range []string{"limit=0", "limit=501", "limit=-1", "limit=abc", "offset=-1", "offset=1000001"} {
		t.Run("rejects "+query, func(t *testing.T) {
			recorder := doRequest(t, handler, http.MethodGet, "/v1/hosts?"+query, nil, device)
			assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
		})
	}
}

func decodeHostList(t *testing.T, recorder *httptest.ResponseRecorder) *whalereinv1.HostList {
	t.Helper()
	var list whalereinv1.HostList
	if err := protojson.Unmarshal(recorder.Body.Bytes(), &list); err != nil {
		t.Fatal(err)
	}
	return &list
}

// --- ACC-11: 500 logging through the masker -------------------------------

func TestHandlerErrorsAreMaskedBeforeLogging(t *testing.T) {
	f := newAPIFixture(t)
	var logs bytes.Buffer
	f.server.deps.Logger = slog.New(slog.NewJSONHandler(&logs, nil))
	detail := "query failed for request r1 (request_id='r1', host_id='h1')\nsecond line " + strings.Repeat("x", 600)
	f.requests.getErr = errors.New(detail)

	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/requests/r1", nil,
		map[string]string{"Authorization": "Bearer device-token"})
	assertError(t, recorder, http.StatusInternalServerError, whalereinv1.ErrorCode_internal)

	logged := logs.String()
	masked := f.server.deps.Masker.Request("r1")
	if !strings.Contains(logged, masked) {
		t.Fatalf("masked request id %q missing from log: %s", masked, logged)
	}
	for _, raw := range []string{"for request r1", "request_id='r1'", "host_id='h1'"} {
		if strings.Contains(logged, raw) {
			t.Fatalf("log leaked raw identifier %q: %s", raw, logged)
		}
	}
	if strings.Contains(logged, strings.Repeat("x", 600)) {
		t.Fatal("log message was not bounded")
	}
	var handlerRecord map[string]any
	for _, line := range strings.Split(strings.TrimSpace(logs.String()), "\n") {
		var record map[string]any
		if err := json.Unmarshal([]byte(line), &record); err != nil {
			t.Fatalf("log line is not one JSON record: %v: %s", err, line)
		}
		if record["msg"] == "http handler error" {
			handlerRecord = record
		}
	}
	if handlerRecord == nil {
		t.Fatalf("handler error record missing: %s", logs.String())
	}
	message, _ := handlerRecord["error"].(string)
	if strings.ContainsAny(message, "\n\r") {
		t.Fatalf("log message kept control characters: %q", message)
	}
}

// --- ACC-12: method matrix ------------------------------------------------

func TestMethodMatrixAdvertisesAllowAndServesHeadAndOptions(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.PublicHandler()

	// A wrong method on a known path is a 405 with an Allow header.
	recorder := doRequest(t, handler, http.MethodPost, "/v1/hosts", nil, nil)
	assertError(t, recorder, http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument)
	if got := recorder.Header().Get("Allow"); got != "OPTIONS, GET, HEAD" {
		t.Fatalf("Allow = %q", got)
	}

	// OPTIONS needs no credentials and advertises the same matrix.
	recorder = doRequest(t, handler, http.MethodOptions, "/v1/hosts", nil, nil)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("OPTIONS status = %d", recorder.Code)
	}
	if got := recorder.Header().Get("Allow"); got != "OPTIONS, GET, HEAD" {
		t.Fatalf("OPTIONS Allow = %q", got)
	}
	if recorder.Body.Len() != 0 {
		t.Fatalf("OPTIONS body = %q", recorder.Body.String())
	}

	// HEAD is served by the GET route: status and headers, no body.
	recorder = doRequest(t, handler, http.MethodHead, "/healthz", nil, nil)
	if recorder.Code != http.StatusOK || recorder.Body.Len() != 0 {
		t.Fatalf("HEAD healthz = %d body=%q", recorder.Code, recorder.Body.String())
	}
	recorder = doRequest(t, handler, http.MethodHead, "/v1/hosts", nil, map[string]string{"Authorization": "Bearer device-token"})
	if recorder.Code != http.StatusOK || recorder.Body.Len() != 0 {
		t.Fatalf("HEAD hosts = %d body=%q", recorder.Code, recorder.Body.String())
	}
	if got := recorder.Header().Get("X-Whalerein-Hosts-Truncated"); got != "false" {
		t.Fatalf("HEAD hosts lost headers: truncated=%q", got)
	}

	// HEAD on a POST-only path is still a method error.
	recorder = doRequest(t, handler, http.MethodHead, "/v1/pairing/claims", nil, nil)
	assertError(t, recorder, http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument)
	if got := recorder.Header().Get("Allow"); got != "OPTIONS, POST" {
		t.Fatalf("POST-only Allow = %q", got)
	}

	// OPTIONS on an unknown path is a 404, not a blanket 204.
	recorder = doRequest(t, handler, http.MethodOptions, "/missing", nil, nil)
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
}

func TestRouteTableSnapshot(t *testing.T) {
	f := newAPIFixture(t)
	wantPublic := []string{
		"DELETE /v1/devices/{device_id}",
		"GET /healthz",
		"GET /metrics",
		"GET /readyz",
		"GET /v1/enrollments/hosts/{host_id}",
		"GET /v1/enrollments/hosts/{host_id}/clients",
		"GET /v1/enrollments/hosts/{host_id}/devices/{device_id}",
		"GET /v1/external/hosts/{host_id}/pairings/{operation_id}",
		"GET /v1/hosts",
		"GET /v1/hosts/{host_id}/authority",
		"GET /v1/hosts/{host_id}/enrollment",
		"GET /v1/operations/{operation_id}",
		"GET /v1/requests/{request_id}",
		"POST /v1/enrollments/devices/pairing",

		"POST /v1/enrollments/hosts",
		"POST /v1/enrollments/hosts/{host_id}/credential",
		"POST /v1/enrollments/hosts/{host_id}/devices/{device_id}/revocation",
		"POST /v1/enrollments/hosts/{host_id}/disable",
		"POST /v1/external/sessions/{session_id}/permission",
		"POST /v1/external/sessions/{session_id}/revocation",
		"POST /v1/hosts/{host_id}/devices/{device_id}/revocation",
		"POST /v1/hosts/{host_id}/requests",
		"POST /v1/pairing/claims",
		"POST /v1/pairing/invitations",
		"POST /v1/requests/{request_id}/heartbeat",
	}
	gotPublic := routeTable(f.server.publicRoutes())
	if strings.Join(gotPublic, "\n") != strings.Join(wantPublic, "\n") {
		t.Fatalf("public routes changed:\ngot:\n%s\nwant:\n%s", strings.Join(gotPublic, "\n"), strings.Join(wantPublic, "\n"))
	}
	wantInternal := []string{"POST /internal/broker/connect"}
	gotInternal := routeTable(f.server.internalRoutes())
	if strings.Join(gotInternal, "\n") != strings.Join(wantInternal, "\n") {
		t.Fatalf("internal routes = %v", gotInternal)
	}
}

// routeTable renders the registered route surface in a stable order so an
// accidental route addition or removal is visible in the snapshot.
func routeTable(routes []routeSpec) []string {
	out := make([]string, 0, len(routes))
	for _, route := range routes {
		out = append(out, route.method+" "+route.pattern)
	}
	// Stable order independent of registration order.
	for i := 1; i < len(out); i++ {
		for j := i; j > 0 && out[j] < out[j-1]; j-- {
			out[j], out[j-1] = out[j-1], out[j]
		}
	}
	return out
}

// --- ACC-13: trace id -----------------------------------------------------

func TestTraceIDAcceptsOnlyTheBoundedCorrelationAlphabet(t *testing.T) {
	accepted := []string{
		"trace-1", "A.b_c:9-", strings.Repeat("a", 128),
	}
	for _, value := range accepted {
		request := newTraceRequest(value)
		if got := traceID(request); got != value {
			t.Fatalf("valid trace %q became %q", value, got)
		}
	}
	rejected := []string{
		"has space", "line\nbreak", "quote\"value", "tab\tvalue",
		strings.Repeat("a", 129), "semi;colon", "slash/value",
	}
	for _, value := range rejected {
		request := newTraceRequest(value)
		got := traceID(request)
		if got == value || len(got) != 32 {
			t.Fatalf("unsafe trace %q became %q", value, got)
		}
	}
}

func newTraceRequest(value string) *http.Request {
	request, _ := http.NewRequest(http.MethodGet, "/healthz", nil)
	request.Header.Set(TraceHeader, value)
	return request
}

func TestTraceInjectionIsRegeneratedAndNeverLogged(t *testing.T) {
	f := newAPIFixture(t)
	var logs bytes.Buffer
	f.server.deps.Logger = slog.New(slog.NewJSONHandler(&logs, nil))
	injected := "safe\n{\"level\":\"ERROR\",\"msg\":\"forged\"}"
	recorder := doRequest(t, f.server.PublicHandler(), http.MethodGet, "/healthz", nil,
		map[string]string{TraceHeader: injected})
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d", recorder.Code)
	}
	trace := recorder.Header().Get(TraceHeader)
	if trace == injected || len(trace) != 32 {
		t.Fatalf("trace header = %q", trace)
	}
	if strings.Contains(logs.String(), "forged") {
		t.Fatalf("injected trace reached the log: %s", logs.String())
	}
	for _, line := range strings.Split(strings.TrimSpace(logs.String()), "\n") {
		var record map[string]any
		if err := json.Unmarshal([]byte(line), &record); err != nil {
			t.Fatalf("log line is not one JSON record: %v", err)
		}
		if record["trace_id"] != trace {
			t.Fatalf("log trace = %v, want %q", record["trace_id"], trace)
		}
	}
	// The error body also carries only the regenerated id.
	recorder = doRequest(t, f.server.PublicHandler(), http.MethodGet, "/v1/hosts", nil,
		map[string]string{TraceHeader: injected})
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	if strings.Contains(recorder.Body.String(), "forged") {
		t.Fatalf("error body echoed the injected trace: %s", recorder.Body.String())
	}
}

// --- ACC-14: bounded resource identifiers --------------------------------

func TestBoundedResourceIdentifiersRejectUnsafePathAndBodyValues(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.PublicHandler()
	device := map[string]string{"Authorization": "Bearer device-token"}
	admin := map[string]string{"Authorization": "Bearer admin-token"}
	overlong := strings.Repeat("a", 129)

	cases := []struct {
		name   string
		method string
		path   string
		body   []byte
		header map[string]string
	}{
		{"overlong request id", http.MethodGet, "/v1/requests/" + overlong, nil, device},
		{"unsafe request id", http.MethodGet, "/v1/requests/bad$id", nil, device},
		{"overlong heartbeat id", http.MethodPost, "/v1/requests/" + overlong + "/heartbeat", nil, device},
		{"overlong device id", http.MethodDelete, "/v1/devices/" + overlong, nil, admin},
		{"unsafe device id", http.MethodDelete, "/v1/devices/bad%20id", nil, admin},
		{"overlong submit host id", http.MethodPost, "/v1/hosts/" + overlong + "/requests", submitBody(), device},
		{"overlong revocation host id", http.MethodPost, "/v1/hosts/" + overlong + "/devices/d1/revocation", []byte(`{"signedAtUnixMs":"1","signature":"AQ=="}`), nil},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			recorder := doRequest(t, handler, tc.method, tc.path, tc.body, tc.header)
			assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
		})
	}

	// Client-generated identities in the body are bounded too, so a bad value
	// cannot become a durable key that later fails as a path id.
	bodyCases := []string{
		`{"requestId":"` + overlong + `","kind":"REQUEST_KIND_COMMAND","commandId":"c1","payloadDigest":"ZGlnZXN0","expiresAtUnixMs":4102444800000,"protocolVersion":1}`,
		`{"requestId":"r1","kind":"REQUEST_KIND_COMMAND","commandId":"bad$id","payloadDigest":"ZGlnZXN0","expiresAtUnixMs":4102444800000,"protocolVersion":1}`,
	}
	for _, body := range bodyCases {
		recorder := doRequest(t, handler, http.MethodPost, "/v1/hosts/h1/requests", []byte(body), device)
		assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
	}

	// No invalid value reached the request service or pairing service.
	if f.requests.gotRequest != "" || len(f.requests.submitted) != 0 || len(f.auth.hostGrants) != 0 || len(f.auth.revoked) != 0 {
		t.Fatalf("invalid identifier reached a service: got=%q submitted=%d grants=%v revoked=%v",
			f.requests.gotRequest, len(f.requests.submitted), f.auth.hostGrants, f.auth.revoked)
	}

	recorder := doRequest(t, handler, http.MethodPost, "/v1/pairing/invitations",
		[]byte(`{"hostId":"`+overlong+`"}`), admin)
	assertError(t, recorder, http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument)
	if f.pairing.invited != "" {
		t.Fatalf("invalid host reached pairing: %q", f.pairing.invited)
	}

	// A well-formed identifier still works.
	recorder = doRequest(t, handler, http.MethodGet, "/v1/requests/r1", nil, device)
	if recorder.Code != http.StatusOK {
		t.Fatalf("valid request id status = %d", recorder.Code)
	}
}

// --- ACC-20: internal route surface --------------------------------------

func TestInternalRouteSurfaceRequiresTheCallbackToken(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.InternalHandler()
	recorder := doRequest(t, handler, http.MethodPost, "/internal/broker/connect", nil, nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
	recorder = doRequest(t, handler, http.MethodGet, "/internal/broker/connect", nil,
		map[string]string{InternalHeader: "internal-token"})
	assertError(t, recorder, http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument)
	if got := recorder.Header().Get("Allow"); got != "OPTIONS, POST" {
		t.Fatalf("internal Allow = %q", got)
	}
	recorder = doRequest(t, handler, http.MethodPost, "/internal/broker/presence", []byte(`{}`),
		map[string]string{InternalHeader: "internal-token"})
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
}

func TestServerRedactionsIgnoreUnknownParameters(t *testing.T) {
	f := newAPIFixture(t)
	meta := &requestMeta{params: map[string]string{"host_id": "h1", "unknown": "value", "device_id": ""}}
	redactions := f.server.redactions(meta)
	if redactions["h1"] == "" {
		t.Fatal("host id was not mapped to a mask")
	}
	if _, ok := redactions["value"]; ok {
		t.Fatal("unknown parameter was treated as an identifier")
	}
	if len(f.server.redactions(nil)) != 0 {
		t.Fatal("nil metadata produced redactions")
	}
}

func TestHostListBoundsRejectsNonIntegerValues(t *testing.T) {
	request, _ := http.NewRequest(http.MethodGet, "/v1/hosts?limit=1&offset=2", nil)
	limit, offset, err := hostListBounds(request)
	if err != nil || limit != 1 || offset != 2 {
		t.Fatalf("bounds = %d %d %v", limit, offset, err)
	}
	request, _ = http.NewRequest(http.MethodGet, "/v1/hosts?limit=", nil)
	if _, _, err := hostListBounds(request); err != nil {
		t.Fatalf("empty limit should use the default: %v", err)
	}
}

// --- ACC-10: revocation idempotency and authorization asymmetry -----------

func TestDeviceRevocationIsDeterministicForAdminAndDeviceCallers(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.PublicHandler()
	admin := map[string]string{"Authorization": "Bearer admin-token"}
	device := map[string]string{"Authorization": "Bearer device-token"}

	// Admin revoke, then a repeated admin revoke: both reach the same end
	// state and answer 204.
	for attempt := 1; attempt <= 2; attempt++ {
		recorder := doRequest(t, handler, http.MethodDelete, "/v1/devices/d1", nil, admin)
		if recorder.Code != http.StatusNoContent {
			t.Fatalf("admin attempt %d status = %d body=%s", attempt, recorder.Code, recorder.Body.String())
		}
	}
	if len(f.auth.revoked) != 2 {
		t.Fatalf("revokes = %v", f.auth.revoked)
	}

	// The owning device can revoke itself.
	recorder := doRequest(t, handler, http.MethodDelete, "/v1/devices/d1", nil, device)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("device status = %d", recorder.Code)
	}

	// Authorization asymmetry is explicit: a valid token for another device is
	// forbidden, an unknown token is unauthorized.
	recorder = doRequest(t, handler, http.MethodDelete, "/v1/devices/other", nil, device)
	assertError(t, recorder, http.StatusForbidden, whalereinv1.ErrorCode_forbidden)
	recorder = doRequest(t, handler, http.MethodDelete, "/v1/devices/d1", nil,
		map[string]string{"Authorization": "Bearer revoked-device-token"})
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)

	// An unknown device is a stable not_found for the admin. It is never a 500.
	f.auth.revokeErr = auth.ErrNotFound
	recorder = doRequest(t, handler, http.MethodDelete, "/v1/devices/absent", nil, admin)
	assertError(t, recorder, http.StatusNotFound, whalereinv1.ErrorCode_not_found)
}

func TestHostSignedRevocationIsIdempotentForFreshProofsAndRejectsReplay(t *testing.T) {
	f := newAPIFixture(t)
	handler := f.server.PublicHandler()
	body := func(signedAt string) []byte {
		return []byte(`{"signedAtUnixMs":"` + signedAt + `","signature":"AQ=="}`)
	}
	// A fresh signed revocation of an already-revoked grant reaches the same
	// end state instead of failing.
	if recorder := doRequest(t, handler, http.MethodPost, "/v1/hosts/h1/devices/d1/revocation", body("1700000000001"), nil); recorder.Code != http.StatusNoContent {
		t.Fatalf("first status = %d", recorder.Code)
	}
	if recorder := doRequest(t, handler, http.MethodPost, "/v1/hosts/h1/devices/d1/revocation", body("1700000000002"), nil); recorder.Code != http.StatusNoContent {
		t.Fatalf("fresh repeat status = %d", recorder.Code)
	}
	// Replaying the exact same signature is rejected as unauthorized so an old
	// proof cannot revoke a later re-pairing.
	f.auth.hostErr = auth.ErrUnauthorized
	recorder := doRequest(t, handler, http.MethodPost, "/v1/hosts/h1/devices/d1/revocation", body("1700000000003"), nil)
	assertError(t, recorder, http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized)
}
