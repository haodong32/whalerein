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

// AccessStageMetricsVersion versions the units and bounds of the Access
// observation-stage aggregates. The stage vocabulary is fixed; adding a stage
// or changing a bucket requires a new version so local trends stay comparable.
const AccessStageMetricsVersion = "1"

// accessStageBuckets bound Access-owned stage durations in milliseconds. They
// cover sub-millisecond admission through a slow multi-second object transfer.
var accessStageBuckets = []float64{0.1, 0.25, 0.5, 1, 2.5, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000}

// accessStages is the fixed stage vocabulary. No request, device, object or
// connection identifier is ever a label.
var accessStages = []string{
	"query_admit", "query_forward", "query_reply", "query_expire",
	"command_admit", "command_receipt", "command_recover", "command_terminal",
	"relay_stage", "relay_upload", "relay_finish", "relay_fetch", "relay_verify",
	"notice_subscribe", "notice_ready", "notice_disconnect",
}

var accessOutcomes = []string{"ok", "error", "expired", "late", "timeout", "failed", "replaced"}

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
	LiveDrops             *prometheus.CounterVec
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

	// ControlLastSuccess is the Unix time of the last accepted Central control
	// message. Zero means Central has never confirmed this node.
	ControlLastSuccess prometheus.Gauge
	RouteAttach        *prometheus.CounterVec
	RouteClose         *prometheus.CounterVec

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
	TransportPackets         *prometheus.CounterVec
	TransportPacketBytes     *prometheus.CounterVec
	TransportQueueDepth      *prometheus.GaugeVec
	TransportInFlightWork    *prometheus.GaugeVec
	TransportBudgetFallbacks *prometheus.CounterVec

	// Temporary ciphertext storage. Every label is a bounded enum and no
	// object id, path, content or credential is ever attached.
	PayloadFreeBytes         prometheus.Gauge
	PayloadHeadroomBytes     prometheus.Gauge
	PayloadReadyBytes        prometheus.Gauge
	PayloadReservedBytes     prometheus.Gauge
	PayloadExpiredBytes      prometheus.Gauge
	PayloadObjects           *prometheus.GaugeVec
	PayloadActiveUploads     prometheus.Gauge
	PayloadExpiredBacklog    prometheus.Gauge
	PayloadAdmissionRefusals *prometheus.CounterVec
	PayloadCleanupDeleted    prometheus.Counter
	PayloadCleanupReclaimed  prometheus.Counter
	PayloadCleanupRetries    prometheus.Counter
	PayloadReconcileActions  *prometheus.CounterVec

	// Durable request-notice intent. Only aggregate counts and ages are
	// exposed; no request, command or content metadata is attached.
	NoticePending        prometheus.Gauge
	NoticeOldestAge      prometheus.Gauge
	NoticePublishRetries prometheus.Counter

	// Access observation-stage aggregates. These are ordinary process-lifetime
	// counters and histograms available in every bench observation mode; they
	// describe the aggregate population, not matched per-operation events.
	// They reset when the Access process restarts.
	AccessStageDuration *prometheus.HistogramVec
	AccessStageTotal    *prometheus.CounterVec
	AccessStageBytes    *prometheus.CounterVec
	AccessStageInfo     *prometheus.GaugeVec
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
		LiveDrops: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "live_preview_dropped_total",
			Help: "QoS 0 live previews dropped by bounded reason.",
		}, []string{"reason"}),
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
		ControlLastSuccess: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "access_control_last_success_unix_seconds",
			Help: "Unix time of the last accepted Central node control message; zero means never.",
		}),
		RouteAttach: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "access_route_attach_total",
			Help: "Host route attachment attempts by accepted outcome.",
		}, []string{"outcome"}),
		RouteClose: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "access_route_close_total",
			Help: "Graceful Host route close attempts by accepted outcome.",
		}, []string{"outcome"}),
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
		TransportPackets: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_packets_total",
			Help: "Complete MQTT packets published by representation.",
		}, []string{"representation"}),
		TransportPacketBytes: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_packet_bytes_total",
			Help: "Complete encoded message bytes published by representation.",
		}, []string{"representation"}),
		TransportQueueDepth: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Name: "transport_queue_depth",
			Help: "Ordered publication queue depth by bounded destination flow.",
		}, []string{"flow"}),
		TransportInFlightWork: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Name: "transport_in_flight",
			Help: "Ordered publication flows with one publication in flight.",
		}, []string{"flow"}),
		TransportBudgetFallbacks: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "transport_budget_fallbacks_total",
			Help: "Routes that used the local budget because an end-to-end peer advertisement is not observable by Access.",
		}, []string{"route"}),
		PayloadFreeBytes: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "payload_free_bytes",
			Help: "Filesystem bytes available on the temporary ciphertext quota volume.",
		}),
		PayloadHeadroomBytes: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "payload_headroom_bytes",
			Help: "Free bytes above the configured reserve and outstanding reserved writes.",
		}),
		PayloadReadyBytes: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "payload_ready_bytes",
			Help: "Charged bytes of complete, readable temporary ciphertext objects.",
		}),
		PayloadReservedBytes: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "payload_reserved_bytes",
			Help: "Charged bytes of partial uploads that are not readable.",
		}),
		PayloadExpiredBytes: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "payload_expired_bytes",
			Help: "Charged bytes of expired objects still awaiting file deletion.",
		}),
		PayloadObjects: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Name: "payload_objects",
			Help: "Temporary ciphertext metadata rows by bounded lifecycle state.",
		}, []string{"state"}),
		PayloadActiveUploads: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "payload_active_uploads",
			Help: "Temporary ciphertext reservations currently accepting an upload.",
		}),
		PayloadExpiredBacklog: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "payload_expired_backlog_seconds",
			Help: "Age in seconds of the oldest expired object still occupying capacity.",
		}),
		PayloadAdmissionRefusals: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "payload_admission_refusals_total",
			Help: "Temporary ciphertext admission refusals by bounded reason.",
		}, []string{"reason"}),
		PayloadCleanupDeleted: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "payload_cleanup_deleted_total",
			Help: "Temporary ciphertext files deleted and capacity released.",
		}),
		PayloadCleanupReclaimed: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "payload_cleanup_reclaimed_bytes_total",
			Help: "Temporary ciphertext capacity released after file deletion.",
		}),
		PayloadCleanupRetries: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "payload_cleanup_retries_total",
			Help: "Temporary ciphertext cleanup attempts that failed and stayed charged.",
		}),
		PayloadReconcileActions: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "payload_reconcile_actions_total",
			Help: "Temporary ciphertext reconciliation outcomes by bounded action.",
		}, []string{"action"}),
		NoticePending: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "request_notices_pending",
			Help: "Durable request state notices waiting for publication.",
		}),
		NoticeOldestAge: prometheus.NewGauge(prometheus.GaugeOpts{
			Name: "request_notices_oldest_age_seconds",
			Help: "Age in seconds of the oldest pending request state notice.",
		}),
		NoticePublishRetries: prometheus.NewCounter(prometheus.CounterOpts{
			Name: "request_notice_publish_retries_total",
			Help: "Request state notice publications that failed and will retry.",
		}),
		AccessStageDuration: prometheus.NewHistogramVec(prometheus.HistogramOpts{
			Name:    "access_stage_duration_ms",
			Help:    "Access-owned query, durable command, relay and broker-notice stage duration in milliseconds; version 1, population access-process-lifetime, aggregate mode.",
			Buckets: accessStageBuckets,
		}, []string{"stage"}),
		AccessStageTotal: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "access_stage_total",
			Help: "Access-owned observation-stage dispositions by fixed stage and outcome; version 1.",
		}, []string{"stage", "outcome"}),
		AccessStageBytes: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "access_stage_bytes_total",
			Help: "Bytes observed at Access-owned stages by fixed stage; version 1, aggregate mode.",
		}, []string{"stage"}),
		AccessStageInfo: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Name: "access_stage_metrics_info",
			Help: "Declares the observation mode, population, unit and version of the Access stage aggregates.",
		}, []string{"version", "unit", "population", "observation_mode"}),
	}
	m.AccessStageInfo.WithLabelValues(AccessStageMetricsVersion, "ms", "access-process-lifetime", "aggregate").Set(1)
	registry.MustRegister(
		m.HostResults,
		m.HTTPRequestDuration,
		m.HTTPRequestErrors,
		m.HostRequestDuration,
		m.HostRequestErrors,
		m.MQTTRouteErrors,
		m.EventRouteDropped,
		m.LiveDrops,
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
		m.ControlLastSuccess,
		m.RouteAttach,
		m.RouteClose,
		m.CleanupBatches,
		m.CleanupRows,
		m.CleanupBacklogTotal,
		m.CleanupLastRun,
		m.TransportRejections,
		m.TransportRejectedBytes,
		m.TransportPackets,
		m.TransportPacketBytes,
		m.TransportQueueDepth,
		m.TransportInFlightWork,
		m.TransportBudgetFallbacks,
		m.PayloadFreeBytes,
		m.PayloadHeadroomBytes,
		m.PayloadReadyBytes,
		m.PayloadReservedBytes,
		m.PayloadExpiredBytes,
		m.PayloadObjects,
		m.PayloadActiveUploads,
		m.PayloadExpiredBacklog,
		m.PayloadAdmissionRefusals,
		m.PayloadCleanupDeleted,
		m.PayloadCleanupReclaimed,
		m.PayloadCleanupRetries,
		m.PayloadReconcileActions,
		m.NoticePending,
		m.NoticeOldestAge,
		m.NoticePublishRetries,
		m.AccessStageDuration,
		m.AccessStageTotal,
		m.AccessStageBytes,
		m.AccessStageInfo,
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

// LiveDropped records one QoS 0 live preview dropped by a bounded reason.
// Identities are never attached.
func (m *Metrics) LiveDropped(reason string) {
	if m == nil || m.LiveDrops == nil {
		return
	}
	switch reason {
	case "disconnected", "backpressure", "publish":
	default:
		reason = "publish"
	}
	m.LiveDrops.WithLabelValues(reason).Inc()
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
	case "packet_limit", "envelope_header", "envelope_budget", "reference_limit",
		"relay_topic", "staging", "queue", "budget_missing", "budget_invalid",
		"relay_epoch", "frame_header":
	default:
		code = "frame_header"
	}
	m.TransportRejections.WithLabelValues(code).Inc()
	if bytes > 0 {
		m.TransportRejectedBytes.WithLabelValues(code).Add(float64(bytes))
	}
	_ = limit
}

// TransportPacket counts one complete MQTT publication by representation.
func (m *Metrics) TransportPacket(representation string, bytes int64) {
	if m == nil || m.TransportPackets == nil {
		return
	}
	switch representation {
	case "inline", "reference":
	default:
		representation = "inline"
	}
	m.TransportPackets.WithLabelValues(representation).Inc()
	if bytes > 0 {
		m.TransportPacketBytes.WithLabelValues(representation).Add(float64(bytes))
	}
}

// TransportQueueDepth reports the current ordered queue depth for one bounded
// destination flow.
func (m *Metrics) TransportQueue(flow string, depth int) {
	if m == nil || m.TransportQueueDepth == nil {
		return
	}
	switch flow {
	case "request", "result", "event", "device_event", "notice", "presence", "other":
	default:
		flow = "other"
	}
	if depth < 0 {
		depth = 0
	}
	m.TransportQueueDepth.WithLabelValues(flow).Set(float64(depth))
}

// TransportInFlight reports whether one bounded destination flow has an
// in-flight publication.
func (m *Metrics) TransportInFlightCount(flow string, inFlight int) {
	if m == nil || m.TransportInFlightWork == nil {
		return
	}
	switch flow {
	case "request", "result", "event", "device_event", "notice", "presence", "other":
	default:
		flow = "other"
	}
	if inFlight < 0 {
		inFlight = 0
	}
	m.TransportInFlightWork.WithLabelValues(flow).Set(float64(inFlight))
}

// TransportReservations is a retained no-op for source compatibility with
// callers that still report aggregate reservations. The fragment reassembly
// path no longer exists, so no series is published.
func (m *Metrics) TransportReservations(_ int, _ int64) {}

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

// PayloadStorage publishes one accounting snapshot. Sizes are aggregate bytes;
// no object identity is exposed.
func (m *Metrics) PayloadStorage(free, headroom, ready, reserved, expired int64,
	readyObjects, reservedObjects, activeUploads int, backlogSeconds float64) {
	if m == nil || m.PayloadObjects == nil {
		return
	}
	clamp := func(value int64) float64 {
		if value < 0 {
			return 0
		}
		return float64(value)
	}
	m.PayloadFreeBytes.Set(clamp(free))
	m.PayloadHeadroomBytes.Set(clamp(headroom))
	m.PayloadReadyBytes.Set(clamp(ready))
	m.PayloadReservedBytes.Set(clamp(reserved))
	m.PayloadExpiredBytes.Set(clamp(expired))
	m.PayloadObjects.WithLabelValues("ready").Set(float64(readyObjects))
	m.PayloadObjects.WithLabelValues("reserved").Set(float64(reservedObjects))
	m.PayloadActiveUploads.Set(float64(activeUploads))
	m.PayloadExpiredBacklog.Set(backlogSeconds)
}

// PayloadRefused records one admission refusal by bounded reason.
func (m *Metrics) PayloadRefused(reason string) {
	if m == nil || m.PayloadAdmissionRefusals == nil {
		return
	}
	switch reason {
	case "size", "quota", "capacity", "headroom", "duplicate", "forbidden", "invalid":
	default:
		reason = "capacity"
	}
	m.PayloadAdmissionRefusals.WithLabelValues(reason).Inc()
}

// PayloadCleanup records one bounded cleanup pass. A failed deletion remains
// charged and is counted as a retry.
func (m *Metrics) PayloadCleanup(deleted int, reclaimed int64, failed int) {
	if m == nil || m.PayloadCleanupDeleted == nil {
		return
	}
	if deleted > 0 {
		m.PayloadCleanupDeleted.Add(float64(deleted))
	}
	if reclaimed > 0 {
		m.PayloadCleanupReclaimed.Add(float64(reclaimed))
	}
	if failed > 0 {
		m.PayloadCleanupRetries.Add(float64(failed))
	}
}

// PayloadReconcile records reconciliation outcomes by bounded action.
func (m *Metrics) PayloadReconcile(action string, count int) {
	if m == nil || m.PayloadReconcileActions == nil || count <= 0 {
		return
	}
	switch action {
	case "repairs", "orphans":
	default:
		action = "repairs"
	}
	m.PayloadReconcileActions.WithLabelValues(action).Add(float64(count))
}

// NoticeBacklog publishes one aggregate snapshot of pending notice work.
func (m *Metrics) NoticeBacklog(pending int, oldestSeconds float64) {
	if m == nil || m.NoticePending == nil {
		return
	}
	if pending < 0 {
		pending = 0
	}
	if oldestSeconds < 0 {
		oldestSeconds = 0
	}
	m.NoticePending.Set(float64(pending))
	m.NoticeOldestAge.Set(oldestSeconds)
}

// NoticePublishRetry records one failed publication that stays pending.
func (m *Metrics) NoticePublishRetry() {
	if m == nil || m.NoticePublishRetries == nil {
		return
	}
	m.NoticePublishRetries.Inc()
}

// ObserveAccessStage records one Access-owned stage duration and disposition.
// stage and outcome are clamped to the fixed vocabularies so cardinality stays
// bounded as requests, devices and objects grow. Durations describe the
// aggregate process-lifetime population; per-operation attribution requires
// trace-mode events.
func (m *Metrics) ObserveAccessStage(stage string, duration time.Duration, outcome string) {
	if m == nil || m.AccessStageDuration == nil || m.AccessStageTotal == nil {
		return
	}
	stage = normalizeAccessStage(stage)
	outcome = normalizeAccessOutcome(outcome)
	milliseconds := float64(duration) / float64(time.Millisecond)
	if milliseconds < 0 {
		milliseconds = 0
	}
	m.AccessStageDuration.WithLabelValues(stage).Observe(milliseconds)
	m.AccessStageTotal.WithLabelValues(stage, outcome).Inc()
}

// ObserveAccessStageDisposition counts a stage disposition without a duration.
// It is used for expiry and connection notices where an interval has no
// meaningful owning start.
func (m *Metrics) ObserveAccessStageDisposition(stage, outcome string) {
	if m == nil || m.AccessStageTotal == nil {
		return
	}
	m.AccessStageTotal.WithLabelValues(normalizeAccessStage(stage), normalizeAccessOutcome(outcome)).Inc()
}

// ObserveAccessStageBytes counts bytes observed at one Access stage.
func (m *Metrics) ObserveAccessStageBytes(stage string, bytes int64) {
	if m == nil || m.AccessStageBytes == nil || bytes <= 0 {
		return
	}
	m.AccessStageBytes.WithLabelValues(normalizeAccessStage(stage)).Add(float64(bytes))
}

func normalizeAccessStage(stage string) string {
	for _, known := range accessStages {
		if stage == known {
			return stage
		}
	}
	return "query_admit"
}

func normalizeAccessOutcome(outcome string) string {
	for _, known := range accessOutcomes {
		if outcome == known {
			return outcome
		}
	}
	return "error"
}
