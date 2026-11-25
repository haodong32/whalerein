//go:build integration

package broker

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
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

const (
	integrationHostPassword   = "host-pass"
	integrationDevicePassword = "device-pass"
)

// This native driver runs only through the container-owned pytest transport case.
// It owns the deferred CONNECT policy callback and never reuses a development
// Access. The Access identity is authenticated locally by the Mosquitto plugin.
func TestMQTTIntegrationRoutesRequestsResultsEventsAndPresenceThroughMosquitto(t *testing.T) {
	mqttURL := os.Getenv("WHALEREIN_BROKER_MQTT_URL")
	if mqttURL == "" {
		mqttURL = "tcp://127.0.0.1:1883"
	}
	accessPassword := integrationCredential(t, "WHALEREIN_TEST_ACCESS_PASSWORD")
	hostPassword := integrationHostPassword
	startIntegrationCallback(t)

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	metrics := diagnostics.NewMetrics()
	presence := NewPresence(time.Minute, metrics, slog.New(slog.NewTextHandler(io.Discard, nil)))
	var service *Service
	connector, err := NewPahoConnector(config.BrokerConfig{
		URL: mqttURL, ClientID: "whalerein-access-integration",
		Username: "access", Password: accessPassword,
		ConnectTimeout: config.Duration(3 * time.Second),
		PublishTimeout: config.Duration(3 * time.Second),
		KeepAlive:      config.Duration(10 * time.Second),
		ReconnectMin:   config.Duration(100 * time.Millisecond),
		ReconnectMax:   config.Duration(time.Second),
	}, func(message Message) {
		service.HandleMessage(message)
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
	// The line readiness hook applies a sequenced snapshot before external
	// traffic. Presence stays unavailable until then.
	service.SetReadyHook(func(context.Context) error {
		presence.ApplySnapshot(nil, 0)
		return nil
	})
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

	// A broker CONNECT alone is not presence: the Host must confirm its request
	// subscription with HostReady before publishing is safe.
	if presence.IsHostOnline(context.Background(), "h-e2e") {
		t.Fatal("connected-but-unready host reported online")
	}

	// Host Ready triggers the readiness handler and confirms presence.
	readyBody, _ := proto.Marshal(&whalereinv1.HostReady{
		HostId: "h-e2e", ConnectedAtUnixMs: time.Now().UnixMilli(), RelayVersion: RelayEpoch,
		Receive: &whalereinv1.ReceiveAdvertisement{
			Profile: whalereinv1.TransportProfile_TRANSPORT_PROFILE_DEFAULT,
			Budget: &whalereinv1.TransportBudget{
				PacketBytes: 64 * 1024, EnvelopeBytes: 16 << 20, BulkConcurrency: 4,
				ReservedBytes: 32 << 20, TransferTimeoutMs: 120000,
			},
			ConnectionGeneration: 1, RelayVersion: RelayEpoch,
		},
	})
	publishProto(t, host, ResultTopic("h-e2e"), &whalereinv1.HostResult{
		HostId: "h-e2e", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: readyBody, RelayVersion: RelayEpoch,
	})
	select {
	case hostID := <-ready:
		if hostID != "h-e2e" {
			t.Fatalf("ready host = %q", hostID)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("host Ready not delivered")
	}
	deadline = time.Now().Add(5 * time.Second)
	for !presence.IsHostOnline(context.Background(), "h-e2e") && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if !presence.IsHostOnline(context.Background(), "h-e2e") {
		t.Fatal("HostReady did not confirm host presence")
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
		RequestId: "permanent-result", HostId: "h-e2e", DeviceId: "d-e2e", Status: whalereinv1.OnlineStatus_host_accepted, RelayVersion: RelayEpoch,
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
	eventPayload, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h-e2e", DeviceId: "d-e2e", Ciphertext: []byte("x"), RelayVersion: RelayEpoch})
	publishRaw(t, host, EventTopic("h-e2e"), eventPayload)
	select {
	case deviceID := <-events:
		if deviceID != "d-e2e" {
			t.Fatalf("event device = %q", deviceID)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("event not delivered")
	}

	// Previews now travel directly from the Host; Access never republishes
	// them, so the retired host live topic has no local subscription.

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
					RequestId: sent.GetRequestId(), HostId: "h-e2e", Status: whalereinv1.OnlineStatus_succeeded, RelayVersion: RelayEpoch,
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

func integrationCredential(t *testing.T, name string) string {
	t.Helper()
	value := os.Getenv(name)
	if value == "" {
		t.Fatalf("missing broker fixture environment %s", name)
	}
	return value
}

func startIntegrationCallback(t *testing.T) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:18081")
	if err != nil {
		t.Fatal("isolated broker callback listener is unavailable")
	}
	token := integrationCredential(t, "WHALEREIN_TEST_INTERNAL_TOKEN")
	callback := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		integrationCallback(w, r, token)
	}))
	callback.Listener.Close()
	callback.Listener = listener
	callback.Start()
	t.Cleanup(callback.Close)
}

func integrationCallback(w http.ResponseWriter, r *http.Request, token string) {
	if r.URL.Path != "/internal/broker/connect" {
		http.NotFound(w, r)
		return
	}
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return
	}
	if r.Header.Get("X-WhaleRein-Internal-Token") != token {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	if err := r.ParseForm(); err != nil {
		writeIntegrationPolicy(w, "whalerein/1\ndeny malformed_request\n")
		return
	}
	username := r.PostForm.Get("username")
	password := r.PostForm.Get("password")
	will := r.PostForm.Get("will")
	switch {
	case username == "host:h-e2e" && password == integrationHostPassword:
		writeIntegrationPolicy(w, integrationHostPolicy("h-e2e"))
	case username == "device:transport-device" && password == integrationDevicePassword:
		if will != "" {
			writeIntegrationPolicy(w, "whalerein/1\ndeny will_not_authorized\n")
			return
		}
		writeIntegrationPolicy(w, integrationDevicePolicy("h-e2e", "transport-device"))
	default:
		// The Access identity never reaches the callback: the plugin checks it
		// against its local password file.
		writeIntegrationPolicy(w, "whalerein/1\ndeny unauthorized\n")
	}
}

func integrationHostPolicy(hostID string) string {
	var builder strings.Builder
	builder.WriteString("whalerein/1\nok\n")
	fmt.Fprintf(&builder, "kind host\nhost %s\nrevision 1\nready 1\n", hostID)
	entries := []struct {
		access string
		filter string
	}{
		{"subscribe", RequestTopic(hostID)},
		{"read", RequestTopic(hostID)},
		{"subscribe", PresenceTopic(hostID)},
		{"read", PresenceTopic(hostID)},
		{"write", ResultTopic(hostID)},
		{"write", EventTopic(hostID)},
	}
	for _, entry := range entries {
		fmt.Fprintf(&builder, "acl %s 1 %s %s\n", entry.access, hostID, entry.filter)
	}
	return builder.String()
}

func integrationDevicePolicy(hostID, deviceID string) string {
	var builder strings.Builder
	builder.WriteString("whalerein/1\nok\nkind device\n")
	fmt.Fprintf(&builder, "device %s\nrevision 1\nready 1\n", deviceID)
	fmt.Fprintf(&builder, "acl subscribe 1 %s %s\n", hostID, DeviceEventTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl read 1 %s %s\n", hostID, DeviceEventTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl subscribe 1 %s %s\n", hostID, DeviceNoticeTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl read 1 %s %s\n", hostID, DeviceNoticeTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl subscribe 1 %s %s\n", hostID, DeviceLiveTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl read 1 %s %s\n", hostID, DeviceLiveTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl subscribe 1 %s %s\n", hostID, DeviceReplyTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl read 1 %s %s\n", hostID, DeviceReplyTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl write 1 %s %s\n", hostID, DeviceQueryTopic(hostID, deviceID))
	fmt.Fprintf(&builder, "acl write 1 %s %s\n", hostID, DeviceReadyTopic(hostID, deviceID))
	return builder.String()
}

func writeIntegrationPolicy(w http.ResponseWriter, body string) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	_, _ = io.WriteString(w, body)
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
	publishRawQoS(t, client, topic, payload, 1)
}

func publishRawQoS(t *testing.T, client *paho.Client, topic string, payload []byte, qos byte) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if _, err := client.Publish(ctx, &paho.Publish{Topic: topic, QoS: qos, Payload: payload}); err != nil {
		t.Fatalf("publish %s: %v", topic, err)
	}
}

// TestMQTTLogicalTransportCarriesLargeMessagesBetweenGoNodeAndRust was the
// fragment-based cross-language relay case. The fragment transport, its
// WRMQTT2 framing and its receipt topics no longer exist; the epoch-4 inline
// and reference relay is covered by tests/integration/drivers/transport/
// mqtt-boundary-message.test.js and mqtt-large-message.test.js. This case is
// retained only to record that the old fixture cannot speak the new relay.
func TestMQTTLogicalTransportCarriesLargeMessagesBetweenGoNodeAndRust(t *testing.T) {
	t.Skip("legacy fragment driver was removed; epoch-4 relay is covered by the boundary and large-message transport cases")
}
