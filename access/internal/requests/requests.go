// Package requests accepts HostRequest envelopes, keeps the single Access
// status copy and forwards encrypted bytes to the host over MQTT.
//
// The package does not execute DSH business logic. It never parses an
// encrypted body and never creates a second in-memory copy of authorization or
// request state.
package requests

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// Stable request failures.
var (
	ErrInvalidRequest  = errors.New("invalid request")
	ErrForbidden       = errors.New("forbidden")
	ErrNotFound        = errors.New("not found")
	ErrExpired         = errors.New("expired")
	ErrProtocolVersion = errors.New("protocol version incompatible")
	ErrDigestMismatch  = errors.New("payload digest mismatch")
	ErrConflict        = errors.New("conflict")
	ErrResultMismatch  = errors.New("host result mismatch")
	ErrInvalidResult   = errors.New("invalid host result")
	ErrUnavailable     = errors.New("unavailable")
)

// Store is the request persistence boundary.
type Store interface {
	CreateRequest(ctx context.Context, req store.Request) (store.CreateOutcome, error)
	GetRequest(ctx context.Context, requestID string) (store.Request, error)
	ClaimDueRequests(ctx context.Context, now time.Time, limit int, lease time.Duration) ([]store.Request, error)
	RequeueDuePublished(ctx context.Context, now time.Time, maxAttempts int) (int64, error)
	MarkPublished(ctx context.Context, requestID string, now time.Time, retryAfter time.Duration) error
	MarkPublishFailure(ctx context.Context, requestID string, now time.Time, retryAfter time.Duration, maxAttempts int) error
	ApplyHostResult(ctx context.Context, update store.HostResultUpdate) (bool, error)
	RenewClientLease(ctx context.Context, deviceID, requestID string, lease time.Duration) (bool, error)
	ExpireClientLeases(ctx context.Context, limit int) ([]store.Request, error)
	PendingCancelRequests(ctx context.Context, limit int) ([]store.Request, error)
	MarkCancelPublished(ctx context.Context, requestID string) error
	MarkCancelAttempt(ctx context.Context, requestID string) error
}

// Broker publishes one HostRequest after the transaction commits.
type Broker interface {
	PublishRequest(ctx context.Context, hostID string, request *whalereinv1.HostRequest) error
	PublishPresence(ctx context.Context, hostID string, payload []byte) error
}

// Presence is broker-derived host connectivity.
//
// Available distinguishes "the broker has not told us the current state yet"
// from "the host is known to be offline". During an Access restart or broker
// reconnect the first answer is unavailable, not offline; treating it as
// offline would terminally fail every in-flight request.
type Presence interface {
	IsHostOnline(ctx context.Context, hostID string) bool
	Available() bool
}

// Config holds forwarding and retention tunables.
type Config struct {
	ProtocolVersion uint32
	MaxTTL          time.Duration
	ForwardInterval time.Duration
	Lease           time.Duration
	RetryInterval   time.Duration
	MaxAttempts     int
	BatchSize       int
}

// Dependencies wires the request service.
type Dependencies struct {
	Store    Store
	Broker   Broker
	Presence Presence
	Config   Config
	Metrics  *diagnostics.Metrics
	Masker   *diagnostics.Masker
	Logger   *slog.Logger
	Now      func() time.Time
}

// Service owns request acceptance, forwarding and result application.
type Service struct {
	store    Store
	broker   Broker
	presence Presence
	cfg      Config
	metrics  *diagnostics.Metrics
	masker   *diagnostics.Masker
	logger   *slog.Logger
	now      func() time.Time
}

const (
	// clientPollLease is the exact-request HTTP liveness window. Only that
	// request's own polling or heartbeat renews it.
	clientPollLease = 10 * time.Second
	// clientSweepBatch bounds one expiry/replay pass. The caller's loop
	// repeats, so a backlog drains without one unbounded statement.
	clientSweepBatch = 100
)

// SubmitInput identifies the authenticated device and the request body.
type SubmitInput struct {
	DeviceID string
	HostID   string
	Request  *whalereinv1.HostRequest
}

// New constructs a request service.
func New(deps Dependencies) (*Service, error) {
	if deps.Store == nil {
		return nil, errors.New("requests: store is required")
	}
	if deps.Broker == nil {
		return nil, errors.New("requests: broker is required")
	}
	if deps.Presence == nil {
		return nil, errors.New("requests: presence is required")
	}
	if deps.Now == nil {
		deps.Now = func() time.Time { return time.Now().UTC() }
	}
	if deps.Logger == nil {
		deps.Logger = slog.Default()
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	if deps.Masker == nil {
		deps.Masker = diagnostics.NewMasker()
	}
	return &Service{
		store:    deps.Store,
		broker:   deps.Broker,
		presence: deps.Presence,
		cfg:      deps.Config,
		metrics:  deps.Metrics,
		masker:   deps.Masker,
		logger:   deps.Logger,
		now:      deps.Now,
	}, nil
}

// Submit authenticates the request envelope, deduplicates commands, writes one
// row, then publishes after the insert committed.
func (s *Service) Submit(ctx context.Context, input SubmitInput) (store.Request, bool, error) {
	started := s.now()
	req, validationErr := s.validate(input)
	if validationErr != nil {
		s.metrics.ObserveHostRequest(s.now().Sub(started), true)
		return store.Request{}, false, validationErr
	}
	switch {
	case !s.presence.Available():
		// Presence has not established broker state yet. Queue the request as
		// non-terminal; the forward loop will publish it once presence is
		// known, or close it as host_offline if the host is genuinely offline.
		req.Status = whalereinv1.OnlineStatus_received
		req.ForwardState = store.ForwardPending
		req.ForwardAfter = s.now()
	case !s.presence.IsHostOnline(ctx, input.HostID):
		req.Status = whalereinv1.OnlineStatus_host_offline
		req.ForwardState = store.ForwardNone
	default:
		req.Status = whalereinv1.OnlineStatus_received
		req.ForwardState = store.ForwardPending
		req.ForwardAfter = s.now()
	}

	outcome, err := s.store.CreateRequest(ctx, req)
	if err != nil {
		s.metrics.ObserveHostRequest(s.now().Sub(started), true)
		if errors.Is(err, store.ErrConflict) {
			return store.Request{}, false, fmt.Errorf("%w: duplicate request_id", ErrConflict)
		}
		return store.Request{}, false, fmt.Errorf("create request: %w", err)
	}
	if outcome.DigestMismatch {
		s.metrics.ObserveHostRequest(s.now().Sub(started), true)
		s.logger.Warn("command digest mismatch",
			"host", s.masker.Host(input.HostID), "device", s.masker.Device(input.DeviceID))
		return outcome.Request, false, ErrDigestMismatch
	}
	if !outcome.Created {
		// Same command id and digest: renew only the existing row's own lease
		// and return it without a second publish.
		if _, renewErr := s.store.RenewClientLease(ctx, input.DeviceID, outcome.Request.RequestID, clientPollLease); renewErr != nil {
			s.metrics.ObserveHostRequest(s.now().Sub(started), true)
			return store.Request{}, false, fmt.Errorf("renew replayed request: %w", renewErr)
		}
		s.metrics.ObserveHostRequest(s.now().Sub(started), false)
		return outcome.Request, false, nil
	}

	if outcome.Request.Status == whalereinv1.OnlineStatus_received {
		forwardErr := s.ForwardDue(ctx)
		if forwardErr != nil {
			s.metrics.ObserveHostRequest(s.now().Sub(started), true)
			s.logger.Warn("forward due requests failed", "error", forwardErr.Error())
		}
	}
	s.metrics.ObserveHostRequest(s.now().Sub(started), false)
	s.logger.Info("host request accepted",
		"host", s.masker.Host(input.HostID), "device", s.masker.Device(input.DeviceID),
		"request", s.masker.Request(outcome.Request.RequestID),
		"status", store.StatusName(outcome.Request.Status),
		"trace_id", outcome.Request.TraceID)
	return outcome.Request, true, nil
}

func (s *Service) validate(input SubmitInput) (store.Request, error) {
	req := input.Request
	if req == nil || input.DeviceID == "" || input.HostID == "" || req.GetRequestId() == "" {
		return store.Request{}, ErrInvalidRequest
	}
	if req.GetHostId() != "" && req.GetHostId() != input.HostID {
		return store.Request{}, ErrInvalidRequest
	}
	if req.GetDeviceId() != "" && req.GetDeviceId() != input.DeviceID {
		return store.Request{}, ErrForbidden
	}
	if req.GetProtocolVersion() != s.cfg.ProtocolVersion || (req.ClientTransportVersion != 0 && req.ClientTransportVersion != 2) {
		return store.Request{}, ErrProtocolVersion
	}
	expiresAt := time.UnixMilli(req.GetExpiresAtUnixMs()).UTC()
	now := s.now()
	if !expiresAt.After(now) {
		return store.Request{}, ErrExpired
	}
	if s.cfg.MaxTTL > 0 && expiresAt.After(now.Add(s.cfg.MaxTTL)) {
		return store.Request{}, fmt.Errorf("%w: expires_at exceeds max_ttl", ErrInvalidRequest)
	}

	switch req.GetKind() {
	case whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE,
		whalereinv1.RequestKind_REQUEST_KIND_QUERY:
		if req.GetCommandId() != "" || len(req.GetPayloadDigest()) != 0 {
			return store.Request{}, fmt.Errorf("%w: command fields on non-command request", ErrInvalidRequest)
		}
	case whalereinv1.RequestKind_REQUEST_KIND_COMMAND:
		if req.GetCommandId() == "" {
			return store.Request{}, fmt.Errorf("%w: command_id is required", ErrInvalidRequest)
		}
		if len(req.GetPayloadDigest()) == 0 {
			return store.Request{}, fmt.Errorf("%w: payload_digest is required", ErrInvalidRequest)
		}
	default:
		return store.Request{}, fmt.Errorf("%w: request kind is not accepted over HTTP", ErrInvalidRequest)
	}

	return store.Request{
		RequestID:              req.GetRequestId(),
		HostID:                 input.HostID,
		DeviceID:               input.DeviceID,
		CommandID:              req.GetCommandId(),
		PayloadDigest:          append([]byte(nil), req.GetPayloadDigest()...),
		Kind:                   req.GetKind(),
		ProtocolVersion:        req.GetProtocolVersion(),
		ClientTransportVersion: req.GetClientTransportVersion(),
		RequestBody:            append([]byte(nil), req.GetBody()...),
		ChannelID:              req.GetChannelId(),
		TraceID:                req.GetTraceId(),
		ExpiresAt:              expiresAt,
		CreatedAt:              now,
		UpdatedAt:              now,
		ClientLeaseUntil:       leaseDeadline(&expiresAt, now, clientPollLease),
		ClientLeaseState:       store.ClientLeaseLive,
	}, nil
}

// leaseDeadline bounds the exact-request liveness deadline by the original
// request deadline, so renewal can never extend past expires_at.
func leaseDeadline(expiresAt *time.Time, now time.Time, lease time.Duration) *time.Time {
	deadline := now.Add(lease)
	if deadline.After(*expiresAt) {
		deadline = *expiresAt
	}
	return &deadline
}

// ForwardDue leases due rows with FOR UPDATE SKIP LOCKED, publishes outside the
// lease transaction and records the publish outcome on the same row.
func (s *Service) ForwardDue(ctx context.Context) error {
	now := s.now()
	if !s.presence.Available() {
		// Broker state is not established yet. Leave queued rows untouched so
		// the next tick can forward them instead of publishing into a topic
		// with no subscriber or closing them as host_offline.
		return nil
	}
	requeued, err := s.store.RequeueDuePublished(ctx, now, s.cfg.MaxAttempts)
	if err != nil {
		return err
	}
	if requeued > 0 && s.metrics != nil {
		s.metrics.RequestForwardRetries.Add(float64(requeued))
	}
	rows, err := s.store.ClaimDueRequests(ctx, now, s.cfg.BatchSize, s.cfg.Lease)
	if err != nil {
		return err
	}
	for _, row := range rows {
		if ctx.Err() != nil {
			return nil
		}
		if !s.presence.IsHostOnline(ctx, row.HostID) {
			if row.Kind == whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE {
				// Pairing COMMIT is a durable outbox item. Keep it leased for
				// the next retry instead of publishing while the host is gone.
				continue
			}
			if _, applyErr := s.store.ApplyHostResult(ctx, store.HostResultUpdate{
				RequestID: row.RequestID,
				HostID:    row.HostID,
				DeviceID:  row.DeviceID,
				Status:    whalereinv1.OnlineStatus_host_offline,
				At:        now,
			}); applyErr != nil {
				return applyErr
			}
			s.logger.Info("host request closed while host offline",
				"host", s.masker.Host(row.HostID),
				"request", s.masker.Request(row.RequestID),
				"trace_id", row.TraceID)
			continue
		}
		envelope := &whalereinv1.HostRequest{
			RequestId:              row.RequestID,
			HostId:                 row.HostID,
			DeviceId:               row.DeviceID,
			Kind:                   row.Kind,
			CommandId:              row.CommandID,
			PayloadDigest:          row.PayloadDigest,
			ExpiresAtUnixMs:        row.ExpiresAt.UnixMilli(),
			TraceId:                row.TraceID,
			ProtocolVersion:        row.ProtocolVersion,
			ClientTransportVersion: row.ClientTransportVersion,
			ChannelId:              row.ChannelID,
			Body:                   row.RequestBody,
		}
		if err := s.broker.PublishRequest(ctx, row.HostID, envelope); err != nil {
			if errors.Is(err, broker.ErrPeerTransportUnsupported) || errors.Is(err, broker.ErrMessageBudget) {
				// Pairing COMMIT is a durable outbox item and must keep retrying
				// until its deadline, including across transient peer capability.
				if row.Kind == whalereinv1.RequestKind_REQUEST_KIND_PAIRING_AUTHORIZE {
					if markErr := s.store.MarkPublishFailure(ctx, row.RequestID, now, s.cfg.RetryInterval, s.cfg.MaxAttempts); markErr != nil {
						return markErr
					}
					continue
				}
				code := whalereinv1.ErrorCode_protocol_version_incompatible
				if errors.Is(err, broker.ErrMessageBudget) {
					code = whalereinv1.ErrorCode_payload_too_large
				}
				if _, applyErr := s.store.ApplyHostResult(ctx, store.HostResultUpdate{RequestID: row.RequestID, HostID: row.HostID, DeviceID: row.DeviceID, Status: whalereinv1.OnlineStatus_rejected, TransportPreflight: true, ErrorCode: code, At: now}); applyErr != nil {
					return applyErr
				}
				s.logger.Warn("logical transport preflight failed", "trace_id", row.TraceID, "code", code.String(), "bytes", len(row.RequestBody))
				continue
			}
			s.metrics.RouteError()
			s.logger.Warn("host request publish failed",
				"host", s.masker.Host(row.HostID),
				"request", s.masker.Request(row.RequestID),
				"attempt", row.ForwardAttempts,
				"trace_id", row.TraceID, "bytes", len(row.RequestBody),
				"error", err.Error())
			if markErr := s.store.MarkPublishFailure(ctx, row.RequestID, now, s.cfg.RetryInterval, s.cfg.MaxAttempts); markErr != nil {
				return markErr
			}
			continue
		}
		if err := s.store.MarkPublished(ctx, row.RequestID, now, s.cfg.RetryInterval); err != nil {
			return err
		}
		s.logger.Info("host request forwarded",
			"host", s.masker.Host(row.HostID),
			"request", s.masker.Request(row.RequestID),
			"status", store.StatusName(row.Status), "trace_id", row.TraceID, "kind", row.Kind, "bytes", len(row.RequestBody))
	}
	return nil
}

func terminalStatus(status whalereinv1.OnlineStatus) bool {
	switch status {
	case whalereinv1.OnlineStatus_succeeded, whalereinv1.OnlineStatus_failed,
		whalereinv1.OnlineStatus_rejected, whalereinv1.OnlineStatus_host_offline,
		whalereinv1.OnlineStatus_unknown:
		return true
	default:
		return false
	}
}

// Touch renews only the exact request's own live lease using database time.
// Unrelated same-device traffic never renews another request, a wrong device
// is rejected, and a terminal, expired or elapsed row cannot be revived.
func (s *Service) Touch(ctx context.Context, deviceID, requestID string) (bool, error) {
	if deviceID == "" || requestID == "" {
		return false, nil
	}
	renewed, err := s.store.RenewClientLease(ctx, deviceID, requestID, clientPollLease)
	if err != nil {
		return false, fmt.Errorf("touch request: %w", err)
	}
	return renewed, nil
}

// cancellationPayload is the bounded exact-scope cancellation published to the
// owning Host presence topic. It carries identity and deadline metadata only;
// no question, answer, request body or result content is included.
type cancellationPayload struct {
	Event     string `json:"event"`
	RequestID string `json:"request_id"`
	HostID    string `json:"host_id"`
	DeviceID  string `json:"device_id"`
	CommandID string `json:"command_id,omitempty"`
	// Digest binds a command notice to the exact payload; absent for queries.
	Digest          string `json:"digest,omitempty"`
	ExpiresAtUnixMs int64  `json:"expires_at_unix_ms"`
	Reason          string `json:"reason"`
	ObservedAt      string `json:"observed_at"`
}

// SweepClientLeases durably expires elapsed exact-request leases and replays
// pending exact cancellations. Expiry and renewal have one transaction winner;
// a failed or unavailable publish leaves cancel_pending set so a later sweep
// (including after restart) retries the same exact identity. Publication alone
// is never reported as native release.
func (s *Service) SweepClientLeases(ctx context.Context) {
	now := s.now()
	expired, err := s.store.ExpireClientLeases(ctx, clientSweepBatch)
	if err != nil {
		s.logger.Warn("client lease expiry failed", "error", err.Error())
		return
	}
	if len(expired) > 0 && s.metrics != nil {
		s.metrics.RequestClaimStale()
	}
	candidates := expired
	if len(expired) < clientSweepBatch {
		pending, err := s.store.PendingCancelRequests(ctx, clientSweepBatch)
		if err != nil {
			s.logger.Warn("pending cancellation query failed", "error", err.Error())
			return
		}
		seen := make(map[string]bool, len(expired)+len(pending))
		for _, row := range expired {
			seen[row.RequestID] = true
		}
		for _, row := range pending {
			if !seen[row.RequestID] {
				candidates = append(candidates, row)
			}
		}
	}
	for _, row := range candidates {
		if ctx.Err() != nil {
			return
		}
		s.publishCancellation(ctx, row, now)
	}
}

// cancellationDigest renders the exact command payload digest for a notice.
// Queries and handshakes have no digest and carry no executable command scope.
func cancellationDigest(row store.Request) string {
	if row.CommandID == "" || len(row.PayloadDigest) == 0 {
		return ""
	}
	return hex.EncodeToString(row.PayloadDigest)
}

func (s *Service) publishCancellation(ctx context.Context, row store.Request, now time.Time) {
	if !s.presence.Available() || !s.presence.IsHostOnline(ctx, row.HostID) {
		// Unknown/offline transport is a retryable delivery condition, not a
		// reason to drop the retained cancellation.
		return
	}
	payload, err := json.Marshal(cancellationPayload{
		Event:           "request_stale",
		RequestID:       row.RequestID,
		HostID:          row.HostID,
		DeviceID:        row.DeviceID,
		CommandID:       row.CommandID,
		Digest:          cancellationDigest(row),
		ExpiresAtUnixMs: row.ExpiresAt.UnixMilli(),
		Reason:          "client_poll_expired",
		ObservedAt:      now.UTC().Format(time.RFC3339Nano),
	})
	if err != nil {
		return
	}
	if err := s.broker.PublishPresence(ctx, row.HostID, payload); err != nil {
		s.logger.Warn("request stale publish failed",
			"host", s.masker.Host(row.HostID), "device", s.masker.Device(row.DeviceID),
			"request", s.masker.Request(row.RequestID), "attempt", row.CancelAttempts+1, "error", err.Error())
		if markErr := s.store.MarkCancelAttempt(ctx, row.RequestID); markErr != nil {
			s.logger.Warn("request stale attempt not recorded",
				"request", s.masker.Request(row.RequestID), "error", markErr.Error())
		}
		return
	}
	if err := s.store.MarkCancelPublished(ctx, row.RequestID); err != nil {
		// The notice may already have reached the Host; retaining pending
		// metadata can duplicate a harmless exact notice, never reopen R.
		s.logger.Warn("request stale publish not recorded",
			"request", s.masker.Request(row.RequestID), "error", err.Error())
		return
	}
	s.logger.Info("request stale released",
		"host", s.masker.Host(row.HostID), "device", s.masker.Device(row.DeviceID),
		"request", s.masker.Request(row.RequestID), "command", s.masker.Request(row.CommandID))
}

// Run drives the forwarding retry loop until ctx is cancelled.
func (s *Service) Run(ctx context.Context) error {
	interval := s.cfg.ForwardInterval
	if interval <= 0 {
		interval = 500 * time.Millisecond
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
			if err := s.ForwardDue(ctx); err != nil && ctx.Err() == nil {
				s.logger.Warn("forward loop failed", "error", err.Error())
			}
			s.SweepClientLeases(ctx)
		}
	}
}

// Get returns the status copy for a request owned by the authenticated device.
func (s *Service) Get(ctx context.Context, deviceID, requestID string) (store.Request, error) {
	req, err := s.store.GetRequest(ctx, requestID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return store.Request{}, ErrNotFound
		}
		return store.Request{}, fmt.Errorf("get request: %w", err)
	}
	if req.DeviceID != deviceID {
		return store.Request{}, ErrNotFound
	}
	if !terminalStatus(req.Status) && !s.now().Before(req.ExpiresAt) {
		// A request that never reached a terminal state is expired, not still
		// host_accepted. Returning the row would make the CLI poll a status
		// that can never change.
		return req, ErrExpired
	}
	return req, nil
}

// HandleHostResult validates a host result against the topic host and updates
// only the matching request row using the allowed conditional transitions.
func (s *Service) HandleHostResult(ctx context.Context, topicHost string, result *whalereinv1.HostResult) error {
	if result == nil || topicHost == "" || result.GetRequestId() == "" {
		return ErrInvalidResult
	}
	if result.GetHostId() != "" && result.GetHostId() != topicHost {
		return fmt.Errorf("%w: host id differs from topic", ErrResultMismatch)
	}
	row, err := s.store.GetRequest(ctx, result.GetRequestId())
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrNotFound
		}
		return fmt.Errorf("get request for result: %w", err)
	}
	if row.HostID != topicHost {
		return fmt.Errorf("%w: request belongs to another host", ErrResultMismatch)
	}
	if result.GetDeviceId() != "" && result.GetDeviceId() != row.DeviceID {
		return fmt.Errorf("%w: device id differs", ErrResultMismatch)
	}
	if result.GetCommandId() != "" && result.GetCommandId() != row.CommandID {
		return fmt.Errorf("%w: command id differs", ErrResultMismatch)
	}
	if len(result.GetPayloadDigest()) > 0 && !bytes.Equal(result.GetPayloadDigest(), row.PayloadDigest) {
		return fmt.Errorf("%w: payload digest differs", ErrResultMismatch)
	}
	// A non-empty result channel must match the channel recorded at submission.
	// An empty channel is allowed for handshakes and older hosts; it is never
	// used to replace a different recorded channel.
	if result.GetChannelId() != "" && row.ChannelID != "" && result.GetChannelId() != row.ChannelID {
		return fmt.Errorf("%w: channel id differs", ErrResultMismatch)
	}
	if !store.ValidStatus(result.GetStatus()) || result.GetStatus() == whalereinv1.OnlineStatus_online_status_unspecified ||
		result.GetStatus() == whalereinv1.OnlineStatus_received {
		return fmt.Errorf("%w: status %s", ErrInvalidResult, whalereinv1.OnlineStatus_name[int32(result.GetStatus())])
	}
	applied, err := s.store.ApplyHostResult(ctx, store.HostResultUpdate{
		RequestID:    row.RequestID,
		HostID:       topicHost,
		DeviceID:     row.DeviceID,
		Status:       result.GetStatus(),
		ResultBody:   append([]byte(nil), result.GetResultBody()...),
		ErrorCode:    result.GetErrorCode(),
		ErrorMessage: result.GetErrorMessage(),
		ChannelID:    result.GetChannelId(),
		At:           s.now(),
	})
	if err != nil {
		return fmt.Errorf("apply host result: %w", err)
	}
	if applied {
		s.metrics.ObserveHostResult(result.GetStatus(), result.GetErrorCode())
		s.logger.Info("host result stored",
			"host", s.masker.Host(topicHost),
			"request", s.masker.Request(row.RequestID),
			"status", store.StatusName(result.GetStatus()),
			"trace_id", row.TraceID, "kind", row.Kind, "code", result.GetErrorCode().String(), "layer", "host", "duration_ms", s.now().Sub(row.CreatedAt).Milliseconds())
	} else {
		s.logger.Debug("host result ignored after terminal or expiry",
			"request", s.masker.Request(row.RequestID))
	}
	return nil
}
