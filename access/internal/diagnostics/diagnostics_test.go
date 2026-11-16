package diagnostics

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
	"unicode/utf8"

	"github.com/prometheus/client_golang/prometheus/testutil"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestMaskerIsStablePerSaltAndDiffersAcrossSalts(t *testing.T) {
	first := NewMaskerWithKey([]byte("key-one"))
	second := NewMaskerWithKey([]byte("key-two"))
	if first.Host("host-1") != first.Host("host-1") {
		t.Fatal("host mask not stable")
	}
	if first.Host("host-1") == second.Host("host-1") {
		t.Fatal("different keys produced the same mask")
	}
	if first.Host("host-1") == "host-1" || !strings.HasPrefix(first.Host("host-1"), "h-") {
		t.Fatalf("unexpected mask %q", first.Host("host-1"))
	}
	if first.Device("device-1") == first.Host("device-1") {
		t.Fatal("device and host prefixes must differ")
	}
	if first.Request("req") == "" || first.Request("req")[:2] != "r-" {
		t.Fatalf("request mask = %q", first.Request("req"))
	}
	if first.Host("") != "" {
		t.Fatal("empty identifier should produce empty mask")
	}
	var nilMasker *Masker
	if nilMasker.Host("x") != "" {
		t.Fatal("nil masker should be safe")
	}
}

func TestNewLoggerWritesJSONAtConfiguredLevel(t *testing.T) {
	var buffer bytes.Buffer
	logger := NewLogger("info", &buffer)
	logger.Debug("hidden debug")
	logger.Info("visible", "trace_id", "t-1", "status", 200)
	var record map[string]any
	if err := json.Unmarshal(buffer.Bytes(), &record); err != nil {
		t.Fatalf("log not JSON: %v: %s", err, buffer.String())
	}
	if record["msg"] != "visible" || record["service"] != "whalerein-access" {
		t.Fatalf("record = %v", record)
	}
	if strings.Contains(buffer.String(), "hidden debug") {
		t.Fatal("debug log emitted at info level")
	}
	buffer.Reset()
	debug := NewLogger("debug", &buffer)
	debug.Debug("shown")
	if !strings.Contains(buffer.String(), "shown") {
		t.Fatal("debug log missing at debug level")
	}
	for _, level := range []string{"warn", "error", "nonsense"} {
		if NewLogger(level, io.Discard) == nil {
			t.Fatalf("nil logger for %q", level)
		}
	}
}

func TestReadinessHandlersReflectDependencyHealth(t *testing.T) {
	readiness := NewReadiness()
	readiness.Add("database", func(context.Context) error { return nil })
	readiness.Add("broker", func(context.Context) error { return errors.New("down") })

	failures := readiness.Check(context.Background())
	if len(failures) != 1 || failures["broker"] != "unavailable" {
		t.Fatalf("failures = %v", failures)
	}

	recorder := httptest.NewRecorder()
	readiness.ReadyHandler().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/readyz", nil))
	if recorder.Code != http.StatusServiceUnavailable {
		t.Fatalf("status = %d", recorder.Code)
	}
	var body map[string]any
	if err := json.Unmarshal(recorder.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body["status"] != "unavailable" {
		t.Fatalf("body = %v", body)
	}
	if _, ok := body["checks"].(map[string]any)["broker"]; !ok {
		t.Fatalf("body = %v", body)
	}

	readiness.Add("broker", func(context.Context) error { return nil })
	recorder = httptest.NewRecorder()
	readiness.ReadyHandler().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/readyz", nil))
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), "ready") {
		t.Fatalf("status=%d body=%s", recorder.Code, recorder.Body.String())
	}

	recorder = httptest.NewRecorder()
	HealthHandler().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if recorder.Code != http.StatusOK || recorder.Body.String() != "ok\n" {
		t.Fatalf("health status=%d body=%q", recorder.Code, recorder.Body.String())
	}
	if got := recorder.Header().Get("Content-Type"); !strings.Contains(got, "text/plain") {
		t.Fatalf("content type = %q", got)
	}
}

func TestReadinessSkipsNilOptionalCheck(t *testing.T) {
	readiness := NewReadiness()
	readiness.Add("nil", nil)
	if failures := readiness.Check(context.Background()); len(failures) != 0 {
		t.Fatalf("failures = %v", failures)
	}
}

func TestMetricsExposeExpectedSeriesWithoutIdentityLabels(t *testing.T) {
	metrics := NewMetrics()
	metrics.ObserveHTTP(25*time.Millisecond, "invalid_argument")
	metrics.ObserveHTTP(5*time.Millisecond, "")
	metrics.ObserveHostRequest(10*time.Millisecond, true)
	metrics.ObserveHostRequest(2*time.Millisecond, false)
	metrics.RouteError()
	metrics.EventDropped()
	metrics.SetConnected(2, 3)
	metrics.ObserveRevocation(100 * time.Millisecond)
	metrics.PresenceEvent("sys", "offline")
	metrics.PresenceEvent("will", "online")
	metrics.PresenceEvent("unexpected", "unexpected")
	metrics.PresenceDuplicate()
	metrics.PresenceInvalid()
	metrics.DeviceOfflinePublishFailed()

	if value := testutil.ToFloat64(metrics.HTTPRequestErrors.WithLabelValues("invalid_argument")); value != 1 {
		t.Fatalf("http errors = %v", value)
	}
	if value := testutil.ToFloat64(metrics.ConnectedHosts); value != 2 {
		t.Fatalf("hosts = %v", value)
	}
	if value := testutil.ToFloat64(metrics.ConnectedDevices); value != 3 {
		t.Fatalf("devices = %v", value)
	}
	if count := testutil.CollectAndCount(metrics.RevocationLatency); count != 1 {
		t.Fatalf("revocation metric count = %d", count)
	}
	if value := testutil.ToFloat64(metrics.DevicePresenceEvents.WithLabelValues("sys", "offline")); value != 1 {
		t.Fatalf("sys offline presence = %v", value)
	}
	if value := testutil.ToFloat64(metrics.DevicePresenceEvents.WithLabelValues("will", "online")); value != 1 {
		t.Fatalf("will online presence = %v", value)
	}
	if value := testutil.ToFloat64(metrics.DevicePresenceEvents.WithLabelValues("unknown", "unknown")); value != 1 {
		t.Fatalf("unknown presence = %v", value)
	}
	if value := testutil.ToFloat64(metrics.PresenceDuplicatesIgnored); value != 1 {
		t.Fatalf("duplicates = %v", value)
	}
	if value := testutil.ToFloat64(metrics.PresenceInvalidPayloads); value != 1 {
		t.Fatalf("invalid payloads = %v", value)
	}
	if value := testutil.ToFloat64(metrics.DeviceOfflinePublishFail); value != 1 {
		t.Fatalf("publish failures = %v", value)
	}

	families, err := metrics.Registry.Gather()
	if err != nil {
		t.Fatal(err)
	}
	names := map[string]bool{}
	for _, family := range families {
		names[family.GetName()] = true
		for _, metric := range family.GetMetric() {
			for _, label := range metric.GetLabel() {
				switch label.GetName() {
				case "host_id", "device_id", "session_id", "trace_id":
					t.Fatalf("forbidden label %q", label.GetName())
				}
			}
		}
	}
	for _, want := range []string{
		"http_request_duration_ms", "http_request_errors_total", "host_request_duration_ms",
		"host_request_errors_total", "mqtt_route_errors_total", "connected_hosts",
		"connected_devices", "revocation_latency_ms",
	} {
		if !names[want] {
			t.Fatalf("missing metric %q in %v", want, names)
		}
	}

	recorder := httptest.NewRecorder()
	metrics.Handler().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/metrics", nil))
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), "connected_hosts 2") {
		t.Fatalf("metrics status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

func TestNilMetricsAcceptObservationsSafely(t *testing.T) {
	var metrics *Metrics
	metrics.ObserveHTTP(time.Second, "internal")
	metrics.ObserveHostRequest(time.Second, true)
	metrics.RouteError()
	metrics.EventDropped()
	metrics.SetConnected(1, 1)
	metrics.ObserveRevocation(time.Second)
}

func TestMetricsHistogramRecordsObservedRequestDurations(t *testing.T) {
	metrics := NewMetrics()
	metrics.ObserveHTTP(1500*time.Millisecond, "")
	families, err := metrics.Registry.Gather()
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, family := range families {
		if family.GetName() != "http_request_duration_ms" {
			continue
		}
		found = true
		metric := family.GetMetric()[0]
		if metric.GetHistogram().GetSampleCount() != 1 {
			t.Fatalf("sample count = %d", metric.GetHistogram().GetSampleCount())
		}
	}
	if !found {
		t.Fatal("http histogram missing")
	}
}

func TestLoggerRedactsRequestAndResultBodies(t *testing.T) {
	var buffer bytes.Buffer
	logger := slog.New(slog.NewJSONHandler(&buffer, nil))
	logger.Info("status", "request_id", "r-1", "status", 200)
	if strings.Contains(buffer.String(), "secret") {
		t.Fatal("unexpected content")
	}
}

func TestTraceContextCorrelatesHostResultMetrics(t *testing.T) {
	if TraceID(context.Background()) != "" {
		t.Fatal("unexpected trace")
	}
	ctx := WithTraceID(context.Background(), "operation-1")
	if TraceID(ctx) != "operation-1" {
		t.Fatal("trace not carried")
	}
	metrics := NewMetrics()
	metrics.ObserveHostResult(whalereinv1.OnlineStatus_rejected, whalereinv1.ErrorCode_unauthorized)
	metrics.ObserveHostResult(whalereinv1.OnlineStatus(999), whalereinv1.ErrorCode(999))
	if got := testutil.ToFloat64(metrics.HostResults.WithLabelValues("rejected", "unauthorized")); got != 1 {
		t.Fatal("host rejection was not counted")
	}
	var absent *Metrics
	absent.ObserveHostResult(0, 0)
	absent.PresenceEvent("sys", "offline")
	absent.PresenceDuplicate()
	absent.PresenceInvalid()
	absent.DeviceOfflinePublishFailed()
}

func TestMaskerRedactMasksKnownIdentifiersAndSQLValues(t *testing.T) {
	masker := NewMaskerWithKey([]byte("redact-key"))
	text := "query failed for host h1 and device d1 (request_id='r1')\nsecond line"
	got := masker.Redact(text, map[string]string{
		"h1": masker.Host("h1"),
		"d1": masker.Device("d1"),
		"r1": masker.Request("r1"),
	})
	if strings.Contains(got, "h1") || strings.Contains(got, "d1") || strings.Contains(got, "r1") {
		t.Fatalf("raw identifier survived: %q", got)
	}
	if !strings.Contains(got, masker.Host("h1")) || !strings.Contains(got, masker.Device("d1")) {
		t.Fatalf("masked identifier missing: %q", got)
	}
	if strings.Contains(got, "'r1'") || !strings.Contains(got, "'?'") {
		t.Fatalf("SQL literal was not redacted: %q", got)
	}
	if strings.ContainsAny(got, "\n\r\t") {
		t.Fatalf("control characters survived: %q", got)
	}
	var nilMasker *Masker
	if string(nilMasker.Redact("plain", nil)) != "plain" {
		t.Fatal("nil masker should pass text through")
	}
}

func TestMaskerRedactBoundsLongMessages(t *testing.T) {
	masker := NewMaskerWithKey([]byte("redact-key"))
	got := masker.Redact(strings.Repeat("x", MaxRedactedLength+100), nil)
	if len(got) > MaxRedactedLength+4 {
		t.Fatalf("message length = %d", len(got))
	}
	if !strings.HasSuffix(got, "…") {
		t.Fatal("truncation marker missing")
	}
	// Multi-byte content is truncated at a rune boundary.
	multi := masker.Redact(strings.Repeat("é", MaxRedactedLength), nil)
	if !utf8.ValidString(multi) {
		t.Fatalf("truncation split a rune: %q", multi)
	}
}

type fakeDatabase struct {
	pingErr  error
	probeErr error
	probes   int
}

func (f *fakeDatabase) Ping(context.Context) error { return f.pingErr }
func (f *fakeDatabase) WriteProbe(context.Context) error {
	f.probes++
	return f.probeErr
}

func TestDatabaseReadinessRequiresAPingAndAWriteProbe(t *testing.T) {
	healthy := &fakeDatabase{}
	if err := DatabaseReadiness(healthy)(context.Background()); err != nil || healthy.probes != 1 {
		t.Fatalf("healthy database = %v probes=%d", err, healthy.probes)
	}
	pingDown := &fakeDatabase{pingErr: errors.New("connection refused")}
	if err := DatabaseReadiness(pingDown)(context.Background()); err == nil {
		t.Fatal("ping failure was not reported")
	}
	if pingDown.probes != 0 {
		t.Fatalf("write probe ran after a failed ping: %d", pingDown.probes)
	}
	readOnly := &fakeDatabase{probeErr: errors.New("read-only transaction")}
	if err := DatabaseReadiness(readOnly)(context.Background()); err == nil {
		t.Fatal("read-only database was reported ready")
	}
}

func TestCleanupMetricsRecordBatchesAndBacklog(t *testing.T) {
	metrics := NewMetrics()
	metrics.CleanupBatch("bodies", 2000)
	metrics.CleanupBatch("queries", 10)
	metrics.CleanupBatch("tokens", 0)
	metrics.CleanupBatch("unexpected", 1)
	metrics.CleanupBacklog()
	if got := testutil.ToFloat64(metrics.CleanupBatches.WithLabelValues("bodies")); got != 1 {
		t.Fatalf("bodies batches = %v", got)
	}
	if got := testutil.ToFloat64(metrics.CleanupBatches.WithLabelValues("unknown")); got != 1 {
		t.Fatalf("unknown batches = %v", got)
	}
	if got := testutil.ToFloat64(metrics.CleanupRows); got != 2011 {
		t.Fatalf("cleanup rows = %v", got)
	}
	if got := testutil.ToFloat64(metrics.CleanupBacklogTotal); got != 1 {
		t.Fatalf("cleanup backlog = %v", got)
	}
	var nilMetrics *Metrics
	nilMetrics.CleanupBatch("bodies", 1)
	nilMetrics.CleanupBacklog()
}
