package broker

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
	"log/slog"
	"net"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
	"github.com/whalerein/whalerein/access/internal/config"
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
	cfg      config.BrokerConfig
	dispatch func(topic string, payload []byte)
	logger   *slog.Logger
	tls      *tls.Config
	address  string
	useTLS   bool
	peers    sync.Map
}

// NewPahoConnector validates the broker URL and builds a paho.golang MQTT 5
// connector. The dispatch callback runs on the read loop and must return
// quickly.
func NewPahoConnector(cfg config.BrokerConfig, dispatch func(topic string, payload []byte), logger *slog.Logger) (Connector, error) {
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
	return &pahoConnector{cfg: cfg, dispatch: dispatch, logger: logger, tls: tlsConfig, address: address, useTLS: useTLS}, nil
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
						c.logger.Warn("logical MQTT frame rejected", "code", "invalid_argument")
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
	}, c.deliver)
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
	if connack.Properties != nil && connack.Properties.MaximumPacketSize != nil && *connack.Properties.MaximumPacketSize < transportPacketBytes {
		_ = conn.Close()
		return nil, errors.New("broker packet budget is below logical transport minimum")
	}
	return &pahoSession{client: client, transport: transport, peers: &c.peers}, nil
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
}

func (s *pahoSession) Publish(ctx context.Context, topic string, payload []byte, qos byte) error {
	if s.transport != nil {
		if parsed, err := ParseTopic(topic); err == nil && parsed.Kind == KindRequest {
			request := &whalereinv1.HostRequest{}
			if proto.Unmarshal(payload, request) == nil && request.DeviceId != "" && (request.Kind == whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE || request.Kind == whalereinv1.RequestKind_REQUEST_KIND_QUERY || request.Kind == whalereinv1.RequestKind_REQUEST_KIND_COMMAND) {
				s.peers.Store(DeviceEventTopic(parsed.HostID, request.DeviceId), request.ClientTransportVersion == transportVersion)
			}
		}
		capable, _ := s.peers.Load(topic)
		return s.transport.send(ctx, topic, payload, transportAckTopic(topic), capable == true)
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
		s.transport.reset()
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
