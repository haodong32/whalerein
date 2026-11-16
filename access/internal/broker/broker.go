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
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// ErrUnavailable is returned when the Access MQTT session is down.
var ErrUnavailable = errors.New("broker unavailable")

// ErrTimeout is returned when a transient host result does not arrive.
var ErrTimeout = errors.New("broker result timeout")

// ResultHandler consumes a permanent HostResult (a request status copy).
type ResultHandler func(ctx context.Context, hostID string, result *whalereinv1.HostResult) error

// EventHandler consumes an EventEnvelope payload published by a host.
type EventHandler func(ctx context.Context, hostID string, payload []byte) error

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
	Presence    *Presence
	Connector   Connector
	DeviceHosts DeviceHosts
	Metrics     *diagnostics.Metrics
	Masker      *diagnostics.Masker
	Logger      *slog.Logger
}

// Service owns the Access MQTT session and routes inbound broker messages.
type Service struct {
	cfg         config.BrokerConfig
	presence    *Presence
	connector   Connector
	control     *Control
	readyHook   func(ctx context.Context) error
	deviceHosts DeviceHosts
	metrics     *diagnostics.Metrics
	masker      *diagnostics.Masker
	logger      *slog.Logger

	handlersMu    sync.RWMutex
	resultHandler ResultHandler
	eventHandler  EventHandler
	readyHandler  ReadyHandler
	keyHandler    HostKeyHandler

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
	service := &Service{
		cfg:         deps.Config,
		presence:    deps.Presence,
		connector:   deps.Connector,
		deviceHosts: deps.DeviceHosts,
		metrics:     deps.Metrics,
		masker:      deps.Masker,
		logger:      deps.Logger,
		transient:   make(map[string]chan *whalereinv1.HostResult),
		now:         func() time.Time { return time.Now().UTC() },
	}
	control, err := NewControl(service.publishControl)
	if err != nil {
		return nil, err
	}
	service.control = control
	control.SetNotification(deps.Presence.HandleNotification)
	deps.Presence.SetDeviceOfflineHandler(service.handleDeviceOffline)
	return service, nil
}

// SetResultHandler installs the permanent result consumer.
func (s *Service) SetResultHandler(handler ResultHandler) {
	s.handlersMu.Lock()
	defer s.handlersMu.Unlock()
	s.resultHandler = handler
}

// SetEventHandler installs the event consumer.
func (s *Service) SetEventHandler(handler EventHandler) {
	s.handlersMu.Lock()
	defer s.handlersMu.Unlock()
	s.eventHandler = handler
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

// Presence returns the broker-derived presence source.
func (s *Service) Presence() *Presence { return s.presence }

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
	if err := session.Publish(publishCtx, topic, payload, 1); err != nil {
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
			ResultFilter, EventFilter, DeviceAckFilter, DeviceWillFilter,
			ControlReplyFilter, ControlEventFilter,
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
func (s *Service) HandleMessage(topic string, payload []byte) {
	if s.control != nil && s.control.HandleMessage(topic, payload) {
		return
	}
	if _, ok := ParseDeviceWillTopic(topic); ok {
		s.presence.HandleDeviceWill(topic, payload)
		return
	}
	parsed, err := ParseTopic(topic)
	if err != nil {
		s.metrics.RouteError()
		s.logger.Warn("mqtt message on unknown topic")
		return
	}
	switch parsed.Kind {
	case KindResult:
		s.handleResult(parsed.HostID, payload)
	case KindEvent:
		s.handleEvent(parsed.HostID, payload)
	case KindPresence:
		// Access publishes host-scoped presence; Hosts consume it. If a
		// broker/ACL change ever echoes it back, it must not enter result or
		// event routing.
		return
	default:
		s.metrics.RouteError()
	}
}

func (s *Service) handleResult(topicHost string, payload []byte) {
	var result whalereinv1.HostResult
	if err := proto.Unmarshal(payload, &result); err != nil {
		s.metrics.RouteError()
		s.logger.Warn("mqtt result decode failed", "error", err.Error())
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
	hostID := ready.GetHostId()
	if hostID == "" {
		hostID = result.GetHostId()
	}
	if hostID == "" || (result.GetHostId() != "" && hostID != result.GetHostId()) {
		s.metrics.RouteError()
		return
	}
	s.presence.MarkHostReady(hostID, s.now())
	s.logger.Info("host ready", "host", s.masker.Host(hostID))
	s.rememberHostKey(hostID, ready.GetAuthorizationPublicKey())

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

func (s *Service) handleEvent(topicHost string, payload []byte) {
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
	request.RelayTransportVersion = transportVersion
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
	if err := session.Publish(publishCtx, RequestTopic(hostID), payload, 1); err != nil {
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
// one device topic, using only the outer device id for routing.
func (s *Service) PublishDeviceEvent(ctx context.Context, hostID, deviceID string, payload []byte) error {
	if !validID(hostID) || !validID(deviceID) {
		return ErrInvalidTopic
	}
	session, ok := s.currentSession()
	if !ok || session == nil {
		return ErrUnavailable
	}
	publishCtx, cancel := context.WithTimeout(ctx, s.publishTimeout())
	defer cancel()
	if err := session.Publish(publishCtx, DeviceEventTopic(hostID, deviceID), payload, 1); err != nil {
		return fmt.Errorf("publish device event: %w", err)
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
		if err := session.Publish(publishCtx, PresenceTopic(hostID), payload, 1); err != nil {
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
	return session.Publish(publishCtx, PresenceTopic(hostID), payload, 1)
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
	_, err := s.control.Request(ctx, "kick", []string{"username device:" + deviceID})
	return err
}
