package httpapi

import (
	"bytes"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/whalerein/whalerein/access/internal/clientaddr"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

func recordClientAddress(t *testing.T, resolver *clientaddr.Resolver,
	reference *clientaddr.Referencer, remote, header string) string {
	t.Helper()
	var logs bytes.Buffer
	server := &Server{deps: Dependencies{
		Logger:          slog.New(slog.NewJSONHandler(&logs, nil)),
		Metrics:         diagnostics.NewMetrics(),
		Masker:          diagnostics.NewMaskerWithKey([]byte("http")),
		ClientAddress:   resolver,
		ClientReference: reference,
	}}
	request := httptest.NewRequest(http.MethodGet, "/readyz", nil)
	request.RemoteAddr = remote
	if header != "" {
		request.Header.Set(clientaddr.ClientHeader, header)
	}
	server.observe(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	})).ServeHTTP(httptest.NewRecorder(), request)
	return logs.String()
}

func TestAccessRecordCarriesAWindowedReferenceNotTheAddress(t *testing.T) {
	resolver, err := clientaddr.NewResolver([]string{"172.18.0.0/16"})
	if err != nil {
		t.Fatal(err)
	}
	reference, err := clientaddr.NewReferencer(
		[]byte{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
			16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31},
		clientaddr.WindowMonthly)
	if err != nil {
		t.Fatal(err)
	}
	record := recordClientAddress(t, resolver, reference, "172.18.0.2:443", "203.0.113.7")
	for _, field := range []string{`"client_addr_status":"forwarded"`, `"client_ref":"c1_`, `"client_ref_window":"`} {
		if !strings.Contains(record, field) {
			t.Fatalf("record lacks %s: %s", field, record)
		}
	}
	if strings.Contains(record, "203.0.113.7") || strings.Contains(record, "172.18.0.2") {
		t.Fatalf("record leaked an address: %s", record)
	}
}

func TestAccessRecordTreatsADirectPeerAsTheClient(t *testing.T) {
	resolver, err := clientaddr.NewResolver(nil)
	if err != nil {
		t.Fatal(err)
	}
	record := recordClientAddress(t, resolver, nil, "198.51.100.9:40123", "203.0.113.7")
	if !strings.Contains(record, `"client_addr_status":"direct"`) {
		t.Fatalf("record = %s", record)
	}
	if strings.Contains(record, "198.51.100.9") || strings.Contains(record, "203.0.113.7") {
		t.Fatalf("record leaked an address: %s", record)
	}
}

func TestAccessRecordReportsAnUnavailableTrustedHeaderWithoutFallingBack(t *testing.T) {
	resolver, err := clientaddr.NewResolver([]string{"172.18.0.0/16"})
	if err != nil {
		t.Fatal(err)
	}
	record := recordClientAddress(t, resolver, nil, "172.18.0.2:443", "")
	if !strings.Contains(record, `"client_addr_status":"unavailable"`) {
		t.Fatalf("record = %s", record)
	}
	if strings.Contains(record, "172.18.0.2") || strings.Contains(record, `"client_ref"`) {
		t.Fatalf("record invented an identity: %s", record)
	}
}
