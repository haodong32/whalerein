package diagnostics

import (
	"testing"

	"github.com/prometheus/client_golang/prometheus/testutil"
)

// Payload metrics expose aggregates and bounded labels only: no object id,
// path, content or endpoint credential.
func TestPayloadMetricsPublishAggregatesAndBoundLabels(t *testing.T) {
	metrics := NewMetrics()
	metrics.PayloadStorage(1000, 400, 600, 50, 25, 3, 2, 1, 12.5)
	if got := testutil.ToFloat64(metrics.PayloadFreeBytes); got != 1000 {
		t.Fatalf("free bytes = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadHeadroomBytes); got != 400 {
		t.Fatalf("headroom = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadReadyBytes); got != 600 {
		t.Fatalf("ready bytes = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadReservedBytes); got != 50 {
		t.Fatalf("reserved bytes = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadExpiredBytes); got != 25 {
		t.Fatalf("expired bytes = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadObjects.WithLabelValues("ready")); got != 3 {
		t.Fatalf("ready objects = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadObjects.WithLabelValues("reserved")); got != 2 {
		t.Fatalf("reserved objects = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadActiveUploads); got != 1 {
		t.Fatalf("active uploads = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadExpiredBacklog); got != 12.5 {
		t.Fatalf("backlog = %v", got)
	}

	metrics.PayloadStorage(-1, -1, -1, -1, -1, 0, 0, 0, 0)
	if got := testutil.ToFloat64(metrics.PayloadFreeBytes); got != 0 {
		t.Fatalf("negative free bytes were not clamped: %v", got)
	}

	metrics.PayloadRefused("size")
	metrics.PayloadRefused("not-a-reason")
	if got := testutil.ToFloat64(metrics.PayloadAdmissionRefusals.WithLabelValues("size")); got != 1 {
		t.Fatalf("size refusals = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadAdmissionRefusals.WithLabelValues("capacity")); got != 1 {
		t.Fatalf("unknown refusal did not collapse: %v", got)
	}

	metrics.PayloadCleanup(2, 512, 1)
	metrics.PayloadCleanup(0, 0, 0)
	if got := testutil.ToFloat64(metrics.PayloadCleanupDeleted); got != 2 {
		t.Fatalf("deleted = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadCleanupReclaimed); got != 512 {
		t.Fatalf("reclaimed = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadCleanupRetries); got != 1 {
		t.Fatalf("retries = %v", got)
	}

	metrics.PayloadReconcile("repairs", 3)
	metrics.PayloadReconcile("not-an-action", 1)
	metrics.PayloadReconcile("orphans", 0)
	if got := testutil.ToFloat64(metrics.PayloadReconcileActions.WithLabelValues("repairs")); got != 4 {
		t.Fatalf("repairs = %v", got)
	}
	if got := testutil.ToFloat64(metrics.PayloadReconcileActions.WithLabelValues("orphans")); got != 0 {
		t.Fatalf("zero orphan count was recorded: %v", got)
	}
}

// Nil metrics stay safe so a partially wired worker cannot panic.
func TestPayloadMetricsAreNilSafe(t *testing.T) {
	var metrics *Metrics
	metrics.PayloadStorage(1, 2, 3, 4, 5, 6, 7, 8, 9)
	metrics.PayloadRefused("size")
	metrics.PayloadCleanup(1, 1, 1)
	metrics.PayloadReconcile("repairs", 1)
}
