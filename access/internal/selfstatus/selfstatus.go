// Package selfstatus owns the bounded HTTPS bootstrap relay used by a Device
// that lost the last ordinary grant's receipt.
//
// The exchange is a service-only lane: an unauthenticated HTTPS submission is
// admitted only for the exact SELF_STATUS purpose, relayed to one Host on a
// private MQTT service topic, and answered by that Host. Access never answers a
// status itself, never persists a row and never creates ordinary broker
// admission. A Host reply is accepted only for the exact one-shot tuple that
// created the waiter; every other reply is indistinguishable from an
// inaccessible tuple and leaves the caller pending.
package selfstatus

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"math"
	"regexp"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// Frame and admission bounds. Every value is a hard ceiling; a deployment may
// lower a limit through Config but cannot raise it past these constants.
const (
	// MaxFrameBytes bounds the serialized SelfStatusRequest body.
	MaxFrameBytes = 512
	// MaxRequestBytes bounds the HTTPS envelope that carries the frame.
	MaxRequestBytes = 2048
	// MinChallengeBytes and MaxChallengeBytes bound the fresh client challenge.
	MinChallengeBytes = 16
	MaxChallengeBytes = 64
	// MaxGrantIDBytes bounds the claimed grant identity.
	MaxGrantIDBytes = 128
	// ExchangeTimeout bounds one full HTTPS -> MQTT -> Host -> HTTPS exchange.
	ExchangeTimeout = 5 * time.Second
	// ReplayTTL is how long a used challenge stays unusable.
	ReplayTTL = 10 * time.Minute
	// MaxInflight bounds concurrent waiters across the node.
	MaxInflight = 256
	// MaxInflightPerHost bounds concurrent waiters for one Host.
	MaxInflightPerHost = 32
	// MaxInflightPerCaller bounds concurrent waiters for one caller.
	MaxInflightPerCaller = 8
	// MaxChallenges bounds the in-memory replay tombstones.
	MaxChallenges = 4096
)

// Typed refusals. None of them carries a peer identity, topic or frame byte, so
// a log line or HTTP error cannot disclose the claimed tuple.
var (
	// ErrInvalid is a malformed frame, identifier or oversize body.
	ErrInvalid = errors.New("self-status: invalid request")
	// ErrInaccessible is the single answer for an unknown Host and for a Host
	// that refused the tuple. The two are deliberately indistinguishable.
	ErrInaccessible = errors.New("self-status: tuple inaccessible")
	// ErrHostOffline is a known Host with no current connection.
	ErrHostOffline = errors.New("self-status: host offline")
	// ErrHostUnavailable is an unreconciled presence view, a publish failure or
	// a Host reply that does not prove any status.
	ErrHostUnavailable = errors.New("self-status: host unavailable")
	// ErrTimeout is an exchange whose bounded wait elapsed without a reply.
	ErrTimeout = errors.New("self-status: reply timeout")
	// ErrBusy is a per-node, per-Host or per-caller admission refusal.
	ErrBusy = errors.New("self-status: admission limit reached")
	// ErrReplay is a challenge that was already submitted inside ReplayTTL.
	ErrReplay = errors.New("self-status: challenge already used")
)

var resourceIDPattern = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,128}$`)

// Presence is the Broker-derived Host directory. known=false means Access has
// no reconciled view, never that the Host is online.
type Presence interface {
	HostPresence(hostID string) (online, known bool)
}

// Publisher publishes one complete MQTT 5 message.
type Publisher interface {
	PublishMessage(ctx context.Context, message broker.Message) error
}

// Input is one bounded HTTPS self-status submission.
type Input struct {
	// HostID is the pinned opaque Host route from the request path.
	HostID string
	// GrantID is the claimed Host-issued grant identity.
	GrantID string
	// Challenge is the fresh random value the Host must echo.
	Challenge []byte
	// Caller is an in-memory admission key. It is never persisted or logged.
	Caller string
	// TraceID is the bounded HTTP diagnostic correlation id.
	TraceID string
}

// Config bounds the relay. Zero values take the package ceilings.
type Config struct {
	ExchangeTimeout      time.Duration
	ReplayTTL            time.Duration
	MaxInflight          int
	MaxInflightPerHost   int
	MaxInflightPerCaller int
	MaxChallenges        int
	MaxFrameBytes        int
	Now                  func() time.Time
	Random               io.Reader
}

// Dependencies wires the self-status relay.
type Dependencies struct {
	Presence  Presence
	Publisher Publisher
	Config    Config
	Metrics   *diagnostics.Metrics
	Logger    *slog.Logger
}

type replayKey struct {
	host      string
	grant     string
	challenge [32]byte
}

type waiter struct {
	requestID string
	hostID    string
	grantID   string
	challenge []byte
	caller    string
	deadline  time.Time
	result    chan outcome
}

type outcome struct {
	status *whalereinv1.SelfStatus
	err    error
}

// Service owns the transient self-status waiters. It holds no durable state;
// an Access restart drops every waiter and the caller retries (pending).
type Service struct {
	presence  Presence
	publisher Publisher
	cfg       Config
	metrics   *diagnostics.Metrics
	logger    *slog.Logger
	random    io.Reader

	mu         sync.Mutex
	waiters    map[string]*waiter
	inflight   int
	byHost     map[string]int
	byCaller   map[string]int
	challenges map[replayKey]time.Time
}

// New builds the bounded self-status relay.
func New(deps Dependencies) (*Service, error) {
	if deps.Presence == nil || deps.Publisher == nil {
		return nil, errors.New("selfstatus: presence and publisher are required")
	}
	cfg := deps.Config
	if cfg.ExchangeTimeout <= 0 {
		cfg.ExchangeTimeout = ExchangeTimeout
	}
	if cfg.ReplayTTL <= 0 {
		cfg.ReplayTTL = ReplayTTL
	}
	if cfg.MaxInflight <= 0 {
		cfg.MaxInflight = MaxInflight
	}
	if cfg.MaxInflightPerHost <= 0 {
		cfg.MaxInflightPerHost = MaxInflightPerHost
	}
	if cfg.MaxInflightPerCaller <= 0 {
		cfg.MaxInflightPerCaller = MaxInflightPerCaller
	}
	if cfg.MaxChallenges <= 0 {
		cfg.MaxChallenges = MaxChallenges
	}
	if cfg.MaxFrameBytes <= 0 {
		cfg.MaxFrameBytes = MaxFrameBytes
	}
	if cfg.Now == nil {
		cfg.Now = func() time.Time { return time.Now().UTC() }
	}
	if cfg.Random == nil {
		cfg.Random = rand.Reader
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	if deps.Logger == nil {
		deps.Logger = slog.Default()
	}
	return &Service{
		presence:   deps.Presence,
		publisher:  deps.Publisher,
		cfg:        cfg,
		metrics:    deps.Metrics,
		logger:     deps.Logger,
		random:     cfg.Random,
		waiters:    map[string]*waiter{},
		byHost:     map[string]int{},
		byCaller:   map[string]int{},
		challenges: map[replayKey]time.Time{},
	}, nil
}

// MaxFrameSize reports the configured frame ceiling.
func (s *Service) MaxFrameSize() int { return s.cfg.MaxFrameBytes }

// ValidateFrame enforces the exact status-frame contract on a decoded
// SelfStatusRequest: a bounded opaque grant claim and a fresh challenge.
func ValidateFrame(grantID string, challenge []byte) error {
	if !resourceIDPattern.MatchString(grantID) {
		return fmt.Errorf("%w: grant id", ErrInvalid)
	}
	if len(challenge) < MinChallengeBytes || len(challenge) > MaxChallengeBytes {
		return fmt.Errorf("%w: challenge length", ErrInvalid)
	}
	return nil
}

// Submit relays one bounded status frame and waits for the exact Host reply.
// It never synthesizes a status: only a matching Host reply yields a value.
func (s *Service) Submit(ctx context.Context, input Input) (*whalereinv1.SelfStatus, error) {
	started := s.cfg.Now()
	if !resourceIDPattern.MatchString(input.HostID) {
		return nil, fmt.Errorf("%w: host id", ErrInvalid)
	}
	if err := ValidateFrame(input.GrantID, input.Challenge); err != nil {
		return nil, err
	}
	challenge := append([]byte(nil), input.Challenge...)
	online, known := s.presence.HostPresence(input.HostID)
	if !known {
		// Access has no reconciled Host directory, so it cannot prove the Host
		// absent; an unknown id and an offline Host both stay bounded pending.
		return nil, ErrHostUnavailable
	}
	if !online {
		return nil, ErrHostOffline
	}

	now := s.cfg.Now()
	s.mu.Lock()
	s.pruneLocked(now)
	key := replayKey{host: input.HostID, grant: input.GrantID, challenge: sha256.Sum256(challenge)}
	if _, used := s.challenges[key]; used {
		s.mu.Unlock()
		s.observe("self_status_submit", started, "replay")
		return nil, ErrReplay
	}
	caller := input.Caller
	if caller == "" {
		caller = "unknown"
	}
	if s.inflight >= s.cfg.MaxInflight ||
		s.byHost[input.HostID] >= s.cfg.MaxInflightPerHost ||
		s.byCaller[caller] >= s.cfg.MaxInflightPerCaller {
		s.mu.Unlock()
		s.observe("self_status_submit", started, "busy")
		return nil, ErrBusy
	}
	requestID, err := s.newRequestIDLocked()
	if err != nil {
		s.mu.Unlock()
		s.logger.Warn("self-status request id unavailable")
		return nil, ErrHostUnavailable
	}
	w := &waiter{
		requestID: requestID,
		hostID:    input.HostID,
		grantID:   input.GrantID,
		challenge: challenge,
		caller:    caller,
		deadline:  now.Add(s.cfg.ExchangeTimeout),
		result:    make(chan outcome, 1),
	}
	s.waiters[requestID] = w
	s.inflight++
	s.byHost[input.HostID]++
	s.byCaller[caller]++
	s.challenges[key] = now.Add(s.cfg.ReplayTTL)
	s.mu.Unlock()

	request := &whalereinv1.HostRequest{
		RequestId:       requestID,
		HostId:          input.HostID,
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_SELF_STATUS,
		ExpiresAtUnixMs: w.deadline.UnixMilli(),
		ProtocolVersion: 1,
		RelayVersion:    broker.RelayEpoch,
		Body:            mustMarshalFrame(input.GrantID, challenge),
	}
	if len(input.TraceID) > 0 && len(input.TraceID) <= 64 {
		request.TraceId = input.TraceID
	}
	payload, err := proto.Marshal(request)
	if err != nil || len(payload) > s.cfg.MaxFrameBytes+256 {
		s.abortWaiter(w, key)
		return nil, ErrInvalid
	}
	expiry := remainingSeconds(s.cfg.ExchangeTimeout)
	if expiry == nil {
		s.abortWaiter(w, key)
		return nil, ErrHostUnavailable
	}
	if err := s.publisher.PublishMessage(ctx, broker.Message{
		Topic:   broker.SelfStatusRequestTopic(input.HostID),
		Payload: payload,
		QoS:     1,
		Properties: broker.MessageProperties{
			CorrelationData: []byte(requestID),
			ResponseTopic:   broker.SelfStatusReplyTopic(input.HostID),
			MessageExpiry:   expiry,
		},
	}); err != nil {
		// The frame never reached the Host, so this challenge was not consumed:
		// a bounded retry must remain possible.
		s.abortWaiter(w, key)
		s.logger.Warn("self-status publish failed", "error", err.Error())
		s.observe("self_status_submit", started, "unavailable")
		return nil, ErrHostUnavailable
	}

	timer := time.NewTimer(s.cfg.ExchangeTimeout)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		s.removeWaiter(w)
		s.observe("self_status_submit", started, "canceled")
		return nil, ErrHostUnavailable
	case <-timer.C:
		s.removeWaiter(w)
		s.observe("self_status_submit", started, "timeout")
		return nil, ErrTimeout
	case result := <-w.result:
		if result.err != nil {
			s.observe("self_status_submit", started, outcomeName(result.err))
			return nil, result.err
		}
		s.observe("self_status_submit", started, "ok")
		return result.status, nil
	}
}

// Complete resolves one Host reply for the exact waiter tuple. It returns true
// only when this reply settled the exchange; a foreign, stale or malformed
// reply returns false and the waiter stays pending until its deadline.
func (s *Service) Complete(_ context.Context, topicHost string, result *whalereinv1.HostResult) bool {
	if result == nil || result.GetRequestId() == "" {
		return false
	}
	if result.GetHostId() != "" && result.GetHostId() != topicHost {
		return false
	}
	s.mu.Lock()
	w := s.waiters[result.GetRequestId()]
	if w == nil || w.hostID != topicHost {
		s.mu.Unlock()
		return false
	}
	var resolved outcome
	switch result.GetStatus() {
	case whalereinv1.OnlineStatus_rejected:
		// The Host refused to disclose any status for this tuple. Access maps
		// it to the same answer an unknown Host produces.
		resolved.err = ErrInaccessible
	case whalereinv1.OnlineStatus_succeeded:
		status, ok := matchStatus(w, result.GetResultBody())
		if !ok {
			// A reply that does not bind the exact claim and challenge is not
			// this exchange's answer; leave the waiter for a genuine reply or
			// the bounded timeout.
			s.mu.Unlock()
			return false
		}
		resolved.status = status
	default:
		resolved.err = ErrHostUnavailable
	}
	delete(s.waiters, w.requestID)
	s.releaseLocked(w)
	s.mu.Unlock()
	select {
	case w.result <- resolved:
	default:
	}
	return true
}

// matchStatus validates that a succeeded reply carries exactly this waiter's
// grant claim and challenge.
func matchStatus(w *waiter, body []byte) (*whalereinv1.SelfStatus, bool) {
	if len(body) == 0 || len(body) > MaxFrameBytes {
		return nil, false
	}
	var status whalereinv1.SelfStatus
	if err := proto.Unmarshal(body, &status); err != nil {
		return nil, false
	}
	if status.GetGrantId() != w.grantID {
		return nil, false
	}
	if len(status.GetChallenge()) != len(w.challenge) ||
		subtle.ConstantTimeCompare(status.GetChallenge(), w.challenge) != 1 {
		return nil, false
	}
	return &status, true
}

func (s *Service) removeWaiter(w *waiter) {
	s.mu.Lock()
	if s.waiters[w.requestID] == w {
		delete(s.waiters, w.requestID)
		s.releaseLocked(w)
	}
	s.mu.Unlock()
}

// abortWaiter releases a waiter whose frame never reached the Host and returns
// the challenge to the replay set, so a bounded retry is possible.
func (s *Service) abortWaiter(w *waiter, key replayKey) {
	s.mu.Lock()
	if s.waiters[w.requestID] == w {
		delete(s.waiters, w.requestID)
		s.releaseLocked(w)
	}
	delete(s.challenges, key)
	s.mu.Unlock()
}

func (s *Service) releaseLocked(w *waiter) {
	if s.inflight > 0 {
		s.inflight--
	}
	if s.byHost[w.hostID] > 1 {
		s.byHost[w.hostID]--
	} else {
		delete(s.byHost, w.hostID)
	}
	if s.byCaller[w.caller] > 1 {
		s.byCaller[w.caller]--
	} else {
		delete(s.byCaller, w.caller)
	}
}

// pruneLocked drops expired challenge tombstones and, at the ceiling, the
// oldest ones first so the replay set is always bounded.
func (s *Service) pruneLocked(now time.Time) {
	for key, until := range s.challenges {
		if !now.Before(until) {
			delete(s.challenges, key)
		}
	}
	for len(s.challenges) >= s.cfg.MaxChallenges {
		var oldest replayKey
		var oldestUntil time.Time
		first := true
		for key, until := range s.challenges {
			if first || until.Before(oldestUntil) {
				oldest, oldestUntil, first = key, until, false
			}
		}
		if first {
			return
		}
		delete(s.challenges, oldest)
	}
}

// newRequestIDLocked returns a fresh opaque correlation id. A random collision
// is retried; the map is under the caller's lock.
func (s *Service) newRequestIDLocked() (string, error) {
	for attempt := 0; attempt < 4; attempt++ {
		raw := make([]byte, 16)
		if _, err := io.ReadFull(s.random, raw); err != nil {
			return "", err
		}
		id := hex.EncodeToString(raw)
		if _, exists := s.waiters[id]; !exists {
			return id, nil
		}
	}
	return "", errors.New("selfstatus: request id collision")
}

func mustMarshalFrame(grantID string, challenge []byte) []byte {
	body, err := proto.Marshal(&whalereinv1.SelfStatusRequest{GrantId: grantID, Challenge: challenge})
	if err != nil {
		// A fixed two-field message cannot fail to marshal.
		return nil
	}
	return body
}

// remainingSeconds renders the positive remaining whole seconds for the MQTT
// expiry interval, rounded up.
func remainingSeconds(timeout time.Duration) *uint32 {
	if timeout <= 0 {
		return nil
	}
	seconds := uint64(math.Ceil(timeout.Seconds()))
	if seconds == 0 || seconds > math.MaxUint32 {
		return nil
	}
	value := uint32(seconds)
	return &value
}

func (s *Service) observe(stage string, started time.Time, outcome string) {
	if s.metrics != nil {
		s.metrics.ObserveAccessStage(stage, s.cfg.Now().Sub(started), outcome)
	}
}

func outcomeName(err error) string {
	switch {
	case errors.Is(err, ErrInaccessible):
		return "inaccessible"
	case errors.Is(err, ErrHostOffline):
		return "offline"
	case errors.Is(err, ErrHostUnavailable):
		return "unavailable"
	case errors.Is(err, ErrTimeout):
		return "timeout"
	case errors.Is(err, ErrBusy):
		return "busy"
	case errors.Is(err, ErrReplay):
		return "replay"
	default:
		return "invalid"
	}
}
