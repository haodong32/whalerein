//go:build integration

package broker

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

// This native driver runs only through the container-owned pytest transport case.
// It owns its HTTP auth/ACL callback listener and never reuses a development Access.
func TestMQTTIntegrationRoutesRequestsResultsEventsAndPresenceThroughEMQX(t *testing.T) {
	mqttURL := os.Getenv("WHALEREIN_EMQX_MQTT_URL")
	if mqttURL == "" {
		mqttURL = "tcp://127.0.0.1:1883"
	}
	accessPassword := "access-pass"
	hostPassword := "host-pass"
	listener, err := net.Listen("tcp", "127.0.0.1:18081")
	if err != nil {
		t.Fatal("isolated broker callback listener is unavailable")
	}
	callback := httptest.NewUnstartedServer(http.HandlerFunc(integrationCallback))
	callback.Listener.Close()
	callback.Listener = listener
	callback.Start()
	defer callback.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	metrics := diagnostics.NewMetrics()
	presence := NewPresence(time.Minute, nil, metrics, slog.New(slog.NewTextHandler(io.Discard, nil)))
	var service *Service
	connector, err := NewPahoConnector(config.BrokerConfig{
		URL: mqttURL, ClientID: "whalerein-access-integration",
		Username: "access", Password: accessPassword,
		ConnectTimeout: config.Duration(3 * time.Second),
		PublishTimeout: config.Duration(3 * time.Second),
		KeepAlive:      config.Duration(10 * time.Second),
		ReconnectMin:   config.Duration(100 * time.Millisecond),
		ReconnectMax:   config.Duration(time.Second),
	}, func(topic string, payload []byte) {
		service.HandleMessage(topic, payload)
	}, slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	service, err = New(Dependencies{
		Config: config.BrokerConfig{
			URL: mqttURL, ClientID: "whalerein-access-integration",
			Username: "access", Password: accessPassword,
			ConnectTimeout: config.Duration(3 * time.Second),
			PublishTimeout: config.Duration(3 * time.Second),
			KeepAlive:      config.Duration(10 * time.Second),
			ReconnectMin:   config.Duration(100 * time.Millisecond),
			ReconnectMax:   config.Duration(time.Second),
		},
		Presence: presence, Connector: connector,
		Metrics: metrics, Masker: diagnostics.NewMaskerWithKey([]byte("integration")),
		Logger: slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	results := make(chan *whalereinv1.HostResult, 8)
	service.SetResultHandler(func(_ context.Context, _ string, result *whalereinv1.HostResult) error {
		results <- result
		return nil
	})
	events := make(chan string, 4)
	service.SetEventHandler(func(_ context.Context, _ string, payload []byte) error {
		var envelope whalereinv1.EventEnvelope
		if err := proto.Unmarshal(payload, &envelope); err != nil {
			return err
		}
		events <- envelope.GetDeviceId()
		return nil
	})
	ready := make(chan string, 4)
	service.SetReadyHandler(func(_ context.Context, hostID string) error {
		ready <- hostID
		return nil
	})
	done := make(chan error, 1)
	go func() { done <- service.Run(ctx) }()
	// A cold EMQX HTTP resource can need a 15-second health-check interval.
	deadline := time.Now().Add(45 * time.Second)
	for !service.Ready() && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if !service.Ready() {
		t.Fatal("Access MQTT session did not become ready")
	}

	host, received := connectIntegrationClient(t, mqttURL, "whalerein-host-h-e2e", "host:h-e2e", hostPassword)
	if _, err := host.Subscribe(ctx, &paho.Subscribe{Subscriptions: []paho.SubscribeOptions{{Topic: RequestTopic("h-e2e"), QoS: 1}}}); err != nil {
		t.Fatalf("host subscribe: %v", err)
	}

	// $SYS presence is broker-authoritative.
	deadline = time.Now().Add(5 * time.Second)
	for !presence.IsHostOnline(context.Background(), "h-e2e") && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if !presence.IsHostOnline(context.Background(), "h-e2e") {
		t.Fatal("$SYS presence did not report the host online")
	}

	// Host Ready triggers the readiness handler.
	readyBody, _ := proto.Marshal(&whalereinv1.HostReady{HostId: "h-e2e", ConnectedAtUnixMs: time.Now().UnixMilli()})
	publishProto(t, host, ResultTopic("h-e2e"), &whalereinv1.HostResult{
		HostId: "h-e2e", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: readyBody,
	})
	select {
	case hostID := <-ready:
		if hostID != "h-e2e" {
			t.Fatalf("ready host = %q", hostID)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("host Ready not delivered")
	}

	// Access -> host request publish.
	request := &whalereinv1.HostRequest{RequestId: "integration-request", HostId: "h-e2e", DeviceId: "d-e2e", Kind: whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE}
	if err := service.PublishRequest(ctx, "h-e2e", request); err != nil {
		t.Fatalf("publish request: %v", err)
	}
	decoded := waitForHostRequest(t, received, "integration-request", 5*time.Second)
	if decoded.GetHostId() != "h-e2e" {
		t.Fatalf("request = %+v", decoded)
	}

	// Host -> Access result reaches the permanent result handler.
	publishProto(t, host, ResultTopic("h-e2e"), &whalereinv1.HostResult{
		RequestId: "permanent-result", HostId: "h-e2e", DeviceId: "d-e2e", Status: whalereinv1.OnlineStatus_host_accepted,
	})
	select {
	case result := <-results:
		if result.GetRequestId() != "permanent-result" {
			t.Fatalf("result = %+v", result)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("permanent result not delivered")
	}

	// Host -> Access event reaches the event handler.
	eventPayload, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h-e2e", DeviceId: "d-e2e", Ciphertext: []byte("x")})
	publishRaw(t, host, EventTopic("h-e2e"), eventPayload)
	select {
	case deviceID := <-events:
		if deviceID != "d-e2e" {
			t.Fatalf("event device = %q", deviceID)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("event not delivered")
	}

	// Transient pairing-style request/result correlation.
	go func() {
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			select {
			case message := <-received:
				var sent whalereinv1.HostRequest
				if err := proto.Unmarshal(message.Payload, &sent); err != nil {
					continue
				}
				if sent.GetRequestId() != "transient-integration" {
					continue
				}
				publishProto(t, host, ResultTopic("h-e2e"), &whalereinv1.HostResult{
					RequestId: sent.GetRequestId(), HostId: "h-e2e", Status: whalereinv1.OnlineStatus_succeeded,
				})
				return
			case <-time.After(100 * time.Millisecond):
			}
		}
	}()
	transient := &whalereinv1.HostRequest{RequestId: "transient-integration", HostId: "h-e2e"}
	result, err := service.PublishAndAwait(ctx, "h-e2e", transient, 5*time.Second)
	if err != nil {
		t.Fatalf("transient await: %v", err)
	}
	if result.GetStatus() != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("transient result = %+v", result)
	}

	cancel()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("Access MQTT run did not stop")
	}
}

func waitForHostRequest(t *testing.T, received <-chan *paho.Publish, requestID string, timeout time.Duration) *whalereinv1.HostRequest {
	t.Helper()
	deadline := time.After(timeout)
	for {
		select {
		case message := <-received:
			var decoded whalereinv1.HostRequest
			if err := proto.Unmarshal(message.Payload, &decoded); err != nil {
				continue
			}
			if decoded.GetRequestId() == requestID {
				return &decoded
			}
		case <-deadline:
			t.Fatalf("host did not receive request %q", requestID)
		}
	}
}

func integrationCallback(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	switch r.URL.Path {
	case "/internal/broker/auth":
		var payload struct {
			Username string `json:"username"`
			Password string `json:"password"`
		}
		_ = json.NewDecoder(r.Body).Decode(&payload)
		allow := (payload.Username == "access" && payload.Password == "access-pass") ||
			(payload.Username == "host:h-e2e" && payload.Password == "host-pass")
		writeIntegrationResult(w, allow)
	case "/internal/broker/acl":
		var payload struct {
			Username string `json:"username"`
			Topic    string `json:"topic"`
			Action   string `json:"action"`
		}
		_ = json.NewDecoder(r.Body).Decode(&payload)
		allow := false
		switch {
		case payload.Username == "access":
			allow = payload.Topic == ResultFilter || payload.Topic == EventFilter ||
				payload.Topic == SysConnectedFilter || payload.Topic == SysDisconnectedFilter ||
				strings.HasPrefix(payload.Topic, "whalerein/v1/h/")
		case payload.Username == "device:transport-device":
			allow = (payload.Action == "subscribe" && payload.Topic == DeviceEventTopic("h-e2e", "transport-device")) || (payload.Action == "publish" && payload.Topic == DeviceAckTopic("h-e2e", "transport-device"))
		case payload.Username == "host:h-e2e":
			allow = (payload.Action == "subscribe" && payload.Topic == RequestTopic("h-e2e")) ||
				(payload.Action == "publish" && (payload.Topic == ResultTopic("h-e2e") || payload.Topic == EventTopic("h-e2e")))
		}
		writeIntegrationResult(w, allow)
	default:
		http.NotFound(w, r)
	}
}

func writeIntegrationResult(w http.ResponseWriter, allow bool) {
	result := "deny"
	if allow {
		result = "allow"
	}
	_ = json.NewEncoder(w).Encode(map[string]any{"result": result, "is_superuser": false})
}

func connectIntegrationClient(t *testing.T, brokerURL, clientID, username, password string) (*paho.Client, <-chan *paho.Publish) {
	t.Helper()
	address := strings.TrimPrefix(strings.TrimPrefix(brokerURL, "tcp://"), "mqtt://")
	conn, err := net.DialTimeout("tcp", address, 3*time.Second)
	if err != nil {
		t.Fatalf("dial broker: %v", err)
	}
	received := make(chan *paho.Publish, 8)
	client := paho.NewClient(paho.ClientConfig{
		ClientID: clientID,
		Conn:     packets.NewThreadSafeConn(conn),
		OnPublishReceived: []func(paho.PublishReceived) (bool, error){
			func(message paho.PublishReceived) (bool, error) {
				if message.Packet != nil {
					received <- message.Packet
				}
				return true, nil
			},
		},
	})
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	connack, err := client.Connect(ctx, &paho.Connect{
		ClientID: clientID, CleanStart: true, KeepAlive: 10,
		UsernameFlag: true, Username: username,
		PasswordFlag: true, Password: []byte(password),
	})
	if err != nil || connack == nil || connack.ReasonCode != 0 {
		t.Fatalf("connect host: %v %+v", err, connack)
	}
	t.Cleanup(func() { _ = client.Disconnect(&paho.Disconnect{ReasonCode: 0}) })
	return client, received
}

func publishProto(t *testing.T, client *paho.Client, topic string, message proto.Message) {
	t.Helper()
	payload, err := proto.Marshal(message)
	if err != nil {
		t.Fatal(err)
	}
	publishRaw(t, client, topic, payload)
}

func publishRaw(t *testing.T, client *paho.Client, topic string, payload []byte) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if _, err := client.Publish(ctx, &paho.Publish{Topic: topic, QoS: 1, Payload: payload}); err != nil {
		t.Fatalf("publish %s: %v", topic, err)
	}
}

// TestMQTTLogicalTransportCarriesLargeMessagesBetweenGoNodeAndRust uses a separate broker (1 MiB
// maximum). Auth/ACL are tested separately. No production broker or user devices are modified.
func TestMQTTLogicalTransportCarriesLargeMessagesBetweenGoNodeAndRust(t *testing.T) {
	mqttURL := os.Getenv("WHALEREIN_TEST_MQTT_URL")
	if mqttURL == "" {
		mqttURL = "mqtt://127.0.0.1:1884"
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	presence := NewPresence(time.Minute, nil, nil, logger)
	var service *Service
	cfg := config.BrokerConfig{URL: mqttURL, ClientID: "transport-access-probe", Username: "access", Password: "access-pass", ConnectTimeout: config.Duration(5 * time.Second), PublishTimeout: config.Duration(10 * time.Second), KeepAlive: config.Duration(10 * time.Second)}
	connector, err := NewPahoConnector(cfg, func(topic string, payload []byte) { service.HandleMessage(topic, payload) }, logger)
	if err != nil {
		t.Fatal(err)
	}
	service, err = New(Dependencies{Config: cfg, Presence: presence, Connector: connector, Logger: logger})
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- service.Run(ctx) }()
	defer func() { cancel(); <-done }()
	root, err := filepath.Abs("../../..")
	if err != nil {
		t.Fatal(err)
	}
	host := exec.CommandContext(ctx, "node", filepath.Join(root, "tests/integration/drivers/transport/node_host.mjs"))
	host.Env = append(os.Environ(), "WHALEREIN_TEST_MQTT_URL="+mqttURL)
	stdout, err := host.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	var hostError bytes.Buffer
	host.Stderr = &hostError
	if err := host.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = host.Process.Kill(); _ = host.Wait() }()
	scanner := bufio.NewScanner(stdout)
	if !scanner.Scan() || scanner.Text() != "transport-host-ready" {
		t.Fatalf("JS host did not start: %s", hostError.String())
	}
	deadline := time.Now().Add(5 * time.Second)
	for !service.SupportsPairing("h-e2e") && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	if !service.SupportsPairing("h-e2e") {
		t.Fatal("host capability not negotiated")
	}
	request := &whalereinv1.HostRequest{RequestId: "transport-cross-language", HostId: "h-e2e", DeviceId: "transport-device", ClientTransportVersion: 2, Kind: whalereinv1.RequestKind_REQUEST_KIND_QUERY, Body: make([]byte, 2*1024*1024), ProtocolVersion: 1, ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli()}
	result, err := service.PublishAndAwait(ctx, "h-e2e", request, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.ResultBody) != 2*1024*1024 {
		t.Fatal("partial logical result delivered")
	}
	// Start the real Rust MQTT receiver and wait until its subscription is active.
	device := exec.CommandContext(ctx, filepath.Join(root, "target/debug/examples/mqtt_transport_driver"))
	device.Env = append(os.Environ(), "WHALEREIN_TEST_MQTT_URL="+mqttURL)
	device.Stderr = io.Discard
	deviceOutput, err := device.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := device.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() {
		if device.ProcessState == nil {
			_ = device.Process.Kill()
			_ = device.Wait()
		}
	}()
	deviceReady := false
	scan := bufio.NewScanner(deviceOutput)
	for scan.Scan() {
		if strings.Contains(scan.Text(), "transport-device-ready") {
			deviceReady = true
			break
		}
	}
	if !deviceReady {
		t.Fatal("Rust subscription did not become ready")
	}
	envelope, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h-e2e", DeviceId: "transport-device", ChannelId: "synthetic", Ciphertext: make([]byte, 2*1024*1024)})
	if err := service.PublishDeviceEvent(ctx, "h-e2e", "transport-device", envelope); err != nil {
		t.Fatal(err)
	}
	_, _ = io.Copy(io.Discard, deviceOutput)
	if err := device.Wait(); err != nil {
		t.Fatal("Rust logical receiver failed")
	}
}
