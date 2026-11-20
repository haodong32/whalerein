package broker

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
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
		receivers: NewReceiverBudgets(),
	}, nil
}

// SetReceiverBudgets installs the shared advertisement registry. It is called
// once during construction and before Run establishes a session.
func (c *pahoConnector) SetReceiverBudgets(receivers *ReceiverBudgets) {
	if receivers != nil {
		c.receivers = receivers
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

	var transport *logicalTransport
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
					consumed, err := transport.receive(packet.Topic, packet.Payload, transportAckTopic(packet.Topic))
					if err != nil {
						c.logger.Warn("logical MQTT frame rejected", "code", transportCodeOf(err), "bytes", len(packet.Payload))
					}
					if !consumed {
						c.deliver(packet.Topic, packet.Payload)
					}
				}
				return true, nil
			},
		},
	})
	transport = newLogicalTransport(func(ctx context.Context, topic string, payload []byte) error {
		response, err := client.Publish(ctx, &paho.Publish{Topic: topic, QoS: 1, Payload: payload})
		if err != nil {
			return err
		}
		if response != nil && response.ReasonCode >= 0x80 {
			return errTransport
		}
		return nil
	}, c.deliverFramed)
	transport.setLocal(c.transport, c.metrics, c.logger)
	transport.startSweeper()
	keepAlive := uint16(c.cfg.KeepAlive.D() / time.Second)
	if keepAlive == 0 {
		keepAlive = 30
	}
	if keepAlive > 65535 {
		keepAlive = 65535
	}
	connect := &paho.Connect{
		ClientID:     c.cfg.ClientID,
		CleanStart:   true,
		KeepAlive:    keepAlive,
		UsernameFlag: true,
		Username:     c.cfg.Username,
		PasswordFlag: c.cfg.Password != "",
		Password:     []byte(c.cfg.Password),
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
	if connack.Properties != nil && connack.Properties.MaximumPacketSize != nil && *connack.Properties.MaximumPacketSize < c.transport.PacketBytes {
		_ = conn.Close()
		return nil, fmt.Errorf("broker packet budget %d is below the configured transport packet budget %d",
			*connack.Properties.MaximumPacketSize, c.transport.PacketBytes)
	}
	return &pahoSession{
		client: client, transport: transport, peers: &c.peers,
		budget: c.transport, receivers: c.receivers, metrics: c.metrics,
	}, nil
}

// A completed, valid framed reply is fresh capability evidence after Access
// restarts while the Host's broker connection stays alive. ACK/device topics
// and mismatched identities cannot establish a Host's capability.
func (c *pahoConnector) deliverFramed(topic string, payload []byte) {
	if parsed, err := ParseTopic(topic); err == nil && parsed.Kind == KindResult {
		var result whalereinv1.HostResult
		if proto.Unmarshal(payload, &result) == nil && result.ProtocolVersion == 1 &&
			result.RequestId != "" && (result.HostId == "" || result.HostId == parsed.HostID) {
			c.peers.Store(RequestTopic(parsed.HostID), true)
		}
	}
	c.deliver(topic, payload)
}

func (c *pahoConnector) deliver(topic string, payload []byte) {
	parsed, err := ParseTopic(topic)
	if err == nil && parsed.Kind == KindResult {
		result := &whalereinv1.HostResult{}
		if proto.Unmarshal(payload, result) == nil && result.RequestId == "" {
			ready := &whalereinv1.HostReady{}
			if proto.Unmarshal(result.ResultBody, ready) == nil {
				c.peers.Store(RequestTopic(parsed.HostID), ready.TransportVersion == transportVersion)
			}
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
	client    mqttClient
	transport *logicalTransport
	peers     *sync.Map
	budget    config.TransportEffective
	receivers *ReceiverBudgets
	metrics   *diagnostics.Metrics
}

func (s *pahoSession) Publish(ctx context.Context, topic string, payload []byte, qos byte) error {
	if s.transport != nil {
		var parsed Topic
		var parseErr error
		if parsed, parseErr = ParseTopic(topic); parseErr == nil && parsed.Kind == KindRequest {
			request := &whalereinv1.HostRequest{}
			if proto.Unmarshal(payload, request) == nil && request.DeviceId != "" && (request.Kind == whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE || request.Kind == whalereinv1.RequestKind_REQUEST_KIND_QUERY || request.Kind == whalereinv1.RequestKind_REQUEST_KIND_COMMAND) {
				s.peers.Store(DeviceEventTopic(parsed.HostID, request.DeviceId), request.ClientTransportVersion == transportVersion)
			}
		}
		capable, _ := s.peers.Load(topic)
		var peer *config.TransportEffective
		if s.receivers != nil {
			if budget, ok := s.receivers.Lookup(topic); ok {
				peer = &budget
			}
		}
		if peer == nil && parseErr == nil && parsed.Kind == KindDeviceEvent {
			// The device receive advertisement is exchanged inside the
			// end-to-end Noise handshake and is opaque to Access. The device
			// event route therefore uses Access's own configured budget, which
			// can never raise a local limit, and records the fallback.
			local := s.budget
			peer = &local
			if s.metrics != nil {
				s.metrics.TransportBudgetFallback("device")
			}
		}
		return s.transport.sendTo(ctx, topic, payload, transportAckTopic(topic), capable == true, peer)
	}
	response, err := s.client.Publish(ctx, &paho.Publish{Topic: topic, QoS: qos, Payload: payload})
	if err != nil {
		return err
	}
	if response != nil && response.ReasonCode >= 0x80 {
		return fmt.Errorf("mqtt publish rejected with reason %d", response.ReasonCode)
	}
	return nil
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
	if s.transport != nil {
		s.transport.close()
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
