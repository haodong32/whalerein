package relay

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/observation"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func openRelaySink(t *testing.T) (*observation.Sink, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "access.ndjson")
	sink, err := observation.Open(observation.Config{
		Mode: observation.ModeTrace, Outlet: path, RunID: "run-relay-test",
		CaptureKey: "0123456789abcdef0123456789abcdef", Lifetime: "lt-relay-test",
	})
	if err != nil {
		t.Fatal(err)
	}
	return sink, path
}

func newObservedRelay(t *testing.T, sink *observation.Sink) (*Service, *fakeAuth, *fakeStore, *fakePayloads) {
	t.Helper()
	authority, metadata, payloads := newFakes()
	service, err := New(Dependencies{
		Auth: authority, Store: metadata, Payloads: payloads,
		Config: Config{
			Enabled: true, NodeID: "node-1", OwnerID: "owner-1",
			Retention: 10 * time.Minute, UploadTimeout: 30 * time.Second, ReadTimeout: time.Minute,
		},
		Now:         func() time.Time { return testNow },
		Metrics:     diagnostics.NewMetrics(),
		Observation: sink,
	})
	if err != nil {
		t.Fatal(err)
	}
	return service, authority, metadata, payloads
}

func relayEvents(t *testing.T, path string) []map[string]any {
	t.Helper()
	records, err := observation.ReadRecords(path)
	if err != nil {
		t.Fatal(err)
	}
	events := make([]map[string]any, 0, len(records))
	for _, record := range records {
		if record["event"] == "access_relay" {
			events = append(events, record)
		}
	}
	return events
}

// latency/AC-04.referenced-attachment: the referenced route exercises
// reservation, physical upload, finish, digest verification and fetch, and
// keeps physical bytes distinct from verified logical bytes.
func TestReferencedAttachmentLifecycleEmitsObjectFacts(t *testing.T) {
	sink, path := openRelaySink(t)
	service, _, metadata, payloads := newObservedRelay(t, sink)
	metadata.grant = testGrant()
	object := testObject()
	object.Purpose = "PAYLOAD_PURPOSE_ATTACHMENT_PART"
	metadata.objects[object.ObjectID] = object
	payloads.object = object

	request := validReserveRequest()
	request.PartID = "part-1"
	request.Purpose = whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART
	reservation, err := service.Reserve(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	// The reservation mints the opaque object id; the metadata row must exist
	// under that exact id before the object routes run.
	object.ObjectID = reservation.ObjectID
	metadata.objects[object.ObjectID] = object
	payloads.object = object
	target, err := service.PrepareUpload(context.Background(), UploadRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: reservation.ObjectID, RelayVersion: Epoch,
			SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
		},
		ContentLength: 4, HasContentLength: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := service.Upload(context.Background(), target, bytes.NewReader([]byte("body"))); err != nil {
		t.Fatal(err)
	}
	if _, err := service.Finish(context.Background(), FinishRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: reservation.ObjectID, RelayVersion: Epoch,
			SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
		},
		CiphertextBytes: 4, SHA256: object.Digest,
	}); err != nil {
		t.Fatal(err)
	}
	fetched, err := service.Fetch(context.Background(), ObjectRequest{
		ObjectID: reservation.ObjectID, RelayVersion: Epoch,
		SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
	})
	if err != nil {
		t.Fatal(err)
	}
	_ = fetched.Body.Close()
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}

	events := relayEvents(t, path)
	var stages []string
	var objectToken, requestToken any
	for _, event := range events {
		data := event["data"].(map[string]any)
		stages = append(stages, data["stage"].(string))
		if data["route"] != "reference" {
			t.Fatalf("route = %v", data["route"])
		}
		if objectToken == nil {
			objectToken = data["object_op"]
			requestToken = data["request"]
		} else if data["object_op"] != objectToken || data["request"] != requestToken {
			t.Fatal("object or parent token changed across the lifecycle")
		}
		if data["bytes"] != nil {
			if data["bytes"].(json.Number).String() != "4" {
				t.Fatalf("stage bytes = %v", data["bytes"])
			}
		}
	}
	want := []string{"stage", "upload", "finish", "verify", "fetch"}
	if len(stages) != len(want) {
		t.Fatalf("relay stages = %v want %v", stages, want)
	}
	for index := range want {
		if stages[index] != want[index] {
			t.Fatalf("relay stage %d = %q want %q", index, stages[index], want[index])
		}
	}
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(contents), object.ObjectID) {
		t.Fatal("raw object id leaked into the outlet")
	}
}

// latency/AC-04.inline: an inline body creates no relay object, so no Access
// object interval exists. Only the reference route emits access_relay facts.
func TestInlineRouteHasNoRelayObjectFacts(t *testing.T) {
	sink, path := openRelaySink(t)
	_, _, _, payloads := newObservedRelay(t, sink)
	if len(payloads.reserved) != 0 {
		t.Fatal("inline route unexpectedly reserved an object")
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	if events := relayEvents(t, path); len(events) != 0 {
		t.Fatalf("inline route emitted object facts: %v", events)
	}
	if len(payloads.reserved) != 0 {
		t.Fatal("inline route reached the payload store")
	}
}

func TestNewRejectsEnabledConfigurationWithoutOwnerOrRetention(t *testing.T) {
	base := Dependencies{Auth: &fakeAuth{}, Store: &fakeStore{}, Payloads: &fakePayloads{}, Metrics: diagnostics.NewMetrics()}
	noOwner := base
	noOwner.Config = Config{Enabled: true, Retention: time.Minute}
	if _, err := New(noOwner); err == nil {
		t.Fatal("enabled directory without an owner was accepted")
	}
	noRetention := base
	noRetention.Config = Config{Enabled: true, NodeID: "node-1"}
	if _, err := New(noRetention); err == nil {
		t.Fatal("enabled directory without retention was accepted")
	}
}

// latency/AC-05.concurrent: a stale grant or malformed revision binding never
// authorizes object access.
func TestCheckGrantRejectsMissingNegativeAndMismatchedBindings(t *testing.T) {
	service, _, metadata, _ := newTestService(t, true)
	object := testObject()
	object.GrantGeneration = nil
	if err := service.checkGrant(context.Background(), object); !errors.Is(err, ErrForbidden) {
		t.Fatalf("missing generation: %v", err)
	}
	object = testObject()
	negative := int64(-1)
	object.GrantGeneration = &negative
	if err := service.checkGrant(context.Background(), object); !errors.Is(err, ErrForbidden) {
		t.Fatalf("negative generation: %v", err)
	}
	object = testObject()
	metadata.grant = testGrant()
	metadata.grant.Active = false
	if err := service.checkGrant(context.Background(), object); !errors.Is(err, ErrForbidden) {
		t.Fatalf("inactive grant: %v", err)
	}
	metadata.grant = testGrant()
	metadata.grant.EnrollmentGeneration = 1
	enrollment := int64(2)
	object.EnrollmentGeneration = &enrollment
	if err := service.checkGrant(context.Background(), object); !errors.Is(err, ErrForbidden) {
		t.Fatalf("enrollment mismatch: %v", err)
	}
	metadata.grantErr = errors.New("database unavailable")
	if err := service.checkGrant(context.Background(), testObject()); err == nil || errors.Is(err, ErrForbidden) {
		t.Fatalf("transport error was collapsed: %v", err)
	}
}

func TestStagerOwnerFallsBackToNodeID(t *testing.T) {
	stager := &Stager{config: StagerConfig{NodeID: "node-1"}}
	if stager.owner() != "node-1" {
		t.Fatal("owner fallback")
	}
	stager.config.OwnerID = "owner-1"
	if stager.owner() != "owner-1" {
		t.Fatal("explicit owner was not preferred")
	}
}

// latency/AC-04.corrupt: a referenced object whose declared digest does not
// match its reservation fails finish verification and never produces a
// successful verify stage.
func TestCorruptReferencedAttachmentFailsVerification(t *testing.T) {
	sink, path := openRelaySink(t)
	service, _, metadata, payloads := newObservedRelay(t, sink)
	metadata.grant = testGrant()
	object := testObject()
	object.Purpose = "PAYLOAD_PURPOSE_ATTACHMENT_PART"
	metadata.objects[object.ObjectID] = object
	payloads.object = object

	request := validReserveRequest()
	request.PartID = "part-1"
	request.Purpose = whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_ATTACHMENT_PART
	reservation, err := service.Reserve(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	object.ObjectID = reservation.ObjectID
	metadata.objects[object.ObjectID] = object
	payloads.object = object

	if _, err := service.Finish(context.Background(), FinishRequest{
		ObjectRequest: ObjectRequest{
			ObjectID: reservation.ObjectID, RelayVersion: Epoch,
			SignedAtUnixMs: testNow.UnixMilli(), Signature: []byte("signed"),
		},
		CiphertextBytes: 4, SHA256: digestOf([]byte("corrupt")),
	}); !errors.Is(err, ErrDigestMismatch) {
		t.Fatalf("corrupt finish = %v", err)
	}
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	var sawFinishError, sawVerify bool
	for _, event := range relayEvents(t, path) {
		data := event["data"].(map[string]any)
		if data["stage"] == "finish" && data["outcome"] == "error" {
			sawFinishError = true
		}
		if data["stage"] == "verify" {
			sawVerify = true
		}
	}
	if !sawFinishError {
		t.Fatal("corrupt finish was not recorded as an error")
	}
	if sawVerify {
		t.Fatal("corrupt finish produced a verification stage")
	}
}

// latency/AC-07.aggregate: relay stage disposition metrics remain available
// without a trace sink, so ordinary Prometheus aggregation is independent of
// bench observation mode.
func TestRelayStageMetricsRemainAvailableWithoutTraceSink(t *testing.T) {
	service, _, metadata, _ := newTestService(t, true)
	metadata.grant = testGrant()
	if _, err := service.Reserve(context.Background(), validReserveRequest()); err != nil {
		t.Fatal(err)
	}
	if got := testutil.ToFloat64(service.metrics.AccessStageTotal.WithLabelValues("relay_stage", "ok")); got != 1 {
		t.Fatalf("relay stage metric = %v", got)
	}
	if got := testutil.ToFloat64(service.metrics.AccessStageBytes.WithLabelValues("relay_stage")); got != 4 {
		t.Fatalf("relay stage bytes = %v", got)
	}
}
