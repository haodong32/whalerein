package observation

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

const contractDir = "../../../contracts/latency"

type fixedClock struct {
	value uint64
	err   error
}

func (c *fixedClock) Now() (uint64, error) { return c.value, c.err }

type countingClock struct {
	next atomic.Uint64
}

func (c *countingClock) Now() (uint64, error) { return c.next.Add(1_000_000), nil }

func loadContract(t *testing.T) (*Manifest, []map[string]any, []map[string]any) {
	t.Helper()
	manifest, err := LoadManifest(filepath.Join(contractDir, "observation.json"))
	if err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(filepath.Join(contractDir, "vectors.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors struct {
		Valid   []map[string]any `json:"valid"`
		Invalid []struct {
			Why   string         `json:"why"`
			Value map[string]any `json:"value"`
		} `json:"invalid"`
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&vectors); err != nil {
		t.Fatal(err)
	}
	invalid := make([]map[string]any, 0, len(vectors.Invalid))
	for _, vector := range vectors.Invalid {
		invalid = append(invalid, vector.Value)
	}
	return manifest, vectors.Valid, invalid
}

// latency/AC-05.clock: every shared vector validates against the checked-in
// manifest, and every malformed vector is rejected.
func TestContractVectorsValidateAgainstManifest(t *testing.T) {
	manifest, valid, invalid := loadContract(t)
	if len(valid) == 0 || len(invalid) == 0 {
		t.Fatal("contract vectors are empty")
	}
	for index, record := range valid {
		if err := manifest.Validate(record); err != nil {
			t.Fatalf("valid vector %d rejected: %v", index, err)
		}
	}
	for index, record := range invalid {
		if err := manifest.Validate(record); err == nil {
			t.Fatalf("invalid vector %d accepted: %v", index, record)
		}
	}
}

// latency/AC-06.privacy: emitted Access records pass the same manifest and
// carry no raw identity or payload canary.
func TestSinkEmitsManifestValidAccessRecords(t *testing.T) {
	manifest, _, _ := loadContract(t)
	outlet := filepath.Join(t.TempDir(), "access.ndjson")
	clock := &countingClock{}
	sink, err := Open(Config{
		Mode: ModeTrace, Outlet: outlet, RunID: "run-access-test",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-access-test",
		Clock: clock,
	})
	if err != nil {
		t.Fatal(err)
	}
	sink.Query(QueryEvent{Scope: "SCOPE-CANARY", Request: "REQ-CANARY", Attempt: 1, Route: RouteInline, Stage: QueryAdmit, Outcome: OutcomeOK})
	sink.Query(QueryEvent{Scope: "SCOPE-CANARY", Request: "REQ-CANARY", Attempt: 1, Route: RouteInline, Stage: QueryReply, Outcome: OutcomeExpired})
	sink.Command(CommandEvent{Scope: "SCOPE-CANARY", Request: "CMD-CANARY", Attempt: 2, Stage: CommandRecover, Durable: true, Outcome: OutcomeOK, Bytes: 42, HasBytes: true})
	sink.Relay(RelayEvent{Scope: "SCOPE-CANARY", Request: "CMD-CANARY", ObjectOp: "OBJ-CANARY", Stage: RelayVerify, Route: RouteReference, Bytes: 42, HasBytes: true, Outcome: OutcomeOK})
	sink.Notice(NoticeEvent{Connection: "CONN-CANARY", Generation: 9, Role: RoleHost, Stage: NoticeReady, Outcome: OutcomeOK})
	sink.Transport(TransportEvent{Connection: "CONN-CANARY", Generation: 9, Role: RoleClient, Direction: DirectionBrokerToClient, Bytes: 42, Outcome: TransportOutcomeOK})
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	file, err := os.Open(outlet)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 64*1024), 1<<20)
	events := map[string]bool{}
	for scanner.Scan() {
		line := scanner.Bytes()
		for _, canary := range []string{"REQ-CANARY", "CMD-CANARY", "OBJ-CANARY", "CONN-CANARY", "SCOPE-CANARY"} {
			if bytes.Contains(line, []byte(canary)) {
				t.Fatalf("raw identity %q leaked into outlet: %s", canary, line)
			}
		}
		record, err := DecodeRecord(line)
		if err != nil {
			t.Fatal(err)
		}
		if err := manifest.Validate(record); err != nil {
			t.Fatalf("emitted record rejected: %v (%s)", err, line)
		}
		if record["event"] != "access_notice" && record["event"] != "access_transport" {
			data := record["data"].(map[string]any)
			if _, ok := data["scope"]; !ok {
				t.Fatalf("%s is missing its scope token", record["event"])
			}
		}
		events[record["event"].(string)] = true
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	for _, event := range []string{"access_query", "access_command", "access_relay", "access_notice", "access_transport"} {
		if !events[event] {
			t.Fatalf("missing %s", event)
		}
	}
	if sink.Stats().Emitted != 6 {
		t.Fatalf("emitted = %d", sink.Stats().Emitted)
	}
}

// latency/AC-06.cleanup: off and aggregate create no writer, no file and no
// tokenizer; the nil sink is the zero-cost disabled path.
func TestOpenOffAndAggregateCreateNoWriter(t *testing.T) {
	dir := t.TempDir()
	for _, mode := range []string{ModeOff, ModeAggregate, ""} {
		outlet := filepath.Join(dir, mode+".ndjson")
		sink, err := Open(Config{Mode: mode, Outlet: outlet})
		if err != nil {
			t.Fatalf("%s: %v", mode, err)
		}
		if sink != nil {
			t.Fatalf("%s: expected nil sink", mode)
		}
		sink.Query(QueryEvent{Request: "r-1", Attempt: 1, Route: RouteInline, Stage: QueryAdmit})
		sink.Transport(TransportEvent{Connection: "c-1", Generation: 1, Role: RoleClient, Direction: DirectionBrokerToClient})
		sink.Close()
		if _, err := os.Stat(outlet); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("%s: outlet was created", mode)
		}
	}
}

// latency/AC-06.saturation: a stalled consumer cannot block emission. The
// bounded channel drops and counts the excess records.
func TestSinkSaturationDropsWithoutBlocking(t *testing.T) {
	writer := newBlockingWriter()
	defer writer.releaseAll()
	clock := &fixedClock{value: 1}
	sink, err := Open(Config{
		Mode: ModeTrace, Output: writer, RunID: "run-sat",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-sat",
		Capacity: 4, Clock: clock,
	})
	if err != nil {
		t.Fatal(err)
	}
	sink.Query(QueryEvent{Request: "r-1", Attempt: 1, Route: RouteInline, Stage: QueryForward})
	select {
	case <-writer.started:
	case <-time.After(time.Second):
		t.Fatal("writer did not start")
	}
	started := time.Now()
	for index := 0; index < 200; index++ {
		sink.Query(QueryEvent{Request: "r-1", Attempt: 1, Route: RouteInline, Stage: QueryForward})
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("saturated emit blocked for %s", elapsed)
	}
	if got := sink.Stats().Dropped; got == 0 {
		t.Fatal("expected dropped records after saturation")
	}
	if got := sink.Stats().Emitted; got > 250 {
		t.Fatalf("emitted exceeds offered records: %d", got)
	}
}

// latency/AC-06.sink-failure: a write failure is absorbed by the writer and
// never propagates into business handling.
func TestSinkWriteFailureDoesNotBlockOrPanic(t *testing.T) {
	sink, err := Open(Config{
		Mode: ModeTrace, Output: &failingWriter{}, RunID: "run-fail",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-fail",
		Clock: &fixedClock{value: 7}, Capacity: 8,
	})
	if err != nil {
		t.Fatal(err)
	}
	for index := 0; index < 50; index++ {
		sink.Notice(NoticeEvent{Connection: "c-1", Generation: 1, Role: RoleClient, Stage: NoticeSubscribe})
	}
	deadline := time.Now().Add(time.Second)
	for sink.Stats().WriteErrors == 0 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	stats := sink.Stats()
	if stats.WriteErrors == 0 {
		t.Fatal("write failure was not recorded")
	}
	if stats.Dropped == 0 {
		t.Fatal("failed writes were not counted as drops")
	}
	if err := sink.Close(); err == nil {
		t.Fatal("close should report the write failure as incomplete evidence")
	}
}

// latency/AC-06.cleanup: shutdown drains the queue, flushes and is bounded
// even when the underlying writer stalls.
func TestSinkCloseDrainsAndIsBounded(t *testing.T) {
	outlet := filepath.Join(t.TempDir(), "flush.ndjson")
	sink, err := Open(Config{
		Mode: ModeTrace, Outlet: outlet, RunID: "run-flush",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-flush",
		Clock: &fixedClock{value: 3},
	})
	if err != nil {
		t.Fatal(err)
	}
	for index := 0; index < 20; index++ {
		sink.Query(QueryEvent{Request: "r-1", Attempt: uint64(index + 1), Route: RouteInline, Stage: QueryAdmit})
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(outlet)
	if err != nil {
		t.Fatal(err)
	}
	if lines := bytes.Count(data, []byte("\n")); lines != 20 {
		t.Fatalf("flushed lines = %d", lines)
	}
	if !sink.Stats().Closed {
		t.Fatal("stats did not report closed")
	}

	stalled := newBlockingWriter()
	sink, err = Open(Config{
		Mode: ModeTrace, Output: stalled, RunID: "run-stall",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-stall",
		Clock: &fixedClock{value: 4}, CloseTimeout: 50 * time.Millisecond,
	})
	if err != nil {
		t.Fatal(err)
	}
	sink.Query(QueryEvent{Request: "r-1", Attempt: 1, Route: RouteInline, Stage: QueryAdmit})
	select {
	case <-stalled.started:
	case <-time.After(time.Second):
		t.Fatal("writer did not start")
	}
	started := time.Now()
	err = sink.Close()
	if err == nil {
		t.Fatal("expected bounded shutdown error")
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("Close exceeded its bound: %s", elapsed)
	}
	stalled.releaseAll()
}

// latency/AC-05.clock: trace requires a readable shared clock.
func TestOpenRejectsUnverifiedClock(t *testing.T) {
	if _, err := Open(Config{
		Mode: ModeTrace, Output: &bytes.Buffer{}, RunID: "run-clock",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-clock",
		Clock: &fixedClock{err: errors.New("no monotonic clock")},
	}); err == nil {
		t.Fatal("unverified clock was accepted")
	}
	if _, err := Open(Config{
		Mode: ModeTrace, Output: &bytes.Buffer{}, RunID: "run-clock",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-clock",
		Clock: &fixedClock{value: 1},
	}); err != nil {
		t.Fatal(err)
	}
}

// latency/AC-07.cardinality: tokens are fixed-length, deterministic per
// domain and never collide across the domain separator.
func TestTokensAreOpaqueAndDomainSeparated(t *testing.T) {
	tokenizer, err := NewTokenizer([]byte("0123456789abcdef0123456789abcdef"))
	if err != nil {
		t.Fatal(err)
	}
	request := tokenizer.Token(DomainRequest, "same-raw-id")
	if len(request) != 32 {
		t.Fatalf("token length = %d", len(request))
	}
	if request != tokenizer.Token(DomainRequest, "same-raw-id") {
		t.Fatal("token is not deterministic")
	}
	for _, domain := range []Domain{DomainSession, DomainStream, DomainObject, DomainConnection} {
		if other := tokenizer.Token(domain, "same-raw-id"); other == request {
			t.Fatalf("domain %s did not separate the token", domain)
		}
	}
	other, err := NewTokenizer([]byte("fedcba9876543210fedcba9876543210"))
	if err != nil {
		t.Fatal(err)
	}
	if other.Token(DomainRequest, "same-raw-id") == request {
		t.Fatal("capture key did not separate the token")
	}
}

// latency/AC-06.saturation: the 32 MiB byte cap stops retaining detail while
// keeping the outlet responsive.
func TestSinkByteCapStopsRetention(t *testing.T) {
	var buffer bytes.Buffer
	sink, err := Open(Config{
		Mode: ModeTrace, Output: &buffer, RunID: "run-cap",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-cap",
		Clock: &fixedClock{value: 5}, MaxBytes: 256,
	})
	if err != nil {
		t.Fatal(err)
	}
	for index := 0; index < 100; index++ {
		sink.Relay(RelayEvent{Request: "r-1", ObjectOp: "o-1", Stage: RelayUpload, Route: RouteReference, Bytes: 1, HasBytes: true})
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	if buffer.Len() > 256 {
		t.Fatalf("capture exceeded cap: %d", buffer.Len())
	}
	stats := sink.Stats()
	if !stats.CapReached || stats.Dropped == 0 {
		t.Fatalf("cap not reported: %+v", stats)
	}
}

type blockingWriter struct {
	started     chan struct{}
	release     chan struct{}
	startOnce   sync.Once
	releaseOnce sync.Once
}

func newBlockingWriter() *blockingWriter {
	return &blockingWriter{started: make(chan struct{}), release: make(chan struct{})}
}

func (w *blockingWriter) Write(data []byte) (int, error) {
	w.startOnce.Do(func() { close(w.started) })
	<-w.release
	return len(data), nil
}

func (w *blockingWriter) releaseAll() { w.releaseOnce.Do(func() { close(w.release) }) }

type failingWriter struct{}

func (failingWriter) Write([]byte) (int, error) { return 0, errors.New("disk full") }

// latency/AC-05.concurrent: the request token is scope-free so cross-producer
// request joins keep working, while a separate scope token distinguishes the
// same client request id across devices.
func TestScopeTokenSeparatesSameRequestIDAcrossDevices(t *testing.T) {
	outlet := filepath.Join(t.TempDir(), "scope.ndjson")
	sink, err := Open(Config{
		Mode: ModeTrace, Outlet: outlet, RunID: "run-scope",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-scope",
		Clock: &fixedClock{value: 1},
	})
	if err != nil {
		t.Fatal(err)
	}
	sink.Query(QueryEvent{Scope: "host-1\x00device-1", Request: "shared-request", Attempt: 1, Route: RouteInline, Stage: QueryAdmit})
	sink.Query(QueryEvent{Scope: "host-1\x00device-2", Request: "shared-request", Attempt: 1, Route: RouteInline, Stage: QueryAdmit})
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	records, err := ReadRecords(outlet)
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 2 {
		t.Fatalf("records = %d", len(records))
	}
	first := records[0]["data"].(map[string]any)
	second := records[1]["data"].(map[string]any)
	if first["request"] != second["request"] {
		t.Fatal("scope-free request token changed across devices")
	}
	if first["scope"] == second["scope"] {
		t.Fatal("scope token did not distinguish devices")
	}
	if first["scope"] == first["request"] {
		t.Fatal("scope and request tokens share a namespace")
	}
}

// latency/AC-01.inline latency/AC-11.normal: a transport fact exports exactly
// the six bounded contract fields, uses the connection token namespace and
// never carries the raw connection reference, a payload, a topic or a message
// identity.
func TestTransportRecordsAreManifestValidAndBounded(t *testing.T) {
	manifest, _, _ := loadContract(t)
	outlet := filepath.Join(t.TempDir(), "transport.ndjson")
	sink, err := Open(Config{
		Mode: ModeTrace, Outlet: outlet, RunID: "run-transport",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-transport",
		Clock: &fixedClock{value: 1},
	})
	if err != nil {
		t.Fatal(err)
	}
	const clientRef = "epoch-1:7"
	for _, outcome := range []TransportOutcome{
		TransportOutcomeOK, TransportOutcomeFailed, TransportOutcomeDropped,
	} {
		sink.Transport(TransportEvent{
			Connection: clientRef, Generation: 7, Role: RoleClient,
			Direction: DirectionBrokerToClient, Bytes: 42, Outcome: outcome,
		})
	}
	sink.Transport(TransportEvent{
		Connection: "epoch-1:3", Generation: 3, Role: RoleHost,
		Direction: DirectionHostToBroker, Bytes: 9, Outcome: TransportOutcomeFailed,
	})
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	records, err := ReadRecords(outlet)
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 4 {
		t.Fatalf("records = %d", len(records))
	}
	allowed := map[string]bool{
		"connection": true, "generation": true, "role": true,
		"direction": true, "bytes": true, "outcome": true,
	}
	for index, record := range records {
		if err := manifest.Validate(record); err != nil {
			t.Fatalf("record %d rejected: %v", index, err)
		}
		if record["event"] != "access_transport" {
			t.Fatalf("record %d event = %v", index, record["event"])
		}
		data := record["data"].(map[string]any)
		if len(data) != len(allowed) {
			t.Fatalf("record %d has %d fields: %v", index, len(data), data)
		}
		for key := range data {
			if !allowed[key] {
				t.Fatalf("record %d carries unbounded field %q", index, key)
			}
		}
		if _, ok := data["generation"].(json.Number); !ok {
			t.Fatalf("record %d generation is %T", index, data["generation"])
		}
		if _, ok := data["bytes"].(json.Number); !ok {
			t.Fatalf("record %d bytes is %T", index, data["bytes"])
		}
	}

	tokenizer, err := NewTokenizer([]byte("0123456789abcdef0123456789abcdef"))
	if err != nil {
		t.Fatal(err)
	}
	wantToken := tokenizer.Token(DomainConnection, clientRef)
	first := records[0]["data"].(map[string]any)
	if first["connection"] != wantToken {
		t.Fatalf("connection token = %v want %s", first["connection"], wantToken)
	}
	if first["connection"] == clientRef {
		t.Fatal("raw connection reference was exported")
	}
	if records[1]["data"].(map[string]any)["connection"] != wantToken {
		t.Fatal("same connection produced different tokens")
	}
	if records[3]["data"].(map[string]any)["connection"] == wantToken {
		t.Fatal("distinct connections share a token")
	}
	raw, err := os.ReadFile(outlet)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(raw, []byte(clientRef)) || bytes.Contains(raw, []byte("epoch-1:3")) {
		t.Fatal("raw connection reference leaked into the outlet")
	}
}

// latency/AC-06.privacy: the manifest rejects any transport record that adds a
// payload, topic, message identity or peer address instead of the six bounded
// fields.
func TestTransportManifestRejectsUnboundedFields(t *testing.T) {
	manifest, _, _ := loadContract(t)
	base := func() map[string]any {
		return map[string]any{
			"v": json.Number("1"), "run": "run-1", "producer": "access",
			"lifetime": "lt-1", "seq": json.Number("1"), "ts": json.Number("1"),
			"event": "access_transport",
			"data": map[string]any{
				"connection": "token", "generation": json.Number("7"),
				"role": "client", "direction": "broker_to_client",
				"bytes": json.Number("42"), "outcome": "ok",
			},
		}
	}
	for _, field := range []string{"payload", "topic", "message_id", "peer", "host_id", "stream", "text"} {
		record := base()
		record["data"].(map[string]any)[field] = "canary"
		if err := manifest.Validate(record); err == nil {
			t.Fatalf("unbounded field %q accepted", field)
		}
	}
	cases := map[string]func(map[string]any){
		"missing connection": func(record map[string]any) {
			delete(record["data"].(map[string]any), "connection")
		},
		"missing bytes": func(record map[string]any) {
			delete(record["data"].(map[string]any), "bytes")
		},
		"wrong role": func(record map[string]any) {
			record["data"].(map[string]any)["role"] = "device"
		},
		"wrong direction": func(record map[string]any) {
			record["data"].(map[string]any)["direction"] = "client_to_broker"
		},
		"wrong outcome": func(record map[string]any) {
			record["data"].(map[string]any)["outcome"] = "sent"
		},
		"negative bytes": func(record map[string]any) {
			record["data"].(map[string]any)["bytes"] = json.Number("-1")
		},
		"string generation": func(record map[string]any) {
			record["data"].(map[string]any)["generation"] = "7"
		},
	}
	for name, mutate := range cases {
		record := base()
		mutate(record)
		if err := manifest.Validate(record); err == nil {
			t.Fatalf("%s: malformed transport record accepted", name)
		}
	}
}
