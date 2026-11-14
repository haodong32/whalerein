package broker

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/eclipse/paho.golang/packets"
	"github.com/eclipse/paho.golang/paho"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func TestTopicHelpersBuildAndParseValidTopics(t *testing.T) {
	cases := []struct {
		topic Topic
		want  string
	}{
		{Topic{HostID: "h1", Kind: KindRequest}, "whalerein/v1/h/h1/request"},
		{Topic{HostID: "h1", Kind: KindResult}, "whalerein/v1/h/h1/result"},
		{Topic{HostID: "h1", Kind: KindEvent}, "whalerein/v1/h/h1/event"},
		{Topic{HostID: "h1", DeviceID: "d1", Kind: KindDeviceEvent}, "whalerein/v1/h/h1/d/d1/event"},
	}
	for _, tc := range cases {
		var got string
		switch tc.topic.Kind {
		case KindRequest:
			got = RequestTopic(tc.topic.HostID)
		case KindResult:
			got = ResultTopic(tc.topic.HostID)
		case KindEvent:
			got = EventTopic(tc.topic.HostID)
		case KindDeviceEvent:
			got = DeviceEventTopic(tc.topic.HostID, tc.topic.DeviceID)
		}
		if got != tc.want {
			t.Fatalf("got %q want %q", got, tc.want)
		}
		parsed, err := ParseTopic(got)
		if err != nil {
			t.Fatalf("parse %q: %v", got, err)
		}
		if parsed != tc.topic {
			t.Fatalf("parsed = %+v want %+v", parsed, tc.topic)
		}
	}
	for _, bad := range []string{
		"", "whalerein/v1/h/", "whalerein/v1/h/h1", "whalerein/v1/h/h1/other",
		"whalerein/v1/h/h1/d/d1/other", "whalerein/v1/h/h1/d/d1", "whalerein/v1/h/+/request",
		"whalerein/v1/h/h1/d/+/event", "other/v1/h/h1/request", "whalerein/v1/h/h1/request/",
	} {
		if _, err := ParseTopic(bad); err == nil {
			t.Fatalf("expected parse error for %q", bad)
		}
	}
	if ClientIDForHost("h1") != "whalerein-host-h1" || ClientIDForDevice("d1") != "whalerein-device-d1" {
		t.Fatal("client id helpers")
	}
	if PresenceTopic("h1") != "whalerein/v1/h/h1/presence" {
		t.Fatalf("presence topic = %q", PresenceTopic("h1"))
	}
	if DeviceWillTopic("d1") != "whalerein/v1/presence/device/d1" || DeviceWillFilter != "whalerein/v1/presence/device/+" {
		t.Fatalf("device will topics = %q %q", DeviceWillTopic("d1"), DeviceWillFilter)
	}
	if deviceID, ok := ParseDeviceWillTopic(DeviceWillTopic("d1")); !ok || deviceID != "d1" {
		t.Fatalf("parse device will = %q %v", deviceID, ok)
	}
	for _, bad := range []string{"", DeviceWillFilter, "whalerein/v1/presence/device/", "whalerein/v1/presence/device/d1/x", "whalerein/v1/presence/device/#"} {
		if _, ok := ParseDeviceWillTopic(bad); ok {
			t.Fatalf("expected invalid device will topic %q", bad)
		}
	}
	parsed, err := ParseTopic(PresenceTopic("h1"))
	if err != nil || parsed.HostID != "h1" || parsed.Kind != KindPresence {
		t.Fatalf("parse presence = %+v %v", parsed, err)
	}
}

func TestParseSysClientTopicRecognizesConnectionEvents(t *testing.T) {
	clientID, online, ok := ParseSysClientTopic("$SYS/brokers/emqx@127.0.0.1/clients/whalerein-host-h1/connected")
	if !ok || !online || clientID != "whalerein-host-h1" {
		t.Fatalf("connected = %q %v %v", clientID, online, ok)
	}
	clientID, online, ok = ParseSysClientTopic("$SYS/brokers/n/clients/whalerein-device-d1/disconnected")
	if !ok || online || clientID != "whalerein-device-d1" {
		t.Fatalf("disconnected = %q %v %v", clientID, online, ok)
	}
	for _, bad := range []string{"", "$SYS/brokers/n/clients/x", "$SYS/brokers/n/clients/x/other", "whalerein/v1/h/h1/event"} {
		if _, _, ok := ParseSysClientTopic(bad); ok {
			t.Fatalf("expected invalid topic %q", bad)
		}
	}
}

func identityFor(t *testing.T, clientID, username, kind, id string) {
	t.Helper()
	identity := identityForClient(clientID, username)
	if identity.kind != kind {
		t.Fatalf("identity = %+v", identity)
	}
	if kind == "host" && identity.hostID != id {
		t.Fatalf("host = %q", identity.hostID)
	}
	if kind == "device" && identity.deviceID != id {
		t.Fatalf("device = %q", identity.deviceID)
	}
}

func TestIdentityForClientParsesKnownClientCredentials(t *testing.T) {
	identityFor(t, "", "host:h1", "host", "h1")
	identityFor(t, "", "device:d1", "device", "d1")
	identityFor(t, "whalerein-host-h1", "", "host", "h1")
	identityFor(t, "whalerein-device-d1", "", "device", "d1")
	if identity := identityForClient("other", ""); identity.kind != "" {
		t.Fatalf("identity = %+v", identity)
	}
}

func TestProbeLifecycleDoesNotChangePresence(t *testing.T) {
	clientID := "whalerein-probe-0123456789abcdef0123456789abcdef"
	for _, pair := range [][2]string{
		{clientID, "probe:h1"}, {clientID, ""}, {"", "probe:h1"},
		{clientID, "host:h1"}, {"whalerein-host-h1", "probe:h1"},
	} {
		if identity := identityForClient(pair[0], pair[1]); identity.kind != "" {
			t.Fatalf("probe became presence: %+v", identity)
		}
	}
	presence := newPresence()
	presence.SetHostOnline("h1", true, time.Now())
	presence.HandleSysMessage("$SYS/brokers/n/clients/"+clientID+"/disconnected", []byte(`{"username":"probe:h1"}`))
	if !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("probe disconnected the host presence")
	}
	presence.SetHostOnline("h1", false, time.Now())
	if err := presence.HandlePresenceWebhook([]byte(`{"event":"client.connected","clientid":"` + clientID + `","username":"probe:h1"}`)); err != nil {
		t.Fatal(err)
	}
	if hosts, devices := presence.ConnectedCounts(); hosts != 0 || devices != 0 {
		t.Fatal("probe added an online principal")
	}
}

func newPresence() *Presence {
	return NewPresence(50*time.Millisecond, nil, diagnostics.NewMetrics(), slog.New(slog.NewTextHandler(io.Discard, nil)))
}

func TestPresenceCacheTracksConnectionCountsAndExpiry(t *testing.T) {
	presence := newPresence()
	now := time.Now()
	if presence.IsHostOnline(context.Background(), "") {
		t.Fatal("empty host")
	}
	if presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("unknown host")
	}
	presence.SetHostOnline("h1", true, now)
	presence.SetDeviceOnline("d1", true, now)
	if !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("host should be online")
	}
	hosts, devices := presence.ConnectedCounts()
	if hosts != 1 || devices != 1 {
		t.Fatalf("counts = %d/%d", hosts, devices)
	}
	presence.SetHostOnline("", true, now)
	presence.SetDeviceOnline("", true, now)
	presence.MarkHostReady("h2", now)
	if !presence.IsHostOnline(context.Background(), "h2") {
		t.Fatal("ready host")
	}
	presence.SetHostOnline("h1", false, now)
	if presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("host should be offline")
	}
}

func TestPresenceUsesStaleCacheFallbackAndRESTLookup(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.Contains(r.URL.Path, "whalerein-host-h1") {
			_, _ = io.WriteString(w, `{"connected":false}`)
			return
		}
		_, _ = io.WriteString(w, `{"connected":true}`)
	}))
	defer server.Close()
	rest := &RESTClient{base: mustURL(t, server.URL), key: "k", secret: "s", client: server.Client()}
	presence := NewPresence(time.Hour, rest, diagnostics.NewMetrics(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if !presence.IsHostOnline(context.Background(), "h2") {
		t.Fatal("REST host should be online")
	}
	// A fresh cache entry wins over REST.
	presence.SetHostOnline("h1", true, time.Now())
	if !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("cache host should be online")
	}
	// Make the cache stale and let REST report offline.
	presence.cacheTTL = time.Nanosecond
	time.Sleep(time.Millisecond)
	if presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("stale REST host should be offline")
	}
	// The stale value is used when the REST query fails.
	presence.SetHostOnline("h3", true, time.Now().Add(-time.Hour))
	presence.rest = &RESTClient{base: mustURL(t, "http://127.0.0.1:1"), key: "k", secret: "s", client: &http.Client{Timeout: time.Millisecond}}
	if !presence.IsHostOnline(context.Background(), "h3") {
		t.Fatal("stale fallback should be used")
	}
}

func mustURL(t *testing.T, raw string) *url.URL {
	t.Helper()
	parsed, err := url.Parse(raw)
	if err != nil {
		t.Fatal(err)
	}
	return parsed
}

func TestPresenceUpdatesFromSysEventsAndWebhook(t *testing.T) {
	presence := newPresence()
	connected := []byte(`{"clientid":"whalerein-host-h1","username":"host:h1","timestamp":1000}`)
	presence.HandleSysMessage("$SYS/brokers/n/clients/whalerein-host-h1/connected", connected)
	if !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("host not online")
	}
	presence.HandleSysMessage("$SYS/brokers/n/clients/whalerein-device-d1/connected", []byte(`{}`))
	if hosts, devices := presence.ConnectedCounts(); hosts != 1 || devices != 1 {
		t.Fatalf("counts = %d/%d", hosts, devices)
	}
	presence.HandleSysMessage("$SYS/brokers/n/clients/whalerein-host-h1/connected", []byte(`{"username":"host:h2"}`))
	if !presence.IsHostOnline(context.Background(), "h2") {
		t.Fatal("payload username not used")
	}
	presence.HandleSysMessage("$SYS/brokers/n/clients/whalerein-host-h1/disconnected", nil)
	if !presence.IsHostOnline(context.Background(), "h2") {
		t.Fatal("h2 should be unaffected")
	}
	presence.HandleSysMessage("not-a-sys-topic", nil)
	presence.HandleSysMessage("$SYS/brokers/n/clients/whalerein-host-h1/other", nil)

	webhook := []byte(`{"event":"client.connected","clientid":"whalerein-host-h3","timestamp":1700000000000}`)
	if err := presence.HandlePresenceWebhook(webhook); err != nil {
		t.Fatal(err)
	}
	if !presence.IsHostOnline(context.Background(), "h3") {
		t.Fatal("webhook host not online")
	}
	if err := presence.HandlePresenceWebhook([]byte(`[{"event":"client.disconnected","clientid":"whalerein-host-h3"}]`)); err != nil {
		t.Fatal(err)
	}
	if presence.IsHostOnline(context.Background(), "h3") {
		t.Fatal("webhook disconnect not applied")
	}
	if err := presence.HandlePresenceWebhook([]byte(`{"event":"session.subscribed","clientid":"whalerein-host-h4"}`)); err != nil {
		t.Fatal(err)
	}
	if err := presence.HandlePresenceWebhook([]byte(`[]`)); err != nil {
		t.Fatal(err)
	}
	if err := presence.HandlePresenceWebhook(nil); err == nil {
		t.Fatal("empty webhook")
	}
	if err := presence.HandlePresenceWebhook([]byte(`{`)); err == nil {
		t.Fatal("malformed webhook")
	}
	if online, ok := lifecycleState("client.disconnected"); ok && online {
		t.Fatal("disconnected state")
	}
	if online, ok := lifecycleState("client.connected"); !ok || !online {
		t.Fatal("connected state")
	}
	if _, ok := lifecycleState("other"); ok {
		t.Fatal("unknown state")
	}
}

func TestRESTClientListsClientsAndDisconnectsDevices(t *testing.T) {
	if client, err := NewRESTClient(config.EMQXConfig{}); err != nil || client != nil {
		t.Fatalf("empty config = %v %v", client, err)
	}
	if _, err := NewRESTClient(config.EMQXConfig{RESTURL: ":::"}); err == nil {
		t.Fatal("bad URL")
	}
	if _, err := NewRESTClient(config.EMQXConfig{RESTURL: "https://example.com", CAFile: filepath.Join(t.TempDir(), "missing")}); err == nil {
		t.Fatal("missing CA")
	}
	emptyCA := filepath.Join(t.TempDir(), "empty.pem")
	if err := os.WriteFile(emptyCA, []byte("not a cert"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := NewRESTClient(config.EMQXConfig{RESTURL: "https://example.com", CAFile: emptyCA}); err == nil {
		t.Fatal("invalid CA")
	}
	if _, err := tlsConfigFor("", true, "x"); err != nil {
		t.Fatal(err)
	}

	var gotAuth string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotAuth = r.Header.Get("Authorization")
		switch {
		case r.Method == http.MethodDelete && strings.Contains(r.URL.Path, "gone"):
			w.WriteHeader(http.StatusNotFound)
		case r.Method == http.MethodDelete && strings.Contains(r.URL.Path, "fail"):
			w.WriteHeader(http.StatusInternalServerError)
		case r.Method == http.MethodDelete:
			w.WriteHeader(http.StatusNoContent)
		case strings.Contains(r.URL.Path, "missing"):
			w.WriteHeader(http.StatusNotFound)
		case strings.Contains(r.URL.Path, "fail"):
			w.WriteHeader(http.StatusInternalServerError)
		case strings.Contains(r.URL.Path, "malformed"):
			_, _ = io.WriteString(w, `{`)
		case strings.Contains(r.URL.Path, "data"):
			_, _ = io.WriteString(w, `{"data":{"connected":true}}`)
		default:
			_, _ = io.WriteString(w, `{"connected":true}`)
		}
	}))
	defer server.Close()
	client := &RESTClient{base: mustURL(t, server.URL), key: "key", secret: "secret", client: server.Client()}
	if !client.Enabled() {
		t.Fatal("client should be enabled")
	}
	connected, err := client.ClientConnected(context.Background(), "client")
	if err != nil || !connected || !strings.HasPrefix(gotAuth, "Basic ") {
		t.Fatalf("connected = %v err=%v auth=%q", connected, err, gotAuth)
	}
	if connected, err := client.ClientConnected(context.Background(), "data"); err != nil || !connected {
		t.Fatalf("data = %v err=%v", connected, err)
	}
	if connected, err := client.ClientConnected(context.Background(), "missing"); err != nil || connected {
		t.Fatalf("missing = %v err=%v", connected, err)
	}
	if _, err := client.ClientConnected(context.Background(), "fail"); err == nil {
		t.Fatal("expected HTTP error")
	}
	if _, err := client.ClientConnected(context.Background(), "malformed"); err == nil {
		t.Fatal("expected decode error")
	}
	if err := client.DisconnectClient(context.Background(), "client"); err != nil {
		t.Fatal(err)
	}
	if err := client.Disconnect(context.Background(), "gone"); err != nil {
		t.Fatal(err)
	}
	if err := client.DisconnectClient(context.Background(), "fail"); err == nil {
		t.Fatal("expected disconnect error")
	}
	disabled := &RESTClient{}
	if disabled.Enabled() {
		t.Fatal("disabled")
	}
	if _, err := disabled.ClientConnected(context.Background(), "x"); err == nil {
		t.Fatal("disabled query")
	}
	if err := disabled.DisconnectClient(context.Background(), "x"); err == nil {
		t.Fatal("disabled disconnect")
	}
	tlsServer := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, `{"connected":true}`)
	}))
	defer tlsServer.Close()
	caFile := filepath.Join(t.TempDir(), "ca.pem")
	cert := tlsServer.Certificate()
	pem := pemEncode(cert.Raw)
	if err := os.WriteFile(caFile, pem, 0o600); err != nil {
		t.Fatal(err)
	}
	tlsClient, err := NewRESTClient(config.EMQXConfig{RESTURL: tlsServer.URL, CAFile: caFile, APIKey: "k", APISecret: "s"})
	if err != nil {
		t.Fatal(err)
	}
	tlsClient.client = tlsServer.Client()
	if connected, err := tlsClient.ClientConnected(context.Background(), "x"); err != nil || !connected {
		t.Fatalf("TLS connected = %v err=%v", connected, err)
	}
}

func pemEncode(der []byte) []byte {
	return []byte("-----BEGIN CERTIFICATE-----\n" + base64Wrap(der) + "\n-----END CERTIFICATE-----\n")
}

func base64Wrap(data []byte) string {
	const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
	var out strings.Builder
	for i := 0; i < len(data); i += 3 {
		var chunk [3]byte
		n := copy(chunk[:], data[i:])
		value := int(chunk[0])<<16 | int(chunk[1])<<8 | int(chunk[2])
		for j := 0; j < 4; j++ {
			if j > n {
				out.WriteByte('=')
				continue
			}
			out.WriteByte(alphabet[(value>>(18-6*j))&0x3f])
		}
	}
	return out.String()
}

func TestDisconnectersUseConfiguredMechanisms(t *testing.T) {
	if _, err := NewDisconnecter(config.EMQXConfig{DisconnectMode: "ctl"}, nil, nil); err == nil {
		t.Fatal("empty ctl path")
	}
	var got []string
	runner := func(_ context.Context, name string, args ...string) ([]byte, error) {
		got = append([]string{name}, args...)
		return []byte("ok"), nil
	}
	disconnecter, err := NewDisconnecter(config.EMQXConfig{DisconnectMode: "ctl", CTLPath: "emqx"}, nil, runner)
	if err != nil {
		t.Fatal(err)
	}
	if err := disconnecter.Disconnect(context.Background(), "whalerein-device-d1"); err != nil {
		t.Fatal(err)
	}
	if len(got) != 5 || got[0] != "emqx" || got[3] != "kick" || got[4] != "whalerein-device-d1" {
		t.Fatalf("command = %v", got)
	}
	runner = func(context.Context, string, ...string) ([]byte, error) {
		return []byte("client not found"), errors.New("exit 1")
	}
	disconnecter, _ = NewDisconnecter(config.EMQXConfig{DisconnectMode: "ctl", CTLPath: "emqx"}, nil, runner)
	if err := disconnecter.Disconnect(context.Background(), "x"); err != nil {
		t.Fatal("not found should be idempotent")
	}
	runner = func(context.Context, string, ...string) ([]byte, error) { return []byte("boom"), errors.New("exit 1") }
	disconnecter, _ = NewDisconnecter(config.EMQXConfig{DisconnectMode: "ctl", CTLPath: "emqx"}, nil, runner)
	if err := disconnecter.Disconnect(context.Background(), "x"); err == nil {
		t.Fatal("expected error")
	}
	if err := disconnecter.Disconnect(context.Background(), ""); err == nil {
		t.Fatal("empty client id")
	}
	if _, err := NewDisconnecter(config.EMQXConfig{DisconnectMode: "rest"}, nil, nil); err == nil {
		t.Fatal("rest without client")
	}
	if _, err := NewDisconnecter(config.EMQXConfig{DisconnectMode: "bad"}, nil, nil); err == nil {
		t.Fatal("bad mode")
	}
	disabledREST := &RESTClient{}
	if _, err := NewDisconnecter(config.EMQXConfig{DisconnectMode: "rest"}, disabledREST, nil); err == nil {
		t.Fatal("disabled rest client")
	}
	enabledREST := &RESTClient{key: "k", secret: "s"}
	if _, err := NewDisconnecter(config.EMQXConfig{DisconnectMode: "rest"}, enabledREST, nil); err != nil {
		t.Fatal(err)
	}
	service := &Service{}
	if err := service.DisconnectDevice(context.Background(), "d1"); err == nil {
		t.Fatal("no disconnecter")
	}
	service.disconnecter = &fakeDisconnecter{}
	if err := service.DisconnectDevice(context.Background(), "d1"); err != nil {
		t.Fatal(err)
	}
}

type fakeDisconnecter struct{ ids []string }

func (f *fakeDisconnecter) Disconnect(_ context.Context, clientID string) error {
	f.ids = append(f.ids, clientID)
	return nil
}

type fakeSession struct {
	mu           sync.Mutex
	published    []string
	payloads     [][]byte
	subscribed   []string
	publishErr   error
	subscribeErr error
	done         chan struct{}
	closed       bool
	onPublish    func(string, []byte)
	qos          []byte
}

func newFakeSession() *fakeSession {
	return &fakeSession{done: make(chan struct{})}
}

func (f *fakeSession) Publish(_ context.Context, topic string, payload []byte, qos byte) error {
	f.mu.Lock()
	f.published = append(f.published, topic)
	f.payloads = append(f.payloads, append([]byte(nil), payload...))
	f.qos = append(f.qos, qos)
	f.mu.Unlock()
	if f.onPublish != nil {
		f.onPublish(topic, payload)
	}
	return f.publishErr
}

func (f *fakeSession) Subscribe(_ context.Context, topics []string, _ byte) error {
	f.mu.Lock()
	f.subscribed = append(f.subscribed, topics...)
	f.mu.Unlock()
	return f.subscribeErr
}

func (f *fakeSession) Done() <-chan struct{} { return f.done }

func (f *fakeSession) Close() error {
	f.mu.Lock()
	f.closed = true
	f.mu.Unlock()
	return nil
}

type fakeConnector struct {
	mu       sync.Mutex
	sessions []*fakeSession
	errs     []error
	connects int
}

func (f *fakeConnector) Connect(context.Context) (Session, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	index := f.connects
	f.connects++
	if index < len(f.errs) && f.errs[index] != nil {
		return nil, f.errs[index]
	}
	if len(f.sessions) == 0 {
		return nil, errors.New("no session")
	}
	if index >= len(f.sessions) {
		return f.sessions[len(f.sessions)-1], nil
	}
	return f.sessions[index], nil
}

func testService(t *testing.T, connector Connector) *Service {
	t.Helper()
	metrics := diagnostics.NewMetrics()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	presence := NewPresence(50*time.Millisecond, nil, metrics, logger)
	service, err := New(Dependencies{
		Config:   config.BrokerConfig{PublishTimeout: config.Duration(time.Second), ReconnectMin: config.Duration(time.Millisecond), ReconnectMax: config.Duration(5 * time.Millisecond)},
		Presence: presence, Connector: connector,
		Metrics: metrics, Masker: diagnostics.NewMaskerWithKey([]byte("broker")),
		Logger: logger,
	})
	if err != nil {
		t.Fatal(err)
	}
	return service
}

func TestNewRejectsMissingRequiredDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected error")
	}
	if _, err := New(Dependencies{Connector: &fakeConnector{}}); err == nil {
		t.Fatal("expected presence error")
	}
}

func TestBrokerReconnectsAfterFailureAndStopsOnCancellation(t *testing.T) {
	session := newFakeSession()
	connector := &fakeConnector{sessions: []*fakeSession{session}}
	service := testService(t, connector)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- service.Run(ctx) }()
	deadline := time.Now().Add(time.Second)
	for !service.Ready() && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	if !service.Ready() {
		t.Fatal("service not ready")
	}
	session.mu.Lock()
	subscribed := append([]string(nil), session.subscribed...)
	session.mu.Unlock()
	if len(subscribed) != 6 {
		t.Fatalf("subscribed = %v", subscribed)
	}
	if subscribed[3] != DeviceWillFilter {
		t.Fatalf("device will filter not subscribed: %v", subscribed)
	}
	close(session.done)
	time.Sleep(5 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("run did not stop")
	}
	if service.Ready() {
		t.Fatal("service still ready")
	}
}

func TestBrokerRetriesConnectionAndSubscriptionErrors(t *testing.T) {
	connector := &fakeConnector{errs: []error{errors.New("dial")}, sessions: []*fakeSession{newFakeSession()}}
	service := testService(t, connector)
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if err := service.Run(ctx); err != nil {
		t.Fatal(err)
	}

	badSubscription := newFakeSession()
	badSubscription.subscribeErr = errors.New("denied")
	connector = &fakeConnector{sessions: []*fakeSession{badSubscription}}
	service = testService(t, connector)
	ctx, cancel = context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if err := service.Run(ctx); err != nil {
		t.Fatal(err)
	}
	if !badSubscription.closed {
		t.Fatal("session not closed")
	}

	cancelled, cancelNow := context.WithCancel(context.Background())
	cancelNow()
	if err := service.Run(cancelled); err != nil {
		t.Fatal(err)
	}
}

func TestPublishRoutesRequestsAndDeviceEventsAndRejectsInvalidInputs(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	ctx := context.Background()
	request := &whalereinv1.HostRequest{RequestId: "r1", HostId: "h1", Kind: whalereinv1.RequestKind_REQUEST_KIND_HANDSHAKE}
	if err := service.PublishRequest(ctx, "h1", request); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("no session = %v", err)
	}
	session := newFakeSession()
	service.setSession(session, true)
	if err := service.PublishRequest(ctx, "h1", nil); err == nil {
		t.Fatal("nil request")
	}
	if err := service.PublishRequest(ctx, "bad/host", request); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("bad host = %v", err)
	}
	if err := service.PublishRequest(ctx, "h1", &whalereinv1.HostRequest{RequestId: "r1", HostId: "h2"}); err == nil {
		t.Fatal("host mismatch")
	}
	if err := service.PublishRequest(ctx, "h1", request); err != nil {
		t.Fatal(err)
	}
	if len(session.published) != 1 || session.published[0] != "whalerein/v1/h/h1/request" || session.qos[0] != 1 {
		t.Fatalf("published = %v qos=%v", session.published, session.qos)
	}
	session.publishErr = errors.New("mqtt down")
	if err := service.PublishRequest(ctx, "h1", request); err == nil {
		t.Fatal("publish error")
	}
	session.publishErr = nil

	if err := service.PublishDeviceEvent(ctx, "h1", "d1", []byte("event")); err != nil {
		t.Fatal(err)
	}
	if session.published[len(session.published)-1] != "whalerein/v1/h/h1/d/d1/event" {
		t.Fatalf("device event topic = %v", session.published)
	}
	if err := service.PublishDeviceEvent(ctx, "h1", "bad/d1", []byte("event")); !errors.Is(err, ErrInvalidTopic) {
		t.Fatalf("bad device = %v", err)
	}
	service.setSession(nil, false)
	if err := service.PublishDeviceEvent(ctx, "h1", "d1", nil); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("no session device = %v", err)
	}
}

func TestPublishAndAwaitReturnsMatchingHostResult(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	ctx := context.Background()
	request := &whalereinv1.HostRequest{RequestId: "r1", HostId: "h1"}
	session.onPublish = func(topic string, payload []byte) {
		var sent whalereinv1.HostRequest
		if err := proto.Unmarshal(payload, &sent); err != nil {
			t.Errorf("decode: %v", err)
			return
		}
		result, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: sent.GetRequestId(), HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded})
		service.HandleMessage(ResultTopic("h1"), result)
	}
	service.SetResultHandler(func(context.Context, string, *whalereinv1.HostResult) error { return nil })
	result, err := service.PublishAndAwait(ctx, "h1", request, time.Second)
	if err != nil || result.GetStatus() != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("result = %+v err=%v", result, err)
	}
	if _, err := service.PublishAndAwait(ctx, "h1", nil, time.Second); err == nil {
		t.Fatal("nil request")
	}
	if _, err := service.PublishAndAwait(ctx, "h1", &whalereinv1.HostRequest{}, time.Second); err == nil {
		t.Fatal("empty request id")
	}
	session.onPublish = nil
	if _, err := service.PublishAndAwait(ctx, "h1", &whalereinv1.HostRequest{RequestId: "timeout", HostId: "h1"}, 10*time.Millisecond); !errors.Is(err, ErrTimeout) {
		t.Fatalf("timeout = %v", err)
	}
	session.publishErr = errors.New("mqtt down")
	if _, err := service.PublishAndAwait(ctx, "h1", &whalereinv1.HostRequest{RequestId: "pubfail", HostId: "h1"}, time.Second); err == nil {
		t.Fatal("publish error")
	}
	session.publishErr = nil
	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	if _, err := service.PublishAndAwait(cancelled, "h1", &whalereinv1.HostRequest{RequestId: "cancel", HostId: "h1"}, time.Second); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancel = %v", err)
	}
}

func TestHandleMessageRoutesResultsAndEvents(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	var results []string
	service.SetResultHandler(func(_ context.Context, hostID string, result *whalereinv1.HostResult) error {
		results = append(results, hostID+":"+result.GetRequestId())
		return nil
	})
	var events []string
	service.SetEventHandler(func(_ context.Context, hostID string, payload []byte) error {
		events = append(events, hostID+":"+string(payload))
		return nil
	})
	service.HandleMessage("bogus/topic", nil)
	service.HandleMessage(RequestTopic("h1"), nil)
	result, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r1", HostId: "h1"})
	service.HandleMessage(ResultTopic("h1"), result)
	if len(results) != 1 || results[0] != "h1:r1" {
		t.Fatalf("results = %v", results)
	}
	service.HandleMessage(ResultTopic("h1"), []byte("bad"))
	mismatch, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r2", HostId: "h2"})
	service.HandleMessage(ResultTopic("h1"), mismatch)
	service.HandleMessage(EventTopic("h1"), []byte("event"))
	if len(events) != 1 || events[0] != "h1:event" {
		t.Fatalf("events = %v", events)
	}
	service.SetEventHandler(func(context.Context, string, []byte) error { return errors.New("route failed") })
	service.HandleMessage(EventTopic("h1"), []byte("event"))

	// Transient results take precedence over the permanent handler.
	channel := make(chan *whalereinv1.HostResult, 1)
	service.transient["transient"] = channel
	transient, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "transient", HostId: "h1"})
	service.HandleMessage(ResultTopic("h1"), transient)
	select {
	case result := <-channel:
		if result.GetRequestId() != "transient" {
			t.Fatal("wrong transient")
		}
	default:
		t.Fatal("transient not delivered")
	}

	// No permanent handler is a routing error, not a panic.
	service.SetResultHandler(nil)
	service.HandleMessage(ResultTopic("h1"), result)
	service.SetEventHandler(nil)
	service.HandleMessage(EventTopic("h1"), []byte("event"))
}

func TestHostReadyRegistersHostAndUpdatesSysPresence(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	ready := make(chan string, 1)
	service.SetReadyHandler(func(_ context.Context, hostID string) error {
		ready <- hostID
		return errors.New("replay failed")
	})
	var storedKey []byte
	service.SetHostKeyHandler(func(_ context.Context, hostID string, publicKey []byte) error {
		if hostID != "h1" {
			t.Fatalf("key host = %q", hostID)
		}
		storedKey = append([]byte(nil), publicKey...)
		return nil
	})
	key := bytes.Repeat([]byte{7}, 32)
	body, _ := proto.Marshal(&whalereinv1.HostReady{HostId: "h1", AuthorizationPublicKey: key})
	result, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: body})
	service.HandleMessage(ResultTopic("h1"), result)
	select {
	case host := <-ready:
		if host != "h1" {
			t.Fatalf("ready = %q", host)
		}
	case <-time.After(time.Second):
		t.Fatal("ready handler not called")
	}
	if !service.Presence().IsHostOnline(context.Background(), "h1") {
		t.Fatal("presence not marked")
	}
	if !bytes.Equal(storedKey, key) {
		t.Fatal("authorization key was not stored")
	}
	// Non-succeeded ready is rejected.
	failed, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "", HostId: "h1", Status: whalereinv1.OnlineStatus_failed, ResultBody: body})
	service.HandleMessage(ResultTopic("h1"), failed)
	// Malformed ready body is rejected.
	bad, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: []byte("bad")})
	service.HandleMessage(ResultTopic("h1"), bad)
	// Ready host id must match the topic.
	mismatch, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: mustMarshalHostReady(t, "h2")})
	service.HandleMessage(ResultTopic("h1"), mismatch)

	service.HandleMessage("$SYS/brokers/n/clients/whalerein-host-h9/connected", []byte(`{}`))
	if !service.Presence().IsHostOnline(context.Background(), "h9") {
		t.Fatal("sys presence not applied")
	}
	service.HandleMessage("$SYS/brokers/n/clients/whalerein-host-h9/disconnected", []byte(`{}`))
	if service.Presence().IsHostOnline(context.Background(), "h9") {
		t.Fatal("sys disconnect not applied")
	}
}

func mustMarshalHostReady(t *testing.T, hostID string) []byte {
	t.Helper()
	data, err := proto.Marshal(&whalereinv1.HostReady{HostId: hostID})
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func TestPahoConnectorRejectsInvalidConfiguration(t *testing.T) {
	dispatch := func(string, []byte) {}
	if _, err := NewPahoConnector(config.BrokerConfig{URL: "bad://host"}, dispatch, nil); err == nil {
		t.Fatal("bad scheme")
	}
	if _, err := NewPahoConnector(config.BrokerConfig{URL: "tcp://"}, dispatch, nil); err == nil {
		t.Fatal("empty host")
	}
	if _, err := NewPahoConnector(config.BrokerConfig{URL: "tls://host"}, nil, nil); err == nil {
		t.Fatal("nil dispatch")
	}
	if _, err := NewPahoConnector(config.BrokerConfig{URL: "tls://127.0.0.1:8883", CAFile: filepath.Join(t.TempDir(), "missing")}, dispatch, nil); err == nil {
		t.Fatal("missing CA")
	}
	if _, err := NewPahoConnector(config.BrokerConfig{URL: "tls://127.0.0.1"}, dispatch, nil); err != nil {
		t.Fatalf("default port: %v", err)
	}
	if _, err := NewPahoConnector(config.BrokerConfig{URL: "mqtts://[::1]"}, dispatch, nil); err != nil {
		t.Fatalf("ipv6: %v", err)
	}
}

func TestPahoConnectorDistinguishesDialAndHandshakeFailures(t *testing.T) {
	listener, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	connector, err := NewPahoConnector(config.BrokerConfig{
		URL: "tcp://" + listener.Addr().String(), ClientID: "test-client",
	}, func(string, []byte) {}, nil)
	if err != nil {
		t.Fatal(err)
	}

	cancelled, cancel := context.WithCancel(t.Context())
	cancel()
	if session, err := connector.Connect(cancelled); session != nil || !errors.Is(err, context.Canceled) ||
		!strings.HasPrefix(err.Error(), "broker dial:") {
		t.Fatalf("expected cancelled dial, got %v", err)
	}

	ctx, stop := context.WithTimeout(t.Context(), 5*time.Second)
	defer stop()
	deadline, _ := ctx.Deadline()
	if err := listener.SetDeadline(deadline); err != nil {
		t.Fatal(err)
	}
	server := make(chan error, 1)
	go func() {
		conn, err := listener.AcceptTCP()
		if err != nil {
			server <- err
			return
		}
		defer conn.Close()
		if err := conn.SetReadDeadline(deadline); err != nil {
			server <- err
			return
		}
		packet, err := packets.ReadPacket(conn)
		if err == nil && packet.Type != packets.CONNECT {
			err = errors.New("expected MQTT CONNECT")
		}
		server <- err
	}()
	if session, err := connector.Connect(ctx); session != nil || err == nil ||
		!strings.HasPrefix(err.Error(), "broker connect:") {
		t.Fatalf("expected failed handshake, got %v", err)
	}
	if err := <-server; err != nil {
		t.Fatal(err)
	}
}

func TestHandleReadyIgnoresMissingResultHandler(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	result, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: mustMarshalHostReady(t, "h1")})
	service.HandleMessage(ResultTopic("h1"), result)
}

func TestJSONPresencePayloadParsesConnectionState(t *testing.T) {
	var payload presencePayload
	if err := json.Unmarshal([]byte(`{"event":"client.connected","clientid":"x"}`), &payload); err != nil {
		t.Fatal(err)
	}
	if payload.ClientID != "x" {
		t.Fatalf("payload = %+v", payload)
	}
}

func TestBrokerDefaultsToFiveSecondPublishAndFifteenSecondPresenceTTL(t *testing.T) {
	presence := newPresence()
	service, err := New(Dependencies{Connector: &fakeConnector{}, Presence: presence})
	if err != nil {
		t.Fatal(err)
	}
	if service.Ready() {
		t.Fatal("new service should not be ready")
	}
	presence.SetHostOnline("h1", true, time.Now())
	if !service.IsHostOnline(context.Background(), "h1") {
		t.Fatal("IsHostOnline should delegate")
	}
	if service.Presence() != presence {
		t.Fatal("presence accessor")
	}
	if service.publishTimeout() != 5*time.Second {
		t.Fatalf("default publish timeout = %v", service.publishTimeout())
	}
	if service.baseContext() == nil {
		t.Fatal("base context")
	}
	empty := NewPresence(0, nil, nil, nil)
	if empty.cacheTTL != 15*time.Second {
		t.Fatalf("cache ttl = %v", empty.cacheTTL)
	}
	empty.SetHostOnline("h1", true, time.Now())
}

func TestPublishAndAwaitUsesDefaultTimeoutAndRejectsDuplicateOutstandingRequestID(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	session.onPublish = func(string, []byte) {
		result, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r1", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded})
		service.HandleMessage(ResultTopic("h1"), result)
	}
	if _, err := service.PublishAndAwait(context.Background(), "h1", &whalereinv1.HostRequest{RequestId: "r1", HostId: "h1"}, 0); err != nil {
		t.Fatal(err)
	}
	service.transient["dup"] = make(chan *whalereinv1.HostResult, 1)
	if _, err := service.PublishAndAwait(context.Background(), "h1", &whalereinv1.HostRequest{RequestId: "dup", HostId: "h1"}, time.Second); err == nil {
		t.Fatal("expected duplicate transient error")
	}
}

func TestHandleResultFillsMissingHostIDFromTopic(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	var gotHost string
	service.SetResultHandler(func(_ context.Context, hostID string, result *whalereinv1.HostResult) error {
		gotHost = hostID
		if result.GetHostId() != "h1" {
			t.Errorf("result host = %q", result.GetHostId())
		}
		return nil
	})
	data, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "r1"})
	service.HandleMessage(ResultTopic("h1"), data)
	if gotHost != "h1" {
		t.Fatalf("host = %q", gotHost)
	}
}

func TestPublishDeviceEventReturnsBrokerFailure(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	session.publishErr = errors.New("mqtt down")
	if err := service.PublishDeviceEvent(context.Background(), "h1", "d1", []byte("x")); err == nil {
		t.Fatal("expected publish error")
	}
}

func TestReadyUsesBaseContextWhenRequestContextIsNil(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	ready := make(chan struct{}, 1)
	service.SetReadyHandler(func(context.Context, string) error {
		ready <- struct{}{}
		return nil
	})
	result, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "", HostId: "h1", Status: whalereinv1.OnlineStatus_succeeded, ResultBody: mustMarshalHostReady(t, "h1")})
	service.HandleMessage(ResultTopic("h1"), result)
	select {
	case <-ready:
	case <-time.After(time.Second):
		t.Fatal("ready handler not called")
	}
}

func TestPresenceIgnoresUnknownWebhookAndParsesConnectedClientPayload(t *testing.T) {
	presence := newPresence()
	if err := presence.HandlePresenceWebhook([]byte(`{"event":"client.connected","clientid":"other"}`)); err != nil {
		t.Fatal(err)
	}
	if connected, err := parseConnected([]byte(`{"data":{"connected":true}}`)); err != nil || !connected {
		t.Fatalf("data = %v %v", connected, err)
	}
	if connected, err := parseConnected([]byte(`{"data":[]}`)); err != nil || connected {
		t.Fatalf("bad data = %v %v", connected, err)
	}
	if _, err := parseConnected([]byte(`{`)); err == nil {
		t.Fatal("malformed")
	}
	if connected, err := parseConnected([]byte(`{}`)); err != nil || connected {
		t.Fatalf("empty = %v %v", connected, err)
	}
}

type fakeMQTTClient struct {
	publishResponse *paho.PublishResponse
	publishErr      error
	suback          *paho.Suback
	subscribeErr    error
	disconnectErr   error
	done            chan struct{}
	publishTopic    string
	publishQoS      byte
	publishPayload  []byte
	subscribeQoS    []byte
	subscribeTopics []string
}

func (f *fakeMQTTClient) Publish(_ context.Context, publish *paho.Publish) (*paho.PublishResponse, error) {
	f.publishTopic = publish.Topic
	f.publishQoS = publish.QoS
	f.publishPayload = append([]byte(nil), publish.Payload...)
	return f.publishResponse, f.publishErr
}

func (f *fakeMQTTClient) Subscribe(_ context.Context, subscribe *paho.Subscribe) (*paho.Suback, error) {
	for _, subscription := range subscribe.Subscriptions {
		f.subscribeTopics = append(f.subscribeTopics, subscription.Topic)
		f.subscribeQoS = append(f.subscribeQoS, subscription.QoS)
	}
	return f.suback, f.subscribeErr
}

func (f *fakeMQTTClient) Done() <-chan struct{} {
	if f.done == nil {
		f.done = make(chan struct{})
	}
	return f.done
}

func (f *fakeMQTTClient) Disconnect(*paho.Disconnect) error { return f.disconnectErr }

func TestPahoSessionPreservesRequestedQoSAndReportsBrokerRejections(t *testing.T) {
	ctx := context.Background()
	client := &fakeMQTTClient{publishResponse: &paho.PublishResponse{ReasonCode: 0}, suback: &paho.Suback{Reasons: []byte{0}}}
	session := &pahoSession{client: client}
	if err := session.Publish(ctx, "topic", []byte("x"), 1); err != nil || client.publishTopic != "topic" || client.publishQoS != 1 || !bytes.Equal(client.publishPayload, []byte("x")) {
		t.Fatalf("publish err=%v topic=%q", err, client.publishTopic)
	}
	client.publishResponse = &paho.PublishResponse{ReasonCode: 0x80}
	if err := session.Publish(ctx, "topic", nil, 1); err == nil {
		t.Fatal("expected publish rejection")
	}
	client.publishErr = errors.New("network")
	if err := session.Publish(ctx, "topic", nil, 1); err == nil {
		t.Fatal("expected publish error")
	}
	client.publishErr = nil
	if err := session.Subscribe(ctx, []string{"a", "b"}, 1); err != nil || len(client.subscribeTopics) != 2 || client.subscribeTopics[0] != "a" || client.subscribeTopics[1] != "b" || !bytes.Equal(client.subscribeQoS, []byte{1, 1}) {
		t.Fatalf("subscribe err=%v topics=%v", err, client.subscribeTopics)
	}
	client.subscribeErr = errors.New("denied")
	if err := session.Subscribe(ctx, []string{"a"}, 1); err == nil {
		t.Fatal("expected subscribe error")
	}
	client.subscribeErr = nil
	client.suback = nil
	if err := session.Subscribe(ctx, []string{"a"}, 1); err == nil {
		t.Fatal("expected empty suback error")
	}
	client.suback = &paho.Suback{Reasons: []byte{0x80}}
	if err := session.Subscribe(ctx, []string{"a"}, 1); err == nil {
		t.Fatal("expected rejected subscription")
	}
	if session.Done() == nil {
		t.Fatal("done channel")
	}
	if err := session.Close(); err != nil {
		t.Fatal(err)
	}
	client.disconnectErr = errors.New("connection lost")
	if err := session.Close(); err != nil {
		t.Fatalf("connection lost should be ignored: %v", err)
	}
	if err := (&pahoSession{}).Close(); err != nil {
		t.Fatal(err)
	}
}

func TestPahoSessionPublishesPresenceJsonAsRawMqtt(t *testing.T) {
	client := &fakeMQTTClient{publishResponse: &paho.PublishResponse{ReasonCode: 0}}
	peers := &sync.Map{}
	transport := newLogicalTransport(func(ctx context.Context, topic string, payload []byte) error {
		_, err := client.Publish(ctx, &paho.Publish{Topic: topic, QoS: 1, Payload: payload})
		return err
	}, func(string, []byte) {})
	session := &pahoSession{client: client, transport: transport, peers: peers}
	payload := []byte(`{"event":"device_offline","device_id":"d1","reason":"will","observed_at":"2026-11-13T12:00:00Z"}`)
	if err := session.Publish(context.Background(), PresenceTopic("h1"), payload, 1); err != nil {
		t.Fatal(err)
	}
	if client.publishTopic != "whalerein/v1/h/h1/presence" || client.publishQoS != 1 || !bytes.Equal(client.publishPayload, payload) {
		t.Fatalf("publish topic=%q qos=%d payload=%q", client.publishTopic, client.publishQoS, client.publishPayload)
	}
}
