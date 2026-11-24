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
		return c, connack, err
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
	if !ok {
		fmt.Fprint(w, "whalerein/1\ndeny unauthorized\n")
		return
	}
	if !ready {
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
	pluginFile := pluginPath(t)
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
		port, pluginFile, passwordPath, authURL, tokenPath, ttl.Milliseconds())
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

func hostPolicyFor(revision uint64, host string) string {
	return fmt.Sprintf(`whalerein/1
ok
kind host
host %s
revision %d
ready 1
acl subscribe %d - whalerein/v1/h/%s/request
acl read %d - whalerein/v1/h/%s/request
acl write %d - whalerein/v1/h/%s/result
acl write %d - whalerein/v1/h/%s/event
acl write %d - whalerein/v1/h/%s/live
acl subscribe %d - whalerein/v1/h/%s/presence
acl read %d - whalerein/v1/h/%s/presence
`, host, revision, revision, host, revision, host, revision, host, revision, host, revision, host, revision, host, revision, host)
}

func hostPolicy(revision uint64) string { return hostPolicyFor(revision, "host1") }

// devicePolicyFor builds one device policy with live grants for each host,
// modelling one device identity authorized by several Hosts on one connection.
func devicePolicyFor(revision uint64, hosts ...string) string {
	builder := fmt.Sprintf(`whalerein/1
ok
kind device
device dev1
revision %d
ready 1
will whalerein/v1/presence/device/dev1
`, revision)
	for _, host := range hosts {
		builder += fmt.Sprintf(`acl subscribe %d %s whalerein/v1/h/%s/d/dev1/event
acl read %d %s whalerein/v1/h/%s/d/dev1/event
acl subscribe %d %s whalerein/v1/h/%s/d/dev1/notice
acl read %d %s whalerein/v1/h/%s/d/dev1/notice
acl subscribe %d %s whalerein/v1/h/%s/d/dev1/live
acl read %d %s whalerein/v1/h/%s/d/dev1/live
acl write %d %s whalerein/v1/h/%s/d/dev1/ready
`, revision, host, host, revision, host, host,
			revision, host, host, revision, host, host,
			revision, host, host, revision, host, host,
			revision, host, host)
	}
	builder += fmt.Sprintf(`acl write %d - whalerein/v1/presence/device/dev1
`, revision)
	return builder
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
acl subscribe %d host1 whalerein/v1/h/host1/d/dev1/notice
acl read %d host1 whalerein/v1/h/host1/d/dev1/notice
acl subscribe %d host1 whalerein/v1/h/host1/d/dev1/live
acl read %d host1 whalerein/v1/h/host1/d/dev1/live
acl write %d host1 whalerein/v1/h/host1/d/dev1/ready
acl write %d host1 whalerein/v1/presence/device/dev1
`, revision, revision, revision, revision, revision, revision, revision, revision, revision)
}

func controlClient(t *testing.T, b *broker) *client {
	return controlClientID(t, b, "whalerein-access")
}

func controlClientID(t *testing.T, b *broker, id string) *client {
	t.Helper()
	c, _, err := dial(t, b.address, id, "access", "bootstrap-password")
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
		if err := host.publish(t, "whalerein/v1/h/host1/live", []byte("preview")); err != nil {
			t.Fatalf("host live publish: %v", err)
		}
		if err := host.publish(t, "whalerein/v1/h/host1/d/dev1/live", []byte("preview")); err == nil {
			t.Fatal("host device live publish accepted")
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
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev1/live"); err != nil {
			t.Fatalf("device live subscribe: %v", err)
		}
		if err := control.publish(t, "whalerein/v1/h/host1/d/dev1/event", []byte("payload")); err != nil {
			t.Fatalf("event publish: %v", err)
		}
		if got := string(device.next(t, "whalerein/v1/h/host1/d/dev1/event")); got != "payload" {
			t.Fatalf("delivered %q", got)
		}
		if err := control.publish(t, "whalerein/v1/h/host1/d/dev1/live", []byte("preview")); err != nil {
			t.Fatalf("live publish: %v", err)
		}
		if got := string(device.next(t, "whalerein/v1/h/host1/d/dev1/live")); got != "preview" {
			t.Fatalf("delivered live %q", got)
		}
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev2/event"); err == nil {
			t.Fatal("foreign device subscription accepted")
		}
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev2/live"); err == nil {
			t.Fatal("foreign device live subscription accepted")
		}
		if err := device.publish(t, "whalerein/v1/presence/device/dev2", []byte("{}")); err == nil {
			t.Fatal("foreign will topic publish accepted")
		}
		// The ready advertisement is the only Device transport publication; the
		// retired acknowledgement topic is denied.
		if err := device.publish(t, "whalerein/v1/h/host1/d/dev1/ready", []byte("whalerein/1")); err != nil {
			t.Fatalf("device ready publish: %v", err)
		}
		if err := device.publish(t, "whalerein/v1/h/host1/d/dev1/ack", []byte("x")); err == nil {
			t.Fatal("retired device acknowledgement accepted")
		}
	})

	t.Run("access_relay_topics", func(t *testing.T) {
		// Access publishes the device notice and live topics and subscribes to
		// device advertisements and host live previews. The internal Access
		// identity is trusted by the plugin and is not subject to its own ACL
		// array; external principals are checked against their installed policy.
		if err := control.publish(t, "whalerein/v1/h/host1/d/dev1/notice", []byte("notice")); err != nil {
			t.Fatalf("access notice publish: %v", err)
		}
		if err := control.subscribe(t, "whalerein/v1/h/+/d/+/ready"); err != nil {
			t.Fatalf("access ready subscribe: %v", err)
		}
		if err := control.subscribe(t, "whalerein/v1/h/+/live"); err != nil {
			t.Fatalf("access host live subscribe: %v", err)
		}
		if err := control.publish(t, "whalerein/v1/h/host1/d/dev1/live", []byte("preview")); err != nil {
			t.Fatalf("access device live publish: %v", err)
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
		if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev1/live"); err != nil {
			t.Fatalf("device live subscribe: %v", err)
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
		if err := device.publish(t, "whalerein/v1/h/host1/d/dev1/ready", []byte("x")); err == nil {
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
		sendControl(t, control, "ping-will", "ping", "revision 0\n")
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
		sendControl(t, control, "ping-fence", "ping", "revision 0\n")
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
		sendControl(t, control, "ping-auth", "ping", "revision 0\n")
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
		sendControl(t, control, "ping-outage", "ping", "revision 0\n")
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

// dialWithWill connects an external principal that registers a last will, which
// a clean DISCONNECT must not publish and an abrupt socket close must.
func dialWithWill(t *testing.T, address, id, username, password, willTopic string) (*client, *paho.Connack, error) {
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
		UsernameFlag: true,
		Username:     username,
		PasswordFlag: password != "",
		Password:     []byte(password),
		WillMessage:  &paho.WillMessage{Topic: willTopic, Payload: []byte(`{"event":"device_offline","device_id":"dev1"}`), QoS: 1},
	})
	if err != nil {
		c.close()
		return c, connack, err
	}
	if connack.ReasonCode != 0 {
		c.close()
		return c, connack, fmt.Errorf("connack reason %d", connack.ReasonCode)
	}
	return c, connack, nil
}

// nextEventContaining consumes control events until one carries every marker.
func nextEventContaining(t *testing.T, control *client, markers ...string) []byte {
	t.Helper()
	deadline := time.After(5 * time.Second)
	for {
		select {
		case received := <-control.msgs:
			if received.Packet.Topic != "whalerein/v1/control/events" {
				continue
			}
			payload := received.Packet.Payload
			matched := true
			for _, marker := range markers {
				if !strings.Contains(string(payload), marker) {
					matched = false
					break
				}
			}
			if matched {
				return payload
			}
		case <-deadline:
			t.Fatalf("no control event containing %v", markers)
		}
	}
}

// TestNativeBrokerPresenceLifecycleAndRevocationScopes covers the broker-side
// edges the Go unit fixtures cannot reach: a real last will on a clean versus
// abrupt close, duplicate client-id takeover, one device connection holding
// grants for two Hosts, and the CONNACK reason that distinguishes stale policy
// from invalid credentials.
func TestNativeBrokerPresenceLifecycleAndRevocationScopes(t *testing.T) {
	if _, err := exec.LookPath("cc"); err != nil {
		t.Skip("C compiler not available")
	}
	policy := newAuthPolicy()
	auth := httptest.NewServer(policy)
	t.Cleanup(auth.Close)
	policy.set("host:host1", hostPolicyFor(1, "host1"))
	policy.set("host:host2", hostPolicyFor(1, "host2"))
	policy.set("device:dev1", devicePolicyFor(5, "host1"))

	port := freePort(t)
	b := startBroker(t, port, auth.URL+"/internal/broker/connect", 5*time.Second)
	control := controlClientID(t, b, "whalerein-access-lifecycle")
	sendControl(t, control, "ping-lifecycle", "ping", "revision 0\n")

	t.Run("clean_close_does_not_publish_a_will_but_abrupt_close_does", func(t *testing.T) {
		if err := control.subscribe(t, "whalerein/v1/presence/device/dev1"); err != nil {
			t.Fatalf("subscribe will: %v", err)
		}
		clean, _, err := dialWithWill(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token", "whalerein/v1/presence/device/dev1")
		if err != nil {
			t.Fatalf("clean connect: %v", err)
		}
		// A clean MQTT DISCONNECT must not be reported as an ungraceful edge.
		clean.close()
		control.expectNoMessage(t, "whalerein/v1/presence/device/dev1", 500*time.Millisecond)

		abrupt, _, err := dialWithWill(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token", "whalerein/v1/presence/device/dev1")
		if err != nil {
			t.Fatalf("abrupt connect: %v", err)
		}
		// Close the transport without DISCONNECT: the broker publishes the will.
		_ = abrupt.conn.Close()
		payload := control.next(t, "whalerein/v1/presence/device/dev1")
		if !strings.Contains(string(payload), "device_offline") {
			t.Fatalf("will payload = %q", payload)
		}
	})

	t.Run("duplicate_client_id_takeover_suppresses_the_superseded_disconnect", func(t *testing.T) {
		events := controlClientID(t, b, "whalerein-access-takeover")
		first, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("first connect: %v", err)
		}
		connected := nextEventContaining(t, events, "whalerein-host-host1", "event connected")
		_ = connected
		second, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
		if err != nil {
			t.Fatalf("takeover connect: %v", err)
		}
		nextEventContaining(t, events, "whalerein-host-host1", "event connected")
		// The superseded connection's close must not report the newer
		// connection offline; the plugin owns that branch.
		first.close()
		events.expectNoMessage(t, "whalerein/v1/control/events", 400*time.Millisecond)
		second.close()
		nextEventContaining(t, events, "whalerein-host-host1", "event disconnected")
	})

	t.Run("host_scoped_revocation_keeps_another_hosts_connection", func(t *testing.T) {
		policy.set("device:dev1", devicePolicyFor(5, "host1", "host2"))
		revoke := controlClientID(t, b, "whalerein-access-revoke")
		device, _, err := dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
		if err != nil {
			t.Fatalf("device connect: %v", err)
		}
		defer device.close()
		if err := device.subscribe(t, "whalerein/v1/h/host2/d/dev1/event"); err != nil {
			t.Fatalf("host2 subscribe: %v", err)
		}
		if err := device.publish(t, "whalerein/v1/h/host1/d/dev1/ready", []byte("before")); err != nil {
			t.Fatalf("host1 publish before revoke: %v", err)
		}
		fields := sendControl(t, revoke, "deny-host1", "deny",
			"device dev1\ngeneration 5\nhost host1\nkick 1\n")
		if fields["matched"] != "1" {
			t.Fatalf("host1 deny matched %q", fields["matched"])
		}
		// The connection still holds host2 authority, so it must stay open.
		select {
		case <-device.closed:
			t.Fatal("host-scoped revocation closed a connection with another Host's grant")
		case <-time.After(300 * time.Millisecond):
		}
		if err := device.publish(t, "whalerein/v1/h/host2/d/dev1/ready", []byte("after")); err != nil {
			t.Fatalf("host2 publish after revoking host1: %v", err)
		}
		if err := device.publish(t, "whalerein/v1/h/host1/d/dev1/ready", []byte("after")); err == nil {
			t.Fatal("revoked host1 topic still accepts publishes")
		}
		sendControl(t, revoke, "deny-host2", "deny",
			"device dev1\ngeneration 5\nhost host2\nkick 1\n")
		if !waitFor(t, 3*time.Second, func() bool {
			select {
			case <-device.closed:
				return true
			default:
				return false
			}
		}) {
			t.Fatal("last Host revocation did not close the connection")
		}
	})

	t.Run("stale_policy_connack_is_distinct_from_invalid_credentials", func(t *testing.T) {
		stalePort := freePort(t)
		stale := startBroker(t, stalePort, auth.URL+"/internal/broker/connect", time.Second)
		staleControl := controlClientID(t, stale, "whalerein-access-stale")
		sendControl(t, staleControl, "ping-stale", "ping", "revision 0\n")
		// Invalid credentials while policy is fresh: not authorized.
		_, connack, _ := dial(t, stale.address, "whalerein-host-unknown", "host:unknown", "token")
		if connack == nil || connack.ReasonCode != 0x87 {
			t.Fatalf("invalid credential connack = %+v", connack)
		}
		// Let the confirmed policy window lapse without a refresh. The stale
		// gate runs synchronously in the auth callback, so Mosquitto preserves
		// server-unavailable; the deferred completion path collapses every
		// failure to not-authorized and cannot carry a distinct reason.
		time.Sleep(1300 * time.Millisecond)
		_, connack, _ = dial(t, stale.address, "whalerein-host-host1", "host:host1", "host-token")
		if connack == nil || connack.ReasonCode != 0x88 {
			t.Fatalf("stale policy connack = %+v", connack)
		}
	})
}

func TestNativeBrokerManagedSessionExpiryRenewalAndScopedLogout(t *testing.T) {
	policy := newAuthPolicy()
	auth := httptest.NewServer(policy)
	t.Cleanup(auth.Close)
	b := startBroker(t, freePort(t), auth.URL+"/internal/broker/connect", 10*time.Second)
	control := controlClient(t, b)
	snapshot := sendControl(t, control, "managed-capability", "snapshot", "")
	if snapshot["session_capability"] != "1" {
		t.Fatal("broker does not announce the managed session capability")
	}
	const readyTopic = "whalerein/v1/h/host1/d/dev1/ready"
	const eventTopic = "whalerein/v1/h/host1/d/dev1/event"
	setPolicy := func(session string, deadline time.Time) {
		policy.set("session:"+session, fmt.Sprintf(
			"whalerein/1\nok\nkind device\ndevice dev1\nrevision 1\nready 1\nsession %s 1 %d\n"+
				"acl write 1 host1 %s\nacl subscribe 1 host1 %s\nacl read 1 host1 %s\n",
			session, deadline.UnixMilli(), readyTopic, eventTopic, eventTopic))
	}
	open := func(session string) *client {
		t.Helper()
		conn, _, err := dial(t, b.address, "whalerein-session-"+session, "session:"+session, "fixture-token")
		if err != nil {
			t.Fatalf("open %s: %v", session, err)
		}
		t.Cleanup(conn.close)
		if err := conn.subscribe(t, eventTopic); err != nil {
			t.Fatalf("subscribe %s: %v", session, err)
		}
		return conn
	}
	start := time.Now()
	initial := start.Add(1200 * time.Millisecond)
	setPolicy("old-login", initial)
	setPolicy("new-login", start.Add(8*time.Second))
	old := open("old-login")
	current := open("new-login")
	renewed := start.Add(3500 * time.Millisecond)
	sendControl(t, control, "managed-renew", "renew_session", fmt.Sprintf(
		"session old-login\nsession_revision 2\ndeadline_ms %d\n", renewed.UnixMilli()))
	time.Sleep(time.Until(initial.Add(20 * time.Millisecond)))
	if err := old.publish(t, readyTopic, []byte("after renewal")); err != nil {
		t.Fatalf("renewed connection did not survive the first deadline: %v", err)
	}
	sendControl(t, control, "managed-revoke", "end_session", "session old-login\n")
	select {
	case <-old.closed:
	case <-time.After(2 * time.Second):
		t.Fatal("revocation did not close the live old session")
	}
	// A delayed auth response still claims this credential is valid.
	setPolicy("old-login", start.Add(8*time.Second))
	if conn, _, err := dial(t, b.address, "whalerein-session-old-login-late", "session:old-login", "fixture-token"); err == nil {
		conn.close()
		t.Fatal("deferred CONNECT reopened a fenced session")
	}
	if err := current.publish(t, readyTopic, []byte("sibling is live")); err != nil {
		t.Fatalf("old logout interrupted the new session of the same Device: %v", err)
	}
	setPolicy("partitioned", time.Now().Add(time.Second))
	idle := open("partitioned")
	select {
	case <-idle.closed:
	case <-time.After(3 * time.Second):
		t.Fatal("idle transport survived without renewal")
	}
	if err := current.publish(t, readyTopic, []byte("still live")); err != nil {
		t.Fatalf("unrelated expiry interrupted the new session: %v", err)
	}
}

func (c *client) publishWith(t *testing.T, topic string, qos byte, properties *paho.PublishProperties, payload []byte) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	response, err := c.paho.Publish(ctx, &paho.Publish{
		Topic: topic, QoS: qos, Properties: properties, Payload: payload,
	})
	if err != nil {
		return err
	}
	if response != nil && response.ReasonCode >= 0x80 {
		return fmt.Errorf("publish rejected with reason %d", response.ReasonCode)
	}
	return nil
}

func expiry(seconds uint32) *paho.PublishProperties {
	return &paho.PublishProperties{MessageExpiry: &seconds}
}

// nextConnectionRef reads the ordered lifecycle event for one client and
// returns the Broker-issued connection reference.
func nextConnectionRef(t *testing.T, control *client, clientID string) string {
	t.Helper()
	event := nextEventContaining(t, control, "event connected", clientID)
	for _, line := range strings.Split(string(event), "\n") {
		if strings.HasPrefix(line, "ref ") {
			return strings.TrimPrefix(line, "ref ")
		}
	}
	t.Fatalf("connect event carries no connection ref: %s", event)
	return ""
}

func TestNativeBrokerConnectionIdentityAndDeliveryFencing(t *testing.T) {
	if _, err := exec.LookPath("cc"); err != nil {
		t.Skip("C compiler not available")
	}
	policy := newAuthPolicy()
	auth := httptest.NewServer(policy)
	t.Cleanup(auth.Close)
	policy.set("host:host1", `whalerein/1
ok
kind host
host host1
revision 3
ready 1
acl subscribe 3 - whalerein/v1/h/host1/query
acl read 3 - whalerein/v1/h/host1/query
acl write 3 - whalerein/v1/h/host1/query-result
`)
	policy.set("device:dev1", `whalerein/1
ok
kind device
device dev1
revision 3
ready 1
acl write 3 host1 whalerein/v1/h/host1/d/dev1/query
acl subscribe 3 host1 whalerein/v1/h/host1/d/dev1/reply
acl read 3 host1 whalerein/v1/h/host1/d/dev1/reply
`)

	port := freePort(t)
	b := startBroker(t, port, auth.URL+"/internal/broker/connect", 30*time.Second)
	control := controlClient(t, b)
	if err := control.subscribe(t, "whalerein/v1/h/host1/query-result"); err != nil {
		t.Fatalf("access subscribe query-result: %v", err)
	}

	host, _, err := dial(t, b.address, "whalerein-host-host1", "host:host1", "host-token")
	if err != nil {
		t.Fatalf("host connect: %v", err)
	}
	defer host.close()

	t.Run("ingress_is_stamped_with_the_broker_connection_reference", func(t *testing.T) {
		time.Sleep(100 * time.Millisecond)
		if err := host.publishWith(t, "whalerein/v1/h/host1/query-result", 1, expiry(30), []byte("result")); err != nil {
			t.Fatalf("host query-result publish: %v", err)
		}
		wantRef := nextConnectionRef(t, control, "whalerein-host-host1")
		deadline := time.After(5 * time.Second)
		for {
			select {
			case received := <-control.msgs:
				if received.Packet.Topic != "whalerein/v1/h/host1/query-result" {
					continue
				}
				got := received.Packet.Properties.User.Get("whalerein-connection")
				if got != wantRef {
					t.Fatalf("ingress stamp %q, want %q", got, wantRef)
				}
				return
			case <-deadline:
				t.Fatal("no stamped query-result delivered")
			}
		}
	})

	t.Run("client_supplied_connection_stamp_is_rejected", func(t *testing.T) {
		properties := expiry(30)
		properties.User.Add("whalerein-connection", "forged:1")
		if err := host.publishWith(t, "whalerein/v1/h/host1/query-result", 1, properties, []byte("forged")); err == nil {
			t.Fatal("forged connection stamp accepted")
		}
	})

	device, _, err := dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
	if err != nil {
		t.Fatalf("device connect: %v", err)
	}
	defer device.close()
	if err := device.subscribe(t, "whalerein/v1/h/host1/d/dev1/reply"); err != nil {
		t.Fatalf("device subscribe reply: %v", err)
	}
	time.Sleep(100 * time.Millisecond)
	deviceRef := nextConnectionRef(t, control, "whalerein-device-dev1")

	t.Run("reply_requires_the_exact_live_connection_target", func(t *testing.T) {
		properties := expiry(30)
		if err := control.publishWith(t, "whalerein/v1/h/host1/d/dev1/reply", 1, properties, []byte("untargeted")); err != nil {
			t.Fatalf("untargeted reply publish: %v", err)
		}
		device.expectNoMessage(t, "whalerein/v1/h/host1/d/dev1/reply", 300*time.Millisecond)

		properties = expiry(30)
		properties.User.Add("whalerein-target", "stale:1")
		if err := control.publishWith(t, "whalerein/v1/h/host1/d/dev1/reply", 1, properties, []byte("stale")); err != nil {
			t.Fatalf("stale targeted reply publish: %v", err)
		}
		device.expectNoMessage(t, "whalerein/v1/h/host1/d/dev1/reply", 300*time.Millisecond)

		properties = expiry(30)
		properties.User.Add("whalerein-target", deviceRef)
		if err := control.publishWith(t, "whalerein/v1/h/host1/d/dev1/reply", 1, properties, []byte("targeted")); err != nil {
			t.Fatalf("targeted reply publish: %v", err)
		}
		if got := string(device.next(t, "whalerein/v1/h/host1/d/dev1/reply")); got != "targeted" {
			t.Fatalf("delivered %q", got)
		}
	})

}

func TestNativeBrokerEpochChangesAcrossRestart(t *testing.T) {
	if _, err := exec.LookPath("cc"); err != nil {
		t.Skip("C compiler not available")
	}
	policy := newAuthPolicy()
	auth := httptest.NewServer(policy)
	t.Cleanup(auth.Close)
	policy.set("host:host1", hostPolicy(1))
	policy.set("device:dev1", devicePolicy(1))

	port := freePort(t)
	b := startBroker(t, port, auth.URL+"/internal/broker/connect", 10*time.Second)
	control := controlClient(t, b)
	first := sendControl(t, control, "snap-epoch-1", "snapshot", "")["epoch"]
	if first == "" {
		t.Fatal("first broker epoch is empty")
	}
	b.stop()
	restarted := startBroker(t, port, auth.URL+"/internal/broker/connect", 10*time.Second)
	second := sendControl(t, controlClient(t, restarted), "snap-epoch-2", "snapshot", "")["epoch"]
	if second == "" || second == first {
		t.Fatalf("broker epoch did not change across restart: %q then %q", first, second)
	}
}

// nextConnectionIdentity returns the Broker reference and numeric connection
// id from the ordered lifecycle event for one accepted connection.
func nextConnectionIdentity(t *testing.T, control *client, clientID string) (string, string) {
	t.Helper()
	event := nextEventContaining(t, control, "event connected", clientID)
	ref := ""
	conn := ""
	for _, line := range strings.Split(string(event), "\n") {
		switch {
		case strings.HasPrefix(line, "ref "):
			ref = strings.TrimPrefix(line, "ref ")
		case strings.HasPrefix(line, "conn "):
			conn = strings.TrimPrefix(line, "conn ")
		}
	}
	if ref == "" || conn == "" {
		t.Fatalf("connect event lacks connection identity: %s", event)
	}
	return ref, conn
}

func TestNativeBrokerSameIDReplacementIsolation(t *testing.T) {
	if _, err := exec.LookPath("cc"); err != nil {
		t.Skip("C compiler not available")
	}
	policy := newAuthPolicy()
	auth := httptest.NewServer(policy)
	t.Cleanup(auth.Close)
	policy.set("device:dev1", `whalerein/1
ok
kind device
device dev1
revision 6
ready 1
acl write 6 host1 whalerein/v1/h/host1/d/dev1/query
acl subscribe 6 host1 whalerein/v1/h/host1/d/dev1/reply
acl read 6 host1 whalerein/v1/h/host1/d/dev1/reply
acl write 6 host1 whalerein/v1/h/host1/d/dev1/ready
`)

	port := freePort(t)
	b := startBroker(t, port, auth.URL+"/internal/broker/connect", 30*time.Second)
	control := controlClient(t, b)

	first, _, err := dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
	if err != nil {
		t.Fatalf("first connect: %v", err)
	}
	if err := first.subscribe(t, "whalerein/v1/h/host1/d/dev1/reply"); err != nil {
		t.Fatalf("first subscribe: %v", err)
	}
	firstRef, firstConn := nextConnectionIdentity(t, control, "whalerein-device-dev1")

	// A replacement after a fresh grant carries a higher generation.
	policy.set("device:dev1", `whalerein/1
ok
kind device
device dev1
revision 7
ready 1
acl write 7 host1 whalerein/v1/h/host1/d/dev1/query
acl subscribe 7 host1 whalerein/v1/h/host1/d/dev1/reply
acl read 7 host1 whalerein/v1/h/host1/d/dev1/reply
acl write 7 host1 whalerein/v1/h/host1/d/dev1/ready
`)
	second, _, err := dial(t, b.address, "whalerein-device-dev1", "device:dev1", "device-token")
	if err != nil {
		t.Fatalf("replacement connect: %v", err)
	}
	defer second.close()
	if err := second.subscribe(t, "whalerein/v1/h/host1/d/dev1/reply"); err != nil {
		t.Fatalf("replacement subscribe: %v", err)
	}
	secondRef, _ := nextConnectionIdentity(t, control, "whalerein-device-dev1")
	if firstRef == secondRef {
		t.Fatalf("replacement reused connection reference %q", firstRef)
	}

	t.Run("stale_target_cannot_deliver_to_the_replacement", func(t *testing.T) {
		properties := expiry(30)
		properties.User.Add("whalerein-target", firstRef)
		if err := control.publishWith(t, "whalerein/v1/h/host1/d/dev1/reply", 1, properties, []byte("stale")); err != nil {
			t.Fatalf("publish stale target: %v", err)
		}
		second.expectNoMessage(t, "whalerein/v1/h/host1/d/dev1/reply", 300*time.Millisecond)

		properties = expiry(30)
		properties.User.Add("whalerein-target", secondRef)
		if err := control.publishWith(t, "whalerein/v1/h/host1/d/dev1/reply", 1, properties, []byte("current")); err != nil {
			t.Fatalf("publish current target: %v", err)
		}
		if got := string(second.next(t, "whalerein/v1/h/host1/d/dev1/reply")); got != "current" {
			t.Fatalf("delivered %q", got)
		}
	})

	t.Run("stale_kick_cannot_close_the_replacement", func(t *testing.T) {
		fields := sendControl(t, control, "kick-stale", "kick",
			"client whalerein-device-dev1\nconn "+firstConn+"\n")
		if fields["absent"] != "1" {
			t.Fatalf("stale kick was not reported absent: %v", fields)
		}
		if err := second.publish(t, "whalerein/v1/h/host1/d/dev1/ready", []byte("alive")); err != nil {
			t.Fatalf("replacement was closed by a stale kick: %v", err)
		}
	})

	t.Run("stale_deny_scope_cannot_revoke_the_replacement", func(t *testing.T) {
		fields := sendControl(t, control, "deny-stale", "deny",
			"device dev1\ngeneration 6\nhost host1\nconn "+firstConn+"\nkick 1\n")
		if fields["matched"] != "0" {
			t.Fatalf("stale connection-scoped deny matched %q", fields["matched"])
		}
		if err := second.publish(t, "whalerein/v1/h/host1/d/dev1/ready", []byte("still-authorized")); err != nil {
			t.Fatalf("replacement lost authority from a stale deny: %v", err)
		}
	})

	t.Run("revocation_fences_final_delivery_without_kicking", func(t *testing.T) {
		fields := sendControl(t, control, "deny-current", "deny",
			"device dev1\ngeneration 7\nhost host1\nkick 0\n")
		if fields["matched"] != "1" {
			t.Fatalf("current deny matched %q", fields["matched"])
		}
		properties := expiry(30)
		properties.User.Add("whalerein-target", secondRef)
		if err := control.publishWith(t, "whalerein/v1/h/host1/d/dev1/reply", 1, properties, []byte("after-revoke")); err != nil {
			t.Fatalf("publish after revoke: %v", err)
		}
		second.expectNoMessage(t, "whalerein/v1/h/host1/d/dev1/reply", 300*time.Millisecond)
		if err := second.publish(t, "whalerein/v1/h/host1/d/dev1/ready", []byte("after-revoke")); err == nil {
			t.Fatal("revoked connection still publishes")
		}
	})
}
