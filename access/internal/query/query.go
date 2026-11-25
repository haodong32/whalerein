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

// Scope is the immutable authorization captured at query admission. Host
// cannot restamp it with a later grant, enrollment or connection.
type Scope struct {
	DeviceSessionID string
	HostID          string
	DeviceID        string
	ChannelID       string
	RequestID       string
	Kind            whalereinv1.RequestKind
	Conn            broker.ConnectionRef
	Grant           store.AdmissionGrant
	Deadline        time.Time
	// Correlation is the exact Correlation Data the Device sent; the reply
	// echoes it.
	Correlation []byte
	ReplyTopic  string
}

type waiter struct {
	scope   Scope
	request *whalereinv1.HostRequest
}

// Service owns the transient query waiters.
type Service struct {
	store       Store
	connections Connections
	hosts       Hosts
	publisher   Publisher
	config      Config
	now         func() time.Time

	mu      sync.Mutex
	waiters map[string]*waiter
}

// Dependencies wires the transient query service.
type Dependencies struct {
	Store       Store
	Connections Connections
	Hosts       Hosts
	Publisher   Publisher
	Config      Config
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
	return &Service{
		store:       deps.Store,
		connections: deps.Connections,
		hosts:       deps.Hosts,
		publisher:   deps.Publisher,
		config:      config,
		now:         config.Now,
		waiters:     map[string]*waiter{},
	}, nil
}

// waiterKey is (host, device, request). A duplicate request id on a different
// connection is rejected at admission, so one key identifies one waiter.
func waiterKey(hostID, deviceID, requestID string) string {
	return hostID + "\x00" + deviceID + "\x00" + requestID
}

// Admit validates one inbound query envelope and forwards it to the Host. The
// waiter is registered before publication so a fast result cannot race it.
func (s *Service) Admit(ctx context.Context, message broker.Message) error {
	if message.QoS != 1 {
		return ErrInvalidTopic
	}
	parsed, err := broker.ParseTopic(message.Topic)
	if err != nil || parsed.Kind != broker.KindDeviceQuery || parsed.HostID == "" || parsed.DeviceID == "" {
		return ErrInvalidTopic
	}
	var request whalereinv1.HostRequest
	if err := proto.Unmarshal(message.Payload, &request); err != nil {
		return ErrInvalidTopic
	}
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
	now := s.now()
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
	if !deadline.After(now) {
		return ErrExpired
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
	scope := Scope{
		DeviceSessionID: deviceSession, HostID: parsed.HostID, DeviceID: parsed.DeviceID,
		ChannelID: request.GetChannelId(), RequestID: request.GetRequestId(),
		Kind: request.GetKind(), Conn: ref, Grant: grant, Deadline: deadline,
		Correlation: append([]byte(nil), message.Properties.CorrelationData...),
		ReplyTopic:  replyTopic,
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
			return s.publishReply(ctx, scope, result, message.Properties.MessageExpiry)
		}
	}
	return s.registerAndForward(ctx, scope, &request, message.Properties.MessageExpiry)
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

func (s *Service) registerAndForward(ctx context.Context, scope Scope, request *whalereinv1.HostRequest, upstream *uint32) error {
	key := waiterKey(scope.HostID, scope.DeviceID, scope.RequestID)
	s.mu.Lock()
	if existing, ok := s.waiters[key]; ok {
		s.mu.Unlock()
		// A repeated publication of the same request on the same connection is
		// idempotent: the waiter already exists and the Host was already
		// asked. A second connection must not attach to that waiter.
		if existing.scope.Conn == scope.Conn {
			return nil
		}
		return ErrDuplicate
	}
	if len(s.waiters) >= s.config.MaxWaiters || s.connectionWaitersLocked(scope.Conn) >= s.config.MaxWaitersPerConnection {
		s.mu.Unlock()
		return ErrCapacity
	}
	s.waiters[key] = &waiter{scope: scope, request: proto.Clone(request).(*whalereinv1.HostRequest)}
	s.mu.Unlock()

	forward := proto.Clone(request).(*whalereinv1.HostRequest)
	forward.ConnectionRef = scope.Conn.String()
	forward.GrantGeneration = scope.Grant.GrantGeneration
	forward.EnrollmentGeneration = scope.Grant.EnrollmentGeneration
	transport := forward.GetTransport()
	if transport == nil {
		transport = &whalereinv1.RequestTransportMetadata{}
	}
	transport.GrantGeneration = scope.Grant.GrantGeneration
	transport.RevocationGeneration = scope.Grant.RevocationGeneration
	transport.EnrollmentGeneration = scope.Grant.EnrollmentGeneration
	forward.Transport = transport
	forward.RelayVersion = broker.RelayEpoch
	payload, err := proto.Marshal(forward)
	if err != nil {
		s.remove(key)
		return fmt.Errorf("%w: marshal: %v", ErrInvalidBody, err)
	}
	expiry := remainingExpiry(scope.Deadline, s.now(), upstream)
	if expiry == nil {
		s.remove(key)
		return ErrExpired
	}
	err = s.publisher.PublishMessage(ctx, broker.Message{
		Topic:   broker.RequestTopic(scope.HostID),
		Payload: payload,
		QoS:     1,
		Properties: broker.MessageProperties{
			CorrelationData: append([]byte(nil), scope.Correlation...),
			ResponseTopic:   broker.ResultTopic(scope.HostID),
			MessageExpiry:   expiry,
		},
	})
	if err != nil {
		s.remove(key)
		return err
	}
	return nil
}

func (s *Service) connectionWaitersLocked(ref broker.ConnectionRef) int {
	count := 0
	for _, waiter := range s.waiters {
		if waiter.scope.Conn == ref {
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
	key := waiterKey(hostID, result.GetDeviceId(), result.GetRequestId())
	s.mu.Lock()
	current := s.waiters[key]
	if current == nil {
		// A result may omit device_id; resolve by host and request id then.
		if result.GetDeviceId() == "" {
			for candidateKey, candidate := range s.waiters {
				if candidate.scope.HostID == hostID && candidate.scope.RequestID == result.GetRequestId() {
					key, current = candidateKey, candidate
					break
				}
			}
		}
	}
	if current == nil {
		s.mu.Unlock()
		return false
	}
	s.mu.Unlock()

	if result.GetDeviceId() != "" && result.GetDeviceId() != current.scope.DeviceID {
		return false
	}
	if result.GetChannelId() != "" && current.scope.ChannelID != "" &&
		result.GetChannelId() != current.scope.ChannelID {
		return false
	}
	// Exactly one completion removes the waiter.
	if !s.remove(key) {
		return true
	}
	scope := current.scope
	now := s.now()
	if !now.Before(scope.Deadline) {
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

// publishReply sends a terminal result only to the admitted connection.
func (s *Service) publishReply(ctx context.Context, scope Scope, reply *whalereinv1.HostResult, upstream *uint32) error {
	reply.HostId = scope.HostID
	reply.DeviceId = scope.DeviceID
	reply.RequestId = scope.RequestID
	reply.ChannelId = scope.ChannelID
	reply.RelayVersion = broker.RelayEpoch
	payload, err := proto.Marshal(reply)
	if err != nil {
		return err
	}
	expiry := remainingExpiry(scope.Deadline, s.now(), upstream)
	if expiry == nil {
		return ErrExpired
	}
	properties := broker.MessageProperties{
		CorrelationData: scope.Correlation,
		MessageExpiry:   expiry,
	}.WithUserProperty(broker.PropertyTarget, scope.Conn.String())
	return s.publisher.PublishMessage(ctx, broker.Message{
		Topic: scope.ReplyTopic, Payload: payload, QoS: 1, Properties: properties,
	})
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
func (s *Service) issueReference(ctx context.Context, scope Scope, grant store.AdmissionGrant, reference *whalereinv1.PayloadReference) error {
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
	if object.State != store.PayloadReady || !object.ExpiresAt.After(s.now()) ||
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
	}, s.now())
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
	current := s.waiters[waiterKey(hostID, deviceID, requestID)]
	s.mu.Unlock()
	if current == nil {
		return "", ErrNoWaiter
	}
	scope := current.scope
	if !s.now().Before(scope.Deadline) {
		return "", ErrExpired
	}
	if scope.HostID != hostID || scope.DeviceID != deviceID ||
		scope.ChannelID != channelID || scope.RequestID != requestID {
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
		if waiter.scope.Conn == ref {
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
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	removed := 0
	for key, waiter := range s.waiters {
		if !now.Before(waiter.scope.Deadline) {
			delete(s.waiters, key)
			removed++
		}
	}
	return removed
}

// WaiterCount reports the current bounded waiter count.
func (s *Service) WaiterCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.waiters)
}

// Waiter returns a copy of one waiter scope, for focused tests and diagnostics.
func (s *Service) Waiter(hostID, deviceID, requestID string) (Scope, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	waiter := s.waiters[waiterKey(hostID, deviceID, requestID)]
	if waiter == nil {
		return Scope{}, false
	}
	return waiter.scope, true
}

func (s *Service) remove(key string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.waiters[key]; !ok {
		return false
	}
	delete(s.waiters, key)
	return true
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
		if uint64(*upstream) < seconds {
			seconds = uint64(*upstream)
		}
		if *upstream == 0 {
			return nil
		}
	}
	if seconds == 0 || seconds > math.MaxUint32 {
		return nil
	}
	value := uint32(seconds)
	return &value
}
