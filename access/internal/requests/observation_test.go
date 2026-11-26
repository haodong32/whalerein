package requests

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/observation"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func openRequestsSink(t *testing.T) (*observation.Sink, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "access.ndjson")
	sink, err := observation.Open(observation.Config{
		Mode: observation.ModeTrace, Outlet: path, RunID: "run-command-test",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-command-test",
	})
	if err != nil {
		t.Fatal(err)
	}
	return sink, path
}

func newObservedFixture(t *testing.T, sink *observation.Sink) *fixture {
	t.Helper()
	f := newFixture(t)
	service, err := New(Dependencies{
		Store: f.store, Broker: f.broker, Presence: f.presence, Grants: f.grants, Payloads: f.payloads,
		Config: Config{
			ProtocolVersion: 1, MaxTTL: time.Hour, ForwardInterval: time.Second,
			Lease: 30 * time.Second, RetryInterval: 5 * time.Second, MaxAttempts: 3, BatchSize: 10,
		},
		Metrics:     diagnostics.NewMetrics(),
		Masker:      diagnostics.NewMaskerWithKey([]byte("req-obs")),
		Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
		Now:         func() time.Time { return f.now },
		Observation: sink,
	})
	if err != nil {
		t.Fatal(err)
	}
	f.service = service
	return f
}

func commandEvents(t *testing.T, path string) []map[string]any {
	t.Helper()
	records, err := observation.ReadRecords(path)
	if err != nil {
		t.Fatal(err)
	}
	events := make([]map[string]any, 0, len(records))
	for _, record := range records {
		if record["event"] == "access_command" {
			events = append(events, record)
		}
	}
	return events
}

// latency/AC-02.durable-reconnect: a durable command keeps one opaque request
// identity while a reconnect creates a new transport attempt; receipt and the
// client terminal read carry the stable identity.
func TestDurableCommandReconnectKeepsIdentityAndAttempt(t *testing.T) {
	sink, path := openRequestsSink(t)
	f := newObservedFixture(t, sink)
	request := validRequest(f.now, "cmd-1")
	if _, created, err := f.service.Submit(context.Background(), input(request)); err != nil || !created {
		t.Fatalf("submit: %v (created %v)", err, created)
	}

	// One earlier publication was not confirmed. The forward loop retries the
	// same row on a new transport attempt.
	row := f.store.rows["req-1"]
	row.ForwardAttempts = 1
	f.store.claimRows = []store.Request{row}
	if err := f.service.ForwardDue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(f.broker.published) != 1 {
		t.Fatalf("reconnect publication = %d", len(f.broker.published))
	}
	row.ForwardAttempts = 1
	f.store.rows["req-1"] = row

	result := &whalereinv1.HostResult{
		RequestId: "req-1", HostId: "h1", DeviceId: "d1", CommandId: "cmd-1",
		PayloadDigest: []byte("digest"), Status: whalereinv1.OnlineStatus_succeeded,
		ResultBody: []byte("answer"), RelayVersion: RelayEpoch,
	}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatal(err)
	}
	stored := f.store.rows["req-1"]
	stored.Status = whalereinv1.OnlineStatus_succeeded
	stored.ResultBody = []byte("answer")
	stored.ForwardAttempts = 1
	f.store.rows["req-1"] = stored
	if _, err := f.service.Get(context.Background(), "d1", "req-1"); err != nil {
		t.Fatal(err)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := commandEvents(t, path)
	type stageAttempt struct {
		stage   string
		attempt string
	}
	var observed []stageAttempt
	tokens := map[any]bool{}
	for _, event := range events {
		data := event["data"].(map[string]any)
		observed = append(observed, stageAttempt{stage: data["stage"].(string), attempt: data["attempt"].(json.Number).String()})
		tokens[data["request"]] = true
	}
	want := []stageAttempt{{"admit", "1"}, {"recover", "2"}, {"receipt", "2"}, {"terminal", "2"}}
	if len(observed) != len(want) {
		t.Fatalf("command events = %v want %v", observed, want)
	}
	for index := range want {
		if observed[index] != want[index] {
			t.Fatalf("command event %d = %v want %v", index, observed[index], want[index])
		}
	}
	if len(tokens) != 1 {
		t.Fatal("durable command identity changed across the reconnect")
	}
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(contents), "req-1") || strings.Contains(string(contents), "cmd-1") {
		t.Fatal("raw command identity leaked into the outlet")
	}
}

// latency/AC-02.expired: an elapsed durable command is an explicit expired
// admission or terminal disposition and is never reported as success.
func TestDurableCommandExpiryIsExplicit(t *testing.T) {
	sink, path := openRequestsSink(t)
	f := newObservedFixture(t, sink)

	expired := validRequest(f.now, "cmd-expired")
	expired.ExpiresAtUnixMs = f.now.Add(-time.Second).UnixMilli()
	if _, _, err := f.service.Submit(context.Background(), input(expired)); err == nil {
		t.Fatal("expired command was admitted")
	}

	request := validRequest(f.now, "cmd-1")
	if _, _, err := f.service.Submit(context.Background(), input(request)); err != nil {
		t.Fatal(err)
	}
	row := f.store.rows["req-1"]
	f.store.expireRows = []store.Request{row}
	f.service.SweepClientLeases(context.Background())
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := commandEvents(t, path)
	var admitExpired, terminalExpired bool
	for _, event := range events {
		data := event["data"].(map[string]any)
		switch data["stage"] {
		case "admit":
			if data["outcome"] == "expired" {
				admitExpired = true
			}
		case "terminal":
			if data["outcome"] == "expired" {
				terminalExpired = true
			}
		}
	}
	if !admitExpired {
		t.Fatal("expired admission was not recorded")
	}
	if !terminalExpired {
		t.Fatal("expired terminal disposition was not recorded")
	}
}

// latency/AC-02.late: a result that cannot be applied after the caller
// deadline is recorded as late, keeps the stored row non-terminal and never
// rewrites the observed timeout as success.
func TestDurableCommandLateResultStaysLate(t *testing.T) {
	sink, path := openRequestsSink(t)
	f := newObservedFixture(t, sink)
	request := validRequest(f.now, "cmd-1")
	if _, _, err := f.service.Submit(context.Background(), input(request)); err != nil {
		t.Fatal(err)
	}
	apply := false
	f.store.applyResult = &apply
	f.now = f.now.Add(6 * time.Minute)
	result := &whalereinv1.HostResult{
		RequestId: "req-1", HostId: "h1", DeviceId: "d1", CommandId: "cmd-1",
		PayloadDigest: []byte("digest"), Status: whalereinv1.OnlineStatus_succeeded,
		ResultBody: []byte("late answer"), RelayVersion: RelayEpoch,
	}
	if err := f.service.HandleHostResult(context.Background(), "h1", result); err != nil {
		t.Fatal(err)
	}
	if stored := f.store.rows["req-1"]; stored.Status != whalereinv1.OnlineStatus_received {
		t.Fatalf("late result changed the stored status to %s", stored.Status)
	}
	if _, err := f.service.Get(context.Background(), "d1", "req-1"); err == nil {
		t.Fatal("late result turned the observed timeout into success")
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	var late, successfulReceipt bool
	for _, event := range commandEvents(t, path) {
		data := event["data"].(map[string]any)
		if data["stage"] == "receipt" && data["outcome"] == "late" {
			late = true
		}
		if data["stage"] == "receipt" && data["outcome"] == "ok" {
			successfulReceipt = true
		}
	}
	if !late {
		t.Fatal("late receipt was not recorded")
	}
	if successfulReceipt {
		t.Fatal("late result was recorded as a successful receipt")
	}
}
