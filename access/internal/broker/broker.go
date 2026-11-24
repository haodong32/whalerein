// Package broker owns the MQTT 5 transport, broker callbacks, presence and
// event routing for WhaleRein Access.
//
// The package never imports the auth package. Authentication and ACL decisions
// are injected by cmd/server, which keeps database-derived authorization out
// of the transport layer.
package broker

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// ErrUnavailable is returned when the Access MQTT session is down.
var ErrUnavailable = errors.New("broker unavailable")

// ErrTimeout is returned when a transient host result does not arrive.
var ErrTimeout = errors.New("broker result timeout")

// ResultHandler consumes a permanent HostResult (a request status copy).
type ResultHandler func(ctx context.Context, hostID string, result *whalereinv1.HostResult) error

// EventHandler consumes a control EventEnvelope payload published by a host.
type EventHandler func(ctx context.Context, hostID string, payload []byte) error

// QueryHandler admits deadline-bound device queries and completes their
// transient waiters. It is implemented by access/internal/query.
type QueryHandler interface {
	Admit(ctx context.Context, message Message) error
	Complete(ctx context.Context, hostID string, message Message) bool
	CancelConnection(ref ConnectionRef)
	CancelAll()
}

// ReadyHandler runs host readiness work (revocation replay).
type ReadyHandler func(ctx context.Context, hostID string) error

// HostKeyHandler stores the Ed25519 public key carried by HostReady.
type HostKeyHandler func(ctx context.Context, hostID string, publicKey []byte) error

// DeviceHosts resolves a device to every host holding a live grant. Presence
// events are scoped per authorized host; a device id never implies a host.
type DeviceHosts func(ctx context.Context, deviceID string) ([]string, error)

// Dependencies wires a broker service.
type Dependencies struct {
	Config      config.BrokerConfig
	Transport   config.TransportEffective
	Presence    *Presence
	Connector   Connector
	DeviceHosts DeviceHosts
	Receivers   *ReceiverBudgets
	Stager      PayloadStager
	Metrics     *diagnostics.Metrics
	Masker      *diagnostics.Masker
	Logger      *slog.Logger
}

// Service owns the Access MQTT session and routes inbound broker messages.
type Service struct {
	cfg         config.BrokerConfig
	transport   config.TransportEffective
	presence    *Presence
	connector   Connector
	receivers   *ReceiverBudgets
	control     *Control
	readyHook   func(ctx context.Context) error
	deviceHosts DeviceHosts
	metrics     *diagnostics.Metrics
	masker      *diagnostics.Masker
	logger      *slog.Logger

	advertisedMu sync.Mutex
	advertised   map[string]string

	handlersMu    sync.RWMutex
	resultHandler ResultHandler
	eventHandler  EventHandler
	readyHandler  ReadyHandler
	keyHandler    HostKeyHandler
	queryHandler  QueryHandler

	sessionMu sync.RWMutex
	session   Session
	connected bool

	transientMu sync.Mutex
	transient   map[string]chan *whalereinv1.HostResult

	runCtxMu sync.RWMutex
	runCtx   context.Context

	now func() time.Time
}

// New constructs a broker service. Handlers are installed after construction
// to break the requests/broker wiring cycle.
func New(deps Dependencies) (*Service, error) {
	if deps.Connector == nil {
		return nil, errors.New("broker: connector is required")
	}
	if deps.Presence == nil {
		return nil, errors.New("broker: presence is required")
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
	if deps.Transport.EnvelopeBytes == 0 {
		deps.Transport = config.DefaultTransportEffective()
	}
	receivers := deps.Receivers
	if receivers == nil {
		receivers = NewReceiverBudgets()
	}
	service := &Service{
		cfg:         deps.Config,
		transport:   deps.Transport,
		presence:    deps.Presence,
		connector:   deps.Connector,
		receivers:   receivers,
		deviceHosts: deps.DeviceHosts,
		metrics:     deps.Metrics,
		masker:      deps.Masker,
		logger:      deps.Logger,
		transient:   make(map[string]chan *whalereinv1.HostResult),
		advertised:  map[string]string{},
		now:         func() time.Time { return time.Now().UTC() },
	}
	if registrar, ok := deps.Connector.(interface{ SetReceiverBudgets(*ReceiverBudgets) }); ok {
		registrar.SetReceiverBudgets(receivers)
	}
	if deps.Stager != nil {
		if registrar, ok := deps.Connector.(interface{ SetPayloadStager(PayloadStager) }); ok {
			registrar.SetPayloadStager(deps.Stager)
		}
	}
	control, err := NewControl(service.publishControl)
	if err != nil {
		return nil, err
	}
	service.control = control
	control.SetNotification(service.handleNotification)
	deps.Presence.SetDeviceOfflineHandler(service.handleDeviceOffline)
	return service, nil
}

// SetResultHandler installs the permanent result consumer.
func (s *Service) SetResultHandler(handler ResultHandler) {
	s.handlersMu.Lock()
	defer s.handlersMu.Unlock()
	s.resultHandler = handler
}

// SetEventHandler installs the control event consumer.
func (s *Service) SetEventHandler(handler EventHandler) {
	s.handlersMu.Lock()
	defer s.handlersMu.Unlock()
	s.eventHandler = handler
}

// SetQueryHandler installs the transient query/handshake service.
func (s *Service) SetQueryHandler(handler QueryHandler) {
	s.handlersMu.Lock()
	defer s.handlersMu.Unlock()
	s.queryHandler = handler
}

// SetReadyHandler installs the host readiness consumer.
func (s *Service) SetReadyHandler(handler ReadyHandler) {
	s.handlersMu.Lock()
	defer s.handlersMu.Unlock()
	s.readyHandler = handler
}

// SetHostKeyHandler installs the HostReady public-key consumer.
func (s *Service) SetHostKeyHandler(handler HostKeyHandler) {
	s.handlersMu.Lock()
	defer s.handlersMu.Unlock()
	s.keyHandler = handler
}

// SetPayloadStager installs the temporary ciphertext staging seam. It is
// optional: without it an oversized hop fails explicitly before publish.
func (s *Service) SetPayloadStager(stager PayloadStager) {
	if registrar, ok := s.connector.(interface{ SetPayloadStager(PayloadStager) }); ok {
		registrar.SetPayloadStager(stager)
	}
}

// Presence returns the broker-derived presence source.
func (s *Service) Presence() *Presence { return s.presence }

// Receivers returns the authenticated receive-advertisement registry.
func (s *Service) Receivers() *ReceiverBudgets { return s.receivers }

// handleNotification applies one ordered broker connection notification and
// invalidates the receive advertisement of a superseded connection before any
// delayed readiness proof can arrive.
func (s *Service) handleNotification(notification *Notification) {
	s.presence.HandleNotification(notification)
	if notification == nil {
		return
	}
	online := notification.Event == "connected"
	switch notification.Kind {
	case "host":
		if notification.HostID != "" {
			s.receivers.NoteConnection(receiverRoute{kind: "host", hostID: notification.HostID}, notification.ConnID, notification.ClientID, online)
		}
	case "device", "session":
		if notification.DeviceID != "" {
			s.receivers.NoteConnection(receiverRoute{kind: "device", deviceID: notification.DeviceID}, notification.ConnID, notification.ClientID, online)
		}
		if !online {
			ref := ConnectionRef{Epoch: notification.Epoch, ID: notification.ConnID}
			s.handlersMu.RLock()
			query := s.queryHandler
			s.handlersMu.RUnlock()
			if query != nil && ref.Valid() {
				query.CancelConnection(ref)
			}
		}
	}
}

// Control returns the authenticated management client for the broker policy,
// snapshot and disconnect operations.
func (s *Service) Control() *Control { return s.control }

// SetReadyHook installs the work that must finish before the line admits
// external traffic: policy reconciliation and presence snapshot application.
func (s *Service) SetReadyHook(hook func(ctx context.Context) error) {
	s.sessionMu.Lock()
	s.readyHook = hook
	s.sessionMu.Unlock()
}

func (s *Service) publishControl(ctx context.Context, topic string, payload []byte) error {
	session, ok := s.currentSession()
	if !ok || session == nil {
		return ErrUnavailable
	}
	publishCtx, cancel := context.WithTimeout(ctx, s.publishTimeout())
	defer cancel()
	if err := session.Publish(publishCtx, Message{Topic: topic, Payload: payload, QoS: 1}); err != nil {
		return fmt.Errorf("publish control: %w", err)
	}
	return nil
}

// Ready reports whether Access has an established, subscribed MQTT session.
func (s *Service) Ready() bool {
	s.sessionMu.RLock()
	defer s.sessionMu.RUnlock()
	return s.connected
}

// Run maintains one MQTT 5 session, reconnecting with capped exponential
// backoff until ctx is cancelled.
func (s *Service) Run(ctx context.Context) error {
	s.runCtxMu.Lock()
	s.runCtx = ctx
	s.runCtxMu.Unlock()

	reconnectMin := s.cfg.ReconnectMin.D()
	if reconnectMin <= 0 {
		reconnectMin = 500 * time.Millisecond
	}
	backoff := reconnectMin
	maxBackoff := s.cfg.ReconnectMax.D()
	if maxBackoff <= 0 {
		maxBackoff = 30 * time.Second
	}
	for {
		if ctx.Err() != nil {
			return nil
		}
		session, err := s.connector.Connect(ctx)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			s.logger.Warn("mqtt connect failed", "retry_in_ms", backoff.Milliseconds(), "error", err.Error())
			if !sleepContext(ctx, backoff) {
				return nil
			}
			backoff *= 2
			if backoff > maxBackoff {
				backoff = maxBackoff
			}
			continue
		}
		subscribeCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
		err = session.Subscribe(subscribeCtx, []string{
			ResultFilter, EventFilter, DeviceQueryFilter, DeviceReadyFilter,
			DeviceWillFilter, ControlReplyFilter, ControlEventFilter,
		}, 1)
		cancel()
		if err != nil {
			_ = session.Close()
			if ctx.Err() != nil {
				return nil
			}
			s.logger.Warn("mqtt subscribe failed", "error", err.Error())
			if !sleepContext(ctx, backoff) {
				return nil
			}
			continue
		}
		s.setSession(session, true)
		backoff = reconnectMin
		s.logger.Info("mqtt connected")
		s.presence.SetAvailable(false)
		s.sessionMu.RLock()
		hook := s.readyHook
		s.sessionMu.RUnlock()
		if hook != nil {
			hookCtx, hookCancel := context.WithTimeout(ctx, 15*time.Second)
			if err := hook(hookCtx); err != nil {
				s.logger.Warn("broker readiness work failed", "error", err.Error())
			}
			hookCancel()
		}

		select {
		case <-ctx.Done():
			s.setSession(nil, false)
			_ = session.Close()
			return nil
		case <-session.Done():
			s.setSession(nil, false)
			s.presence.SetAvailable(false)
			// A lost Access session cannot receive or publish replies for the
			// waiters it holds; a safe read retries with its remaining deadline.
			s.handlersMu.RLock()
			query := s.queryHandler
			s.handlersMu.RUnlock()
			if query != nil {
				query.CancelAll()
			}
			s.logger.Warn("mqtt disconnected")
			if !sleepContext(ctx, reconnectMin) {
				return nil
			}
		}
	}
}

func (s *Service) setSession(session Session, connected bool) {
	s.sessionMu.Lock()
	defer s.sessionMu.Unlock()
	s.session = session
	s.connected = connected
}

func (s *Service) currentSession() (Session, bool) {
	s.sessionMu.RLock()
	defer s.sessionMu.RUnlock()
	return s.session, s.connected
}

func sleepContext(ctx context.Context, d time.Duration) bool {
	if d <= 0 {
		d = time.Millisecond
	}
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

func (s *Service) baseContext() context.Context {
	s.runCtxMu.RLock()
	defer s.runCtxMu.RUnlock()
	if s.runCtx != nil {
		return s.runCtx
	}
	return context.Background()
}

// HandleMessage routes one MQTT message. It is called by the session read
// loop, so it performs only validation, a non-blocking transient handoff and
// bounded handler calls.
func (s *Service) HandleMessage(message Message) {
	if s.control != nil && s.control.HandleMessage(message.Topic, message.Payload) {
		return
	}
	if _, ok := ParseDeviceWillTopic(message.Topic); ok {
		s.presence.HandleDeviceWill(message.Topic, message.Payload)
		return
	}
	parsed, err := ParseTopic(message.Topic)
	if err != nil {
		s.metrics.RouteError()
		s.logger.Warn("mqtt message on unknown topic")
		return
	}
	switch parsed.Kind {
	case KindResult:
		s.handleResult(parsed.HostID, message)
	case KindEvent:
		s.handleEvent(parsed.HostID, message.Payload)
	case KindDeviceQuery:
		s.handleQuery(message)
	case KindDeviceReply:
		// Access publishes device replies; an echoed copy is not input.
		return
	case KindDeviceLive:
		// Previews travel directly from the Host; Access never republishes
		// them and an echoed copy is not control input.
		return
	case KindDeviceReady:
		s.handleDeviceReady(parsed.HostID, parsed.DeviceID, message.Payload)
	case KindPresence:
		// Access publishes host-scoped presence; Hosts consume it. If a
		// broker/ACL change ever echoes it back, it must not enter result or
		// event routing.
		return
	default:
		s.metrics.RouteError()
	}
}

// handleQuery admits one deadline-bound query on a short bounded context. A
// rejected query is dropped without a durable row or a Host dispatch.
func (s *Service) handleQuery(message Message) {
	s.handlersMu.RLock()
	handler := s.queryHandler
	s.handlersMu.RUnlock()
	if handler == nil {
		s.metrics.RouteError()
		return
	}
	ctx, cancel := context.WithTimeout(s.baseContext(), 10*time.Second)
	defer cancel()
	if err := handler.Admit(ctx, message); err != nil {
		s.metrics.RouteError()
		s.logger.Debug("query admission rejected", "error", err.Error())
	}
}

func (s *Service) handleResult(topicHost string, message Message) {
	var result whalereinv1.HostResult
	if err := proto.Unmarshal(message.Payload, &result); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("mqtt result decode failed", "error", err.Error())
		return
	}
	if !s.requireRelayEpoch("result", result.GetHostId(), result.GetRelayVersion()) {
		return
	}
	if result.GetHostId() == "" {
		result.HostId = topicHost
	} else if result.GetHostId() != topicHost {
		s.metrics.RouteError()
		s.logger.Warn("mqtt result host mismatch")
		return
	}
	if result.GetRequestId() == "" {
		s.handleReady(&result)
		return
	}
	// A matching transient query waiter consumes the result before the
	// durable command path. A result with no waiter falls through so a
	// command result is still persisted by the requests service.
	s.handlersMu.RLock()
	query := s.queryHandler
	s.handlersMu.RUnlock()
	if query != nil && query.Complete(s.baseContext(), topicHost, message) {
		return
	}
	if s.deliverTransient(&result) {
		return
	}
	s.handlersMu.RLock()
	handler := s.resultHandler
	s.handlersMu.RUnlock()
	if handler == nil {
		s.metrics.RouteError()
		return
	}
	ctx, cancel := context.WithTimeout(s.baseContext(), 15*time.Second)
	defer cancel()
	if err := handler(ctx, topicHost, &result); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("host result rejected", "error", err.Error())
	}
}

func (s *Service) rememberHostKey(hostID string, publicKey []byte) {
	if len(publicKey) == 0 {
		return
	}
	s.handlersMu.RLock()
	handler := s.keyHandler
	s.handlersMu.RUnlock()
	if handler == nil {
		return
	}
	if len(publicKey) != ed25519.PublicKeySize {
		s.logger.Warn("host authorization key rejected", "host", s.masker.Host(hostID))
		return
	}
	ctx, cancel := context.WithTimeout(s.baseContext(), 5*time.Second)
	defer cancel()
	if err := handler(ctx, hostID, publicKey); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("host authorization key was not stored", "host", s.masker.Host(hostID), "error", err.Error())
	}
}

// registerHostAdvertisement binds one HostReady receive advertisement to the
// authenticated Host and its current broker connection generation. The
// advertisement's connection_generation is the Host's authenticated generation
// for that broker connection; Access scopes it through the broker-derived
// connection and never compares it with a device-leg generation. Missing,
// invalid, replayed or stale-generation metadata fails explicitly and leaves no
// stale entry behind.
func (s *Service) registerHostAdvertisement(hostID string, advertisement *whalereinv1.ReceiveAdvertisement) bool {
	route := receiverRoute{kind: "host", hostID: hostID}
	connID, _, ready, known := s.presence.HostConnection(hostID)
	if known && !ready {
		return false
	}
	if advertisement == nil {
		s.receivers.Clear(route)
		s.metrics.TransportRejected(transportCodeBudget, 0, 0)
		s.logger.Warn("host receive advertisement missing", "host", s.masker.Host(hostID), "code", transportCodeBudget)
		return false
	}
	budget, generation, err := validateAdvertisement(advertisement)
	if err != nil {
		s.receivers.Clear(route)
		s.metrics.TransportRejected(transportCodeOf(err), 0, 0)
		s.logger.Warn("host receive advertisement rejected", "host", s.masker.Host(hostID), "code", transportCodeOf(err))
		return false
	}
	if !s.receivers.Register(route, connID, generation, budget) {
		s.logger.Warn("stale host receive advertisement ignored", "host", s.masker.Host(hostID), "code", transportCodeBudgetBad)
		return false
	}
	return true
}

// handleDeviceReady registers one authenticated Device receive advertisement.
// The advertisement generation must be one of the broker's current connection
// generations for that Device; a sibling reconnect does not clear another
// receiver's limits, and an old generation never refreshes a newer one.
func (s *Service) handleDeviceReady(hostID, deviceID string, payload []byte) {
	var advertisement whalereinv1.ReceiveAdvertisement
	if err := proto.Unmarshal(payload, &advertisement); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("device receive advertisement decode failed", "device", s.masker.Device(deviceID))
		return
	}
	budget, generation, err := validateAdvertisement(&advertisement)
	if err != nil {
		s.metrics.TransportRejected(transportCodeOf(err), 0, 0)
		s.logger.Warn("device receive advertisement rejected", "device", s.masker.Device(deviceID), "code", transportCodeOf(err))
		return
	}
	route := receiverRoute{kind: "device", deviceID: deviceID}
	// The broker subscriber cannot observe which sibling connection published
	// this topic, so the advertised generation is the per-receiver key. An
	// offline broker notification for the last known connection clears the
	// whole route; a sibling's advertisement persists across another sibling's
	// reconnect.
	if !s.receivers.Register(route, 0, generation, budget) {
		s.logger.Warn("replayed device receive advertisement ignored", "device", s.masker.Device(deviceID), "code", transportCodeBudgetBad)
	}
}

func (s *Service) handleReady(result *whalereinv1.HostResult) {
	if result.GetStatus() != whalereinv1.OnlineStatus_succeeded {
		s.metrics.RouteError()
		return
	}
	var ready whalereinv1.HostReady
	if err := proto.Unmarshal(result.GetResultBody(), &ready); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("host ready decode failed", "error", err.Error())
		return
	}
	// The readiness gate requires the relay epoch and a valid advertisement.
	// A foreign epoch is rejected before any presence or capability effect.
	if !s.requireRelayEpoch("ready", result.GetHostId(), ready.GetRelayVersion()) {
		return
	}
	if _, _, err := validateAdvertisement(ready.GetReceive()); err != nil {
		s.metrics.TransportRejected(transportCodeOf(err), 0, 0)
		s.logger.Warn("host ready has no valid receive advertisement",
			"host", s.masker.Host(result.GetHostId()), "code", transportCodeOf(err))
		return
	}
	hostID := ready.GetHostId()
	if hostID == "" {
		hostID = result.GetHostId()
	}
	if hostID == "" || (result.GetHostId() != "" && hostID != result.GetHostId()) {
		s.metrics.RouteError()
		return
	}
	// HostReady carries the publication time. A delayed readiness proof from a
	// superseded connection must not confirm the replacement connection that
	// the broker already reported, so presence compares this timestamp with the
	// observed connection time.
	readyAt := s.now()
	if ready.GetConnectedAtUnixMs() > 0 {
		readyAt = time.UnixMilli(ready.GetConnectedAtUnixMs()).UTC()
	}
	accepted := s.presence.MarkHostReady(hostID, readyAt)
	s.logger.Info("host ready", "host", s.masker.Host(hostID))
	s.rememberHostKey(hostID, ready.GetAuthorizationPublicKey())
	if accepted {
		s.registerHostAdvertisement(hostID, ready.GetReceive())
		connID, _, _, _ := s.presence.HostConnection(hostID)
		s.publishAccessReady(hostID, accessReceiveAdvertisement(s.transport, connID))
	}

	s.handlersMu.RLock()
	handler := s.readyHandler
	s.handlersMu.RUnlock()
	if handler == nil {
		return
	}
	go func() {
		ctx, cancel := context.WithTimeout(s.baseContext(), 60*time.Second)
		defer cancel()
		if err := handler(ctx, hostID); err != nil {
			s.metrics.RouteError()
			s.logger.Warn("host readiness handling failed", "host", s.masker.Host(hostID), "error", err.Error())
		}
	}()
}

// publishAccessReady answers one accepted HostReady with a single
// REQUEST_KIND_ACCESS_READY control request whose inline body is Access's own
// receive advertisement. It refreshes only when the effective budget or the
// broker connection generation changes and is never routed as business.
func (s *Service) publishAccessReady(hostID string, advertisement *whalereinv1.ReceiveAdvertisement) {
	if hostID == "" || advertisement == nil {
		return
	}
	fingerprint := fmt.Sprintf("%d\x00%d\x00%d\x00%d\x00%d\x00%d",
		advertisement.GetConnectionGeneration(), advertisement.GetRelayVersion(),
		advertisement.GetBudget().GetPacketBytes(), advertisement.GetBudget().GetEnvelopeBytes(),
		advertisement.GetBudget().GetBulkConcurrency(), advertisement.GetBudget().GetReservedBytes())
	s.advertisedMu.Lock()
	previous, seen := s.advertised[hostID]
	s.advertised[hostID] = fingerprint
	s.advertisedMu.Unlock()
	if seen && previous == fingerprint {
		return
	}
	body, err := proto.Marshal(advertisement)
	if err != nil {
		s.metrics.RouteError()
		return
	}
	request := &whalereinv1.HostRequest{
		RequestId:       newControlID(),
		HostId:          hostID,
		Kind:            whalereinv1.RequestKind_REQUEST_KIND_ACCESS_READY,
		Body:            body,
		ProtocolVersion: 1,
		RelayVersion:    RelayEpoch,
		ExpiresAtUnixMs: s.now().Add(5 * time.Minute).UnixMilli(),
	}
	go func() {
		ctx, cancel := context.WithTimeout(s.baseContext(), 10*time.Second)
		defer cancel()
		if err := s.PublishRequest(ctx, hostID, request); err != nil {
			s.metrics.TransportRejected(transportCodeOf(err), 0, 0)
			s.logger.Warn("access receive advertisement publish failed",
				"host", s.masker.Host(hostID), "code", transportCodeOf(err))
		}
	}()
}

func (s *Service) handleEvent(topicHost string, payload []byte) {
	var event whalereinv1.EventEnvelope
	if err := proto.Unmarshal(payload, &event); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("mqtt event decode failed", "error", err.Error())
		return
	}
	if !s.requireRelayEpoch("event", topicHost, event.GetRelayVersion()) {
		return
	}
	s.handlersMu.RLock()
	handler := s.eventHandler
	s.handlersMu.RUnlock()
	if handler == nil {
		s.metrics.RouteError()
		return
	}
	ctx, cancel := context.WithTimeout(s.baseContext(), 15*time.Second)
	defer cancel()
	if err := handler(ctx, topicHost, payload); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("event route failed", "error", err.Error())
	}
}

// requireRelayEpoch rejects every foreign or missing relay epoch before
// business handling. It only logs bounded metadata.
func (s *Service) requireRelayEpoch(kind, hostID string, version uint32) bool {
	if err := validateRelayEpoch(version); err != nil {
		s.metrics.RouteError()
		s.metrics.TransportRejected(transportCodeOf(err), 0, 0)
		s.logger.Warn("relay epoch incompatible", "kind", kind, "host", s.masker.Host(hostID), "code", transportCodeOf(err))
		return false
	}
	return true
}

func (s *Service) deliverTransient(result *whalereinv1.HostResult) bool {
	s.transientMu.Lock()
	channel, ok := s.transient[result.GetRequestId()]
	s.transientMu.Unlock()
	if !ok {
		return false
	}
	select {
	case channel <- result:
	default:
	}
	return true
}

// PublishAndAwait publishes a transient host request and waits for the matching
// HostResult. Pairing claims and revocation notices use this path; no requests
// row exists for them.
func (s *Service) PublishAndAwait(ctx context.Context, hostID string, request *whalereinv1.HostRequest, timeout time.Duration) (*whalereinv1.HostResult, error) {
	if request == nil || request.GetRequestId() == "" {
		return nil, errors.New("broker: request id is required")
	}
	if timeout <= 0 {
		timeout = 15 * time.Second
	}
	channel := make(chan *whalereinv1.HostResult, 1)
	s.transientMu.Lock()
	if _, exists := s.transient[request.GetRequestId()]; exists {
		s.transientMu.Unlock()
		return nil, errors.New("broker: duplicate transient request id")
	}
	s.transient[request.GetRequestId()] = channel
	s.transientMu.Unlock()
	defer func() {
		s.transientMu.Lock()
		delete(s.transient, request.GetRequestId())
		s.transientMu.Unlock()
	}()

	if err := s.Publish(ctx, hostID, request); err != nil {
		return nil, err
	}
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-timer.C:
		return nil, ErrTimeout
	case result := <-channel:
		return result, nil
	}
}

// PublishMessage publishes one complete MQTT 5 message, preserving the
// correlation, response-topic, expiry and user properties of the query lane.
func (s *Service) PublishMessage(ctx context.Context, message Message) error {
	if message.Topic == "" {
		return ErrInvalidTopic
	}
	session, ok := s.currentSession()
	if !ok || session == nil {
		return ErrUnavailable
	}
	publishCtx, cancel := context.WithTimeout(ctx, s.publishTimeout())
	defer cancel()
	if err := session.Publish(publishCtx, message); err != nil {
		return fmt.Errorf("publish message: %w", err)
	}
	return nil
}

// Publish sends a binary protobuf HostRequest to one host's request topic.
func (s *Service) Publish(ctx context.Context, hostID string, request *whalereinv1.HostRequest) error {
	return s.PublishRequest(ctx, hostID, request)
}

// PublishRequest implements the requests package broker boundary.
func (s *Service) PublishRequest(ctx context.Context, hostID string, request *whalereinv1.HostRequest) error {
	if request == nil {
		return errors.New("broker: request is required")
	}
	if !validID(hostID) {
		return ErrInvalidTopic
	}
	if request.GetHostId() != "" && request.GetHostId() != hostID {
		return errors.New("broker: request host mismatch")
	}
	request = proto.Clone(request).(*whalereinv1.HostRequest)
	request.RelayVersion = RelayEpoch
	payload, err := proto.Marshal(request)
	if err != nil {
		return fmt.Errorf("marshal host request: %w", err)
	}
	session, ok := s.currentSession()
	if !ok || session == nil {
		return ErrUnavailable
	}
	publishCtx, cancel := context.WithTimeout(ctx, s.publishTimeout())
	defer cancel()
	if err := session.Publish(publishCtx, Message{
		Topic: RequestTopic(hostID), Payload: payload, QoS: 1,
	}); err != nil {
		return fmt.Errorf("publish host request: %w", err)
	}
	return nil
}

// SupportsPairing prevents activating grants against a host that cannot commit.
func (s *Service) SupportsPairing(hostID string) bool {
	session, ok := s.currentSession()
	if !ok {
		return false
	}
	if mqtt, ok := session.(*pahoSession); ok {
		capable, _ := mqtt.peers.Load(RequestTopic(hostID))
		return capable == true
	}
	return true
}

// PublishDeviceEvent republishes an already validated EventEnvelope to exactly
// one device topic, using only the outer device id for routing. The complete
// message keeps one identity and is never split or retried at packet level.
func (s *Service) PublishDeviceEvent(ctx context.Context, hostID, deviceID string, payload []byte) error {
	if !validID(hostID) || !validID(deviceID) {
		return ErrInvalidTopic
	}
	var event whalereinv1.EventEnvelope
	if err := proto.Unmarshal(payload, &event); err != nil {
		return transportReject(transportCodeHeader, ErrPeerTransportUnsupported)
	}
	if err := validateRelayEpoch(event.GetRelayVersion()); err != nil {
		return err
	}
	if event.GetDeviceId() != "" && event.GetDeviceId() != deviceID {
		return transportReject(transportCodeTopic, ErrInvalidTopic)
	}
	session, ok := s.currentSession()
	if !ok || session == nil {
		return ErrUnavailable
	}
	publishCtx, cancel := context.WithTimeout(ctx, s.publishTimeout())
	defer cancel()
	if err := session.Publish(publishCtx, Message{Topic: DeviceEventTopic(hostID, deviceID), Payload: payload, QoS: 1}); err != nil {
		return fmt.Errorf("publish device event: %w", err)
	}
	return nil
}

// PublishDeviceNotice publishes one packet-sized RequestStateNotice on the
// Device notice topic. It carries status, command identity and an optional
// opaque result reference only; it never carries content or preview text.
func (s *Service) PublishDeviceNotice(ctx context.Context, hostID, deviceID string, notice *whalereinv1.RequestStateNotice) error {
	if !validID(hostID) || !validID(deviceID) {
		return ErrInvalidTopic
	}
	if notice == nil || notice.GetRequestId() == "" {
		return errors.New("broker: notice is required")
	}
	if notice.GetHostId() != "" && notice.GetHostId() != hostID {
		return transportReject(transportCodeTopic, ErrInvalidTopic)
	}
	if notice.GetDeviceId() != "" && notice.GetDeviceId() != deviceID {
		return transportReject(transportCodeTopic, ErrInvalidTopic)
	}
	if err := validateRelayEpoch(notice.GetRelayVersion()); err != nil {
		return err
	}
	clone := proto.Clone(notice).(*whalereinv1.RequestStateNotice)
	clone.HostId = hostID
	clone.DeviceId = deviceID
	clone.RelayVersion = RelayEpoch
	payload, err := proto.Marshal(clone)
	if err != nil {
		return fmt.Errorf("marshal device notice: %w", err)
	}
	if uint64(len(payload)) > s.transport.EnvelopeBytes {
		return transportReject(transportCodeEnvelope, ErrMessageBudget)
	}
	session, ok := s.currentSession()
	if !ok || session == nil {
		return ErrUnavailable
	}
	publishCtx, cancel := context.WithTimeout(ctx, s.publishTimeout())
	defer cancel()
	if err := session.Publish(publishCtx, Message{Topic: DeviceNoticeTopic(hostID, deviceID), Payload: payload, QoS: 1}); err != nil {
		return fmt.Errorf("publish device notice: %w", err)
	}
	return nil
}

func (s *Service) publishTimeout() time.Duration {
	if s.cfg.PublishTimeout.D() > 0 {
		return s.cfg.PublishTimeout.D()
	}
	return 5 * time.Second
}

// deviceOfflineEvent is the only host-scoped presence payload Access publishes.
// It never carries message content, credentials, paths or pairing tokens.
type deviceOfflineEvent struct {
	Event      string `json:"event"`
	DeviceID   string `json:"device_id"`
	Reason     string `json:"reason"`
	ObservedAt string `json:"observed_at"`
}

// handleDeviceOffline publishes one device_offline JSON event to each host
// holding a live grant for the device. Hosts are resolved through the store
// callback; the client id never implies a host.
func (s *Service) handleDeviceOffline(deviceID, reason string, observedAt time.Time) {
	if !validID(deviceID) {
		s.metrics.PresenceInvalid()
		return
	}
	if s.deviceHosts == nil {
		s.metrics.DeviceOfflinePublishFailed()
		s.logger.Warn("device offline has no host resolver")
		return
	}
	if observedAt.IsZero() {
		observedAt = s.now()
	}
	ctx, cancel := context.WithTimeout(s.baseContext(), 10*time.Second)
	defer cancel()
	hostIDs, err := s.deviceHosts(ctx, deviceID)
	if err != nil {
		s.metrics.DeviceOfflinePublishFailed()
		s.logger.Warn("device offline host lookup failed", "device", s.masker.Device(deviceID), "error", err.Error())
		return
	}
	if len(hostIDs) == 0 {
		s.logger.Debug("device offline has no authorized hosts", "device", s.masker.Device(deviceID))
		return
	}
	payload, err := json.Marshal(deviceOfflineEvent{
		Event:      deviceOfflineEventName,
		DeviceID:   deviceID,
		Reason:     reason,
		ObservedAt: observedAt.UTC().Format(time.RFC3339Nano),
	})
	if err != nil {
		s.metrics.DeviceOfflinePublishFailed()
		return
	}
	session, ok := s.currentSession()
	if !ok || session == nil {
		s.metrics.DeviceOfflinePublishFailed()
		s.logger.Warn("device offline could not publish without an mqtt session")
		return
	}
	publishCtx, publishCancel := context.WithTimeout(ctx, s.publishTimeout())
	defer publishCancel()
	for _, hostID := range hostIDs {
		if !validID(hostID) {
			s.metrics.DeviceOfflinePublishFailed()
			continue
		}
		if err := session.Publish(publishCtx, Message{Topic: PresenceTopic(hostID), Payload: payload, QoS: 1}); err != nil {
			s.metrics.DeviceOfflinePublishFailed()
			s.logger.Warn("device offline publish failed", "host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "error", err.Error())
			continue
		}
		s.logger.Info("device offline published", "host", s.masker.Host(hostID), "device", s.masker.Device(deviceID), "reason", reason)
	}
}

// PublishPresence publishes a bounded control event to one host's exact
// presence topic. It is used for request/client liveness, not message content.
func (s *Service) PublishPresence(ctx context.Context, hostID string, payload []byte) error {
	if !validID(hostID) || len(payload) == 0 {
		return errors.New("invalid presence publish")
	}
	session, ok := s.currentSession()
	if !ok || session == nil {
		return errors.New("mqtt session unavailable")
	}
	publishCtx, cancel := context.WithTimeout(ctx, s.publishTimeout())
	defer cancel()
	return session.Publish(publishCtx, Message{Topic: PresenceTopic(hostID), Payload: payload, QoS: 1})
}

// IsHostOnline implements the broker-derived presence boundary used by auth,
// pairing and requests.
func (s *Service) IsHostOnline(ctx context.Context, hostID string) bool {
	return s.presence.IsHostOnline(ctx, hostID)
}

// Available reports whether broker presence has been established since this
// Access session started. Callers must not interpret an unavailable presence
// view as a terminal offline host.
func (s *Service) Available() bool {
	return s.presence.Available()
}

// DisconnectDevice implements auth.Transport's explicit device disconnect.
// One targeted kick covers every connection of the device username; the reply
// confirms whether such a connection still existed.
func (s *Service) DisconnectDevice(ctx context.Context, deviceID string) error {
	if deviceID == "" {
		return errors.New("device id is required")
	}
	// Revocation and explicit disconnect fence every sibling receiver's
	// advertisement before the broker closes their connections.
	s.receivers.FenceDevice(deviceID)
	_, err := s.control.Request(ctx, "kick", []string{"username device:" + deviceID})
	return err
}

// newControlID returns one opaque correlation id for an Access-authored control
// request. It carries no identity or authority.
func newControlID() string {
	raw := make([]byte, 16)
	if _, err := rand.Read(raw); err != nil {
		return fmt.Sprintf("access-%d", time.Now().UnixNano())
	}
	return hex.EncodeToString(raw)
}

// EndDeviceSession fences admissions and closes exactly this Cloud session.
func (s *Service) EndDeviceSession(ctx context.Context, id string) error {
	_, err := s.control.Request(ctx, "end_session", []string{"session " + id})
	return err
}

// RenewDeviceSession advances the deadline on transports already connected to
// this broker. Reconciliation also repairs missed updates after a reconnect.
func (s *Service) RenewDeviceSession(ctx context.Context, permission store.DeviceSession) error {
	_, err := s.control.Request(ctx, "renew_session", []string{
		"session " + permission.ID,
		fmt.Sprintf("session_revision %d", permission.Revision),
		fmt.Sprintf("deadline_ms %d", permission.ExpiresAt.UnixMilli()),
	})
	return err
}
