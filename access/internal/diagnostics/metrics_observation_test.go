package diagnostics

import (
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
)

// latency/AC-07.cardinality: Access stage aggregates keep a fixed stage and
// outcome vocabulary and declare their unit, population and observation mode.
func TestAccessStageMetricsBoundLabelsAndDeclarePopulation(t *testing.T) {
	metrics := NewMetrics()
	metrics.ObserveAccessStage("query_admit", 2*time.Millisecond, "ok")
	metrics.ObserveAccessStage("not-a-stage", time.Second, "not-an-outcome")
	metrics.ObserveAccessStageDisposition("query_expire", "expired")
	metrics.ObserveAccessStageBytes("relay_upload", 128)

	if got := testutil.ToFloat64(metrics.AccessStageTotal.WithLabelValues("query_admit", "ok")); got != 1 {
		t.Fatalf("query admit count = %v", got)
	}
	if got := testutil.ToFloat64(metrics.AccessStageTotal.WithLabelValues("query_admit", "error")); got != 1 {
		t.Fatalf("unknown stage/outcome did not normalize: %v", got)
	}
	if got := testutil.ToFloat64(metrics.AccessStageBytes.WithLabelValues("relay_upload")); got != 128 {
		t.Fatalf("relay upload bytes = %v", got)
	}
	if got := testutil.ToFloat64(metrics.AccessStageInfo.WithLabelValues(
		AccessStageMetricsVersion, "ms", "access-process-lifetime", "aggregate")); got != 1 {
		t.Fatalf("stage metric declaration = %v", got)
	}

	families, err := metrics.Registry.Gather()
	if err != nil {
		t.Fatal(err)
	}
	stages := map[string]bool{}
	for _, stage := range accessStages {
		stages[stage] = true
	}
	outcomes := map[string]bool{}
	for _, outcome := range accessOutcomes {
		outcomes[outcome] = true
	}
	found := false
	for _, family := range families {
		if family.GetName() != "access_stage_total" {
			continue
		}
		found = true
		for _, metric := range family.GetMetric() {
			labels := map[string]string{}
			for _, label := range metric.GetLabel() {
				labels[label.GetName()] = label.GetValue()
			}
			if !stages[labels["stage"]] || !outcomes[labels["outcome"]] {
				t.Fatalf("unbounded stage labels: %v", labels)
			}
			if _, ok := labels["request"]; ok {
				t.Fatal("identity leaked into a metric label")
			}
		}
	}
	if !found {
		t.Fatal("access_stage_total missing")
	}

	var nilMetrics *Metrics
	nilMetrics.ObserveAccessStage("query_admit", time.Second, "ok")
	nilMetrics.ObserveAccessStageDisposition("query_expire", "expired")
	nilMetrics.ObserveAccessStageBytes("relay_upload", 1)
}

// latency/AC-07.cardinality: a caller cannot grow histogram label cardinality
// with an unknown stage.
func TestAccessStageHistogramNormalizesUnknownStage(t *testing.T) {
	metrics := NewMetrics()
	metrics.ObserveAccessStage("made-up", time.Millisecond, "ok")
	families, err := metrics.Registry.Gather()
	if err != nil {
		t.Fatal(err)
	}
	for _, family := range families {
		if family.GetName() != "access_stage_duration_ms" {
			continue
		}
		for _, metric := range family.GetMetric() {
			for _, label := range metric.GetLabel() {
				if label.GetName() == "stage" && label.GetValue() == "made-up" {
					t.Fatal("unknown stage created a new series")
				}
			}
		}
	}
}
