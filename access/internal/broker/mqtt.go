package broker

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/url"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// Session is the small slice of an MQTT 5 session the broker service needs.
type Session interface {
	Publish(ctx context.Context, topic string, payload []byte, qos byte) error
	Subscribe(ctx context.Context, topics []string, qos byte) error
	Done() <-chan struct{}
	Close() error
}

// Connector opens one MQTT 5 session. Reconnection is handled by Service.Run.
type Connector interface {
	Connect(ctx context.Context) (Session, error)
}

// stagerRef lets the broker service install a PayloadStager after the
// connector is constructed, without changing constructor signatures.
type stagerRef struct {
	mu     sync.RWMutex
	stager PayloadStager
}

func (r *stagerRef) get() PayloadStager {
	if r == nil {
		return nil
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	return r.stager
}

func (r *stagerRef) set(stager PayloadStager) {
	if r == nil {
		return
	}
	r.mu.Lock()
	r.stager = stager
	r.mu.Unlock()
}

type pahoConnector struct {
	cfg       config.BrokerConfig
	transport config.TransportEffective
	metrics   *diagnostics.Metrics
	dispatch  func(topic string, payload []byte)
	logger    *slog.Logger
	tls       *tls.Config
	address   string
	useTLS    bool
	peers     sync.Map
	receivers *ReceiverBudgets
	stager    *stagerRef
}

// NewPahoConnector builds a connector with the packaged default transport
// budget. Production wiring uses NewPahoConnectorWithTransport.
func NewPahoConnector(cfg config.BrokerConfig, dispatch func(topic string, payload []byte), logger *slog.Logger) (Connector, error) {
	return NewPahoConnectorWithTransport(cfg, config.DefaultTransportEffective(), nil, dispatch, logger)
}

// NewPahoConnectorWithTransport validates the broker URL and builds a
// paho.golang MQTT 5 connector. transportConfig is the validated local
// transport budget; it takes effect when this process starts. The dispatch
// callback runs on the read loop and must return quickly.
func NewPahoConnectorWithTransport(cfg config.BrokerConfig, transportConfig config.TransportEffective, metrics *diagnostics.Metrics, dispatch func(topic string, payload []byte), logger *slog.Logger) (Connector, error) {
	if dispatch == nil {
		return nil, errors.New("broker: dispatch callback is required")
	}
	u, err := url.Parse(cfg.URL)
	if err != nil || u.Host == "" {
		return nil, errors.New("broker: invalid broker url")
	}
	var useTLS bool
	switch u.Scheme {
	case "tcp", "mqtt":
	case "tls", "mqtts", "ssl":
		useTLS = true
	default:
		return nil, fmt.Errorf("broker: unsupported scheme %q", u.Scheme)
	}
	address := u.Host
	if u.Port() == "" {
		if useTLS {
			address = net.JoinHostPort(u.Hostname(), "8883")
		} else {
			address = net.JoinHostPort(u.Hostname(), "1883")
		}
	}
	var tlsConfig *tls.Config
	if useTLS {
		serverName := cfg.ServerName
		if serverName == "" {
			serverName = u.Hostname()
		}
		tlsConfig, err = tlsConfigFor(cfg.CAFile, cfg.InsecureSkipVerify, serverName)
		if err != nil {
			return nil, err
		}
	}
	if logger == nil {
		logger = slog.Default()
	}
	if transportConfig.EnvelopeBytes == 0 {
		transportConfig = config.DefaultTransportEffective()
	}
	return &pahoConnector{
		cfg: cfg, transport: transportConfig, metrics: metrics, dispatch: dispatch,
		logger: logger, tls: tlsConfig, address: address, useTLS: useTLS,
		receivers: NewReceiverBudgets(), stager: &stagerRef{},
	}, nil
}

// SetReceiverBudgets installs the shared advertisement registry. It is called
// once during construction and before Run establishes a session.
func (c *pahoConnector) SetReceiverBudgets(receivers *ReceiverBudgets) {
	if receivers != nil {
		c.receivers = receivers
	}
}

// SetPayloadStager installs the temporary ciphertext staging seam used when a
// complete body does not fit its outgoing packet. It may be called at any time;
// a missing stager makes an oversized hop fail explicitly before publish.
func (c *pahoConnector) SetPayloadStager(stager PayloadStager) {
	if c.stager != nil {
		c.stager.set(stager)
	}
}

func (c *pahoConnector) Connect(ctx context.Context) (Session, error) {
	timeout := c.cfg.ConnectTimeout.D()
	if timeout <= 0 {
		timeout = 10 * time.Second
	}
	dialCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	dialer := &net.Dialer{Timeout: timeout, KeepAlive: 30 * time.Second}
	var (
		conn net.Conn
		err  error
	)
	if c.useTLS {
		conn, err = (&tls.Dialer{NetDialer: dialer, Config: c.tls}).DialContext(dialCtx, "tcp", c.address)
	} else {
		conn, err = dialer.DialContext(dialCtx, "tcp", c.address)
	}
	if err != nil {
		return nil, fmt.Errorf("broker dial: %w", err)
	}

	client := paho.NewClient(paho.ClientConfig{
		ClientID:      c.cfg.ClientID,
		Conn:          packets.NewThreadSafeConn(conn),
		PacketTimeout: 10 * time.Second,
		OnClientError: func(err error) {
			c.logger.Debug("mqtt client error", "error", err.Error())
		},
		OnPublishReceived: []func(paho.PublishReceived) (bool, error){
			func(received paho.PublishReceived) (bool, error) {
				if received.Packet != nil {
					packet := received.Packet
					c.deliver(packet.Topic, packet.Payload)
				}
				return true, nil
			},
		},
	})
	keepAlive := uint16(c.cfg.KeepAlive.D() / time.Second)
	if keepAlive == 0 {
		keepAlive = 30
	}
	if keepAlive > 65535 {
		keepAlive = 65535
	}
	receiveMaximum := c.transport.MaxReceive()
	localPacket := c.transport.PacketBytes
	connect := &paho.Connect{
		ClientID:     c.cfg.ClientID,
		CleanStart:   true,
		KeepAlive:    keepAlive,
		UsernameFlag: true,
		Username:     c.cfg.Username,
		PasswordFlag: c.cfg.Password != "",
		Password:     []byte(c.cfg.Password),
		Properties: &paho.ConnectProperties{
			ReceiveMaximum:    &receiveMaximum,
			MaximumPacketSize: &localPacket,
		},
	}
	connack, err := client.Connect(dialCtx, connect)
	if err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("broker connect: %w", err)
	}
	if connack == nil || connack.ReasonCode != 0 {
		_ = conn.Close()
		if connack == nil {
			return nil, errors.New("broker connect: empty connack")
		}
		reason := ""
		if connack.Properties != nil {
			reason = connack.Properties.ReasonString
		}
		return nil, fmt.Errorf("broker connect: rejected with reason %d %s", connack.ReasonCode, reason)
	}

	// The publishing hop limit takes the smallest positive value of the local
	// profile, the configured broker ceiling and the CONNACK limit. A broker
	// advertisement below the local profile lowers the hop instead of failing
	// the connection: every packet is selected against the effective limit.
	brokerLimit := whalereinv1.BrokerPacketLimitBytes
	effective := c.transport
	if connack.Properties != nil && connack.Properties.MaximumPacketSize != nil && *connack.Properties.MaximumPacketSize > 0 {
		if uint64(*connack.Properties.MaximumPacketSize) < brokerLimit {
			brokerLimit = uint64(*connack.Properties.MaximumPacketSize)
		}
	}
	if connack.Properties != nil && connack.Properties.ReceiveMaximum != nil && *connack.Properties.ReceiveMaximum > 0 {
		effective.SendInFlight = min(effective.SendInFlight, uint32(*connack.Properties.ReceiveMaximum))
		if effective.SendInFlight == 0 {
			effective.SendInFlight = 1
		}
	}
	if brokerLimit < uint64(c.transport.PacketBytes) {
		c.logger.Warn("broker packet limit lowers the transport hop",
			"broker_packet_limit", brokerLimit, "configured_packet_bytes", c.transport.PacketBytes)
	}
	publisher := newPublisher(func(ctx context.Context, topic string, payload []byte) error {
		response, err := client.Publish(ctx, &paho.Publish{Topic: topic, QoS: 1, Payload: payload})
		if err != nil {
			return err
		}
		if response != nil && response.ReasonCode >= 0x80 {
			return fmt.Errorf("mqtt publish rejected with reason %d", response.ReasonCode)
		}
		return nil
	}, effective, c.metrics)
	return &pahoSession{
		client: client, publisher: publisher, peers: &c.peers,
		budget: c.transport, brokerLimit: brokerLimit, receivers: c.receivers,
		metrics: c.metrics, stager: c.stager,
	}, nil
}

// A completed, valid HostResult envelope is fresh capability evidence after
// Access restarts while the Host's broker connection stays alive. HostReady
// carries the relay epoch; other result kinds do not establish capability.
func (c *pahoConnector) deliver(topic string, payload []byte) {
	if parsed, err := ParseTopic(topic); err == nil && parsed.Kind == KindResult {
		var result whalereinv1.HostResult
		if proto.Unmarshal(payload, &result) == nil && result.ProtocolVersion == 1 &&
			result.RelayVersion == RelayEpoch &&
			result.RequestId != "" && (result.HostId == "" || result.HostId == parsed.HostID) {
			c.peers.Store(RequestTopic(parsed.HostID), true)
		}
	}
	c.dispatch(topic, payload)
}

type mqttClient interface {
	Publish(ctx context.Context, publish *paho.Publish) (*paho.PublishResponse, error)
	Subscribe(ctx context.Context, subscribe *paho.Subscribe) (*paho.Suback, error)
	Done() <-chan struct{}
	Disconnect(disconnect *paho.Disconnect) error
}

type pahoSession struct {
	client      mqttClient
	publisher   *publisher
	peers       *sync.Map
	budget      config.TransportEffective
	brokerLimit uint64
	receivers   *ReceiverBudgets
	metrics     *diagnostics.Metrics
	stager      *stagerRef
}

func (s *pahoSession) Publish(ctx context.Context, topic string, payload []byte, qos byte) error {
	return s.publishComplete(ctx, topic, payload)
}

// publishComplete selects inline or reference for one outgoing hop. It never
// splits a packet and never retries at the packet layer.
func (s *pahoSession) publishComplete(ctx context.Context, topic string, payload []byte) error {
	if len(payload) == 0 {
		return transportReject(transportCodeHeader, ErrMessageBudget)
	}
	if uint64(len(payload)) > s.budget.EnvelopeBytes {
		if s.metrics != nil {
			s.metrics.TransportRejected(transportCodeEnvelope, int64(len(payload)), int64(s.budget.EnvelopeBytes))
		}
		return transportReject(transportCodeEnvelope, ErrMessageBudget)
	}
	parsed, err := ParseTopic(topic)
	if err != nil {
		// The private control topic is Access-only metadata, not a relay
		// envelope. It is still bounded by the local and broker packet limit
		// and is never rewritten into a reference.
		if topic != ControlRequestTopic {
			return transportReject(transportCodeTopic, ErrInvalidTopic)
		}
		limit := selectPacketLimit(s.budget.PacketBytes, s.brokerLimit, nil)
		if uint64(encodedPublishBytes(topic, len(payload), 0)) > limit {
			return transportReject(transportCodePacket, ErrMessageBudget)
		}
		s.recordPacket("inline", len(payload))
		return s.publisher.Submit(ctx, topic, payload)
	}
	var peer *config.TransportEffective
	if s.receivers != nil {
		if budget, ok := s.receivers.Lookup(topic); ok {
			peer = &budget
		}
	}
	if peer == nil && (parsed.Kind == KindDeviceEvent || parsed.Kind == KindDeviceNotice) && s.metrics != nil {
		// The Device hop has not advertised its receive limits yet. Access
		// falls back to its own (never raised) limit and records the bounded
		// route so operators can see the missing advertisement.
		s.metrics.TransportBudgetFallback("device")
	}
	limit := selectPacketLimit(s.budget.PacketBytes, s.brokerLimit, peer)
	encoded := encodedPublishBytes(topic, len(payload), 0)
	if uint64(encoded) <= limit {
		s.recordPacket("inline", len(payload))
		return s.publisher.Submit(ctx, topic, payload)
	}
	prepared, referenced, err := rewriteToReference(ctx, topic, payload, s.stager.get())
	if err != nil {
		if s.metrics != nil {
			s.metrics.TransportRejected(transportCodeOf(err), int64(len(payload)), int64(limit))
		}
		return err
	}
	if !referenced {
		return transportReject(transportCodeReference, ErrReferenceTooLarge)
	}
	referenceBytes := encodedPublishBytes(topic, len(prepared), 0)
	if uint64(referenceBytes) > limit {
		if s.metrics != nil {
			s.metrics.TransportRejected(transportCodeReference, int64(referenceBytes), int64(limit))
		}
		return transportReject(transportCodeReference, ErrReferenceTooLarge)
	}
	if uint64(len(prepared)) > s.budget.EnvelopeBytes {
		return transportReject(transportCodeEnvelope, ErrMessageBudget)
	}
	s.recordPacket("reference", len(prepared))
	return s.publisher.Submit(ctx, topic, prepared)
}

func (s *pahoSession) recordPacket(representation string, bytes int) {
	if s.metrics != nil {
		s.metrics.TransportPacket(representation, int64(bytes))
	}
}

func (s *pahoSession) Subscribe(ctx context.Context, topics []string, qos byte) error {
	subscriptions := make([]paho.SubscribeOptions, 0, len(topics))
	for _, topic := range topics {
		subscriptions = append(subscriptions, paho.SubscribeOptions{Topic: topic, QoS: qos})
	}
	suback, err := s.client.Subscribe(ctx, &paho.Subscribe{Subscriptions: subscriptions})
	if err != nil {
		return err
	}
	if suback == nil {
		return errors.New("mqtt subscribe: empty suback")
	}
	for _, reason := range suback.Reasons {
		if reason >= 0x80 {
			return fmt.Errorf("mqtt subscribe rejected with reason %d", reason)
		}
	}
	return nil
}

func (s *pahoSession) Done() <-chan struct{} { return s.client.Done() }

func (s *pahoSession) Close() error {
	if s.publisher != nil {
		s.publisher.Close()
	}
	if s.client == nil {
		return nil
	}
	err := s.client.Disconnect(&paho.Disconnect{ReasonCode: 0})
	if err != nil && !strings.Contains(err.Error(), "connection lost") {
		return err
	}
	return nil
}

func tlsConfigFor(caFile string, insecure bool, serverName string) (*tls.Config, error) {
	cfg := &tls.Config{ // #nosec G402 -- insecure is an explicit local/test config switch.
		MinVersion:         tls.VersionTLS12,
		ServerName:         serverName,
		InsecureSkipVerify: insecure,
	}
	if caFile != "" {
		pem, err := os.ReadFile(caFile)
		if err != nil {
			return nil, fmt.Errorf("read CA file: %w", err)
		}
		pool, err := x509.SystemCertPool()
		if err != nil || pool == nil {
			pool = x509.NewCertPool()
		}
		if !pool.AppendCertsFromPEM(pem) {
			return nil, errors.New("CA file contains no certificates")
		}
		cfg.RootCAs = pool
	}
	return cfg, nil
}
