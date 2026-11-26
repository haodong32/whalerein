// Package query implements the transient MQTT 5 query and handshake lane.
//
// A Device publishes a deadline-bound HostRequest on
// whalerein/v1/h/{host}/d/{device}/query. Access admits it only with the
// Broker-stamped connection reference, the current grant and enrollment, the
// exact correlation data and the fixed reply topic, then forwards the same
// envelope on the existing Host request topic. No durable request row is
// created. A matching HostResult resolves the transient waiter, issues any
// ciphertext reference and publishes the reply on the Device reply topic with
// the original connection target.
package query

import (
	"context"
	"crypto/subtle"
	"errors"
	"fmt"
	"math"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/observation"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// Typed admission and completion refusals. They never carry a topic, peer
// identity or payload byte.
var (
	ErrInvalidTopic  = errors.New("query: invalid topic")
	ErrInvalidKind   = errors.New("query: unsupported request kind")
	ErrInvalidBody   = errors.New("query: invalid body representation")
	ErrCorrelation   = errors.New("query: missing or mismatched correlation data")
	ErrResponseTopic = errors.New("query: missing or foreign response topic")
	ErrMessageExpiry = errors.New("query: missing or invalid message expiry")
	ErrExpired       = errors.New("query: request deadline has passed")
	ErrStaleConn     = errors.New("query: connection reference is not current")
	ErrScope         = errors.New("query: request scope does not match its topic")
	ErrGrant         = errors.New("query: grant is not active")
	ErrDuplicate     = errors.New("query: request id is already waiting")
	ErrCapacity      = errors.New("query: waiter bound reached")
	ErrProtocol      = errors.New("query: unsupported protocol or relay epoch")
	ErrNoWaiter      = errors.New("query: no matching waiter")
	ErrObject        = errors.New("query: result reference is not authorized")
)

// Store is the durable authority this service reads and updates.
type Store interface {
	AdmissionGrantFor(ctx context.Context, hostID, deviceID string) (store.AdmissionGrant, error)
	GetPayload(ctx context.Context, objectID string) (store.PayloadObject, error)
	MarkPayloadIssued(ctx context.Context, scope store.PayloadIssueScope, now time.Time) (bool, error)
}

// Connections reports the Broker-issued connection instances Access currently
// observes. An old epoch or a replaced connection never satisfies a query.
type Connections interface {
	DeviceConnectionSession(deviceID string, ref broker.ConnectionRef) (string, bool)
}

// Hosts reports Broker-derived Host presence.
type Hosts interface {
	HostPresence(hostID string) (online, known bool)
}

// Publisher publishes one complete MQTT 5 message.
type Publisher interface {
	PublishMessage(ctx context.Context, message broker.Message) error
}

// Config bounds the transient waiter registry and the accepted TTL.
type Config struct {
	MaxWaiters              int
	MaxWaitersPerConnection int
	MaxTTL                  time.Duration
	Now                     func() time.Time
}

type waiterKey struct {
	HostID, DeviceID, RequestID string
}

// A waiter retains only the immutable scope needed after forwarding.
type waiter struct {
	waiterKey
	DeviceSessionID string
	ChannelID       string
	Conn            broker.ConnectionRef
	Grant           store.AdmissionGrant
	Deadline        time.Time
	Route           observation.Route
}

// Service owns the transient query waiters.
type Service struct {
	store       Store
	connections Connections
	hosts       Hosts
	publisher   Publisher
	config      Config
	metrics     *diagnostics.Metrics
	observation *observation.Sink

	mu      sync.Mutex
	waiters map[waiterKey]*waiter
}

// Dependencies wires the transient query service.
type Dependencies struct {
	Store       Store
	Connections Connections
	Hosts       Hosts
	Publisher   Publisher
	Config      Config
	Metrics     *diagnostics.Metrics
	Observation *observation.Sink
}

// New builds the transient query service.
func New(deps Dependencies) (*Service, error) {
	if deps.Store == nil || deps.Connections == nil || deps.Publisher == nil {
		return nil, errors.New("query: store, connections and publisher are required")
	}
	config := deps.Config
	if config.MaxWaiters <= 0 {
		config.MaxWaiters = 4096
	}
	if config.MaxWaitersPerConnection <= 0 {
		config.MaxWaitersPerConnection = 128
	}
	if config.MaxTTL <= 0 {
		config.MaxTTL = 10 * time.Minute
	}
	if config.Now == nil {
		config.Now = func() time.Time { return time.Now().UTC() }
	}
	if deps.Metrics == nil {
		deps.Metrics = diagnostics.NewMetrics()
	}
	return &Service{
		store:       deps.Store,
		connections: deps.Connections,
		hosts:       deps.Hosts,
		publisher:   deps.Publisher,
		config:      config,
		metrics:     deps.Metrics,
		observation: deps.Observation,
		waiters:     map[waiterKey]*waiter{},
	}, nil
}

// Admit validates one inbound query envelope and forwards it to the Host. The
// waiter is registered before publication so a fast result cannot race it.
func (s *Service) Admit(ctx context.Context, message broker.Message) (err error) {
	started := time.Now()
	requestID := ""
	requestScope := ""
	route := observation.RouteInline
	admitted := false
	defer func() {
		if admitted {
			return
		}
		outcome := admitOutcome(err)
		if requestID != "" {
			s.observeQuery(requestScope, requestID, route, 1, observation.QueryAdmit, outcome, 0)
		}
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("query_admit", time.Since(started), string(outcome))
		}
	}()
	if message.QoS != 1 {
		return ErrInvalidTopic
	}
	parsed, err := broker.ParseTopic(message.Topic)
	if err != nil || parsed.Kind != broker.KindDeviceQuery {
		return ErrInvalidTopic
	}
	var request whalereinv1.HostRequest
	if err := proto.Unmarshal(message.Payload, &request); err != nil {
		return ErrInvalidTopic
	}
	requestID = request.GetRequestId()
	requestScope = operationScope(parsed.HostID, parsed.DeviceID)
	route = queryRoute(&request)
	if request.GetProtocolVersion() != 1 || request.GetRelayVersion() != broker.RelayEpoch {
		return ErrProtocol
	}
	if request.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_QUERY &&
		request.GetKind() != whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE {
		return ErrInvalidKind
	}
	if request.GetRequestId() == "" {
		return ErrInvalidBody
	}
	if request.GetHostId() != "" && request.GetHostId() != parsed.HostID {
		return ErrScope
	}
	if request.GetDeviceId() != "" && request.GetDeviceId() != parsed.DeviceID {
		return ErrScope
	}
	if err := validBody(&request); err != nil {
		return err
	}
	if len(message.Properties.CorrelationData) == 0 ||
		subtle.ConstantTimeCompare(message.Properties.CorrelationData, []byte(request.GetRequestId())) != 1 {
		return ErrCorrelation
	}
	replyTopic := broker.DeviceReplyTopic(parsed.HostID, parsed.DeviceID)
	if message.Properties.ResponseTopic != replyTopic {
		return ErrResponseTopic
	}
	if message.Properties.MessageExpiry == nil || *message.Properties.MessageExpiry == 0 {
		return ErrMessageExpiry
	}
	now := s.config.Now()
	deadline := time.UnixMilli(request.GetExpiresAtUnixMs()).UTC()
	if request.GetExpiresAtUnixMs() <= 0 || !deadline.After(now) {
		return ErrExpired
	}
	if deadline.After(now.Add(s.config.MaxTTL)) {
		return ErrExpired
	}
	// The Broker's remaining expiry can only shorten the application deadline.
	upstream := time.Duration(*message.Properties.MessageExpiry) * time.Second
	if capped := now.Add(upstream); capped.Before(deadline) {
		deadline = capped
	}
	// The reserved connection reference must be the Broker-stamped instance
	// Access currently observes for this Device. A client-supplied or stale
	// copy never matches.
	ref, ok := broker.ParseConnectionRef(message.Properties.Get(broker.PropertyConnection))
	if !ok {
		return ErrStaleConn
	}
	deviceSession, active := s.connections.DeviceConnectionSession(parsed.DeviceID, ref)
	if !active {
		return ErrStaleConn
	}
	grant, err := s.store.AdmissionGrantFor(ctx, parsed.HostID, parsed.DeviceID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrGrant
		}
		return err
	}
	if !grant.Active || grant.HostID != parsed.HostID || grant.DeviceID != parsed.DeviceID {
		return ErrGrant
	}
	request.HostId = parsed.HostID
	request.DeviceId = parsed.DeviceID
	scope := &waiter{
		waiterKey:       waiterKey{parsed.HostID, parsed.DeviceID, request.GetRequestId()},
		DeviceSessionID: deviceSession, ChannelID: request.GetChannelId(),
		Conn: ref, Grant: grant, Deadline: deadline, Route: route,
	}
	if s.hosts != nil {
		if online, known := s.hosts.HostPresence(scope.HostID); !online {
			result := &whalereinv1.HostResult{
				Status: whalereinv1.OnlineStatus_host_offline,
			}
			if !known {
				result.Status = whalereinv1.OnlineStatus_failed
				result.ErrorCode = whalereinv1.ErrorCode_unavailable
			}
			s.recordQueryAdmission(scope, started)
			admitted = true
			return s.publishReply(ctx, scope, result, message.Properties.MessageExpiry)
		}
	}
	needsForward, err := s.register(scope)
	if err != nil {
		return err
	}
	s.recordQueryAdmission(scope, started)
	admitted = true
	if !needsForward {
		return nil
	}
	if err := s.forward(ctx, scope, &request, message.Properties.MessageExpiry); err != nil {
		s.removeWaiter(scope)
		return err
	}
	return nil
}

// validBody enforces the per-kind body representation: a handshake is always a
// small inline bootstrap body; a query is exactly one of inline or reference.
func validBody(request *whalereinv1.HostRequest) error {
	switch request.GetKind() {
	case whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE:
		if len(request.GetBody()) == 0 || request.GetBodyReference() != nil {
			return ErrInvalidBody
		}
		if request.GetCommandId() != "" || len(request.GetPayloadDigest()) != 0 {
			return ErrInvalidBody
		}
	case whalereinv1.RequestKind_REQUEST_KIND_QUERY:
		body := len(request.GetBody()) > 0
		reference := request.GetBodyReference() != nil
		if body == reference {
			return ErrInvalidBody
		}
	default:
		return ErrInvalidKind
	}
	return nil
}

// register installs one waiter before forwarding so a fast result cannot race
// admission. It reports whether this admission owns the forward; a repeated
// publication of the same request on the same connection is idempotent and
// must not ask the Host twice.
func (s *Service) register(scope *waiter) (bool, error) {
	key := scope.waiterKey
	s.mu.Lock()
	defer s.mu.Unlock()
	if existing, ok := s.waiters[key]; ok {
		if existing.Conn == scope.Conn {
			return false, nil
		}
		return false, ErrDuplicate
	}
	if len(s.waiters) >= s.config.MaxWaiters || s.connectionWaitersLocked(scope.Conn) >= s.config.MaxWaitersPerConnection {
		return false, ErrCapacity
	}
	s.waiters[key] = scope
	return true, nil
}

func (s *Service) removeWaiter(scope *waiter) {
	s.mu.Lock()
	if s.waiters[scope.waiterKey] == scope {
		delete(s.waiters, scope.waiterKey)
	}
	s.mu.Unlock()
}

// recordQueryAdmission emits the admission boundary once, after the waiter is
// registered and before forwarding begins.
func (s *Service) recordQueryAdmission(scope *waiter, started time.Time) {
	s.observeQuery(operationScope(scope.HostID, scope.DeviceID), scope.RequestID, scope.Route, 1, observation.QueryAdmit, observation.OutcomeOK, 0)
	if s.metrics != nil {
		s.metrics.ObserveAccessStage("query_admit", time.Since(started), string(observation.OutcomeOK))
	}
}

func (s *Service) observeQuery(scope, request string, route observation.Route, attempt uint64, stage observation.QueryStage, outcome observation.Outcome, bytes int) {
	if s.observation == nil {
		return
	}
	event := observation.QueryEvent{
		Scope: scope, Request: request, Attempt: attempt, Route: route, Stage: stage, Outcome: outcome,
	}
	if bytes > 0 {
		event.Bytes = uint64(bytes)
		event.HasBytes = true
	}
	s.observation.Query(event)
}

func admitOutcome(err error) observation.Outcome {
	return observation.OutcomeForError(err, ErrExpired)
}

func queryRoute(request *whalereinv1.HostRequest) observation.Route {
	if request.GetBodyReference() != nil {
		return observation.RouteReference
	}
	return observation.RouteInline
}

func (s *Service) forward(ctx context.Context, scope *waiter, request *whalereinv1.HostRequest, upstream *uint32) (err error) {
	started := time.Now()
	defer func() {
		outcome := admitOutcome(err)
		s.observeQuery(operationScope(scope.HostID, scope.DeviceID), scope.RequestID, scope.Route, 1, observation.QueryForward, outcome, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("query_forward", time.Since(started), string(outcome))
		}
	}()
	request.ConnectionRef = scope.Conn.String()
	request.GrantGeneration = scope.Grant.GrantGeneration
	request.EnrollmentGeneration = scope.Grant.EnrollmentGeneration
	transport := request.GetTransport()
	if transport == nil {
		transport = &whalereinv1.RequestTransportMetadata{}
	}
	transport.GrantGeneration = scope.Grant.GrantGeneration
	transport.RevocationGeneration = scope.Grant.RevocationGeneration
	transport.EnrollmentGeneration = scope.Grant.EnrollmentGeneration
	request.Transport = transport
	payload, err := proto.Marshal(request)
	if err != nil {
		return fmt.Errorf("%w: marshal: %v", ErrInvalidBody, err)
	}
	expiry := remainingExpiry(scope.Deadline, s.config.Now(), upstream)
	if expiry == nil {
		return ErrExpired
	}
	err = s.publisher.PublishMessage(ctx, broker.Message{
		Topic:   broker.RequestTopic(scope.HostID),
		Payload: payload,
		QoS:     1,
		Properties: broker.MessageProperties{
			CorrelationData: []byte(scope.RequestID),
			ResponseTopic:   broker.ResultTopic(scope.HostID),
			MessageExpiry:   expiry,
		},
	})
	if err == nil && s.metrics != nil {
		s.metrics.ObserveAccessStageBytes("query_forward", int64(len(payload)))
	}
	return err
}

func (s *Service) connectionWaitersLocked(ref broker.ConnectionRef) int {
	count := 0
	for _, waiter := range s.waiters {
		if waiter.Conn == ref {
			count++
		}
	}
	return count
}

// Complete resolves one HostResult against a transient waiter. It returns true
// when the result belonged to the query lane (published or deliberately
// dropped); false lets the broker continue to the durable command path.
func (s *Service) Complete(ctx context.Context, hostID string, message broker.Message) bool {
	var result whalereinv1.HostResult
	if err := proto.Unmarshal(message.Payload, &result); err != nil {
		return false
	}
	if result.GetRequestId() == "" {
		return false
	}
	scope := s.takeResultWaiter(hostID, &result)
	if scope == nil {
		return false
	}
	now := s.config.Now()
	if !now.Before(scope.Deadline) {
		// A Host result that arrives after the caller deadline is late. It is
		// recorded as such and can never turn the observed timeout into a
		// successful reply; the waiter settlement is still consumed.
		s.observeQuery(operationScope(scope.HostID, scope.DeviceID), scope.RequestID, scope.Route, 1, observation.QueryExpire, observation.OutcomeLate, 0)
		if s.metrics != nil {
			s.metrics.ObserveAccessStageDisposition("query_expire", string(observation.OutcomeLate))
		}
		return true
	}
	grant, err := s.store.AdmissionGrantFor(ctx, scope.HostID, scope.DeviceID)
	if err != nil || !grant.Active || !sameGrant(grant, scope.Grant) {
		return true
	}
	if reference := result.GetResultReference(); reference != nil {
		if err := s.issueReference(ctx, scope, grant, reference); err != nil {
			// A reference that no longer matches its original scope is never
			// handed to the Device; the unissued object ages out.
			return true
		}
	}
	_ = s.publishReply(ctx, scope, &result, message.Properties.MessageExpiry)
	return true
}

// Match and consume one waiter under the same lock. A mismatched result
// leaves the waiter available for its own reply.
func (s *Service) takeResultWaiter(hostID string, result *whalereinv1.HostResult) *waiter {
	s.mu.Lock()
	defer s.mu.Unlock()
	key := waiterKey{hostID, result.GetDeviceId(), result.GetRequestId()}
	if key.DeviceID == "" {
		for candidate := range s.waiters {
			if candidate.HostID == hostID && candidate.RequestID == key.RequestID {
				if key.DeviceID != "" {
					return nil // Ambiguous across devices.
				}
				key = candidate
			}
		}
	}
	w := s.waiters[key]
	if w == nil || (result.GetChannelId() != "" && w.ChannelID != "" && result.GetChannelId() != w.ChannelID) {
		return nil
	}
	delete(s.waiters, key)
	return w
}

// publishReply sends a terminal result only to the admitted connection.
func (s *Service) publishReply(ctx context.Context, scope *waiter, reply *whalereinv1.HostResult, upstream *uint32) (err error) {
	started := time.Now()
	observedBytes := 0
	defer func() {
		outcome := admitOutcome(err)
		s.observeQuery(operationScope(scope.HostID, scope.DeviceID), scope.RequestID, scope.Route, 1, observation.QueryReply, outcome, observedBytes)
		if s.metrics != nil {
			s.metrics.ObserveAccessStage("query_reply", time.Since(started), string(outcome))
		}
	}()
	reply.HostId = scope.HostID
	reply.DeviceId = scope.DeviceID
	reply.RequestId = scope.RequestID
	reply.ChannelId = scope.ChannelID
	reply.RelayVersion = broker.RelayEpoch
	payload, err := proto.Marshal(reply)
	if err != nil {
		return err
	}
	expiry := remainingExpiry(scope.Deadline, s.config.Now(), upstream)
	if expiry == nil {
		return ErrExpired
	}
	properties := broker.MessageProperties{
		CorrelationData: []byte(scope.RequestID),
		MessageExpiry:   expiry,
	}.WithUserProperty(broker.PropertyTarget, scope.Conn.String())
	err = s.publisher.PublishMessage(ctx, broker.Message{
		Topic: broker.DeviceReplyTopic(scope.HostID, scope.DeviceID), Payload: payload, QoS: 1, Properties: properties,
	})
	if err == nil {
		observedBytes = len(payload)
		if s.metrics != nil {
			s.metrics.ObserveAccessStageBytes("query_reply", int64(len(payload)))
		}
	}
	return err
}

func sameGrant(left, right store.AdmissionGrant) bool {
	return left.GrantGeneration == right.GrantGeneration &&
		left.RevocationGeneration == right.RevocationGeneration &&
		left.DeviceRevocationGeneration == right.DeviceRevocationGeneration &&
		left.EnrollmentGeneration == right.EnrollmentGeneration
}

// issueReference verifies that a ready result object is still bound to the
// waiter's original query scope, grant and enrollment, then durably marks it
// issued before any reply is published.
func (s *Service) issueReference(ctx context.Context, scope *waiter, grant store.AdmissionGrant, reference *whalereinv1.PayloadReference) error {
	objectID := reference.GetObjectId()
	switch reference.GetPurpose() {
	case whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT,
		whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_HISTORY_PAGE:
	default:
		return ErrObject
	}
	object, err := s.store.GetPayload(ctx, objectID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			return ErrObject
		}
		return err
	}
	if object.State != store.PayloadReady || !object.ExpiresAt.After(s.config.Now()) ||
		object.ExpiresAt.After(scope.Deadline) {
		return ErrObject
	}
	if object.HostID != scope.HostID || object.DeviceID != scope.DeviceID ||
		object.ChannelID != scope.ChannelID || object.RequestID != scope.RequestID || object.AppSession != scope.DeviceSessionID {
		return ErrObject
	}
	switch object.Purpose {
	case whalereinv1.PayloadPurpose_name[int32(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_RESULT)],
		whalereinv1.PayloadPurpose_name[int32(whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_HISTORY_PAGE)]:
	default:
		return ErrObject
	}
	if object.ConnectionRef == "" || object.ConnectionRef != scope.Conn.String() {
		return ErrObject
	}
	if object.EnrollmentGeneration == nil || *object.EnrollmentGeneration < 0 ||
		uint64(*object.EnrollmentGeneration) != scope.Grant.EnrollmentGeneration {
		return ErrObject
	}
	if object.GrantGeneration == nil || object.RevocationGeneration == nil ||
		object.DeviceRevocationGeneration == nil {
		return ErrObject
	}
	if uint64(*object.GrantGeneration) != scope.Grant.GrantGeneration ||
		uint64(*object.RevocationGeneration) != scope.Grant.RevocationGeneration ||
		uint64(*object.DeviceRevocationGeneration) != scope.Grant.DeviceRevocationGeneration {
		return ErrObject
	}
	issued, err := s.store.MarkPayloadIssued(ctx, store.PayloadIssueScope{
		ObjectID:                   object.ObjectID,
		HostID:                     scope.HostID,
		DeviceID:                   scope.DeviceID,
		AppSession:                 scope.DeviceSessionID,
		ChannelID:                  scope.ChannelID,
		RequestID:                  scope.RequestID,
		ConnectionRef:              scope.Conn.String(),
		Purpose:                    object.Purpose,
		GrantGeneration:            grant.GrantGeneration,
		RevocationGeneration:       grant.RevocationGeneration,
		DeviceRevocationGeneration: grant.DeviceRevocationGeneration,
		EnrollmentGeneration:       grant.EnrollmentGeneration,
	}, s.config.Now())
	if err != nil {
		return err
	}
	if !issued {
		return ErrObject
	}
	return nil
}

// AuthorizeObjectReserve checks that a query-scoped ciphertext reservation
// still matches one live waiter. relay calls it before reserving bytes.
func (s *Service) AuthorizeObjectReserve(ctx context.Context, hostID, deviceID, channelID, requestID, connectionRef string, enrollment uint64, expiresAtUnixMs int64) (string, error) {
	if requestID == "" {
		return "", ErrNoWaiter
	}
	s.mu.Lock()
	scope := s.waiters[waiterKey{hostID, deviceID, requestID}]
	s.mu.Unlock()
	if scope == nil {
		return "", ErrNoWaiter
	}
	if !s.config.Now().Before(scope.Deadline) {
		return "", ErrExpired
	}
	if scope.ChannelID != channelID {
		return "", ErrScope
	}
	if connectionRef != scope.Conn.String() || enrollment != scope.Grant.EnrollmentGeneration {
		return "", ErrScope
	}
	// The object may not outlive the original admitted deadline.
	if expiresAtUnixMs <= 0 || expiresAtUnixMs > scope.Deadline.UnixMilli() {
		return "", ErrExpired
	}
	grant, err := s.store.AdmissionGrantFor(ctx, hostID, deviceID)
	if err != nil || !grant.Active || !sameGrant(grant, scope.Grant) {
		return "", ErrGrant
	}
	return scope.DeviceSessionID, nil
}

// CancelConnection drops every waiter owned by one Broker connection.
func (s *Service) CancelConnection(ref broker.ConnectionRef) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for key, waiter := range s.waiters {
		if waiter.Conn == ref {
			delete(s.waiters, key)
		}
	}
}

// CancelAll drops every waiter. It is called when Access loses its MQTT
// session and when the process restarts; safe reads retry.
func (s *Service) CancelAll() {
	s.mu.Lock()
	defer s.mu.Unlock()
	clear(s.waiters)
}

// Sweep removes waiters whose deadline has passed and returns the count.
func (s *Service) Sweep() int {
	now := s.config.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	removed := 0
	for key, waiter := range s.waiters {
		if !now.Before(waiter.Deadline) {
			delete(s.waiters, key)
			s.observeQuery(operationScope(waiter.HostID, waiter.DeviceID), waiter.RequestID, waiter.Route, 1, observation.QueryExpire, observation.OutcomeExpired, 0)
			removed++
		}
	}
	if removed > 0 && s.metrics != nil {
		s.metrics.ObserveAccessStageDisposition("query_expire", string(observation.OutcomeExpired))
	}
	return removed
}

// remainingExpiry renders the positive remaining whole seconds from the
// original deadline, rounded up and capped by a shorter upstream expiry. A nil
// result means the publication must be skipped.
func remainingExpiry(deadline, now time.Time, upstream *uint32) *uint32 {
	remaining := deadline.Sub(now)
	if remaining <= 0 {
		return nil
	}
	seconds := uint64(remaining / time.Second)
	if remaining%time.Second != 0 {
		seconds++
	}
	if upstream != nil {
		seconds = min(seconds, uint64(*upstream))
	}
	if seconds == 0 || seconds > math.MaxUint32 {
		return nil
	}
	value := uint32(seconds)
	return &value
}

// operationScope binds a request token to the authenticated host/device pair
// so the same client-generated request id on two devices stays distinct.
func operationScope(hostID, deviceID string) string {
	return observation.Scope(hostID, deviceID)
}
