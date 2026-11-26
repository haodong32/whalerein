package observation

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func TestLoadManifestRejectsInvalidDocuments(t *testing.T) {
	dir := t.TempDir()
	cases := map[string]string{
		"invalid":     `{`,
		"incomplete":  `{"schema":1}`,
		"wrong-clock": `{"schema":1,"header":{"v":1},"events":{"e":{}},"clock":{"source":"REALTIME","unit":"ns"}}`,
	}
	for name, body := range cases {
		path := filepath.Join(dir, name+".json")
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
		if _, err := LoadManifest(path); err == nil {
			t.Fatalf("%s manifest accepted", name)
		}
	}
	if _, err := LoadManifest(filepath.Join(dir, "missing.json")); err == nil {
		t.Fatal("missing manifest accepted")
	}
}

func TestDecodeRecordRejectsMalformedAndTrailingData(t *testing.T) {
	if _, err := DecodeRecord([]byte(`{"v":`)); err == nil {
		t.Fatal("malformed record accepted")
	}
	if _, err := DecodeRecord([]byte(`{"v":1} {"v":2}`)); err == nil {
		t.Fatal("trailing record accepted")
	}
}

func TestValidateRejectsEveryMalformedShape(t *testing.T) {
	manifest, _, _ := loadContract(t)
	base := func() map[string]any {
		return map[string]any{
			"v": json.Number("1"), "run": "run-1", "producer": "access",
			"lifetime": "lt-1", "seq": json.Number("1"), "ts": json.Number("1"),
			"event": "access_query",
			"data": map[string]any{
				"request": "abc", "attempt": json.Number("1"),
				"route": "inline", "stage": "admit",
			},
		}
	}
	cases := map[string]func(map[string]any){
		"unknown header":   func(record map[string]any) { record["extra"] = true },
		"missing header":   func(record map[string]any) { delete(record, "run") },
		"wrong version":    func(record map[string]any) { record["v"] = json.Number("9") },
		"empty run":        func(record map[string]any) { record["run"] = "" },
		"unknown producer": func(record map[string]any) { record["producer"] = "nobody" },
		"empty lifetime":   func(record map[string]any) { record["lifetime"] = "" },
		"bad seq":          func(record map[string]any) { record["seq"] = "one" },
		"bad ts":           func(record map[string]any) { record["ts"] = json.Number("-2") },
		"bad event":        func(record map[string]any) { record["event"] = 4 },
		"unknown event":    func(record map[string]any) { record["event"] = "access_unknown" },
		"wrong producer for event": func(record map[string]any) {
			record["producer"] = "core"
		},
		"missing data": func(record map[string]any) { record["data"] = nil },
		"missing required": func(record map[string]any) {
			delete(record["data"].(map[string]any), "attempt")
		},
		"unknown field": func(record map[string]any) {
			record["data"].(map[string]any)["extra"] = 1
		},
		"token canary": func(record map[string]any) {
			record["data"].(map[string]any)["token"] = "canary"
		},
		"cursor canary": func(record map[string]any) {
			record["data"].(map[string]any)["cursor"] = "canary"
		},
		"payload canary": func(record map[string]any) {
			record["data"].(map[string]any)["payload"] = "canary"
		},
		"forbidden nested": func(record map[string]any) {
			record["data"].(map[string]any)["attempt"] = []any{map[string]any{"text": "secret"}}
		},
		"wrong enum": func(record map[string]any) {
			record["data"].(map[string]any)["route"] = "sideways"
		},
		"wrong u64": func(record map[string]any) {
			record["data"].(map[string]any)["attempt"] = -1
		},
		"wrong bool": func(record map[string]any) {
			record["event"] = "access_command"
			record["data"] = map[string]any{
				"request": "abc", "attempt": json.Number("1"), "stage": "admit", "durable": "yes",
			}
		},
		"wrong str": func(record map[string]any) {
			record["data"].(map[string]any)["request"] = 1
		},
		"wrong producer field": func(record map[string]any) {
			record["event"] = "fixture_clock_check"
			record["producer"] = "fixture"
			record["data"] = map[string]any{
				"producer": "nobody", "reading_ns": json.Number("1"),
				"before_ns": json.Number("1"), "after_ns": json.Number("1"),
				"clock_source": "CLOCK_MONOTONIC", "ok": true,
			}
		},
	}
	for name, mutate := range cases {
		record := base()
		mutate(record)
		if err := manifest.Validate(record); err == nil {
			t.Fatalf("%s: malformed record accepted", name)
		}
	}
	if err := manifest.Validate(nil); err == nil {
		t.Fatal("nil manifest accepted a record")
	}
}

func TestUnsignedAcceptsSupportedIntegerShapesAndRejectsOthers(t *testing.T) {
	accepted := []any{json.Number("5"), float64(5), 5, int64(5), uint64(5), uint(5)}
	for _, value := range accepted {
		if got, ok := unsigned(value); !ok || got != 5 {
			t.Fatalf("unsigned(%T) = %d %v", value, got, ok)
		}
	}
	rejected := []any{json.Number("-1"), json.Number("x"), float64(-1), 1.5, float64(1 << 70), -1, int64(-1), "5"}
	for _, value := range rejected {
		if _, ok := unsigned(value); ok {
			t.Fatalf("unsigned(%T) accepted", value)
		}
	}
}

func TestValidateRejectsUnsupportedManifestFieldType(t *testing.T) {
	manifest, _, _ := loadContract(t)
	event := manifest.Events["access_query"]
	event.Fields["attempt"] = "mystery"
	manifest.Events["access_query"] = event
	record := map[string]any{
		"v": json.Number("1"), "run": "run-1", "producer": "access", "lifetime": "lt-1",
		"seq": json.Number("1"), "ts": json.Number("1"), "event": "access_query",
		"data": map[string]any{"request": "abc", "attempt": json.Number("1"), "route": "inline", "stage": "admit"},
	}
	if err := manifest.Validate(record); err == nil {
		t.Fatal("unsupported field type accepted")
	}
}

func TestReadRecordsReadsAndRejectsMalformedFiles(t *testing.T) {
	path := filepath.Join(t.TempDir(), "records.ndjson")
	if err := os.WriteFile(path, []byte("{\"a\":1}\n{\"b\":2}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	records, err := ReadRecords(path)
	if err != nil || len(records) != 2 {
		t.Fatalf("records = %d, %v", len(records), err)
	}
	if err := os.WriteFile(path, []byte("{\"a\":1}\nnot-json\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := ReadRecords(path); err == nil {
		t.Fatal("malformed record file accepted")
	}
	if _, err := ReadRecords(filepath.Join(t.TempDir(), "missing")); err == nil {
		t.Fatal("missing record file accepted")
	}
}

func TestOpenRejectsInvalidTraceConfiguration(t *testing.T) {
	clock := &fixedClock{value: 1}
	key := "0123456789abcdef0123456789abcdef"
	cases := map[string]Config{
		"unknown mode":      {Mode: "verbose", Clock: clock},
		"missing run":       {Mode: ModeTrace, Output: &discardWriter{}, CaptureKey: key, Clock: clock},
		"missing outlet":    {Mode: ModeTrace, RunID: "run", CaptureKey: key, Clock: clock},
		"short capture key": {Mode: ModeTrace, Output: &discardWriter{}, RunID: "run", CaptureKey: "short", Clock: clock},
		"unopenable outlet": {Mode: ModeTrace, Outlet: filepath.Join(t.TempDir(), "missing-dir", "out.ndjson"), RunID: "run", CaptureKey: key, Clock: clock},
	}
	for name, config := range cases {
		if _, err := Open(config); err == nil {
			t.Fatalf("%s accepted", name)
		}
	}
	if _, err := NewTokenizer([]byte("short")); err == nil {
		t.Fatal("short capture key accepted")
	}
}

func TestSinkAfterCloseDropsAndStatsNilIsZero(t *testing.T) {
	sink, err := Open(Config{
		Mode: ModeTrace, Output: &discardWriter{}, RunID: "run-closed",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-closed",
		Clock: &fixedClock{value: 1},
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	before := sink.Stats().Dropped
	sink.Query(QueryEvent{Request: "r-1", Attempt: 1, Route: RouteInline, Stage: QueryAdmit})
	if sink.Stats().Dropped != before+1 {
		t.Fatal("post-close emit was not counted as a drop")
	}
	var nilSink *Sink
	nilSink.Query(QueryEvent{})
	nilSink.Command(CommandEvent{})
	nilSink.Relay(RelayEvent{})
	nilSink.Notice(NoticeEvent{})
	nilSink.Transport(TransportEvent{})
	if nilSink.Stats() != (Stats{}) {
		t.Fatal("nil sink stats are not zero")
	}
}

func TestEmitClockFailureDrops(t *testing.T) {
	clock := &mutableClock{value: 1}
	sink, err := Open(Config{
		Mode: ModeTrace, Output: &discardWriter{}, RunID: "run-clock-drop",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-clock-drop",
		Clock: clock,
	})
	if err != nil {
		t.Fatal(err)
	}
	clock.err = errors.New("clock lost")
	sink.Query(QueryEvent{Request: "r-1", Attempt: 1, Route: RouteInline, Stage: QueryAdmit})
	if sink.Stats().Dropped == 0 {
		t.Fatal("clock failure was not counted as a drop")
	}
	sink.Close()
}

func TestTokenizerUnknownDomainAndNilReceiver(t *testing.T) {
	tokenizer, err := NewTokenizer([]byte("0123456789abcdef0123456789abcdef"))
	if err != nil {
		t.Fatal(err)
	}
	if tokenizer.Token("mystery", "raw") != "" {
		t.Fatal("unknown domain produced a token")
	}
	var nilTokenizer *Tokenizer
	if nilTokenizer.Token(DomainRequest, "raw") != "" {
		t.Fatal("nil tokenizer produced a token")
	}
}

type discardWriter struct{}

func (discardWriter) Write(data []byte) (int, error) { return len(data), nil }

type mutableClock struct {
	value uint64
	err   error
}

func (c *mutableClock) Now() (uint64, error) { return c.value, c.err }

func TestOpenReadsFixtureEnvironmentBounds(t *testing.T) {
	t.Setenv(EnvMode, ModeTrace)
	t.Setenv(EnvRunID, "run-env")
	t.Setenv(EnvCaptureKey, "0123456789abcdef0123456789abcdef")
	t.Setenv(EnvLifetime, "lt-env")
	t.Setenv(EnvCapacity, "3")
	t.Setenv(EnvBytes, "96")
	t.Setenv(EnvBytesAlt, "1")
	sink, err := Open(Config{Output: &discardWriter{}, Clock: &fixedClock{value: 1}})
	if err != nil {
		t.Fatal(err)
	}
	if sink.capacity != 3 || sink.maxBytes != 96 {
		t.Fatalf("bounds = %d/%d", sink.capacity, sink.maxBytes)
	}
	if sink.runID != "run-env" || sink.lifetime != "lt-env" {
		t.Fatalf("identity = %s/%s", sink.runID, sink.lifetime)
	}
	for index := 0; index < 20; index++ {
		sink.Query(QueryEvent{Request: "r-1", Attempt: 1, Route: RouteInline, Stage: QueryAdmit})
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	if !sink.Stats().CapReached {
		t.Fatal("environment byte bound was not enforced")
	}
}

// latency/AC-05.clock: the token derivation matches the shared cross-language
// vector (hex key decode, version prefix, domain separator, 16-byte hex).
func TestTokenMatchesSharedDerivationVector(t *testing.T) {
	tokenizer, err := NewTokenizer([]byte("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"))
	if err != nil {
		t.Fatal(err)
	}
	if got := tokenizer.Token(DomainRequest, "req-1"); got != "03605144bf609f3f0e2e459d7ccdb397" {
		t.Fatalf("token = %q", got)
	}
}
