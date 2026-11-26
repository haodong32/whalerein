package relay

import (
	"context"
	"encoding/json"
	"testing"

	"errors"

	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

var errTestFinish = errors.New("finish failed")

// latency/AC-04.referenced-attachment: the broker stager reports reservation,
// physical upload, finish and verified logical bytes for the reference route.
func TestStagerEmitsReferencedObjectLifecycle(t *testing.T) {
	sink, path := openRelaySink(t)
	service, _, payloads := stagerFixture(t)
	service.config.Observation = sink
	service.config.Metrics = diagnostics.NewMetrics()
	ciphertext := []byte("sealed command")
	reference, err := service.Stage(context.Background(), broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", RequestID: "r1", MessageID: "r1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, Ciphertext: ciphertext,
	})
	if err != nil {
		t.Fatal(err)
	}
	if reference == nil {
		t.Fatal("no reference")
	}
	// A failed finish is a verify error, not a successful verification.
	payloads.finishErr = errTestFinish
	_, _ = service.Stage(context.Background(), broker.StageRequest{
		HostID: "h1", DeviceID: "d1", ChannelID: "c1", RequestID: "r1", MessageID: "r1",
		Purpose: whalereinv1.PayloadPurpose_PAYLOAD_PURPOSE_REQUEST, Ciphertext: ciphertext,
	})
	if err := sink.Close(); err != nil {
		t.Fatal(err)
	}
	events := relayEvents(t, path)
	var stages []string
	for _, event := range events {
		data := event["data"].(map[string]any)
		stages = append(stages, data["stage"].(string))
		if data["route"] != "reference" {
			t.Fatalf("route = %v", data["route"])
		}
		if data["bytes"] != nil && data["bytes"].(json.Number).String() != "14" {
			t.Fatalf("stage bytes = %v", data["bytes"])
		}
	}
	want := []string{"stage", "upload", "finish", "verify"}
	if len(stages) != len(want)*2 {
		t.Fatalf("stager stages = %v", stages)
	}
	for run := 0; run < 2; run++ {
		for index := range want {
			if stages[run*len(want)+index] != want[index] {
				t.Fatalf("stage %d = %q want %q", run*len(want)+index, stages[run*len(want)+index], want[index])
			}
		}
	}

	var sawVerifyError bool
	for _, event := range relayEvents(t, path) {
		data := event["data"].(map[string]any)
		if data["stage"] == "verify" && data["outcome"] == "error" {
			sawVerifyError = true
		}
	}
	if !sawVerifyError {
		t.Fatal("failed finish did not produce a verify error")
	}
	_ = payloads
}
