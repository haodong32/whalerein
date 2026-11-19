// Package httpapi is the public and internal HTTP surface for WhaleRein Access.
//
// It performs routing, bearer authentication and protojson encoding only. It
// never accesses PostgreSQL or MQTT directly; all state work goes through the
// auth, pairing and requests services.
package httpapi

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"regexp"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Header names fixed by the internal broker callback contract.
const (
	TraceHeader    = "X-Whalerein-Trace-Id"
	InternalHeader = "X-Whalerein-Internal-Token"
)

// traceIDPattern is the accepted client-supplied correlation alphabet. It
// excludes quotes, spaces and newlines so a trace id can never forge a log
// record or an HTTP header value.
var traceIDPattern = regexp.MustCompile(`^[A-Za-z0-9._:-]{1,128}$`)

// AuthService is the auth package boundary used by HTTP.
type AuthService interface {
	AuthenticateAdmin(token string) error
	AuthenticateDeviceToken(ctx context.Context, token string) (auth.Identity, error)
	AuthorizeHost(ctx context.Context, identity auth.Identity, hostID string) error
	ListHosts(ctx context.Context, identity auth.Identity, limit, offset int) ([]auth.HostView, bool, error)
	RevokeDevice(ctx context.Context, deviceID string) error
	AuthenticateHostRequest(ctx context.Context, proof auth.HostProof) error
	RevokeHostGrant(ctx context.Context, hostID, deviceID string) error
	AuthenticateBroker(ctx context.Context, clientID, username, password string) (auth.BrokerIdentity, error)
	BrokerPolicy(ctx context.Context, identity auth.BrokerIdentity) (policy.Connection, error)
}

// PairingService is the pairing package boundary used by HTTP.
type PairingService interface {
	CreateInvitation(ctx context.Context, hostID string) (*whalereinv1.PairingInvitation, error)
	Claim(ctx context.Context, request *whalereinv1.PairingClaimRequest) (*whalereinv1.PairingClaimResponse, error)
}

// RequestService is the requests package boundary used by HTTP.
type RequestService interface {
	Submit(ctx context.Context, input requests.SubmitInput) (store.Request, bool, error)
	Release(ctx context.Context, deviceID, sessionID, requestID string, deliveryLimitBytes int64) (store.Request, error)
	Get(ctx context.Context, deviceID, requestID string) (store.Request, error)
	Touch(ctx context.Context, deviceID, requestID string) (bool, error)
}

// Dependencies wires the HTTP server.
type Dependencies struct {
	RequireDeviceSession     bool
	SessionAuthorityRevision string
	Auth                     AuthService
	Pairing                  PairingService
	Requests                 RequestService
	Readiness                *diagnostics.Readiness
	Metrics                  *diagnostics.Metrics
	Masker                   *diagnostics.Masker
	Logger                   *slog.Logger
	InternalToken            string
	MaxBodyBytes             int64
	HostPresence             external.HostPresence
	// External serves external account mode when it is configured. Standalone
	// deployments leave it nil and keep the invitation routes only.
	External ExternalService
}

// ExternalService is the external account mode boundary.
type ExternalService interface {
	EnrollHost(ctx context.Context, request *whalereinv1.HostEnrollmentRequest) (*whalereinv1.HostEnrollmentResponse, error)
	EnrollmentStatus(ctx context.Context, hostID string) (*whalereinv1.HostEnrollmentStatus, error)
	EnrollmentStatusForHost(ctx context.Context, hostID string, proof external.HostOwnershipProof) (*whalereinv1.HostEnrollmentStatus, error)
	OperationStatus(ctx context.Context, operationID string) (*whalereinv1.OperationStatus, error)
	DisableHost(ctx context.Context, hostID string) (time.Time, error)
	RecoverCredential(ctx context.Context, hostID string, authorization *whalereinv1.ExternalAuthorization, proof external.HostOwnershipProof) (*external.CredentialReplacement, error)
}

// Server owns the public and internal HTTP handlers.
type Server struct {
	deps     Dependencies
	public   http.Handler
	internal http.Handler
}

// New constructs the HTTP server.
func New(deps Dependencies) (*Server, error) {
	if deps.Auth == nil || deps.Pairing == nil || deps.Requests == nil {
		return nil, errors.New("httpapi: auth, pairing and requests are required")
	}
	if deps.Readiness == nil {
		return nil, errors.New("httpapi: readiness is required")
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	if deps.Masker == nil {
		deps.Masker = diagnostics.NewMasker()
	}
	if deps.Logger == nil {
		deps.Logger = slog.Default()
	}
	if deps.MaxBodyBytes <= 0 {
		deps.MaxBodyBytes = 24 << 20
	}
	s := &Server{deps: deps}
	notFound := func(w http.ResponseWriter, r *http.Request) { s.writeError(w, r, errRouteNotFound) }
	methodError := func(w http.ResponseWriter, r *http.Request) { s.writeError(w, r, errMethod) }
	s.public = s.instrument(newRouterWithFallbacks(s.publicRoutes(), notFound, methodError), false)
	s.internal = s.instrument(newRouterWithFallbacks(s.internalRoutes(), notFound, methodError), true)
	return s, nil
}

// PublicHandler returns the handler bound to the configured public listener.
func (s *Server) PublicHandler() http.Handler { return s.public }

// InternalHandler returns the handler bound to the internal listener. Every
// route requires the fixed internal callback header.
func (s *Server) InternalHandler() http.Handler { return s.internal }

func (s *Server) publicRoutes() []routeSpec {
	routes := []routeSpec{
		{method: http.MethodPost, pattern: "/v1/pairing/invitations", handler: s.handleInvitation},
		{method: http.MethodPost, pattern: "/v1/pairing/claims", handler: s.handleClaim},
		{method: http.MethodGet, pattern: "/v1/hosts", handler: s.handleHosts},
		{method: http.MethodPost, pattern: "/v1/hosts/{host_id}/requests", handler: s.handleSubmit},
		{method: http.MethodGet, pattern: "/v1/requests/{request_id}", handler: s.handleGetRequest},
		{method: http.MethodPost, pattern: "/v1/requests/{request_id}/heartbeat", handler: s.handleHeartbeat},
		{method: http.MethodDelete, pattern: "/v1/devices/{device_id}", handler: s.handleDeleteDevice},
		{method: http.MethodPost, pattern: "/v1/hosts/{host_id}/devices/{device_id}/revocation", handler: s.handleHostRevocation},
		{method: http.MethodGet, pattern: "/healthz", handler: s.handleHealth},
		{method: http.MethodGet, pattern: "/readyz", handler: s.handleReady},
		{method: http.MethodGet, pattern: "/metrics", handler: s.handleMetrics},
	}
	// External endpoints are registered in every build. A deployment that did
	// not enable external account mode answers 501 unsupported_capability, so
	// a mistyped path (404) stays distinguishable from a disabled mode.
	return append(routes,
		routeSpec{method: http.MethodGet, pattern: "/v1/external/hosts/{host_id}/pairings/{operation_id}", handler: s.handleExternalPairingStatus},
		routeSpec{method: http.MethodPost, pattern: "/v1/external/sessions/{session_id}/permission", handler: s.handleInstallExternalDeviceSession},
		routeSpec{method: http.MethodPost, pattern: "/v1/external/sessions/{session_id}/revocation", handler: s.handleRevokeExternalDeviceSession},
		routeSpec{method: http.MethodPost, pattern: "/v1/enrollments/hosts", handler: s.handleEnrollHost},
		routeSpec{method: http.MethodPost, pattern: "/v1/enrollments/devices/pairing", handler: s.handleCloudPairing},
		routeSpec{method: http.MethodGet, pattern: "/v1/enrollments/hosts/{host_id}/devices/{device_id}", handler: s.handleCloudDeviceStatus},
		routeSpec{method: http.MethodPost, pattern: "/v1/enrollments/hosts/{host_id}/devices/{device_id}/revocation", handler: s.handleCloudDeviceRevocation},
		routeSpec{method: http.MethodGet, pattern: "/v1/enrollments/hosts/{host_id}", handler: s.handleEnrollmentStatus},
		routeSpec{method: http.MethodGet, pattern: "/v1/enrollments/hosts/{host_id}/clients", handler: s.handleHostClients},
		routeSpec{method: http.MethodPost, pattern: "/v1/enrollments/hosts/{host_id}/disable", handler: s.handleDisableHostEnrollment},
		routeSpec{method: http.MethodPost, pattern: "/v1/enrollments/hosts/{host_id}/credential", handler: s.handleCredentialReplacement},
		routeSpec{method: http.MethodGet, pattern: "/v1/hosts/{host_id}/authority", handler: s.handleHostAuthority},
		routeSpec{method: http.MethodGet, pattern: "/v1/hosts/{host_id}/enrollment", handler: s.handleHostEnrollmentProofStatus},
		routeSpec{method: http.MethodGet, pattern: "/v1/operations/{operation_id}", handler: s.handleOperationStatus},
	)
}

func (s *Server) internalRoutes() []routeSpec {
	return []routeSpec{
		{method: http.MethodPost, pattern: "/internal/broker/connect", handler: s.handleBrokerConnect},
	}
}

func newRouterWithFallbacks(routes []routeSpec, notFound, methodError func(http.ResponseWriter, *http.Request)) *router {
	r := newRouter(routes)
	r.notFound = notFound
	r.methodError = methodError
	return r
}

func (s *Server) instrument(router *router, internal bool) http.Handler {
	if internal {
		base := router
		guard := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if !s.internalTokenOK(r.Header.Get(InternalHeader)) {
				s.writeError(w, r, errInternalToken)
				return
			}
			base.ServeHTTP(w, r)
		})
		return s.observe(guard)
	}
	return s.observe(router)
}

func (s *Server) observe(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		meta := &requestMeta{traceID: traceID(r)}
		ctx := withRequestMeta(diagnostics.WithTraceID(r.Context(), meta.traceID), meta)
		r = r.WithContext(ctx)
		w.Header().Set(TraceHeader, meta.traceID)
		recorder := &statusWriter{ResponseWriter: w}
		started := time.Now()
		defer func() {
			if recovered := recover(); recovered != nil {
				s.deps.Logger.Error("http handler panic", "trace_id", meta.traceID)
				if !recorder.wrote {
					s.writeError(recorder, r, errPanic)
				}
			}
			duration := time.Since(started)
			status := recorder.Status()
			s.deps.Metrics.ObserveHTTP(duration, meta.errorCode)
			attrs := []any{
				"method", r.Method,
				"route", meta.route,
				"status", status,
				"code", meta.errorCode,
				"duration_ms", duration.Milliseconds(),
				"trace_id", meta.traceID,
			}
			if meta.requestID != "" {
				attrs = append(attrs, "request", s.deps.Masker.Request(meta.requestID))
			}
			if status >= 500 {
				s.deps.Logger.Error("http request", attrs...)
			} else if status >= 400 {
				s.deps.Logger.Warn("http request", attrs...)
			} else {
				s.deps.Logger.Info("http request", attrs...)
			}
		}()
		next.ServeHTTP(recorder, r)
	})
}

func traceID(r *http.Request) string {
	if value := r.Header.Get(TraceHeader); validTraceID(value) {
		return value
	}
	raw := make([]byte, 16)
	if _, err := rand.Read(raw); err == nil {
		return hex.EncodeToString(raw)
	}
	return fmt.Sprintf("t-%d", time.Now().UnixNano())
}

// validTraceID accepts the bounded correlation-id alphabet. Arbitrary client
// text is rejected and replaced with a fresh server id, so a caller cannot
// inject log structure or make two operations share a trace.
func validTraceID(value string) bool {
	return traceIDPattern.MatchString(value)
}

func (s *Server) internalTokenOK(value string) bool {
	if value == "" || s.deps.InternalToken == "" {
		return false
	}
	left := sha256.Sum256([]byte(value))
	right := sha256.Sum256([]byte(s.deps.InternalToken))
	return subtle.ConstantTimeCompare(left[:], right[:]) == 1
}

type requestMeta struct {
	traceID   string
	route     string
	requestID string
	errorCode string
	params    map[string]string
}

type statusWriter struct {
	http.ResponseWriter
	status int
	wrote  bool
}

func (w *statusWriter) WriteHeader(code int) {
	if !w.wrote {
		w.status = code
		w.wrote = true
	}
	w.ResponseWriter.WriteHeader(code)
}

func (w *statusWriter) Write(data []byte) (int, error) {
	if !w.wrote {
		w.WriteHeader(http.StatusOK)
	}
	return w.ResponseWriter.Write(data)
}

func (w *statusWriter) Status() int {
	if !w.wrote {
		return http.StatusOK
	}
	return w.status
}

func (w *statusWriter) Flush() {
	if flusher, ok := w.ResponseWriter.(http.Flusher); ok {
		flusher.Flush()
	}
}

func (w *statusWriter) Unwrap() http.ResponseWriter { return w.ResponseWriter }
