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
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"regexp"
	"strings"
	"time"

	"google.golang.org/protobuf/proto"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/observation"
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
	// ErrReleaseFenced denies ciphertext whose grant revision or retention no
	// longer authorizes release. Metadata-only kinds keep their recovery path.
	ErrReleaseFenced = errors.New("ciphertext release fenced")
	// ErrResultTooLarge rejects a host result before it is buffered or stored.
	ErrResultTooLarge = errors.New("host result exceeds request bound")
	// ErrRequestTooLarge rejects an admitted request whose forwarded envelope
	// (including Access-bound metadata) cannot fit the configured budget.
	ErrRequestTooLarge = errors.New("request exceeds forwarded transport budget")
)

// Store is the request persistence boundary.
type Store interface {
	CreateRequest(ctx context.Context, req store.Request) (store.CreateOutcome, error)
	GetRequest(ctx context.Context, requestID string) (store.Request, error)
	ReleaseRequest(ctx context.Context, scope store.ReleaseScope) (store.ReleaseOutcome, error)
	ClaimDueRequests(ctx context.Context, now time.Time, limit int, lease time.Duration) ([]store.Request, error)
	RequeueDuePublished(ctx context.Context, now time.Time, maxAttempts int) (int64, error)
	MarkPublished(ctx context.Context, requestID string, now time.Time, retryAfter time.Duration) error
	MarkPublishFailure(ctx context.Context, requestID string, now time.Time, retryAfter time.Duration, maxAttempts int) error
	ApplyHostResult(ctx context.Context, update store.HostResultUpdate) (bool, error)
	RenewClientLease(ctx context.Context, deviceID, requestID string, lease time.Duration) (bool, error)
	HeartbeatRequest(ctx context.Context, deviceID, sessionID, requestID string, lease time.Duration) (store.HeartbeatOutcome, error)
	ExpireClientLeases(ctx context.Context, limit int) ([]store.Request, error)
	PendingCancelRequests(ctx context.Context, limit int) ([]store.Request, error)
	MarkCancelPublished(ctx context.Context, requestID string) error
	MarkCancelAttempt(ctx context.Context, requestID string) error
	ClaimNoticeIntents(ctx context.Context, now time.Time, limit int, lease time.Duration) ([]store.Request, error)
	MarkNoticePublished(ctx context.Context, requestID string, status whalereinv1.OnlineStatus, updatedAt time.Time, now time.Time) (bool, error)
	MarkNoticeFailure(ctx context.Context, requestID string, now, nextAttempt time.Time) error
	NoticeStats(ctx context.Context) (store.NoticeBacklog, error)
}

// Broker publishes one HostRequest after the transaction commits, the exact
// cancellation notice and one packet-sized RequestStateNotice.
type Broker interface {
	PublishRequest(ctx context.Context, hostID string, request *whalereinv1.HostRequest) error
	PublishPresence(ctx context.Context, hostID string, payload []byte) error
	PublishDeviceNotice(ctx context.Context, hostID, deviceID string, notice *whalereinv1.RequestStateNotice) error
}

// PayloadChecker answers whether an opaque reference is readable under the
// exact stored scope. It never returns ciphertext and never logs an object id.
type PayloadChecker interface {
	CheckPayload(ctx context.Context, reference *whalereinv1.PayloadReference, scope store.PayloadCheckScope) error
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
	SessionAuthorityRevision string
	ProtocolVersion          uint32
	MaxTTL                   time.Duration
	ForwardInterval          time.Duration
	Lease                    time.Duration
	RetryInterval            time.Duration
	MaxAttempts              int
	BatchSize                int
	// MaxResultBytes is the raw ciphertext cap Access retains for one result.
	// MaxDeliveryBytes is the current HTTP response body budget used to derive
	// the encoded result allowance on both admission and release.
	MaxResultBytes   int64
	MaxDeliveryBytes int64
	// EnvelopeBytes is the effective logical envelope budget for one forwarded
	// HostRequest, including Access-bound transport metadata.
	EnvelopeBytes int64
	// PayloadOwnerID is this node's configured reference owner. An empty value
	// accepts any non-empty owner carried by a reference.
	PayloadOwnerID string
	// Notice publication bounds. A pending intent is retried with capped
	// backoff until it is published or superseded.
	NoticeInterval time.Duration
	NoticeBatch    int
	NoticeLease    time.Duration
}

// GrantAuthority reads durable grant revisions. It is deliberately a store
// read, never the realtime admission cache, so a stamp cannot be refreshed
// from stale local state.
type GrantAuthority interface {
	AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error)
}

// Dependencies wires the request service.
type DeviceSessionStore interface {
	PublishDuringDeviceSession(context.Context, string, func() error) error
	SessionCancellationReason(context.Context, string) (bool, error)
}

type Dependencies struct {
	Sessions    DeviceSessionStore
	Store       Store
	Broker      Broker
	Presence    Presence
	Grants      GrantAuthority
	Payloads    PayloadChecker
	Config      Config
	Metrics     *diagnostics.Metrics
	Masker      *diagnostics.Masker
	Logger      *slog.Logger
	Now         func() time.Time
	Observation *observation.Sink
}

// Service owns request acceptance, forwarding and result application.
type Service struct {
	sessions    DeviceSessionStore
	store       Store
	broker      Broker
	presence    Presence
	grants      GrantAuthority
	payloads    PayloadChecker
	cfg         Config
	metrics     *diagnostics.Metrics
	masker      *diagnostics.Masker
	logger      *slog.Logger
	now         func() time.Time
	observation *observation.Sink
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
	SessionID string
	DeviceID  string
	HostID    string
	Request   *whalereinv1.HostRequest
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
	if deps.Grants == nil {
		return nil, errors.New("requests: grant authority is required")
	}
	if deps.Now == nil {
		deps.Now = func() time.Time { return time.Now().UTC() }
	}
	if deps.Config.MaxResultBytes <= 0 {
		deps.Config.MaxResultBytes = 16 << 20
	}
	if deps.Config.MaxDeliveryBytes <= 0 {
		deps.Config.MaxDeliveryBytes = 24 << 20
	}
	if deps.Config.EnvelopeBytes <= 0 {
		deps.Config.EnvelopeBytes = 16 << 20
	}
	if deps.Config.NoticeInterval <= 0 {
		deps.Config.NoticeInterval = time.Second
	}
	if deps.Config.NoticeBatch <= 0 {
		deps.Config.NoticeBatch = 50
	}
	if deps.Config.NoticeLease <= 0 {
		deps.Config.NoticeLease = 30 * time.Second
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
		store:       deps.Store,
		sessions:    deps.Sessions,
		broker:      deps.Broker,
		presence:    deps.Presence,
		grants:      deps.Grants,
		payloads:    deps.Payloads,
		cfg:         deps.Config,
		metrics:     deps.Metrics,
		masker:      deps.Masker,
		logger:      deps.Logger,
		now:         deps.Now,
		observation: deps.Observation,
	}, nil
}

// Submit authenticates the request envelope, deduplicates commands, writes one
// row, then publishes after the insert committed.
func (s *Service) Submit(ctx context.Context, input SubmitInput) (result store.Request, created bool, err error) {
	started := time.Now()
	requestID := ""
	bodyBytes := 0
	if input.Request != nil {
		requestID = input.Request.GetRequestId()
		bodyBytes = len(input.Request.GetBody())
	}
	defer func() {
		if s.observation == nil || requestID == "" {
			return
		}
		outcome := commandAdmissionOutcome(err)
		attempt := uint64(1)
		if result.ForwardAttempts > 0 {
			attempt = uint64(result.ForwardAttempts) + 1
		}
		s.observeCommand(commandScope(input.HostID, input.DeviceID), requestID, attempt, observation.CommandAdmit, outcome, bodyBytes)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("command_admit", time.Since(started), string(outcome))
		}
	}()
	startedAt := s.now()
	ctx = store.WithRequestDeviceSession(ctx, input.SessionID)
	ctx = store.WithDeviceSessionAuthority(ctx, s.cfg.SessionAuthorityRevision)
	req, validationErr := s.validate(input)
	if validationErr != nil {
		s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
		return store.Request{}, false, validationErr
	}
	// Read the durable revisions exactly once for this admission. The request's
	// own wire metadata is never trusted for the stamp, and a replay only
	// returns the already persisted row without rewriting it.
	stamp, stampErr := s.admissionStamp(ctx, input)
	if stampErr != nil {
		s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
		return store.Request{}, false, stampErr
	}
	req.Stamp = stamp
	if len(req.BodyReference) > 0 {
		reference, err := decodeReference(req.BodyReference)
		if err != nil {
			s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
			return store.Request{}, false, err
		}
		if err := s.checkReference(ctx, reference, store.PayloadCheckScope{
			HostID:     req.HostID,
			DeviceID:   req.DeviceID,
			AppSession: input.SessionID,
			ChannelID:  req.ChannelID,
			Purpose:    whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST,
		}); err != nil {
			s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
			return store.Request{}, false, err
		}
	}
	if sizeErr := s.checkForwardedSize(req); sizeErr != nil {
		s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
		return store.Request{}, false, sizeErr
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
		s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
		if errors.Is(err, store.ErrConflict) {
			return store.Request{}, false, fmt.Errorf("%w: duplicate request_id", ErrConflict)
		}
		if errors.Is(err, store.ErrDeviceSessionEnded) {
			return store.Request{}, false, ErrForbidden
		}
		return store.Request{}, false, fmt.Errorf("create request: %w", err)
	}
	if outcome.DigestMismatch {
		s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
		s.logger.Warn("command digest mismatch",
			"host", s.masker.Host(input.HostID), "device", s.masker.Device(input.DeviceID))
		return outcome.Request, false, ErrDigestMismatch
	}
	if !outcome.Created {
		// Same command id and digest: renew only the existing row's own lease
		// and return it without a second publish.
		if _, renewErr := s.store.RenewClientLease(ctx, input.DeviceID, outcome.Request.RequestID, clientPollLease); renewErr != nil {
			s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
			return store.Request{}, false, fmt.Errorf("renew replayed request: %w", renewErr)
		}
		s.metrics.ObserveHostRequest(s.now().Sub(startedAt), false)
		return outcome.Request, false, nil
	}

	if outcome.Request.Status == whalereinv1.OnlineStatus_received {
		forwardErr := s.ForwardDue(ctx)
		if forwardErr != nil {
			s.metrics.ObserveHostRequest(s.now().Sub(startedAt), true)
			s.logger.Warn("forward due requests failed", "error", forwardErr.Error())
		}
	}
	s.metrics.ObserveHostRequest(s.now().Sub(startedAt), false)
	s.logger.Info("host request accepted",
		"host", s.masker.Host(input.HostID), "device", s.masker.Device(input.DeviceID),
		"request", s.masker.Request(outcome.Request.RequestID),
		"status", store.StatusName(outcome.Request.Status),
		"trace_id", outcome.Request.TraceID)
	return outcome.Request, true, nil
}

// resultEnvelopeOverheadBytes reserves the non-body protojson envelope and
// field overhead so both the encoded bound and the raw allowance are derivable
// from one number without inspecting the encrypted body.
const resultEnvelopeOverheadBytes int64 = 2048

// ResultValueLimit converts an encoded HTTPS result-body bound to the raw
// ciphertext bytes it can carry. It never inspects the body.
func ResultValueLimit(encodedBytes int64) int64 {
	if encodedBytes <= 0 {
		return 0
	}
	if encodedBytes <= resultEnvelopeOverheadBytes {
		return 0
	}
	return (encodedBytes - resultEnvelopeOverheadBytes) / 4 * 3
}

// EncodedResultLimit converts a raw ciphertext allowance to the encoded HTTPS
// result-body bound that carries it (base64 plus the bounded envelope).
func EncodedResultLimit(rawBytes int64) int64 {
	if rawBytes <= 0 {
		return resultEnvelopeOverheadBytes
	}
	groups := rawBytes / 3
	if rawBytes%3 != 0 {
		groups++
	}
	if groups > (math.MaxInt64-resultEnvelopeOverheadBytes)/4 {
		return math.MaxInt64
	}
	return groups*4 + resultEnvelopeOverheadBytes
}

func (s *Service) admissionStamp(ctx context.Context, input SubmitInput) (*store.GrantStamp, error) {
	grant, err := s.grants.AdmissionGrantFor(ctx, input.HostID, input.DeviceID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrForbidden
		}
		return nil, fmt.Errorf("read admission grant: %w", err)
	}
	if !grant.Active {
		return nil, ErrForbidden
	}
	return &store.GrantStamp{
		GrantGeneration:            grant.GrantGeneration,
		RevocationGeneration:       grant.RevocationGeneration,
		DeviceRevocationGeneration: grant.DeviceRevocationGeneration,
		ResultLimitBytes:           s.resultLimitFor(input.Request),
	}, nil
}

// resultLimitFor validates the device's declared encoded result bound against
// Access's retained ciphertext cap and its current HTTP delivery budget. It
// clamps rather than trusts, and it ignores any device-supplied generation.
// resultLimitBase is the raw ciphertext allowance implied by Access's own
// retention and HTTP delivery bounds, independent of the device declaration.
func (s *Service) resultLimitBase() int64 {
	limit := s.cfg.MaxResultBytes
	if delivery := ResultValueLimit(s.cfg.MaxDeliveryBytes); delivery < limit {
		limit = delivery
	}
	return limit
}

func (s *Service) resultLimitFor(request *whalereinv1.HostRequest) int64 {
	limit := s.resultLimitBase()
	declared := request.GetTransport().GetResultMaxBytes()
	if declared > 0 {
		declaredLimit := int64(math.MaxInt64)
		if declared < uint64(math.MaxInt64) {
			declaredLimit = int64(declared)
		}
		if raw := ResultValueLimit(declaredLimit); raw < limit {
			limit = raw
		}
	}
	if limit < 0 {
		return 0
	}
	return limit
}

// RelayEpoch is the only relay contract this build admits.
const RelayEpoch uint32 = 5

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
	// A foreign or missing relay epoch is rejected before admission. A v2
	// frame is never accepted in place of a HostRequest.
	if req.GetProtocolVersion() != s.cfg.ProtocolVersion || req.GetRelayVersion() != RelayEpoch {
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

	hasBody := len(req.GetBody()) > 0
	hasReference := req.GetBodyReference() != nil
	reference := store.Request{}
	switch req.GetKind() {
	case whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE,
		whalereinv1.RequestKind_REQUEST_KIND_QUERY:
		// Post-pairing Noise establishment and session queries were retired
		// from the durable HTTP request route in epoch 5. They must arrive on
		// the MQTT query lane; this route fails before creating a row or
		// forwarding anything.
		return store.Request{}, fmt.Errorf("%w: query and handshake use the MQTT query lane", ErrProtocolVersion)
	case whalereinv1.RequestKind_REQUEST_KIND_COMMAND:
		if hasBody == hasReference {
			return store.Request{}, fmt.Errorf("%w: exactly one of body and body_reference is required", ErrInvalidRequest)
		}
		if req.GetCommandId() == "" {
			return store.Request{}, fmt.Errorf("%w: command_id is required", ErrInvalidRequest)
		}
		if len(req.GetPayloadDigest()) == 0 {
			return store.Request{}, fmt.Errorf("%w: payload_digest is required", ErrInvalidRequest)
		}
		if hasReference {
			// A referenced body is bound by its ciphertext digest. The request
			// must carry the same digest so admission and forwarding cannot
			// disagree about which object the command names.
			if len(req.GetPayloadDigest()) == 0 {
				return store.Request{}, fmt.Errorf("%w: payload_digest is required with body_reference", ErrInvalidRequest)
			}
			encoded, err := proto.Marshal(req.GetBodyReference())
			if err != nil {
				return store.Request{}, ErrInvalidRequest
			}
			if err := s.validateReference(req.GetBodyReference(), whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST,
				req.GetPayloadDigest(), 0); err != nil {
				return store.Request{}, err
			}
			reference.BodyReference = encoded
		}
	default:
		return store.Request{}, fmt.Errorf("%w: request kind is not accepted over HTTP", ErrInvalidRequest)
	}

	return store.Request{
		RequestID:        req.GetRequestId(),
		DeviceSessionID:  input.SessionID,
		HostID:           input.HostID,
		DeviceID:         input.DeviceID,
		CommandID:        req.GetCommandId(),
		PayloadDigest:    append([]byte(nil), req.GetPayloadDigest()...),
		Kind:             req.GetKind(),
		ProtocolVersion:  req.GetProtocolVersion(),
		RelayVersion:     req.GetRelayVersion(),
		RequestBody:      append([]byte(nil), req.GetBody()...),
		BodyReference:    reference.BodyReference,
		ChannelID:        req.GetChannelId(),
		TraceID:          req.GetTraceId(),
		ExpiresAt:        expiresAt,
		CreatedAt:        now,
		UpdatedAt:        now,
		ClientLeaseUntil: leaseDeadline(&expiresAt, now, clientPollLease),
		ClientLeaseState: store.ClientLeaseLive,
	}, nil
}

// validateReference checks the opaque reference structure, purpose, owner,
// exact length/digest binding and expiry. It never inspects ciphertext.
func (s *Service) validateReference(reference *whalereinv1.PayloadReference, purpose whalereinv1.PayloadPurpose, digest []byte, length int64) error {
	if reference == nil {
		return fmt.Errorf("%w: missing payload reference", ErrInvalidRequest)
	}
	if !objectIDPattern.MatchString(reference.GetObjectId()) {
		return fmt.Errorf("%w: invalid object id", ErrInvalidRequest)
	}
	owner := reference.GetOwnerNodeId()
	if owner == "" || len(owner) > 200 || strings.ContainsAny(owner, "|\x00\r\n") {
		return fmt.Errorf("%w: invalid owner node", ErrInvalidRequest)
	}
	if s.cfg.PayloadOwnerID != "" && owner != s.cfg.PayloadOwnerID {
		return fmt.Errorf("%w: reference belongs to another owner", ErrInvalidRequest)
	}
	if reference.GetCiphertextBytes() == 0 {
		return fmt.Errorf("%w: reference length is required", ErrInvalidRequest)
	}
	if length > 0 && reference.GetCiphertextBytes() != uint64(length) {
		return fmt.Errorf("%w: reference length mismatch", ErrInvalidRequest)
	}
	if len(reference.GetSha256()) != sha256.Size {
		return fmt.Errorf("%w: reference digest is not SHA-256", ErrInvalidRequest)
	}
	if len(digest) > 0 && !bytes.Equal(reference.GetSha256(), digest) {
		return ErrDigestMismatch
	}
	if reference.GetPurpose() != purpose {
		return fmt.Errorf("%w: reference purpose mismatch", ErrInvalidRequest)
	}
	if reference.GetExpiresAtUnixMs() <= 0 {
		return fmt.Errorf("%w: reference expiry is required", ErrInvalidRequest)
	}
	return nil
}

var objectIDPattern = regexp.MustCompile(`^[0-9a-f]{32}$`)

// checkReference proves an opaque reference is readable under the authorized
// scope. A missing or unreadable object is reported as unavailable; it is
// never replaced by an inline body Access cannot produce.
func (s *Service) checkReference(ctx context.Context, reference *whalereinv1.PayloadReference, scope store.PayloadCheckScope) error {
	if reference == nil {
		return nil
	}
	if s.payloads == nil {
		return ErrUnavailable
	}
	if err := s.payloads.CheckPayload(ctx, reference, scope); err != nil {
		return fmt.Errorf("%w: referenced ciphertext is not readable", ErrUnavailable)
	}
	return nil
}

func decodeReference(encoded []byte) (*whalereinv1.PayloadReference, error) {
	if len(encoded) == 0 {
		return nil, nil
	}
	var reference whalereinv1.PayloadReference
	if err := proto.Unmarshal(encoded, &reference); err != nil {
		return nil, fmt.Errorf("%w: invalid stored reference", ErrInvalidResult)
	}
	return &reference, nil
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
// envelopeFor builds the exact HostRequest Access forwards. Admission uses the
// same function so the size it accepts is the size the broker later publishes.
func (s *Service) envelopeFor(row store.Request) *whalereinv1.HostRequest {
	envelope := &whalereinv1.HostRequest{
		RequestId:       row.RequestID,
		HostId:          row.HostID,
		DeviceId:        row.DeviceID,
		Kind:            row.Kind,
		CommandId:       row.CommandID,
		PayloadDigest:   row.PayloadDigest,
		ExpiresAtUnixMs: row.ExpiresAt.UnixMilli(),
		TraceId:         row.TraceID,
		ProtocolVersion: row.ProtocolVersion,
		RelayVersion:    RelayEpoch,
		ChannelId:       row.ChannelID,
	}
	// Preserve the representation admitted for this exact row. Access never
	// converts a reference back into ciphertext it cannot read.
	if reference, err := decodeReference(row.BodyReference); err == nil && reference != nil {
		envelope.BodyReference = reference
	} else {
		envelope.Body = row.RequestBody
	}
	if row.Stamp != nil {
		// Forward only durable metadata captured at admission. The device's
		// own grant_generation is overwritten, never relayed.
		envelope.Transport = &whalereinv1.RequestTransportMetadata{
			ResultMaxBytes:       uint64(EncodedResultLimit(row.Stamp.ResultLimitBytes)),
			GrantGeneration:      row.Stamp.GrantGeneration,
			RevocationGeneration: row.Stamp.RevocationGeneration,
		}
	}
	return envelope
}

// checkForwardedSize rejects an admission that cannot fit the forwarded
// envelope budget. It runs before the request is journaled so the caller gets a
// definitive payload_too_large instead of an uncertain forward outcome.
func (s *Service) checkForwardedSize(row store.Request) error {
	if s.cfg.EnvelopeBytes <= 0 {
		return nil
	}
	if int64(proto.Size(s.envelopeFor(row))) > s.cfg.EnvelopeBytes {
		return ErrRequestTooLarge
	}
	return nil
}

func (s *Service) ForwardDue(ctx context.Context) error {
	ctx = store.WithDeviceSessionAuthority(ctx, s.cfg.SessionAuthorityRevision)
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
		rowStarted := time.Now()
		attempt := uint64(row.ForwardAttempts) + 1
		retry := row.ForwardAttempts > 0
		if len(row.BodyReference) > 0 {
			// A referenced request is forwarded only while its exact object is
			// readable. The Host cannot resolve an expired or fenced reference;
			// fail explicitly before publish so the client gets a typed outcome.
			reference, decodeErr := decodeReference(row.BodyReference)
			checkErr := decodeErr
			if checkErr == nil {
				checkErr = s.checkReference(ctx, reference, store.PayloadCheckScope{
					HostID:     row.HostID,
					DeviceID:   row.DeviceID,
					AppSession: row.DeviceSessionID,
					ChannelID:  row.ChannelID,
					Purpose:    whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST,
				})
			}
			if checkErr != nil {
				if _, applyErr := s.store.ApplyHostResult(ctx, store.HostResultUpdate{
					RequestID: row.RequestID, HostID: row.HostID, DeviceID: row.DeviceID,
					Status: whalereinv1.OnlineStatus_rejected, ErrorCode: whalereinv1.ErrorCode_unavailable,
					At: now,
				}); applyErr != nil {
					return applyErr
				}
				s.metrics.ResultRejected()
				s.logger.Warn("referenced request no longer readable",
					"request", s.masker.Request(row.RequestID), "trace_id", row.TraceID)
				continue
			}
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
		envelope := s.envelopeFor(row)
		publish := func() error { return s.broker.PublishRequest(ctx, row.HostID, envelope) }
		var err error
		if s.sessions != nil {
			err = s.sessions.PublishDuringDeviceSession(ctx, row.RequestID, publish)
		} else {
			err = publish()
		}
		if errors.Is(err, store.ErrDeviceSessionEnded) || errors.Is(err, store.ErrNotFound) {
			continue
		}
		if err != nil {
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
				if retry {
					s.observeCommandRetry(row, attempt, observation.OutcomeError, rowStarted)
				}
				s.logger.Warn("logical transport preflight failed", "trace_id", row.TraceID, "code", code.String(), "bytes", len(row.RequestBody))
				continue
			}
			s.metrics.RouteError()
			if retry {
				s.observeCommandRetry(row, attempt, observation.OutcomeError, rowStarted)
			}
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
		if retry {
			s.observeCommandRetry(row, attempt, observation.OutcomeOK, rowStarted)
		}
		s.logger.Info("host request forwarded",
			"host", s.masker.Host(row.HostID),
			"request", s.masker.Request(row.RequestID),
			"status", store.StatusName(row.Status), "trace_id", row.TraceID, "kind", row.Kind, "bytes", len(row.RequestBody))
	}
	return nil
}

// observeCommandRetry records one transport attempt that follows an earlier
// unconfirmed publication. The stable request identity is preserved and the
// attempt distinguishes the retry.
func (s *Service) observeCommandRetry(row store.Request, attempt uint64, outcome observation.Outcome, started time.Time) {
	s.observeCommand(commandScope(row.HostID, row.DeviceID), row.RequestID, attempt, observation.CommandRecover, outcome, len(row.RequestBody))
	if s.metrics != nil {
		s.metrics.ObserveAccessStage("command_recover", time.Since(started), string(outcome))
	}
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

// Heartbeat renews only the exact live, non-terminal, unexpired request for
// the authenticated device and session. It returns a typed non-renewal
// (404 not found, 410 expired, 409 conflict) instead of a silent success and
// never extends expires_at.
func (s *Service) Heartbeat(ctx context.Context, deviceID, sessionID, requestID string) (store.Request, error) {
	ctx = store.WithDeviceSessionAuthority(ctx, s.cfg.SessionAuthorityRevision)
	outcome, err := s.store.HeartbeatRequest(ctx, deviceID, sessionID, requestID, clientPollLease)
	if err != nil {
		return store.Request{}, fmt.Errorf("heartbeat request: %w", err)
	}
	if outcome.Renewed {
		return outcome.Request, nil
	}
	switch outcome.Failure {
	case store.HeartbeatNotFound, store.HeartbeatDeviceForeign:
		return store.Request{}, ErrNotFound
	case store.HeartbeatHardDeadline, store.HeartbeatLeaseElapsed:
		return store.Request{}, ErrExpired
	case store.HeartbeatTerminal, store.HeartbeatSessionEnded:
		return store.Request{}, ErrConflict
	default:
		return store.Request{}, ErrConflict
	}
}

// readableResultReference returns the stored result reference only while its
// exact object is readable under the recorded scope. An opaque reference is
// never advertised from metadata alone.
func (s *Service) readableResultReference(ctx context.Context, row store.Request) *whalereinv1.PayloadReference {
	reference, err := decodeReference(row.ResultReference)
	if err != nil || reference == nil {
		return nil
	}
	scope := store.PayloadCheckScope{
		HostID:     row.HostID,
		DeviceID:   row.DeviceID,
		AppSession: row.DeviceSessionID,
		ChannelID:  row.ChannelID,
		RequestID:  row.RequestID,
		Purpose:    whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
	}
	if err := s.checkReference(ctx, reference, scope); err != nil {
		return nil
	}
	return reference
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
	ctx = store.WithDeviceSessionAuthority(ctx, s.cfg.SessionAuthorityRevision)
	now := s.now()
	expired, err := s.store.ExpireClientLeases(ctx, clientSweepBatch)
	if err != nil {
		s.logger.Warn("client lease expiry failed", "error", err.Error())
		return
	}
	if len(expired) > 0 && s.metrics != nil {
		s.metrics.RequestClaimStale()
	}
	expiredIDs := make(map[string]bool, len(expired))
	for _, row := range expired {
		expiredIDs[row.RequestID] = true
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
		if expiredIDs[row.RequestID] {
			// Only a lease that this sweep moved to expired is a new terminal
			// disposition. A replayed pending cancellation keeps its earlier
			// terminal fact and must not inflate the count every pass.
			s.observeCommand(commandScope(row.HostID, row.DeviceID), row.RequestID, uint64(row.ForwardAttempts)+1, observation.CommandTerminal, observation.OutcomeExpired, 0)
			if s.metrics != nil {
				s.metrics.ObserveAccessStageDisposition("command_terminal", string(observation.OutcomeExpired))
			}
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
	reason := "client_poll_expired"
	if s.sessions != nil {
		ended, err := s.sessions.SessionCancellationReason(ctx, row.RequestID)
		if err != nil {
			return
		}
		if ended {
			reason = "cloud_session_ended"
		}
	}
	payload, err := json.Marshal(cancellationPayload{
		Event:           "request_stale",
		RequestID:       row.RequestID,
		HostID:          row.HostID,
		DeviceID:        row.DeviceID,
		CommandID:       row.CommandID,
		Digest:          cancellationDigest(row),
		ExpiresAtUnixMs: row.ExpiresAt.UnixMilli(),
		Reason:          reason,
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

// NoticeRetryDelay is the capped backoff between publication attempts for one
// durable intent. Duplicate notices are harmless, so retries stay bounded in
// frequency rather than being dropped.
func NoticeRetryDelay(attempts int) time.Duration {
	if attempts < 1 {
		attempts = 1
	}
	if attempts > 6 {
		attempts = 6
	}
	delay := 500 * time.Millisecond << uint(attempts-1)
	if delay > 30*time.Second {
		delay = 30 * time.Second
	}
	return delay
}

// PublishDueNotices claims a bounded batch of durable notice intents and
// publishes one packet-sized RequestStateNotice each. Publication happens
// after the authoritative commit and coalesces to the latest stored state.
func (s *Service) PublishDueNotices(ctx context.Context) error {
	now := s.now()
	batch := s.cfg.NoticeBatch
	if batch <= 0 {
		batch = 50
	}
	rows, err := s.store.ClaimNoticeIntents(ctx, now, batch, s.cfg.NoticeLease)
	if err != nil {
		return err
	}
	for _, row := range rows {
		if ctx.Err() != nil {
			return nil
		}
		updatedAt := row.UpdatedAt
		if row.NoticeUpdatedAt != nil {
			updatedAt = *row.NoticeUpdatedAt
		}
		if row.DeviceID == "" || row.NoticeStatus == whalereinv1.OnlineStatus_online_status_unspecified {
			// An Access-internal control row has no device receiver. Record the
			// intent as handled instead of retrying a notice nobody can read.
			if _, err := s.store.MarkNoticePublished(ctx, row.RequestID, row.NoticeStatus, updatedAt, now); err != nil {
				return err
			}
			continue
		}
		notice := &whalereinv1.RequestStateNotice{
			RequestId:       row.RequestID,
			HostId:          row.HostID,
			DeviceId:        row.DeviceID,
			Status:          row.NoticeStatus,
			CommandId:       row.CommandID,
			UpdatedAtUnixMs: updatedAt.UnixMilli(),
			RelayVersion:    RelayEpoch,
		}
		if reference := s.readableResultReference(ctx, row); reference != nil {
			notice.ResultReference = reference
		}
		if err := s.broker.PublishDeviceNotice(ctx, row.HostID, row.DeviceID, notice); err != nil {
			if s.metrics != nil {
				s.metrics.NoticePublishRetry()
			}
			next := now.Add(NoticeRetryDelay(row.NoticeAttempts))
			if markErr := s.store.MarkNoticeFailure(ctx, row.RequestID, now, next); markErr != nil {
				s.logger.Warn("notice retry not recorded",
					"request", s.masker.Request(row.RequestID), "error", markErr.Error())
			}
			s.logger.Warn("request state notice publish failed",
				"host", s.masker.Host(row.HostID), "device", s.masker.Device(row.DeviceID),
				"request", s.masker.Request(row.RequestID), "attempt", row.NoticeAttempts,
				"status", store.StatusName(row.NoticeStatus))
			continue
		}
		if _, err := s.store.MarkNoticePublished(ctx, row.RequestID, row.NoticeStatus, updatedAt, now); err != nil {
			s.logger.Warn("notice publication not recorded",
				"request", s.masker.Request(row.RequestID), "error", err.Error())
		}
	}
	s.observeNoticeBacklog(ctx)
	return nil
}

func (s *Service) observeNoticeBacklog(ctx context.Context) {
	if s.metrics == nil {
		return
	}
	stats, err := s.store.NoticeStats(ctx)
	if err != nil {
		return
	}
	s.metrics.NoticeBacklog(stats.Pending, stats.OldestAge.Seconds())
}

// RunNotices drives durable notice publication until ctx is cancelled. It
// runs one pass immediately so a restart drains committed intents before the
// first interval.
func (s *Service) RunNotices(ctx context.Context) error {
	interval := s.cfg.NoticeInterval
	if interval <= 0 {
		interval = time.Second
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	publish := func() {
		if err := s.PublishDueNotices(ctx); err != nil && ctx.Err() == nil {
			s.logger.Warn("notice loop failed", "error", err.Error())
		}
	}
	publish()
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
			publish()
		}
	}
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
	ctx = store.WithDeviceSessionAuthority(ctx, s.cfg.SessionAuthorityRevision)
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
	if !terminalStatus(req.Status) && s.sessions != nil {
		ended, err := s.sessions.SessionCancellationReason(ctx, req.RequestID)
		if err != nil {
			return store.Request{}, err
		}
		if ended {
			req.Status = whalereinv1.OnlineStatus_unknown
			return req, nil
		}
	}
	if !terminalStatus(req.Status) && !s.now().Before(req.ExpiresAt) {
		// A request that never reached a terminal state is expired, not still
		// host_accepted. Returning the row would make the CLI poll a status
		// that can never change.
		s.observeCommand(commandScope(req.HostID, req.DeviceID), req.RequestID, uint64(req.ForwardAttempts)+1, observation.CommandTerminal, observation.OutcomeExpired, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStageDisposition("command_terminal", string(observation.OutcomeExpired))
		}
		return req, ErrExpired
	}
	if req.ResultReference != nil && s.readableResultReference(ctx, req) == nil {
		req.ResultReference = nil
	}
	if terminalStatus(req.Status) {
		outcome := commandTerminalOutcome(req.Status)
		s.observeCommand(commandScope(req.HostID, req.DeviceID), req.RequestID, uint64(req.ForwardAttempts)+1, observation.CommandTerminal, outcome, len(req.ResultBody))
		if s.metrics != nil {
			s.metrics.ObserveAccessStageDisposition("command_terminal", string(outcome))
		}
	}
	return req, nil
}

// Release authorizes one ciphertext read in a single durable decision and
// applies the per-kind release policy. QUERY ciphertext fails closed with
// ErrReleaseFenced; COMMAND and Access-authored rows keep their metadata-only
// recovery semantics while their body is withheld.
func (s *Service) Release(ctx context.Context, deviceID, sessionID, requestID string, deliveryLimitBytes int64) (store.Request, error) {
	ctx = store.WithDeviceSessionAuthority(ctx, s.cfg.SessionAuthorityRevision)
	outcome, err := s.store.ReleaseRequest(ctx, store.ReleaseScope{
		DeviceID:           deviceID,
		SessionID:          sessionID,
		RequestID:          requestID,
		DeliveryLimitBytes: deliveryLimitBytes,
	})
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return store.Request{}, ErrNotFound
		}
		return store.Request{}, fmt.Errorf("release request: %w", err)
	}
	req := outcome.Request
	if !outcome.SessionValid {
		return store.Request{}, ErrForbidden
	}
	if !terminalStatus(req.Status) && !s.now().Before(req.ExpiresAt) {
		return req, ErrExpired
	}
	allowed := outcome.StampPresent && outcome.GrantActive && outcome.StampCurrent &&
		!outcome.BodiesCleared && outcome.WithinResultBound && outcome.WithinDelivery
	fencedKind := req.Kind == whalereinv1.RequestKind_REQUEST_KIND_QUERY ||
		req.Kind == whalereinv1.RequestKind_REQUEST_KIND_COMMAND
	if fencedKind && !allowed {
		s.metrics.ReleaseDenied()
		s.logger.Warn("ciphertext release fenced",
			"host", s.masker.Host(req.HostID), "device", s.masker.Device(req.DeviceID),
			"request", s.masker.Request(req.RequestID), "kind", req.Kind,
			"stamp_present", outcome.StampPresent, "grant_active", outcome.GrantActive,
			"stamp_current", outcome.StampCurrent, "bodies_cleared", outcome.BodiesCleared,
			"within_bound", outcome.WithinResultBound, "within_delivery", outcome.WithinDelivery,
			"bytes", len(req.ResultBody))
	}
	switch req.Kind {
	case whalereinv1.RequestKind_REQUEST_KIND_QUERY:
		// Attachment pulls are queries too: an unfenced query never releases
		// ciphertext, including after a later re-grant. A query without its
		// stamp fails closed even when no body was stored yet.
		if !allowed {
			return store.Request{}, ErrReleaseFenced
		}
	case whalereinv1.RequestKind_REQUEST_KIND_COMMAND:
		if !allowed {
			req.ResultBody = nil
			req.ResultReference = nil
		}
	}
	// The stored reference is returned only while its exact object is
	// readable. Metadata alone never advertises unavailable ciphertext.
	if len(req.ResultReference) > 0 {
		if s.readableResultReference(ctx, req) == nil {
			req.ResultReference = nil
		}
	}
	return req, nil
}

// resultIngestLimit bounds an incoming result by the stored request stamp, the
// retained ciphertext cap and the current HTTP delivery budget. A legacy row
// without a stamp falls back to the current Access limits; it can never widen
// beyond them.
func firstNonEmptyString(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
}

func (s *Service) resultIngestLimit(row store.Request) int64 {
	limit := s.resultLimitBase()
	if row.Stamp != nil && row.Stamp.ResultLimitBytes < limit {
		limit = row.Stamp.ResultLimitBytes
	}
	if limit < 0 {
		return 0
	}
	return limit
}

// HandleHostResult validates a host result against the topic host and updates
// only the matching request row using the allowed conditional transitions.

func (s *Service) HandleHostResult(ctx context.Context, topicHost string, result *whalereinv1.HostResult) error {
	started := time.Now()
	ctx = store.WithDeviceSessionAuthority(ctx, s.cfg.SessionAuthorityRevision)
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
	if result.GetRelayVersion() != RelayEpoch {
		return fmt.Errorf("%w: unsupported relay epoch", ErrProtocolVersion)
	}
	// A result carries exactly one representation when a body exists. A
	// packet-sized terminal result may carry neither when the bulk body could
	// not be admitted.
	hasBody := len(result.GetResultBody()) > 0
	hasReference := result.GetResultReference() != nil
	if hasBody && hasReference {
		return fmt.Errorf("%w: result carries both representations", ErrInvalidResult)
	}
	var referenceBytes []byte
	if hasReference {
		reference := result.GetResultReference()
		if err := s.validateReference(reference, whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT, nil, 0); err != nil {
			return fmt.Errorf("%w: %v", ErrInvalidResult, err)
		}
		encoded, err := proto.Marshal(reference)
		if err != nil {
			return fmt.Errorf("%w: encode result reference", ErrInvalidResult)
		}
		referenceBytes = encoded
		// Persist the reference opaquely even when its object is currently
		// unreadable. The terminal outcome must not be lost to storage
		// availability; GET and notices return the reference only while it is
		// actually readable.
		if err := s.checkReference(ctx, reference, store.PayloadCheckScope{
			HostID:     row.HostID,
			DeviceID:   row.DeviceID,
			AppSession: row.DeviceSessionID,
			ChannelID:  firstNonEmptyString(result.GetChannelId(), row.ChannelID),
			RequestID:  row.RequestID,
			Purpose:    whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
		}); err != nil {
			s.metrics.ResultRejected()
			s.logger.Warn("host result reference is not readable; keeping outcome",
				"host", s.masker.Host(topicHost), "device", s.masker.Device(row.DeviceID),
				"request", s.masker.Request(row.RequestID), "trace_id", row.TraceID)
		}
	}
	resultBody := append([]byte(nil), result.GetResultBody()...)
	if limit := s.resultIngestLimit(row); int64(len(resultBody)) > limit {
		// Refusal of the bulk body never changes the authoritative outcome,
		// error code or command identity. It becomes a packet-sized terminal
		// result with scoped recovery guidance.
		s.metrics.ResultRejected()
		s.logger.Warn("host result body refused; outcome retained",
			"host", s.masker.Host(topicHost), "device", s.masker.Device(row.DeviceID),
			"request", s.masker.Request(row.RequestID), "kind", row.Kind,
			"bytes", len(resultBody), "limit", limit, "trace_id", row.TraceID)
		resultBody = nil
	}
	applied, err := s.store.ApplyHostResult(ctx, store.HostResultUpdate{
		RequestID:       row.RequestID,
		HostID:          topicHost,
		DeviceID:        row.DeviceID,
		Status:          result.GetStatus(),
		ResultBody:      resultBody,
		ResultReference: referenceBytes,
		ErrorCode:       result.GetErrorCode(),
		ErrorMessage:    result.GetErrorMessage(),
		ChannelID:       result.GetChannelId(),
		At:              s.now(),
	})
	if err != nil {
		return fmt.Errorf("apply host result: %w", err)
	}
	attempt := uint64(row.ForwardAttempts) + 1
	if applied {
		s.observeCommand(commandScope(row.HostID, row.DeviceID), row.RequestID, attempt, observation.CommandReceipt, observation.OutcomeOK, len(resultBody)+len(referenceBytes))
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("command_receipt", time.Since(started), string(observation.OutcomeOK))
		}
		s.metrics.ObserveHostResult(result.GetStatus(), result.GetErrorCode())
		s.logger.Info("host result stored",
			"host", s.masker.Host(topicHost),
			"request", s.masker.Request(row.RequestID),
			"status", store.StatusName(result.GetStatus()),
			"trace_id", row.TraceID, "kind", row.Kind, "code", result.GetErrorCode().String(), "layer", "host", "duration_ms", s.now().Sub(row.CreatedAt).Milliseconds())
	} else {
		// A result for an already terminal or expired row is evidence of a late
		// delivery, not a state transition. It cannot turn an observed timeout
		// into success.
		s.observeCommand(commandScope(row.HostID, row.DeviceID), row.RequestID, attempt, observation.CommandReceipt, observation.OutcomeLate, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("command_receipt", time.Since(started), string(observation.OutcomeLate))
		}
		s.logger.Debug("host result ignored after terminal or expiry",
			"request", s.masker.Request(row.RequestID))
	}
	return nil
}

// observeCommand emits one durable-command boundary. The request identity is
// stable across retries; attempt is the transport attempt.
func (s *Service) observeCommand(scope, request string, attempt uint64, stage observation.CommandStage, outcome observation.Outcome, bytes int) {
	if s.observation == nil {
		return
	}
	event := observation.CommandEvent{
		Scope: scope, Request: request, Attempt: attempt, Stage: stage, Durable: true, Outcome: outcome,
	}
	if bytes > 0 {
		event.Bytes = uint64(bytes)
		event.HasBytes = true
	}
	s.observation.Command(event)
}

func commandAdmissionOutcome(err error) observation.Outcome {
	switch {
	case err == nil:
		return observation.OutcomeOK
	case errors.Is(err, ErrExpired):
		return observation.OutcomeExpired
	default:
		return observation.OutcomeError
	}
}

func commandTerminalOutcome(status whalereinv1.OnlineStatus) observation.Outcome {
	switch status {
	case whalereinv1.OnlineStatus_succeeded:
		return observation.OutcomeOK
	case whalereinv1.OnlineStatus_failed, whalereinv1.OnlineStatus_rejected:
		return observation.OutcomeError
	case whalereinv1.OnlineStatus_host_offline, whalereinv1.OnlineStatus_unknown:
		return observation.OutcomeTimeout
	default:
		return observation.OutcomeError
	}
}

// commandScope binds a durable request token to the authenticated host/device
// pair. A same-id command from another device never joins this operation.
func commandScope(hostID, deviceID string) string {
	return hostID + "\x00" + deviceID
}
