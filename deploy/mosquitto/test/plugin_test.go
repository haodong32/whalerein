//go:build mosquitto

// Package mosquittotest exercises the WhaleRein Mosquitto plugin against a real
// broker process. It is the native feasibility fixture for the
// replace-emqx-with-mosquitto change and stays opt-in because it needs the
// pinned Mosquitto headers and broker binary.
//
// Run with:
//
//	MOSQUITTO_BIN=$(command -v mosquitto) \
//	MOSQUITTO_INCLUDE=$(brew --prefix)/include \
//	go test -tags mosquitto ./deploy/mosquitto/test -v
package mosquittotest

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
)

type client struct {
	id     string
	conn   net.Conn
	paho   *paho.Client
	msgs   chan paho.PublishReceived
	closed chan struct{}
}

func (c *client) close() {
	if c.paho != nil {
		_ = c.paho.Disconnect(&paho.Disconnect{ReasonCode: 0})
	}
	if c.conn != nil {
		_ = c.conn.Close()
	}
}

func dial(t *testing.T, address, id, username, password string) (*client, *paho.Connack, error) {
	t.Helper()
	conn, err := net.DialTimeout("tcp", address, 5*time.Second)
	if err != nil {
		return nil, nil, err
	}
	c := &client{id: id, conn: conn, msgs: make(chan paho.PublishReceived, 64), closed: make(chan struct{})}
	c.paho = paho.NewClient(paho.ClientConfig{
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
		OnClientError: func(err error) { close(c.closed) },
	})
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	connack, err := c.paho.Connect(ctx, &paho.Connect{
		ClientID:     id,
		CleanStart:   true,
		KeepAlive:    30,
		UsernameFlag: username != "",
		Username:     username,
		PasswordFlag: password != "",
		Password:     []byte(password),
	})
	if err != nil {
		c.close()
		return nil, nil, err
	}
	if connack.ReasonCode != 0 {
		c.close()
		return c, connack, fmt.Errorf("connack reason %d", connack.ReasonCode)
	}
	return c, connack, nil
}

func (c *client) publish(t *testing.T, topic string, payload []byte) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	response, err := c.paho.Publish(ctx, &paho.Publish{Topic: topic, QoS: 1, Payload: payload})
	if err != nil {
		return err
	}
	if response != nil && response.ReasonCode >= 0x80 {
		return fmt.Errorf("publish rejected with reason %d", response.ReasonCode)
	}
	return nil
}

func (c *client) subscribe(t *testing.T, filter string) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	suback, err := c.paho.Subscribe(ctx, &paho.Subscribe{
		Subscriptions: []paho.SubscribeOptions{{Topic: filter, QoS: 1}},
	})
	if err != nil {
		return err
	}
	for _, reason := range suback.Reasons {
		if reason >= 0x80 {
			return fmt.Errorf("subscribe rejected with reason %d", reason)
		}
	}
	return nil
}

func (c *client) next(t *testing.T, topicPrefix string) []byte {
	t.Helper()
	deadline := time.After(5 * time.Second)
	for {
		select {
		case received := <-c.msgs:
			if strings.HasPrefix(received.Packet.Topic, topicPrefix) {
				return received.Packet.Payload
			}
		case <-deadline:
			t.Fatalf("no message on %q", topicPrefix)
		}
	}
}

type authPolicy struct {
	mu       sync.Mutex
	ready    bool
	bodies   map[string]string
	fail     bool
	requests int
}

func newAuthPolicy() *authPolicy {
	return &authPolicy{ready: true, bodies: map[string]string{}}
}

func (p *authPolicy) count() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.requests
}

func (p *authPolicy) set(username, body string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.bodies[username] = body
}

func (p *authPolicy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if err := r.ParseForm(); err != nil {
		http.Error(w, "bad form", http.StatusBadRequest)
		return
	}
	if r.Header.Get("X-WhaleRein-Internal-Token") != "test-internal-token" {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	username := r.FormValue("username")
	p.mu.Lock()
	p.requests++
	body, ok := p.bodies[username]
	ready, fail := p.ready, p.fail
	p.mu.Unlock()
	if fail {
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
		return
	}
	if !ok || !ready {
		fmt.Fprint(w, "whalerein/1\ndeny unavailable\n")
		return
	}
	fmt.Fprint(w, body)
}

func freePort(t *testing.T) int {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("reserve port: %v", err)
	}
	defer listener.Close()
	return listener.Addr().(*net.TCPAddr).Port
}

type broker struct {
	address string
	process *exec.Cmd
	dir     string
}

func (b *broker) stop() {
	if b.process == nil || b.process.Process == nil {
		return
	}
	_ = b.process.Process.Signal(os.Interrupt)
	done := make(chan struct{})
	go func() { _ = b.process.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		_ = b.process.Process.Kill()
	}
}

func startBroker(t *testing.T, port int, authURL string, ttl time.Duration) *broker {
	t.Helper()
	binary := os.Getenv("MOSQUITTO_BIN")
	if binary == "" {
		path, err := exec.LookPath("mosquitto")
		if err != nil {
			t.Skip("mosquitto broker binary not available; set MOSQUITTO_BIN")
		}
		binary = path
	}
	dir := t.TempDir()
	passwordPath := filepath.Join(dir, "access-password")
	if err := os.WriteFile(passwordPath, []byte("bootstrap-password\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	tokenPath := filepath.Join(dir, "internal-token")
	if err := os.WriteFile(tokenPath, []byte("test-internal-token\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	config := fmt.Sprintf(`per_listener_settings false
allow_anonymous false
persistence false
max_packet_size 1048576
sys_interval 0
listener %d 127.0.0.1
plugin %s
plugin_opt_access_username access
plugin_opt_access_password_file %s
plugin_opt_access_auth_url %s
plugin_opt_access_auth_token_file %s
plugin_opt_auth_timeout_ms 500
plugin_opt_auth_workers 4
plugin_opt_policy_ttl_ms %d
log_dest stdout
log_type debug
log_type notice
`,
		port, pluginPath(t), passwordPath, authURL, tokenPath, ttl.Milliseconds())
	configPath := filepath.Join(dir, "mosquitto.conf")
	if err := os.WriteFile(configPath, []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}
	command := exec.Command(binary, "-c", configPath)
	command.Stdout = os.Stdout
	command.Stderr = os.Stderr
	if err := command.Start(); err != nil {
		t.Fatalf("start mosquitto: %v", err)
	}
	b := &broker{address: fmt.Sprintf("127.0.0.1:%d", port), process: command, dir: dir}
	t.Cleanup(b.stop)
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		conn, err := net.DialTimeout("tcp", b.address, 100*time.Millisecond)
		if err == nil {
			_ = conn.Close()
			return b
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("mosquitto did not listen on %s", b.address)
	return nil
}

var (
	pluginOnce sync.Once
	plugin     string
	pluginErr  error
)

func pluginPath(t *testing.T) string {
	t.Helper()
	pluginOnce.Do(func() {
		include := os.Getenv("MOSQUITTO_INCLUDE")
		if include == "" {
			if _, err := os.Stat("/opt/homebrew/include/mosquitto/broker_plugin.h"); err == nil {
				include = "/opt/homebrew/include"
			} else {
				include = "/usr/include"
			}
		}
		dir, err := os.Getwd()
		if err != nil {
			pluginErr = err
			return
		}
		pluginDir := filepath.Join(dir, "..", "plugin")
		command := exec.Command("make", "-C", pluginDir, "MOSQUITTO_INCLUDE="+include)
		command.Stdout = os.Stdout
		command.Stderr = os.Stderr
		if err := command.Run(); err != nil {
			pluginErr = fmt.Errorf("build plugin: %w", err)
			return
		}
		plugin = filepath.Join(pluginDir, "whalerein.so")
	})
	if pluginErr != nil {
		t.Fatal(pluginErr)
	}
	return plugin
}

func hostPolicy(revision uint64) string {
	return fmt.Sprintf(`whalerein/1
ok
kind host
host host1
revision %d
ready 1
acl subscribe %d - whalerein/v1/h/host1/request
acl read %d - whalerein/v1/h/host1/request
acl write %d - whalerein/v1/h/host1/result
acl write %d - whalerein/v1/h/host1/event
acl subscribe %d - whalerein/v1/h/host1/presence
acl read %d - whalerein/v1/h/host1/presence
`, revision, revision, revision, revision, revision, revision, revision)
}

func devicePolicy(revision uint64) string {
	return fmt.Sprintf(`whalerein/1
ok
kind device
device dev1
revision %d
ready 1
will whalerein/v1/presence/device/dev1
acl subscribe %d host1 whalerein/v1/h/host1/d/dev1/event
acl read %d host1 whalerein/v1/h/host1/d/dev1/event
acl write %d host1 whalerein/v1/h/host1/d/dev1/ack
acl write %d host1 whalerein/v1/presence/device/dev1
`, revision, revision, revision, revision, revision)
}

func controlClient(t *testing.T, b *broker) *client {
	t.Helper()
	c, _, err := dial(t, b.address, "whalerein-access", "access", "bootstrap-password")
	if err != nil {
		t.Fatalf("access connect: %v", err)
	}
	if err := c.subscribe(t, "whalerein/v1/control/reply"); err != nil {
		t.Fatalf("subscribe reply: %v", err)
	}
	if err := c.subscribe(t, "whalerein/v1/control/events"); err != nil {
		t.Fatalf("subscribe events: %v", err)
	}
	t.Cleanup(c.close)
	return c
}

func sendControl(t *testing.T, control *client, id, op, fields string) map[string]string {
	t.Helper()
	payload := fmt.Sprintf("whalerein/1\nid %s\nop %s\n%s", id, op, fields)
	if err := control.publish(t, "$CONTROL/whalerein/v1", []byte(payload)); err != nil {
		t.Fatalf("control %s: %v", op, err)
	}
	deadline := time.After(5 * time.Second)
	for {
		select {
		case received := <-control.msgs:
			if received.Packet.Topic != "whalerein/v1/control/reply" {
				continue
			}
			lines := strings.Split(string(received.Packet.Payload), "\n")
			fields := map[string]string{}
			for _, line := range lines[1:] {
				key, value, ok := strings.Cut(line, " ")
				if !ok {
					if line != "" {
						fields[line] = "1"
					}
					continue
				}
				fields[key] = value
			}
			if fields["id"] != id {
				t.Fatalf("reply id %q, want %q", fields["id"], id)
			}
			if _, ok := fields["ok"]; !ok {
				t.Fatalf("control %s failed: %s", op, string(received.Packet.Payload))
			}
			return fields
		case <-deadline:
			t.Fatalf("no reply for control %s", op)
		}
	}
}

func (c *client) expectNoMessage(t *testing.T, topicPrefix string, window time.Duration) {
	t.Helper()
	deadline := time.After(window)
	for {
		select {
		case received := <-c.msgs:
			if strings.HasPrefix(received.Packet.Topic, topicPrefix) {
				t.Fatalf("unexpected message on %q: %s", topicPrefix, received.Packet.Payload)
			}
		case <-deadline:
			return
		}
	}
}

func waitFor(t *testing.T, timeout time.Duration, condition func() bool) bool {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if condition() {
			return true
		}
		time.Sleep(20 * time.Millisecond)
	}
	return false
}

func TestNativeBrokerCapabilities(t *testing.T) {
	if _, err := exec.LookPath("cc"); err != nil {
		t.Skip("C compiler not available")
	}
	policy := newAuthPolicy()
	auth := httptest.NewServer(policy)
	t.Cleanup(auth.Close)
	policy.set("host:host1", hostPolicy(1))
	policy.set("device:dev1", devicePolicy(1))

	port := freePort(t)
	b := startBroker(t, port, auth.URL+"/internal/broker/connect", 3*time.Second)
	control := controlClient(t, b)

	t.Run("bootstrap_identity_authenticates_without_access_http", func(t *testing.T) {
		_, connack, err := dial(t, b.address, "whalerein-access-2", "access", "bootstrap-password")
		if err != nil {
			t.Fatalf("bootstrap connect: %v", err)
		}
		if connack.ReasonCode != 0 {
			t.Fatalf("bootstrap connack %d", connack.ReasonCode)
		}
		if _, _, err := dial(t, b.address, "whalerein-access-3", "access", "wrong-password"); err == nil {
			t.Fatal("wrong bootstrap password accepted")
		}
	})

	t.Run("deferred_connect_authentication", func(t *testing.T) {
		started := time.Now()
		host, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("host connect: %v", err)
		}
		defer host.close()
		if elapsed := time.Since(started); elapsed > 2*time.Second {
			t.Fatalf("deferred authentication took %s", elapsed)
		}
	})

	t.Run("acl_write_subscribe_read", func(t *testing.T) {
		host, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("host connect: %v", err)
		}
		defer host.close()
		if err := host.publish(t, "whalerein/v1/h/host1/result", []byte("ok")); err != nil {
			t.Fatalf("allowed publish: %v", err)
		}
		if err := host.publish(t, "whalerein/v1/h/other/result", []byte("x")); err == nil {
			t.Fatal("foreign topic publish accepted")
		}
		if err := host.publish(t, "whalerein/v1/h/host1/request", []byte("x")); err == nil {
			t.Fatal("unauthorized direction accepted")
		}
		if err := host.subscribe(t, "whalerein/v1/h/other/request"); err == nil {
			t.Fatal("foreign subscription accepted")
		}
		if err := host.subscribe(t, "whalerein/v1/h/host1/request"); err != nil {
			t.Fatalf("own subscription: %v", err)
		}
	})

	t.Run("device_subscribe_and_delivery", func(t *testing.T) {
		host, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("host connect: %v", err)
		}
		defer host.close()
		device, _, err := dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
		if err != nil {
			t.Fatalf("device connect: %v", err)
		}
		defer device.close()
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev1/event"); err != nil {
			t.Fatalf("device subscribe: %v", err)
		}
		if err := control.publish(t, "whalerein/v1/h/host1/d/dev1/event", []byte("payload")); err != nil {
			t.Fatalf("event publish: %v", err)
		}
		if got := string(device.next(t, "whalerein/v1/h/host1/d/dev1/event")); got != "payload" {
			t.Fatalf("delivered %q", got)
		}
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev2/event"); err == nil {
			t.Fatal("foreign device subscription accepted")
		}
		if err := device.publish(t, "whalerein/v1/presence/device/dev2", []byte("{}")); err == nil {
			t.Fatal("foreign will topic publish accepted")
		}
	})

	t.Run("probe_denied_everywhere", func(t *testing.T) {
		policy.set("probe:host1", "whalerein/1\nok\nkind probe\nrevision 1\nready 1\n")
		probe, _, err := dial(t, b.address, "whalerein-probe-00000000000000000000000000000001", "probe:host1", "host-token")
		if err != nil {
			t.Fatalf("probe connect: %v", err)
		}
		defer probe.close()
		if err := probe.publish(t, "whalerein/v1/h/host1/result", []byte("x")); err == nil {
			t.Fatal("probe publish accepted")
		}
		if err := probe.subscribe(t, "whalerein/v1/h/host1/request"); err == nil {
			t.Fatal("probe subscribe accepted")
		}
	})

	t.Run("snapshot_and_presence_notifications", func(t *testing.T) {
		host, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("host connect: %v", err)
		}
		defer host.close()
		// Drain connect notifications and then snapshot.
		time.Sleep(100 * time.Millisecond)
		for len(control.msgs) > 0 {
			<-control.msgs
		}
		fields := sendControl(t, control, "snap-1", "snapshot", "")
		if _, ok := fields["seq"]; !ok {
			t.Fatalf("snapshot without seq: %v", fields)
		}
		host.close()
		payment := control.next(t, "whalerein/v1/control/events")
		if !strings.Contains(string(payment), "event disconnected") || !strings.Contains(string(payment), "whalerein-host-host1") {
			t.Fatalf("disconnect notification: %s", payment)
		}
	})

	t.Run("control_policy_update_and_kick", func(t *testing.T) {
		device, _, err := dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
		if err != nil {
			t.Fatalf("device connect: %v", err)
		}
		defer device.close()
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev1/event"); err != nil {
			t.Fatalf("device subscribe: %v", err)
		}
		fields := sendControl(t, control, "deny-1", "deny",
			"device dev1\ngeneration 2\nhost host1\nkick 1\n")
		if _, ok := fields["applied_generation"]; !ok {
			t.Fatalf("deny reply: %v", fields)
		}
		if waitFor(t, 3*time.Second, func() bool {
			select {
			case <-device.closed:
				return true
			default:
				return false
			}
		}) {
			return
		}
		// A broker that closes without OnClientError still fails writes.
		if err := device.publish(t, "whalerein/v1/h/host1/d/dev1/ack", []byte("x")); err == nil {
			t.Fatal("revoked connection still publishes")
		}
	})

	t.Run("policy_validity_expires_without_confirmation", func(t *testing.T) {
		policy.set("host:host1", hostPolicy(5))
		host, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("host connect: %v", err)
		}
		defer host.close()
		if err := host.publish(t, "whalerein/v1/h/host1/result", []byte("ok")); err != nil {
			t.Fatalf("fresh publish: %v", err)
		}
		time.Sleep(3500 * time.Millisecond)
		if err := host.publish(t, "whalerein/v1/h/host1/result", []byte("stale")); err == nil {
			t.Fatal("stale policy still authorized delivery")
		}
		fields := sendControl(t, control, "ping-2", "ping", "revision 5\n")
		if _, ok := fields["ok"]; !ok {
			t.Fatalf("ping failed: %v", fields)
		}
	})

	t.Run("unauthorized_will_is_denied", func(t *testing.T) {
		if err := control.subscribe(t, "whalerein/v1/presence/device/dev2"); err != nil {
			t.Fatalf("subscribe will topic: %v", err)
		}
		conn, err := net.DialTimeout("tcp", b.address, 5*time.Second)
		if err != nil {
			t.Fatal(err)
		}
		c := &client{id: "whalerein-device-dev1", conn: conn, msgs: make(chan paho.PublishReceived, 4)}
		c.paho = paho.NewClient(paho.ClientConfig{ClientID: c.id, Conn: packets.NewThreadSafeConn(conn)})
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		connack, err := c.paho.Connect(ctx, &paho.Connect{
			ClientID:     c.id,
			CleanStart:   true,
			KeepAlive:    30,
			UsernameFlag: true,
			Username:     "device:dev1",
			PasswordFlag: true,
			Password:     []byte("device-token"),
			WillMessage:  &paho.WillMessage{Topic: "whalerein/v1/presence/device/dev2", Payload: []byte("{}"), QoS: 1},
		})
		if err != nil || connack == nil || connack.ReasonCode != 0 {
			// Rejecting the connection outright is also correct.
			c.close()
			return
		}
		// An abrupt close publishes the Will. The broker must deny it because
		// the topic is outside this principal's local policy.
		_ = conn.Close()
		control.expectNoMessage(t, "whalerein/v1/presence/device/dev2", 500*time.Millisecond)
	})

	t.Run("late_policy_cannot_restore_revoked_authority", func(t *testing.T) {
		policy.set("device:dev1", devicePolicy(1))
		sendControl(t, control, "deny-fence", "deny", "device dev1\ngeneration 3\nhost host1\nkick 0\n")
		device, _, err := dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
		if err != nil {
			t.Fatalf("device connect: %v", err)
		}
		defer device.close()
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev1/event"); err == nil {
			t.Fatal("stale policy was installed after a newer fence")
		}
		// A newer revision is required to restore authority.
		policy.set("device:dev1", devicePolicy(4))
		device.close()
		device, _, err = dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
		if err != nil {
			t.Fatalf("device reconnect: %v", err)
		}
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev1/event"); err != nil {
			t.Fatalf("newer policy subscribe: %v", err)
		}
		fields := sendControl(t, control, "set-older", "set",
			"client whalerein-device-dev1\nkind device\ndevice dev1\nrevision 2\nttl 5000\n"+
				"acl subscribe 2 host1 whalerein/v1/h/host1/d/dev1/event\n")
		if fields["stale"] != "1" {
			t.Fatalf("older set was not fenced: %v", fields)
		}
	})

	t.Run("authorization_requests_only_on_connect", func(t *testing.T) {
		before := policy.count()
		host, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("host connect: %v", err)
		}
		defer host.close()
		for i := 0; i < 20; i++ {
			if err := host.publish(t, "whalerein/v1/h/host1/result", []byte("ok")); err != nil {
				t.Fatalf("publish %d: %v", i, err)
			}
		}
		if err := host.subscribe(t, "whalerein/v1/h/host1/request"); err != nil {
			t.Fatalf("subscribe: %v", err)
		}
		if delta := policy.count() - before; delta != 1 {
			t.Fatalf("expected one authentication request per connection, got %d", delta)
		}
	})

	t.Run("access_outage_denies_new_external_connections", func(t *testing.T) {
		policy.mu.Lock()
		policy.fail = true
		policy.mu.Unlock()
		if _, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token"); err == nil {
			t.Fatal("connection accepted while policy source unavailable")
		}
		policy.mu.Lock()
		policy.fail = false
		policy.mu.Unlock()
	})

	t.Run("broker_restart_clears_policy_and_presence", func(t *testing.T) {
		b.stop()
		policy.mu.Lock()
		policy.fail = true
		policy.mu.Unlock()
		restarted := startBroker(t, port, auth.URL+"/internal/broker/connect", 3*time.Second)
		if _, _, err := dial(t, restarted.address, "whalerein-host-host1", "host:host1", "host-token"); err == nil {
			t.Fatal("restarted broker accepted an external client without fresh policy")
		}
		policy.mu.Lock()
		policy.fail = false
		policy.mu.Unlock()
		restartedControl := controlClient(t, restarted)
		fields := sendControl(t, restartedControl, "snap-restart", "snapshot", "")
		if fields["clients"] != "" {
			t.Fatalf("restarted broker reported stale presence")
		}
		host, _, err := dial(t, restarted.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("host connect after restart: %v", err)
		}
		defer host.close()
		if err := host.publish(t, "whalerein/v1/h/host1/result", []byte("ok")); err != nil {
			t.Fatalf("publish after restart: %v", err)
		}
	})
}
