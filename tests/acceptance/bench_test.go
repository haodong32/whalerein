//go:build acceptance

package acceptance

import (
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type benchConfig struct {
	MQTTAddr    string
	AccessURL   string
	DBURL       string
	HostToken   string
	Devices     int
	AdminToken  string
	Output      string
	TLSInsecure bool
}

func benchConfigFromEnv() benchConfig {
	get := func(key, fallback string) string {
		if value := os.Getenv(key); value != "" {
			return value
		}
		return fallback
	}
	devices := 2000
	fmt.Sscanf(get("WHALEREIN_BENCH_DEVICES", "2000"), "%d", &devices)
	return benchConfig{
		MQTTAddr:    get("WHALEREIN_BENCH_MQTT", "127.0.0.1:8883"),
		AccessURL:   get("WHALEREIN_BENCH_ACCESS", "http://127.0.0.1:8080"),
		DBURL:       get("WHALEREIN_BENCH_DATABASE_URL", "postgres://postgres@127.0.0.1:55432/whalerein_dev?sslmode=disable"),
		HostToken:   get("WHALEREIN_HOST_PROVISIONING_TOKEN", "dev-host-token"),
		AdminToken:  get("WHALEREIN_ADMIN_TOKEN", "dev-admin-token"),
		Devices:     devices,
		Output:      os.Getenv("WHALEREIN_BENCH_OUT"),
		TLSInsecure: true,
	}
}

type benchClient struct {
	index  int
	client *paho.Client
	conn   net.Conn
	msgs   chan paho.PublishReceived
}

func benchDial(cfg benchConfig, index int, id, username, password string) (*benchClient, error) {
	dialer := &tls.Dialer{NetDialer: &net.Dialer{Timeout: 10 * time.Second}, Config: &tls.Config{
		InsecureSkipVerify: cfg.TLSInsecure, // #nosec G402 -- pinned self-signed acceptance certificate.
		MinVersion:         tls.VersionTLS12,
	}}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	conn, err := dialer.DialContext(ctx, "tcp", cfg.MQTTAddr)
	if err != nil {
		return nil, err
	}
	msgs := make(chan paho.PublishReceived, 1024)
	client := paho.NewClient(paho.ClientConfig{
		ClientID: id,
		Conn:     packets.NewThreadSafeConn(conn),
		OnPublishReceived: []func(paho.PublishReceived) (bool, error){
			func(received paho.PublishReceived) (bool, error) {
				select {
				case msgs <- received:
				default:
				}
				return true, nil
			},
		},
	})
	connack, err := client.Connect(ctx, &paho.Connect{
		ClientID:     id,
		CleanStart:   true,
		KeepAlive:    30,
		UsernameFlag: true,
		Username:     username,
		PasswordFlag: password != "",
		Password:     []byte(password),
	})
	if err != nil {
		_ = conn.Close()
		return nil, err
	}
	if connack.ReasonCode != 0 {
		_ = conn.Close()
		return nil, fmt.Errorf("connack %d", connack.ReasonCode)
	}
	return &benchClient{index: index, client: client, conn: conn, msgs: msgs}, nil
}

func (c *benchClient) close() {
	_ = c.client.Disconnect(&paho.Disconnect{ReasonCode: 0})
	_ = c.conn.Close()
}

func seedBenchDevices(t *testing.T, cfg benchConfig) (string, []string, []string) {
	t.Helper()
	ctx := context.Background()
	db, err := pgxpool.New(ctx, cfg.DBURL)
	if err != nil {
		t.Fatalf("database: %v", err)
	}
	defer db.Close()
	suffix := fmt.Sprintf("%d", time.Now().UnixNano())
	hostID := "bench-host-" + suffix
	if _, err := db.Exec(ctx, "INSERT INTO hosts (host_id) VALUES ($1)", hostID); err != nil {
		t.Fatalf("seed host: %v", err)
	}
	tokens := make([]string, cfg.Devices)
	ids := make([]string, cfg.Devices)
	deviceRows := make([][]any, 0, cfg.Devices)
	credentialRows := make([][]any, 0, cfg.Devices)
	grantRows := make([][]any, 0, cfg.Devices)
	for i := 0; i < cfg.Devices; i++ {
		deviceID := fmt.Sprintf("bench-device-%s-%05d", suffix, i)
		token := fmt.Sprintf("bench-token-%s-%05d", suffix, i)
		hash := sha256.Sum256([]byte(token))
		ids[i] = deviceID
		tokens[i] = token
		deviceRows = append(deviceRows, []any{deviceID, ""})
		credentialRows = append(credentialRows, []any{hash[:], deviceID})
		grantRows = append(grantRows, []any{hostID, deviceID})
	}
	if _, err := db.CopyFrom(ctx, pgx.Identifier{"devices"}, []string{"device_id", "public_key_fingerprint"}, pgx.CopyFromRows(deviceRows)); err != nil {
		t.Fatalf("copy devices: %v", err)
	}
	if _, err := db.CopyFrom(ctx, pgx.Identifier{"device_credentials"}, []string{"auth_hash", "device_id"}, pgx.CopyFromRows(credentialRows)); err != nil {
		t.Fatalf("copy credentials: %v", err)
	}
	if _, err := db.CopyFrom(ctx, pgx.Identifier{"grants"}, []string{"host_id", "device_id"}, pgx.CopyFromRows(grantRows)); err != nil {
		t.Fatalf("copy grants: %v", err)
	}
	t.Cleanup(func() {
		_, _ = db.Exec(context.Background(), "DELETE FROM grants WHERE host_id=$1", hostID)
		_, _ = db.Exec(context.Background(), "DELETE FROM device_credentials WHERE device_id LIKE $1", "bench-device-"+suffix+"-%")
		_, _ = db.Exec(context.Background(), "DELETE FROM devices WHERE device_id LIKE $1", "bench-device-"+suffix+"-%")
		_, _ = db.Exec(context.Background(), "DELETE FROM hosts WHERE host_id=$1", hostID)
	})
	return hostID, tokens, ids
}

func dockerStats(t *testing.T, names ...string) map[string]map[string]string {
	t.Helper()
	args := append([]string{"stats", "--no-stream", "--format", "{{json .}}"}, names...)
	output, err := exec.Command("docker", args...).Output()
	if err != nil {
		return nil
	}
	result := map[string]map[string]string{}
	for _, line := range strings.Split(strings.TrimSpace(string(output)), "\n") {
		var entry map[string]string
		if json.Unmarshal([]byte(line), &entry) == nil {
			result[entry["Name"]] = entry
		}
	}
	return result
}

func netBytes(t *testing.T, container string) (rx, tx uint64) {
	t.Helper()
	output, err := exec.Command("docker", "exec", container, "sh", "-c", "cat /proc/net/dev").Output()
	if err != nil {
		return 0, 0
	}
	for _, line := range strings.Split(string(output), "\n") {
		if !strings.Contains(line, "eth0:") {
			continue
		}
		fields := strings.Fields(strings.ReplaceAll(line, "eth0:", ""))
		if len(fields) >= 9 {
			fmt.Sscanf(fields[0], "%d", &rx)
			fmt.Sscanf(fields[8], "%d", &tx)
		}
	}
	return rx, tx
}

func percentile(sorted []time.Duration, p float64) time.Duration {
	if len(sorted) == 0 {
		return 0
	}
	index := int(float64(len(sorted)-1) * p)
	return sorted[index]
}

func TestMosquittoLineBenchmark(t *testing.T) {
	if os.Getenv("WHALEREIN_BENCH") != "1" {
		t.Skip("set WHALEREIN_BENCH=1 to run the target-line benchmark")
	}
	cfg := benchConfigFromEnv()
	hostID, tokens, ids := seedBenchDevices(t, cfg)

	start := time.Now()
	clients := make([]*benchClient, cfg.Devices)
	var failures atomic.Int64
	var errorSamples sync.Map
	var mu sync.Mutex
	semaphore := make(chan struct{}, 50)
	var wg sync.WaitGroup
	for i := 0; i < cfg.Devices; i++ {
		wg.Add(1)
		go func(index int) {
			defer wg.Done()
			semaphore <- struct{}{}
			defer func() { <-semaphore }()
			client, err := benchDial(cfg, index, "whalerein-device-"+ids[index], "device:"+ids[index], tokens[index])
			if err != nil {
				failures.Add(1)
				if _, loaded := errorSamples.LoadOrStore(index, err.Error()); !loaded {
					if len(fmt.Sprint(err.Error())) > 0 {
						// bounded sample; the first few errors name the cause
					}
				}
				return
			}
			mu.Lock()
			clients[index] = client
			mu.Unlock()
		}(i)
	}
	wg.Wait()
	connectElapsed := time.Since(start)
	connected := 0
	for _, client := range clients {
		if client != nil {
			connected++
		}
	}
	if connected < cfg.Devices/2 {
		samples := []string{}
		errorSamples.Range(func(key, value any) bool {
			if len(samples) < 5 {
				samples = append(samples, fmt.Sprint(value))
			}
			return true
		})
		t.Fatalf("only %d/%d connections established; sample errors: %v", connected, cfg.Devices, samples)
	}
	t.Cleanup(func() {
		for _, client := range clients {
			if client != nil {
				client.close()
			}
		}
	})

	// Sample idle resource use.
	time.Sleep(10 * time.Second)
	statsIdle := dockerStats(t, "whalerein-access", "whalerein-mosquitto-bench")
	rxBefore, txBefore := netBytes(t, "whalerein-mosquitto-bench")

	// Active streaming through Access: the host publishes events, one warmed
	// device receives them, latency measured from publish start to delivery.
	streaming := clients[0]
	if streaming != nil {
		subscribed := false
		for attempt := 0; attempt < 5 && !subscribed; attempt++ {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			_, err := streaming.client.Subscribe(ctx, &paho.Subscribe{Subscriptions: []paho.SubscribeOptions{{Topic: "whalerein/v1/h/" + hostID + "/d/" + ids[0] + "/event", QoS: 1}}})
			cancel()
			if err == nil {
				subscribed = true
				break
			}
			time.Sleep(500 * time.Millisecond)
		}
		if !subscribed {
			t.Fatal("streaming subscribe denied after retries")
		}
	}
	var messages <-chan paho.PublishReceived
	if streaming != nil {
		messages = streaming.msgs
	}
	host, err := benchDial(cfg, -1, "whalerein-host-"+hostID, "host:"+hostID, cfg.HostToken)
	if err != nil {
		t.Fatalf("host connect: %v", err)
	}
	defer host.close()

	// Warm-up: a live event for a grant not yet in the admission cache is
	// dropped while the bounded background load runs. Repeat until the grant
	// is warm so the measured phase starts from steady state.
	warm := false
	for attempt := 0; attempt < 40 && !warm; attempt++ {
		envelope := &whalereinv1.EventEnvelope{
			HostId: hostID, DeviceId: ids[0], ChannelId: "bench-warm",
			Nonce: []byte("nonce"), Ciphertext: []byte("ciphertext"), CreatedAtUnixMs: time.Now().UnixMilli(),
		}
		payload, _ := proto.Marshal(envelope)
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		_, err := host.client.Publish(ctx, &paho.Publish{Topic: "whalerein/v1/h/" + hostID + "/event", QoS: 1, Payload: payload})
		cancel()
		if err != nil {
			t.Fatalf("warm-up publish: %v", err)
		}
		select {
		case <-messages:
			warm = true
		case <-time.After(500 * time.Millisecond):
		}
	}
	if !warm {
		t.Fatal("streaming grant never warmed")
	}

	const eventCount = 500
	latencies := make([]time.Duration, 0, eventCount)
	streamStart := time.Now()
	for i := 0; i < eventCount; i++ {
		envelope := &whalereinv1.EventEnvelope{
			HostId: hostID, DeviceId: ids[0], ChannelId: "bench",
			Nonce: []byte("nonce"), Ciphertext: []byte("ciphertext"), CreatedAtUnixMs: time.Now().UnixMilli(),
		}
		payload, _ := proto.Marshal(envelope)
		sent := time.Now()
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		response, err := host.client.Publish(ctx, &paho.Publish{Topic: "whalerein/v1/h/" + hostID + "/event", QoS: 1, Payload: payload})
		cancel()
		if err != nil || (response != nil && response.ReasonCode >= 0x80) {
			t.Fatalf("event %d publish: %v", i, err)
		}
		select {
		case <-messages:
			latencies = append(latencies, time.Since(sent))
		case <-time.After(10 * time.Second):
			t.Fatalf("event %d not delivered", i)
		}
	}
	streamElapsed := time.Since(streamStart)
	rxAfter, txAfter := netBytes(t, "whalerein-mosquitto-bench")
	statsActive := dockerStats(t, "whalerein-access", "whalerein-mosquitto-bench")

	// Concentrated reconnect and catch-up.
	const reconnectCount = 500
	reconnectStart := time.Now()
	var reconnectFailures atomic.Int64
	reconnected := make([]*benchClient, reconnectCount)
	var reconnectWG sync.WaitGroup
	for i := 0; i < reconnectCount; i++ {
		index := i * 4
		if index >= cfg.Devices || clients[index] == nil {
			continue
		}
		clients[index].close()
		clients[index] = nil
		reconnectWG.Add(1)
		go func(slot, deviceIndex int) {
			defer reconnectWG.Done()
			semaphore <- struct{}{}
			defer func() { <-semaphore }()
			client, err := benchDial(cfg, deviceIndex, "whalerein-device-"+ids[deviceIndex], "device:"+ids[deviceIndex], tokens[deviceIndex])
			if err != nil {
				reconnectFailures.Add(1)
				return
			}
			reconnected[slot] = client
		}(i, index)
	}
	reconnectWG.Wait()
	reconnectElapsed := time.Since(reconnectStart)
	reconnectedOK := 0
	for slot, client := range reconnected {
		if client != nil {
			reconnectedOK++
			clients[slot*4] = client
		}
	}

	sort.Slice(latencies, func(i, j int) bool { return latencies[i] < latencies[j] })
	metrics := map[string]float64{}
	if response, err := http.Get(cfg.AccessURL + "/metrics"); err == nil {
		fields := strings.Fields(string(mustRead(response)))
		for i := 0; i+1 < len(fields); i += 2 {
			if strings.HasPrefix(fields[i], "grant_admission") || strings.HasPrefix(fields[i], "http_request_") {
				var value float64
				if _, err := fmt.Sscanf(fields[i+1], "%g", &value); err == nil {
					metrics[fields[i]] = value
				}
			}
		}
	}
	samples := []string{}
	errorSamples.Range(func(key, value any) bool {
		if len(samples) < 5 {
			samples = append(samples, fmt.Sprint(value))
		}
		return true
	})
	summary := map[string]any{
		"error_samples":       samples,
		"mqtt":                cfg.MQTTAddr,
		"tls":                 true,
		"configured_devices":  cfg.Devices,
		"connected_devices":   connected,
		"connection_failures": failures.Load(),
		"connect_seconds":     connectElapsed.Seconds(),
		"idle_stats":          statsIdle,
		"active_stats":        statsActive,
		"events":              len(latencies),
		"stream_seconds":      streamElapsed.Seconds(),
		"events_per_second":   float64(len(latencies)) / streamElapsed.Seconds(),
		"latency_p50_us":      percentile(latencies, 0.50).Microseconds(),
		"latency_p95_us":      percentile(latencies, 0.95).Microseconds(),
		"latency_p99_us":      percentile(latencies, 0.99).Microseconds(),
		"reconnect_devices":   reconnectCount,
		"reconnected":         reconnectedOK,
		"reconnect_failures":  reconnectFailures.Load(),
		"reconnect_seconds":   reconnectElapsed.Seconds(),
		"broker_rx_bytes":     rxAfter - rxBefore,
		"broker_tx_bytes":     txAfter - txBefore,
		"access_metrics":      metrics,
		"sampled_at":          time.Now().UTC().Format(time.RFC3339),
	}
	encoded, _ := json.MarshalIndent(summary, "", "  ")
	fmt.Println(string(encoded))
	if cfg.Output != "" {
		if err := os.WriteFile(cfg.Output, encoded, 0o600); err != nil {
			t.Fatalf("write summary: %v", err)
		}
	}
}

func mustRead(response *http.Response) []byte {
	defer response.Body.Close()
	body, _ := io.ReadAll(response.Body)
	return body
}
