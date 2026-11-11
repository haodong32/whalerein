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
}
