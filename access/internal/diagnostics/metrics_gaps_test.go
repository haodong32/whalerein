package diagnostics

import (
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus/testutil"
)

// Bounded label normalization keeps transport and retention series finite even
// when a caller passes an unknown code, direction, operation or route.
func TestTransportAndCleanupMetricsBoundTheirLabels(t *testing.T) {
	metrics := NewMetrics()
	metrics.TransportRejected("packet_limit", 10, 100)
	metrics.TransportRejected("not-a-code", 0, 0)
	metrics.TransportPacket("inline", 2048)
	metrics.TransportPacket("reference", 0)
	metrics.TransportPacket("sideways", 5)
	metrics.TransportQueue("request", 2)
	metrics.TransportQueue("not-a-flow", -1)
	metrics.TransportInFlightCount("event", 1)
	metrics.TransportInFlightCount("not-a-flow", -1)
	metrics.CleanupCompleted("bodies")
	metrics.CleanupCompleted("not-an-operation")
	metrics.TransportBudgetFallback("device")
	metrics.TransportBudgetFallback("not-a-route")

	for code, want := range map[string]float64{
		"packet_limit": 1,
		"frame_header": 1, // unknown codes collapse to the header code
	} {
		if got := testutil.ToFloat64(metrics.TransportRejections.WithLabelValues(code)); got != want {
			t.Fatalf("rejections %q = %v, want %v", code, got, want)
		}
	}
	if got := testutil.ToFloat64(metrics.TransportRejectedBytes.WithLabelValues("packet_limit")); got != 10 {
		t.Fatalf("rejected bytes = %v", got)
	}
	if got := testutil.ToFloat64(metrics.TransportPackets.WithLabelValues("inline")); got != 2 {
		t.Fatalf("inline packets = %v", got)
	}
	if got := testutil.ToFloat64(metrics.TransportPackets.WithLabelValues("reference")); got != 1 {
		t.Fatalf("unknown representation did not normalize: %v", got)
	}
	if got := testutil.ToFloat64(metrics.TransportPacketBytes.WithLabelValues("inline")); got != 2053 {
		t.Fatalf("inline bytes = %v", got)
	}
	if got := testutil.ToFloat64(metrics.TransportQueueDepth.WithLabelValues("request")); got != 2 {
		t.Fatalf("queue depth = %v", got)
	}
	if got := testutil.ToFloat64(metrics.TransportQueueDepth.WithLabelValues("other")); got != 0 {
		t.Fatalf("negative queue depth was not clamped: %v", got)
	}
	if got := testutil.ToFloat64(metrics.TransportInFlightWork.WithLabelValues("event")); got != 1 {
		t.Fatalf("in flight = %v", got)
	}
	if got := testutil.ToFloat64(metrics.CleanupLastRun.WithLabelValues("bodies")); got <= 0 {
		t.Fatalf("cleanup completion = %v", got)
	}
	if got := testutil.ToFloat64(metrics.CleanupLastRun.WithLabelValues("unknown")); got <= 0 {
		t.Fatalf("unknown cleanup completion = %v", got)
	}
	if got := testutil.ToFloat64(metrics.TransportBudgetFallbacks.WithLabelValues("device")); got != 2 {
		t.Fatalf("device fallback = %v", got)
	}
}

// Result and release counters are the alerting signals for the new fencing
// paths, so each increments exactly once per denied outcome.
func TestGrantFencingMetricsCountDenials(t *testing.T) {
	metrics := NewMetrics()
	metrics.ResultRejected()
	metrics.ResultRejected()
	metrics.ReleaseDenied()
	metrics.RequestClaimStale()
	if got := testutil.ToFloat64(metrics.ResultBoundRejected); got != 2 {
		t.Fatalf("result rejections = %v", got)
	}
	if got := testutil.ToFloat64(metrics.ReleaseFenced); got != 1 {
		t.Fatalf("release denials = %v", got)
	}
	if got := testutil.ToFloat64(metrics.ClientPollStale); got != 1 {
		t.Fatalf("stale claim releases = %v", got)
	}
}

// A partially initialized Metrics value must not panic when a bounded
// observation arrives, matching the existing nil-receiver contract.
func TestZeroValueMetricsIgnoreUninitializedSubsystems(t *testing.T) {
	var metrics Metrics
	metrics.ResultRejected()
	metrics.ReleaseDenied()
	metrics.RequestClaimStale()
	metrics.TransportRejected("packet_limit", 1, 1)
	metrics.TransportPacket("inline", 1)
	metrics.TransportQueue("request", 1)
	metrics.TransportInFlightCount("request", 1)
	metrics.CleanupCompleted("bodies")
	metrics.TransportBudgetFallback("device")

	var nilMetrics *Metrics
	nilMetrics.ResultRejected()
	nilMetrics.ReleaseDenied()
	nilMetrics.RequestClaimStale()
	nilMetrics.TransportRejected("frame_header", 0, 0)
	nilMetrics.TransportPacket("reference", 0)
	nilMetrics.TransportQueue("request", 0)
	nilMetrics.TransportInFlightCount("request", 0)
	nilMetrics.CleanupCompleted("tokens")
	nilMetrics.TransportBudgetFallback("device")
}

// Retention operations outside the fixed vocabulary are counted as unknown
// rather than creating an unbounded label.
func TestCleanupBatchNormalizesUnknownOperation(t *testing.T) {
	metrics := NewMetrics()
	metrics.CleanupBatch("bodies", 2)
	metrics.CleanupBatch("not-an-operation", 0)
	if got := testutil.ToFloat64(metrics.CleanupBatches.WithLabelValues("bodies")); got != 1 {
		t.Fatalf("body batches = %v", got)
	}
	if got := testutil.ToFloat64(metrics.CleanupBatches.WithLabelValues("unknown")); got != 1 {
		t.Fatalf("unknown batches = %v", got)
	}
	var nilMetrics *Metrics
	nilMetrics.CleanupBatch("queries", 1)
}

// Redaction must keep unknown diagnostic text but replace configured
// identifiers, quoted SQL values and control characters.
func TestRedactReplacesIdentifiersAndControls(t *testing.T) {
	masker := NewMaskerWithKey([]byte("redaction-test"))
	text := "host host-1 device device-1 failed at 'secret value'\nline\x00hidden\tend"
	redacted := masker.Redact(text, map[string]string{
		"host-1":   masker.Host("host-1"),
		"device-1": masker.Device("device-1"),
		"":         "ignored",
		"unused":   "",
	})
	for _, raw := range []string{"host-1", "device-1", "secret value", "\x00"} {
		if strings.Contains(redacted, raw) {
			t.Fatalf("redacted text still contains %q: %q", raw, redacted)
		}
	}
	if !strings.Contains(redacted, masker.Host("host-1")) || !strings.Contains(redacted, "'?'") {
		t.Fatalf("redacted text lost diagnostic detail: %q", redacted)
	}
	if strings.ContainsAny(redacted, "\n\t") {
		t.Fatalf("control separators survived: %q", redacted)
	}
}

func TestRedactBoundsLongMessages(t *testing.T) {
	masker := NewMaskerWithKey([]byte("bounds-test"))
	redacted := masker.Redact(strings.Repeat("x", MaxRedactedLength*2), nil)
	if runes := []rune(redacted); len(runes) != MaxRedactedLength+1 || runes[len(runes)-1] != '…' {
		t.Fatalf("bounded length = %d", len(runes))
	}
}
