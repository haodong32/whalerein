// Package diagnostics provides structured logging, readiness checks, salted
// diagnostic identifiers and aggregate metrics for WhaleRein Access.
package diagnostics

import (
	"net/http"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

var latencyBuckets = []float64{1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000}

// Metrics is the complete first-version metric set. Labels never contain
// host, device, session, request or trace identifiers.
type Metrics struct {
	Registry    *prometheus.Registry
	HostResults *prometheus.CounterVec

	HTTPRequestDuration   prometheus.Histogram
	HTTPRequestErrors     *prometheus.CounterVec
	HostRequestDuration   prometheus.Histogram
	HostRequestErrors     prometheus.Counter
	MQTTRouteErrors       prometheus.Counter
	EventRouteDropped     prometheus.Counter
	ConnectedHosts        prometheus.Gauge
	ConnectedDevices      prometheus.Gauge
	RevocationLatency     prometheus.Histogram
	RequestForwardRetries prometheus.Counter
	ResultBoundRejected   prometheus.Counter
	ReleaseFenced         prometheus.Counter

	DevicePresenceEvents      *prometheus.CounterVec
	PresenceDuplicatesIgnored prometheus.Counter
	PresenceInvalidPayloads   prometheus.Counter
	DeviceOfflinePublishFail  prometheus.Counter
	ClientPollStale           prometheus.Counter

	AdmissionHits            prometheus.Counter
	AdmissionLoads           prometheus.Counter
	AdmissionLoadErrors      prometheus.Counter
	AdmissionDenied          prometheus.Counter
	AdmissionReconcileErrors prometheus.Counter

	// CleanupBatches counts bounded retention batches by operation. A full
	// batch leaves work behind, which CleanupBacklog reports so an operator can
	// alert on a backlog that outlives a long pause instead of discovering it
	// from unbounded storage growth.
	CleanupBatches      *prometheus.CounterVec
	CleanupRows         prometheus.Counter
	CleanupBacklogTotal prometheus.Counter
	CleanupLastRun      *prometheus.GaugeVec

	TransportRejections      *prometheus.CounterVec
	TransportRejectedBytes   *prometheus.CounterVec
	TransportFrames          *prometheus.CounterVec
	TransportBytes           *prometheus.CounterVec
	TransportIncoming        prometheus.Gauge
	TransportReserved        prometheus.Gauge
	TransportBudgetFallbacks *prometheus.CounterVec
}

// NewMetrics creates a registry with the Access metric set registered.
func NewMetrics() *Metrics {
	registry := prometheus.NewRegistry()
	m := &Metrics{
		Registry:    registry,
		HostResults: prometheus.NewCounterVec(prometheus.CounterOpts{Name: "host_results_total", Help: "Applied host results by bounded wire status and error code."}, []string{"status", "code"}),
		HTTPRequestDuration: prometheus.NewHistogram(prometheus.HistogramOpts{
			Name:    "http_request_duration_ms",
			Help:    "Public/internal HTTP request latency in milliseconds.",
			Buckets: latencyBuckets,
		}),
		HTTPRequestErrors: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "http_request_errors_total",
			Help: "HTTP requests that failed, labelled by stable error code.",
		}, []string{"code"}),
		HostRequestDuration: prometheus.NewHistogram(prometheus.HistogramOpts{
			Name:    "host_request_duration_ms",
			Help:    "HostRequest acceptance latency in milliseconds.",
			Buckets: latencyBuckets,
		}),
		HostRequestErrors: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "host_request_errors_total",
			Help: "HostRequest operations that failed.",
		}),
		MQTTRouteErrors: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "mqtt_route_errors_total",
			Help: "MQTT messages that could not be validated or routed.",
		}),
		EventRouteDropped: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "event_route_dropped_total",
			Help: "Events dropped because the device grant is missing or revoked.",
		}),
		ConnectedHosts: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "connected_hosts",
			Help: "Broker-reported connected hosts.",
		}),
		ConnectedDevices: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "connected_devices",
			Help: "Broker-reported connected devices.",
		}),
		RevocationLatency: prometheus.NewHistogram(prometheus.HistogramOpts{
			Name:    "revocation_latency_ms",
			Help:    "Time from revocation commit to host acknowledgement in milliseconds.",
			Buckets: latencyBuckets,
		}),
		RequestForwardRetries: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "request_forward_retries_total",
			Help: "Request forwarding attempts after the first attempt.",
		}),
		ResultBoundRejected: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "result_bound_rejected_total",
			Help: "Host result bodies rejected before storage because they exceeded a stored or current bound.",
		}),
		ReleaseFenced: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "request_release_fenced_total",
			Help: "Result GETs whose ciphertext was fenced by grant revisions or retention.",
		}),
		DevicePresenceEvents: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "device_presence_events_total",
			Help: "Accepted device presence events by bounded source and state.",
		}, []string{"source", "state"}),
		PresenceDuplicatesIgnored: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "presence_duplicates_ignored_total",
			Help: "Repeated device offline events ignored without a new callback.",
		}),
		PresenceInvalidPayloads: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "presence_invalid_payloads_total",
			Help: "Presence payloads or topics dropped as unknown or invalid.",
		}),
		DeviceOfflinePublishFail: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "device_offline_publish_failures_total",
			Help: "Device offline events that could not be published to a host.",
		}),
		ClientPollStale: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "request_client_stale_total",
			Help: "HTTP-polling clients that stopped renewing a non-terminal request claim.",
		}),
		AdmissionHits: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "grant_admission_hits_total",
			Help: "Realtime authorization decisions served from local grant state.",
		}),
		AdmissionLoads: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "grant_admission_loads_total",
			Help: "Cold grant loads against PostgreSQL.",
		}),
		AdmissionLoadErrors: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "grant_admission_load_errors_total",
			Help: "Cold grant loads that failed closed.",
		}),
		AdmissionDenied: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "grant_admission_denied_total",
			Help: "Realtime authorization decisions denied by missing, revoked or fenced grants.",
		}),
		AdmissionReconcileErrors: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "grant_admission_reconcile_errors_total",
			Help: "Grant reconciliation passes that could not read durable state.",
		}),
		CleanupBatches: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "cleanup_batches_total",
			Help: "Bounded retention batches by cleanup operation.",
		}, []string{"operation"}),
		CleanupRows: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "cleanup_rows_total",
			Help: "Rows cleared or deleted by retention cleanup.",
		}),
		CleanupBacklogTotal: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "cleanup_backlog_detected_total",
			Help: "Cleanup batches that filled their bound and left work behind.",
		}),
		CleanupLastRun: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Name: "cleanup_last_run_timestamp_seconds",
			Help: "Unix timestamp of the last completed cleanup pass by bounded operation.",
		}, []string{"operation"}),
		TransportRejections: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_rejections_total",
			Help: "Logical transport rejections by bounded stable code.",
		}, []string{"code"}),
		TransportRejectedBytes: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_rejected_bytes_total",
			Help: "Payload bytes seen by logical transport rejections by bounded stable code.",
		}, []string{"code"}),
		TransportFrames: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_frames_total",
			Help: "Completed logical transport transfers by direction.",
		}, []string{"direction"}),
		TransportBytes: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_bytes_total",
			Help: "Completed logical transport bytes by direction.",
		}, []string{"direction"}),
		TransportIncoming: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "transport_incoming_transfers",
			Help: "Inbound partial transfers currently holding a reassembly reservation.",
		}),
		TransportReserved: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "transport_reserved_bytes",
			Help: "Inbound payload plus per-fragment bookkeeping currently reserved.",
		}),
		TransportBudgetFallbacks: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_budget_fallbacks_total",
			Help: "Routes that used the local budget because an end-to-end peer advertisement is not observable by Access.",
		}, []string{"route"}),
	}
	registry.MustRegister(
		m.HostResults,
		m.HTTPRequestDuration,
		m.HTTPRequestErrors,
		m.HostRequestDuration,
		m.HostRequestErrors,
		m.MQTTRouteErrors,
		m.EventRouteDropped,
		m.ConnectedHosts,
		m.ConnectedDevices,
		m.RevocationLatency,
		m.RequestForwardRetries,
		m.ResultBoundRejected,
		m.ReleaseFenced,
		m.DevicePresenceEvents,
		m.PresenceDuplicatesIgnored,
		m.PresenceInvalidPayloads,
		m.DeviceOfflinePublishFail,
		m.ClientPollStale,
		m.AdmissionHits,
		m.AdmissionLoads,
		m.AdmissionLoadErrors,
		m.AdmissionDenied,
		m.AdmissionReconcileErrors,
		m.CleanupBatches,
		m.CleanupRows,
		m.CleanupBacklogTotal,
		m.CleanupLastRun,
		m.TransportRejections,
		m.TransportRejectedBytes,
		m.TransportFrames,
		m.TransportBytes,
		m.TransportIncoming,
		m.TransportReserved,
		m.TransportBudgetFallbacks,
	)
	return m
}

// Handler returns the Prometheus exposition handler.
func (m *Metrics) Handler() http.Handler {
	return promhttp.HandlerFor(m.Registry, promhttp.HandlerOpts{})
}

// ObserveHTTP records one public/internal HTTP request.
func (m *Metrics) ObserveHTTP(duration time.Duration, errorCode string) {
	if m == nil {
		return
	}
	m.HTTPRequestDuration.Observe(float64(duration.Milliseconds()))
	if errorCode != "" {
		m.HTTPRequestErrors.WithLabelValues(errorCode).Inc()
	}
}

// ObserveHostRequest records HostRequest acceptance latency and failure.
func (m *Metrics) ObserveHostRequest(duration time.Duration, failed bool) {
	if m == nil {
		return
	}
	m.HostRequestDuration.Observe(float64(duration.Milliseconds()))
	if failed {
		m.HostRequestErrors.Inc()
	}
}

// RouteError records an MQTT routing failure.
func (m *Metrics) RouteError() {
	if m != nil {
		m.MQTTRouteErrors.Inc()
	}
}

// EventDropped records an event dropped for a missing/revoked grant.
func (m *Metrics) EventDropped() {
	if m != nil {
		m.EventRouteDropped.Inc()
	}
}

// SetConnected updates broker-derived connection gauges.
func (m *Metrics) SetConnected(hosts, devices int) {
	if m == nil {
		return
	}
	m.ConnectedHosts.Set(float64(hosts))
	m.ConnectedDevices.Set(float64(devices))
}

// ObserveRevocation records the acknowledgement latency.
func (m *Metrics) ObserveRevocation(duration time.Duration) {
	if m == nil {
		return
	}
	m.RevocationLatency.Observe(float64(duration.Milliseconds()))
}

// ResultRejected records a host result body rejected before storage.
func (m *Metrics) ResultRejected() {
	if m != nil && m.ResultBoundRejected != nil {
		m.ResultBoundRejected.Inc()
	}
}

// ReleaseFenced records a ciphertext read denied by the release policy.
func (m *Metrics) ReleaseDenied() {
	if m != nil && m.ReleaseFenced != nil {
		m.ReleaseFenced.Inc()
	}
}

// PresenceEvent records one accepted broker/device presence event. Labels are
// bounded: an unknown source or state collapses to "unknown" so a malformed
// caller cannot grow metric cardinality.
func (m *Metrics) PresenceEvent(source, state string) {
	if m == nil {
		return
	}
	switch source {
	case "sys", "will", "broker":
	default:
		source = "unknown"
	}
	switch state {
	case "online", "offline":
	default:
		state = "unknown"
	}
	m.DevicePresenceEvents.WithLabelValues(source, state).Inc()
}

// PresenceDuplicate records a repeated device offline event.
func (m *Metrics) PresenceDuplicate() {
	if m != nil {
		m.PresenceDuplicatesIgnored.Inc()
	}
}

// PresenceInvalid records a presence topic or payload that was dropped.
func (m *Metrics) PresenceInvalid() {
	if m != nil {
		m.PresenceInvalidPayloads.Inc()
	}
}

// DeviceOfflinePublishFailed records a device offline event that did not reach
// every authorized host.
// RequestClaimStale records one request released after its HTTP client
// stopped polling.
func (m *Metrics) RequestClaimStale() {
	if m != nil && m.ClientPollStale != nil {
		m.ClientPollStale.Inc()
	}
}

func (m *Metrics) DeviceOfflinePublishFailed() {
	if m != nil {
		m.DeviceOfflinePublishFail.Inc()
	}
}

// ObserveHostResult counts host-level results separately from HTTP success.
func (m *Metrics) ObserveHostResult(status whalereinv1.OnlineStatus, code whalereinv1.ErrorCode) {
	if m == nil {
		return
	}
	if _, ok := whalereinv1.OnlineStatus_name[int32(status)]; !ok {
		status = whalereinv1.OnlineStatus(0)
	}
	if _, ok := whalereinv1.ErrorCode_name[int32(code)]; !ok {
		code = whalereinv1.ErrorCode_error_code_unspecified
	}
	m.HostResults.WithLabelValues(status.String(), code.String()).Inc()
}

// CleanupBatch records one bounded retention batch. operation is bounded to a
// fixed label set so a caller cannot grow metric cardinality.
func (m *Metrics) CleanupBatch(operation string, rows int) {
	if m == nil {
		return
	}
	switch operation {
	case "bodies", "tokens", "queries":
	default:
		operation = "unknown"
	}
	m.CleanupBatches.WithLabelValues(operation).Inc()
	if rows > 0 {
		m.CleanupRows.Add(float64(rows))
	}
}

// CleanupBacklog records that a cleanup batch filled its bound. It is the
// alert signal for a backlog that survives a long pause; it never substitutes
// for actually draining the work.
func (m *Metrics) CleanupBacklog() {
	if m != nil {
		m.CleanupBacklogTotal.Inc()
	}
}

// TransportRejected records one bounded transport rejection. code is restricted
// to the stable transport rejection vocabulary so a caller cannot grow metric
// cardinality; bytes and limit are aggregate sizes only.
func (m *Metrics) TransportRejected(code string, bytes, limit int64) {
	if m == nil || m.TransportRejections == nil {
		return
	}
	switch code {
	case "frame_packet", "frame_header", "frame_width", "frame_geometry",
		"envelope_budget", "fragment_count", "reassembly_budget", "frame_integrity",
		"frame_topic", "receipt", "bulk_admission", "budget_missing", "budget_invalid":
	default:
		code = "frame_header"
	}
	m.TransportRejections.WithLabelValues(code).Inc()
	if bytes > 0 {
		m.TransportRejectedBytes.WithLabelValues(code).Add(float64(bytes))
	}
	_ = limit
}

// TransportFrame counts one completed logical transfer.
func (m *Metrics) TransportFrame(direction string, bytes int64) {
	if m == nil || m.TransportFrames == nil {
		return
	}
	switch direction {
	case "in", "out":
	default:
		direction = "in"
	}
	m.TransportFrames.WithLabelValues(direction).Inc()
	if bytes > 0 {
		m.TransportBytes.WithLabelValues(direction).Add(float64(bytes))
	}
}

// TransportReservations publishes the current inbound reassembly reservation.
func (m *Metrics) TransportReservations(transfers int, reserved int64) {
	if m == nil || m.TransportIncoming == nil {
		return
	}
	m.TransportIncoming.Set(float64(transfers))
	if reserved < 0 {
		reserved = 0
	}
	m.TransportReserved.Set(float64(reserved))
}

// CleanupCompleted records the completion instant of one cleanup operation.
func (m *Metrics) CleanupCompleted(operation string) {
	if m == nil || m.CleanupLastRun == nil {
		return
	}
	switch operation {
	case "bodies", "tokens", "queries":
	default:
		operation = "unknown"
	}
	m.CleanupLastRun.WithLabelValues(operation).SetToCurrentTime()
}

// TransportBudgetFallback records a route that used the local budget because
// the peer receive advertisement is not observable by Access.
func (m *Metrics) TransportBudgetFallback(route string) {
	if m == nil || m.TransportBudgetFallbacks == nil {
		return
	}
	switch route {
	case "device":
	default:
		route = "device"
	}
	m.TransportBudgetFallbacks.WithLabelValues(route).Inc()
}
