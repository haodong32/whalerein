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
