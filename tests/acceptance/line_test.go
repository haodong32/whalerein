//go:build acceptance

// Package acceptance drives the deployed Mosquitto line over real MQTT and
// PostgreSQL. It is opt-in and expects the Compose stack from
// deploy/compose/stack.yaml to be running.
//
//	WHALEREIN_ACCEPTANCE=1 go test -tags acceptance ./tests/acceptance -v
//
// Evidence for the replace-emqx-with-mosquitto change is recorded under
// .local-docs/acceptance/. The test never prints device credentials.
package acceptance

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
	"github.com/jackc/pgx/v5/pgxpool"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type mqttClient struct {
	id     string
	conn   net.Conn
	client *paho.Client
	msgs   chan paho.PublishReceived
	closed chan struct{}
}

func (c *mqttClient) close() {
	if c.client != nil {
		_ = c.client.Disconnect(&paho.Disconnect{ReasonCode: 0})
	}
	if c.conn != nil {
		_ = c.conn.Close()
	}
}

func (c *mqttClient) connect(t *testing.T, address, id, username, password string) {
	t.Helper()
	conn, err := net.DialTimeout("tcp", address, 5*time.Second)
	if err != nil {
		t.Fatalf("dial broker: %v", err)
	}
	c.id = id
	c.conn = conn
	c.msgs = make(chan paho.PublishReceived, 256)
	c.closed = make(chan struct{})
	c.client = paho.NewClient(paho.ClientConfig{
		ClientID: id,
		Conn:     packets.NewThreadSafeConn(conn),
		OnPublishReceived: []func(paho.PublishReceived) (bool, error){
			func(received paho.PublishReceived) (bool, error) {
				select {
				case c.msgs <- received:
				default:
				}
				return true, nil
			},
		},
		OnClientError: func(error) { close(c.closed) },
	})
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	connack, err := c.client.Connect(ctx, &paho.Connect{
		ClientID:     id,
		CleanStart:   true,
		KeepAlive:    15,
		UsernameFlag: true,
		Username:     username,
		PasswordFlag: password != "",
		Password:     []byte(password),
	})
	if err != nil {
		c.close()
		t.Fatalf("connect %s: %v", username[:min(len(username), 12)], err)
	}
	if connack.ReasonCode != 0 {
		c.close()
		t.Fatalf("connack %d for %s", connack.ReasonCode, username)
	}
}

func (c *mqttClient) publish(t *testing.T, topic string, payload []byte) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	response, err := c.client.Publish(ctx, &paho.Publish{Topic: topic, QoS: 1, Payload: payload})
	if err != nil {
		return err
	}
	if response != nil && response.ReasonCode >= 0x80 {
		return fmt.Errorf("reason %d", response.ReasonCode)
	}
	return nil
}

func (c *mqttClient) subscribe(t *testing.T, filter string) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	suback, err := c.client.Subscribe(ctx, &paho.Subscribe{
		Subscriptions: []paho.SubscribeOptions{{Topic: filter, QoS: 1}},
	})
	if err != nil {
		return err
	}
	for _, reason := range suback.Reasons {
		if reason >= 0x80 {
			return fmt.Errorf("reason %d", reason)
		}
	}
	return nil
}

func (c *mqttClient) next(t *testing.T, prefix string, timeout time.Duration) []byte {
	t.Helper()
	deadline := time.After(timeout)
	for {
		select {
		case received := <-c.msgs:
			if strings.HasPrefix(received.Packet.Topic, prefix) {
				return received.Packet.Payload
			}
		case <-deadline:
			return nil
		}
	}
}

type stack struct {
	root              *testing.T
	mqttAddr          string
	accessURL         string
	db                *pgxpool.Pool
	hostToken         string
	adminToken        string
	hostID            string
	devices           []string
	tokens            []string
	restartAccess     string
	restartBroker     string
	restartPostgres   string
	stopAccess        string
	startAccess       string
	accessContainer   string
	postgresContainer string
}

// requiredEnv returns a required acceptance setting. A missing restart command
// is a configuration failure, never a skip: the deployment suite exists to
// exercise restarts, so a run without them proves nothing.
func requiredEnv(t *testing.T, key string) string {
	t.Helper()
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		t.Fatalf("%s is required for the deployment acceptance suite; see docs/testing.md#deployment-acceptance", key)
	}
	return value
}

func newStack(t *testing.T) *stack {
	t.Helper()
	if os.Getenv("WHALEREIN_ACCEPTANCE") != "1" {
		t.Skip("set WHALEREIN_ACCEPTANCE=1 with the Compose stack running")
	}
	get := func(key, fallback string) string {
		if value := os.Getenv(key); value != "" {
			return value
		}
		return fallback
	}
	db, err := pgxpool.New(context.Background(), get("WHALEREIN_ACCEPTANCE_DATABASE_URL", "postgres://postgres@127.0.0.1:55432/whalerein_dev?sslmode=disable"))
	if err != nil {
		t.Fatalf("database: %v", err)
	}
	t.Cleanup(db.Close)
	if err := db.Ping(context.Background()); err != nil {
		t.Fatalf("database ping: %v", err)
	}
	unique := fmt.Sprintf("%d", time.Now().UnixNano())
	hostID := "accept-host-" + unique
	s := &stack{
		root:              t,
		mqttAddr:          get("WHALEREIN_ACCEPTANCE_MQTT", "127.0.0.1:1883"),
		accessURL:         get("WHALEREIN_ACCEPTANCE_ACCESS", "http://127.0.0.1:8080"),
		db:                db,
		hostToken:         get("WHALEREIN_HOST_PROVISIONING_TOKEN", "dev-host-token"),
		adminToken:        get("WHALEREIN_ADMIN_TOKEN", "dev-admin-token"),
		hostID:            hostID,
		restartAccess:     requiredEnv(t, "WHALEREIN_ACCEPTANCE_RESTART_ACCESS"),
		restartBroker:     requiredEnv(t, "WHALEREIN_ACCEPTANCE_RESTART_BROKER"),
		restartPostgres:   get("WHALEREIN_ACCEPTANCE_RESTART_POSTGRES", ""),
		stopAccess:        get("WHALEREIN_ACCEPTANCE_STOP_ACCESS", ""),
		startAccess:       get("WHALEREIN_ACCEPTANCE_START_ACCESS", ""),
		accessContainer:   get("WHALEREIN_ACCEPTANCE_ACCESS_CONTAINER", "whalerein-access"),
		postgresContainer: get("WHALEREIN_ACCEPTANCE_POSTGRES_CONTAINER", "whalerein-postgres"),
	}
	seedGrant(t, db, hostID, "accept-device-a-"+unique, "acceptance-device-a-"+unique)
	seedGrant(t, db, hostID, "accept-device-b-"+unique, "acceptance-device-b-"+unique)
	s.devices = []string{"accept-device-a-" + unique, "accept-device-b-" + unique}
	s.tokens = []string{"acceptance-device-a-" + unique, "acceptance-device-b-" + unique}
	t.Cleanup(func() {
		ctx := context.Background()
		for _, device := range s.devices {
			_, _ = db.Exec(ctx, "DELETE FROM grants WHERE device_id=$1", device)
			_, _ = db.Exec(ctx, "DELETE FROM device_credentials WHERE device_id=$1", device)
			_, _ = db.Exec(ctx, "DELETE FROM devices WHERE device_id=$1", device)
		}
		_, _ = db.Exec(ctx, "DELETE FROM grants WHERE host_id=$1", hostID)
		_, _ = db.Exec(ctx, "DELETE FROM hosts WHERE host_id=$1", hostID)
	})
	return s
}

func seedGrant(t *testing.T, db *pgxpool.Pool, hostID, deviceID, token string) {
	t.Helper()
	ctx := context.Background()
	hash := sha256.Sum256([]byte(token))
	if _, err := db.Exec(ctx, "INSERT INTO hosts (host_id) VALUES ($1) ON CONFLICT DO NOTHING", hostID); err != nil {
		t.Fatalf("seed host: %v", err)
	}
	if _, err := db.Exec(ctx, "INSERT INTO devices (device_id, public_key_fingerprint) VALUES ($1, '') ON CONFLICT DO NOTHING", deviceID); err != nil {
		t.Fatalf("seed device: %v", err)
	}
	if _, err := db.Exec(ctx, "INSERT INTO device_credentials (auth_hash, device_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", hash[:], deviceID); err != nil {
		t.Fatalf("seed credential: %v", err)
	}
	if _, err := db.Exec(ctx, "INSERT INTO grants (host_id, device_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", hostID, deviceID); err != nil {
		t.Fatalf("seed grant: %v", err)
	}
}

func (s *stack) hostClient(t *testing.T) *mqttClient {
	c := &mqttClient{}
	c.connect(t, s.mqttAddr, "whalerein-host-"+s.hostID, "host:"+s.hostID, s.hostToken)
	s.root.Cleanup(c.close)

	// Announce readiness after subscribing, as the Host runtime does on each connection.
	topic := "whalerein/v1/h/" + s.hostID
	if err := c.subscribe(t, topic+"/request"); err != nil {
		t.Fatalf("host request subscription: %v", err)
	}
	now := time.Now().UnixMilli()
	ready, err := proto.Marshal(&whalereinv1.HostReady{
		HostId:            s.hostID,
		ConnectedAtUnixMs: now,
	})
	if err != nil {
		t.Fatalf("encode HostReady: %v", err)
	}
	result, err := proto.Marshal(&whalereinv1.HostResult{
		HostId:            s.hostID,
		Status:            whalereinv1.OnlineStatus_succeeded,
		ResultBody:        ready,
		CompletedAtUnixMs: now,
		ProtocolVersion:   1,
	})
	if err != nil {
		t.Fatalf("encode HostReady result: %v", err)
	}
	if err := c.publish(t, topic+"/result", result); err != nil {
		t.Fatalf("publish HostReady: %v", err)
	}
	return c
}

func (s *stack) deviceClient(t *testing.T, index int) *mqttClient {
	c := &mqttClient{}
	device := s.devices[index]
	c.connect(t, s.mqttAddr, "whalerein-device-"+device, "device:"+device, s.tokens[index])
	s.root.Cleanup(c.close)
	return c
}

func (s *stack) request(t *testing.T, method, path, token string) (int, []byte) {
	t.Helper()
	request, err := http.NewRequest(method, s.accessURL+path, nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer "+token)
	client := &http.Client{Timeout: 10 * time.Second}
	response, err := client.Do(request)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	defer response.Body.Close()
	body, _ := io.ReadAll(response.Body)
	return response.StatusCode, body
}

func (s *stack) hostOnline(t *testing.T, index int) bool {
	t.Helper()
	status, body := s.request(t, http.MethodGet, "/v1/hosts", s.tokens[index])
	if status != http.StatusOK {
		t.Fatalf("hosts status %d", status)
	}
	var payload struct {
		Hosts []struct {
			HostID string `json:"hostId"`
			Online bool   `json:"online"`
		} `json:"hosts"`
	}
	if err := json.Unmarshal(body, &payload); err != nil {
		t.Fatalf("hosts decode: %v", err)
	}
	for _, host := range payload.Hosts {
		if host.HostID == s.hostID {
			return host.Online
		}
	}
	return false
}

func (s *stack) waitOnline(t *testing.T, index int, want bool) bool {
	t.Helper()
	// A lost notification is repaired by the next policy/presence reconcile;
	// the deployment bound is line.reconcile_interval (30 s by default).
	deadline := time.Now().Add(90 * time.Second)
	for time.Now().Before(deadline) {
		if s.hostOnline(t, index) == want {
			return true
		}
		time.Sleep(200 * time.Millisecond)
	}
	return false
}

func (s *stack) metrics(t *testing.T) map[string]float64 {
	t.Helper()
	response, err := http.Get(s.accessURL + "/metrics")
	if err != nil {
		t.Fatalf("metrics: %v", err)
	}
	defer response.Body.Close()
	values := map[string]float64{}
	scanner := bufio.NewScanner(response.Body)
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) != 2 {
			continue
		}
		var value float64
		if _, err := fmt.Sscanf(fields[1], "%g", &value); err == nil {
			values[fields[0]] = value
		}
	}
	return values
}

func runCommand(t *testing.T, command string) (time.Duration, error) {
	t.Helper()
	if strings.TrimSpace(command) == "" {
		t.Fatal("command not configured; the deployment acceptance suite does not skip restart work")
	}
	parts := strings.Fields(command)
	if len(parts) == 0 {
		t.Fatal("command not configured; the deployment acceptance suite does not skip restart work")
	}
	started := time.Now()
	commandProcess := exec.Command(parts[0], parts[1:]...)
	commandProcess.Dir = repositoryRoot()
	output, err := commandProcess.CombinedOutput()
	elapsed := time.Since(started)
	if err != nil {
		return elapsed, fmt.Errorf("%q: %w (%s)", command, err, output)
	}
	return elapsed, nil
}

func restart(t *testing.T, command string) {
	t.Helper()
	if _, err := runCommand(t, command); err != nil {
		t.Fatalf("restart: %v", err)
	}
}

func waitReady(t *testing.T, accessURL string) {
	t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		response, err := http.Get(accessURL + "/readyz")
		if err == nil {
			response.Body.Close()
			if response.StatusCode == http.StatusOK {
				return
			}
		}
		time.Sleep(500 * time.Millisecond)
	}
	t.Fatal("Access did not become ready")
}

func waitNotReady(t *testing.T, accessURL string) {
	t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		response, err := http.Get(accessURL + "/readyz")
		if err == nil {
			response.Body.Close()
			if response.StatusCode != http.StatusOK {
				return
			}
		}
		time.Sleep(500 * time.Millisecond)
	}
	t.Fatal("Access stayed ready on a read-only database")
}

func eventPayload(hostID, deviceID string) []byte {
	envelope := &whalereinv1.EventEnvelope{
		HostId:          hostID,
		DeviceId:        deviceID,
		ChannelId:       "acceptance",
		Nonce:           []byte("nonce-acceptance"),
		Ciphertext:      []byte("ciphertext-acceptance"),
		TraceId:         "acceptance",
		CreatedAtUnixMs: time.Now().UnixMilli(),
	}
	encoded, err := proto.Marshal(envelope)
	if err != nil {
		panic(err)
	}
	return encoded
}

func TestMosquittoLineAcceptance(t *testing.T) {
	s := newStack(t)
	host := s.hostClient(t)
	deviceA := s.deviceClient(t, 0)
	deviceB := s.deviceClient(t, 1)

	if err := deviceA.subscribe(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[0]+"/event"); err != nil {
		t.Fatalf("device subscribe: %v", err)
	}
	if err := deviceB.subscribe(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[1]+"/event"); err != nil {
		t.Fatalf("device subscribe: %v", err)
	}

	t.Run("AC-06 identity and local ACL", func(t *testing.T) {
		if err := host.publish(t, "whalerein/v1/h/"+s.hostID+"/result", []byte("ok")); err != nil {
			t.Fatalf("own result publish: %v", err)
		}
		if err := host.publish(t, "whalerein/v1/h/other/result", []byte("x")); err == nil {
			t.Fatal("foreign host topic accepted")
		}
		if err := host.subscribe(t, "whalerein/v1/h/other/request"); err == nil {
			t.Fatal("foreign host subscription accepted")
		}
		if err := host.subscribe(t, "#"); err == nil {
			t.Fatal("wildcard subscription accepted")
		}
		if err := deviceA.publish(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[1]+"/ack", []byte("x")); err == nil {
			t.Fatal("foreign device ack accepted")
		}
		if err := deviceA.subscribe(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[1]+"/event"); err == nil {
			t.Fatal("foreign device subscription accepted")
		}
	})

	t.Run("AC-03 pairing-equivalent grant is usable after policy readiness", func(t *testing.T) {
		// The seeded grant is the same durable state a completed pairing writes.
		// Access must project it to the broker and admit event forwarding.
		payload := eventPayload(s.hostID, s.devices[0])
		deadline := time.Now().Add(90 * time.Second)
		for {
			if err := host.publish(t, "whalerein/v1/h/"+s.hostID+"/event", payload); err != nil {
				t.Fatalf("event publish: %v", err)
			}
			if received := deviceA.next(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[0]+"/event", 10*time.Second); received != nil {
				if string(received) != string(payload) {
					t.Fatal("forwarded event differs")
				}
				return
			}
			if time.Now().After(deadline) {
				t.Fatal("grant was never admitted for event forwarding")
			}
		}
	})

	t.Run("AC-01 warm events make no per-event grant loads", func(t *testing.T) {
		before := s.metrics(t)
		const events = 20
		for i := 0; i < events; i++ {
			payload := eventPayload(s.hostID, s.devices[0])
			if err := host.publish(t, "whalerein/v1/h/"+s.hostID+"/event", payload); err != nil {
				t.Fatalf("event %d: %v", i, err)
			}
			if received := deviceA.next(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[0]+"/event", 10*time.Second); received == nil {
				t.Fatalf("event %d not delivered", i)
			}
		}
		after := s.metrics(t)
		hits := after["grant_admission_hits_total"] - before["grant_admission_hits_total"]
		loads := after["grant_admission_loads_total"] - before["grant_admission_loads_total"]
		if hits < events {
			t.Fatalf("admission hits = %v, want >= %d", hits, events)
		}
		if loads != 0 {
			t.Fatalf("warm events performed %v grant loads", loads)
		}
	})

	t.Run("AC-06 receipts are authorized locally", func(t *testing.T) {
		before := s.metrics(t)
		for i := 0; i < 10; i++ {
			if err := deviceA.publish(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[0]+"/ack", []byte("ack")); err != nil {
				t.Fatalf("ack %d: %v", i, err)
			}
		}
		after := s.metrics(t)
		if delta := after["grant_admission_loads_total"] - before["grant_admission_loads_total"]; delta != 0 {
			t.Fatalf("receipts performed %v grant loads", delta)
		}
	})

	t.Run("AC-10 presence follows the current connection", func(t *testing.T) {
		if !s.waitOnline(t, 0, true) {
			t.Fatal("host never reported online")
		}
		host.close()
		if !s.waitOnline(t, 0, false) {
			t.Fatal("host stayed online after disconnect")
		}
		host = s.hostClient(t)
		if !s.waitOnline(t, 0, true) {
			t.Fatal("host did not return online after reconnect")
		}
	})

	t.Run("AC-11 Access restart preserves live host presence", func(t *testing.T) {
		restart(t, os.Getenv("WHALEREIN_ACCEPTANCE_RESTART_ACCESS"))
		waitReady(t, s.accessURL)
		if !s.waitOnline(t, 0, true) {
			t.Fatal("host presence was not reconciled after Access restart")
		}
	})

	t.Run("AC-05 broker restart keeps committed policy", func(t *testing.T) {
		restart(t, os.Getenv("WHALEREIN_ACCEPTANCE_RESTART_BROKER"))
		waitReady(t, s.accessURL)
		time.Sleep(time.Second)
		deviceB.close()
		deviceB = s.deviceClient(t, 1)
		if err := deviceB.subscribe(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[1]+"/event"); err != nil {
			t.Fatalf("device B subscribe after broker restart: %v", err)
		}
		host.close()
		host = s.hostClient(t)
		deadline := time.Now().Add(90 * time.Second)
		for {
			if err := host.publish(t, "whalerein/v1/h/"+s.hostID+"/event", eventPayload(s.hostID, s.devices[1])); err != nil {
				t.Fatalf("event publish after restart: %v", err)
			}
			if deviceB.next(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[1]+"/event", 2*time.Second) != nil {
				return
			}
			if time.Now().After(deadline) {
				t.Fatal("committed grant did not survive the broker restart")
			}
		}
	})

	t.Run("AC-04 revocation stops forwarding and survives restarts", func(t *testing.T) {
		waitReady(t, s.accessURL)
		status, body := s.request(t, http.MethodDelete, "/v1/devices/"+s.devices[0], s.adminToken)
		if status != http.StatusNoContent {
			t.Fatalf("revoke status %d: %s", status, body)
		}
		// The broker connection must be closed with its published policy.
		deadline := time.Now().Add(10 * time.Second)
		for {
			if err := deviceA.publish(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[0]+"/ack", []byte("x")); err != nil {
				break
			}
			if time.Now().After(deadline) {
				t.Fatal("revoked device connection stayed open")
			}
			time.Sleep(200 * time.Millisecond)
		}
		// Existing subscriptions stop receiving forwarded events.
		if err := host.publish(t, "whalerein/v1/h/"+s.hostID+"/event", eventPayload(s.hostID, s.devices[0])); err != nil {
			t.Fatalf("event publish after revoke: %v", err)
		}
		if deviceA.next(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[0]+"/event", 5*time.Second) != nil {
			t.Fatal("revoked device still received an event")
		}
		// Stored credentials cannot reconnect.
		retry := &mqttClient{}
		retry.conn, _ = net.DialTimeout("tcp", s.mqttAddr, 5*time.Second)
		if retry.conn != nil {
			retry.client = paho.NewClient(paho.ClientConfig{ClientID: "whalerein-device-" + s.devices[0], Conn: packets.NewThreadSafeConn(retry.conn)})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			connack, err := retry.client.Connect(ctx, &paho.Connect{ClientID: "whalerein-device-" + s.devices[0], CleanStart: true, KeepAlive: 15, UsernameFlag: true, Username: "device:" + s.devices[0], PasswordFlag: true, Password: []byte(s.tokens[0])})
			cancel()
			if err == nil && connack != nil && connack.ReasonCode == 0 {
				t.Fatal("revoked device reconnected")
			}
			retry.close()
		}
		// Other grants remain usable.
		if err := deviceB.publish(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[1]+"/ack", []byte("ok")); err != nil {
			t.Fatalf("unrelated grant lost after revoke: %v", err)
		}
		// Revocation survives an Access restart.
		restart(t, os.Getenv("WHALEREIN_ACCEPTANCE_RESTART_ACCESS"))
		waitReady(t, s.accessURL)
		if err := host.publish(t, "whalerein/v1/h/"+s.hostID+"/event", eventPayload(s.hostID, s.devices[0])); err != nil {
			t.Fatalf("event publish after restart: %v", err)
		}
		if deviceA.next(t, "whalerein/v1/h/"+s.hostID+"/d/"+s.devices[0]+"/event", 5*time.Second) != nil {
			t.Fatal("revocation did not survive the Access restart")
		}
	})
}
