package events

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

type admissionState struct {
	allowed bool
	known   bool
}

type fakeAdmission struct {
	grants map[string]admissionState
}

func (f *fakeAdmission) Check(hostID, deviceID string) (bool, bool) {
	state, ok := f.grants[hostID+"|"+deviceID]
	if !ok {
		return false, false
	}
	return state.allowed, state.known
}

type fakeBroker struct {
	hostID    string
	deviceID  string
	payload   []byte
	err       error
	calls     int
	liveCalls int
	liveErr   error
}

func (f *fakeBroker) PublishDeviceEvent(_ context.Context, hostID, deviceID string, payload []byte) error {
	f.calls++
	f.hostID = hostID
	f.deviceID = deviceID
	f.payload = append([]byte(nil), payload...)
	return f.err
}

func (f *fakeBroker) PublishDeviceLive(_ context.Context, hostID, deviceID string, payload []byte) error {
	f.liveCalls++
	f.hostID = hostID
	f.deviceID = deviceID
	f.payload = append([]byte(nil), payload...)
	return f.liveErr
}

func envelope(t *testing.T, hostID, deviceID string) []byte {
	t.Helper()
	data, err := proto.Marshal(&whalereinv1.EventEnvelope{HostId: hostID, DeviceId: deviceID, Ciphertext: []byte("ciphertext")})
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func newHandler(t *testing.T) (*Handler, *fakeAdmission, *fakeBroker) {
	t.Helper()
	admission := &fakeAdmission{grants: map[string]admissionState{"h1|d1": {allowed: true, known: true}}}
	broker := &fakeBroker{}
	handler, err := New(Dependencies{
		Admission: admission, Broker: broker, Metrics: diagnostics.NewMetrics(),
		Masker: diagnostics.NewMaskerWithKey([]byte("event")),
		Logger: slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	return handler, admission, broker
}

func TestNewRejectsMissingRequiredDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected error")
	}
	if _, err := New(Dependencies{Admission: &fakeAdmission{}}); err == nil {
		t.Fatal("expected broker error")
	}
}

func TestHandleRoutesValidEventToAuthorizedDevice(t *testing.T) {
	handler, _, broker := newHandler(t)
	payload := envelope(t, "h1", "d1")
	if err := handler.Handle(context.Background(), "h1", payload); err != nil {
		t.Fatal(err)
	}
	if broker.calls != 1 || broker.hostID != "h1" || broker.deviceID != "d1" || string(broker.payload) != string(payload) {
		t.Fatalf("broker = %+v", broker)
	}
	// Empty envelope host id is filled from the topic.
	data, _ := proto.Marshal(&whalereinv1.EventEnvelope{DeviceId: "d1"})
	if err := handler.Handle(context.Background(), "h1", data); err != nil {
		t.Fatal(err)
	}
}

func TestHandleDropsColdUnknownAndWarmedInactiveGrantsWithoutError(t *testing.T) {
	handler, admission, broker := newHandler(t)
	// A cold lookup is unknown: fail closed without surfacing an error.
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h1", "d-cold")); err != nil {
		t.Fatalf("cold grant returned error: %v", err)
	}
	if broker.calls != 0 {
		t.Fatal("cold grant event was published")
	}
	// A warmed grant that is inactive must also be dropped.
	admission.grants["h1|d1"] = admissionState{allowed: false, known: true}
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h1", "d1")); err != nil {
		t.Fatalf("inactive grant returned error: %v", err)
	}
	if broker.calls != 0 {
		t.Fatal("inactive grant event was published")
	}
	// Allowed grants forward the exact bytes.
	admission.grants["h1|d1"] = admissionState{allowed: true, known: true}
	payload := envelope(t, "h1", "d1")
	if err := handler.Handle(context.Background(), "h1", payload); err != nil {
		t.Fatal(err)
	}
	if broker.calls != 1 || string(broker.payload) != string(payload) {
		t.Fatalf("broker = %+v", broker)
	}
}

func TestHandleRejectsMalformedEventEnvelopes(t *testing.T) {
	handler, _, broker := newHandler(t)
	if err := handler.Handle(context.Background(), "", envelope(t, "h1", "d1")); err == nil {
		t.Fatal("empty topic host")
	}
	if err := handler.Handle(context.Background(), "h1", []byte("bad")); err == nil {
		t.Fatal("bad protobuf")
	}
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h2", "d1")); err == nil {
		t.Fatal("host mismatch")
	}
	if err := handler.Handle(context.Background(), "h1", envelope(t, "", "d1")); err != nil {
		t.Fatalf("empty envelope host should be filled: %v", err)
	}
	data, _ := proto.Marshal(&whalereinv1.EventEnvelope{HostId: "h1"})
	if err := handler.Handle(context.Background(), "h1", data); err == nil {
		t.Fatal("missing device id")
	}
	broker.err = errors.New("mqtt down")
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h1", "d1")); err == nil {
		t.Fatal("publish error")
	}
}

func TestHandleLiveUsesTheSameGrantCheckAndOnlyTheLivePublisher(t *testing.T) {
	handler, admission, broker := newHandler(t)
	payload := envelope(t, "h1", "d1")
	if err := handler.HandleLive(context.Background(), "h1", payload); err != nil {
		t.Fatal(err)
	}
	if broker.liveCalls != 1 || broker.calls != 0 || broker.hostID != "h1" || broker.deviceID != "d1" ||
		string(broker.payload) != string(payload) {
		t.Fatalf("broker = %+v", broker)
	}
	admission.grants["h1|d1"] = admissionState{allowed: false, known: true}
	if err := handler.HandleLive(context.Background(), "h1", envelope(t, "h1", "d1")); err != nil {
		t.Fatalf("denied live preview returned an error: %v", err)
	}
	if broker.liveCalls != 1 || broker.calls != 0 {
		t.Fatal("denied live preview was published")
	}
	admission.grants["h1|d1"] = admissionState{allowed: true, known: true}
	if err := handler.HandleLive(context.Background(), "h1", []byte("bad")); err == nil {
		t.Fatal("malformed live envelope accepted")
	}
	if err := handler.HandleLive(context.Background(), "h1", envelope(t, "h2", "d1")); err == nil {
		t.Fatal("live host mismatch accepted")
	}
	broker.liveErr = errors.New("mqtt down")
	if err := handler.HandleLive(context.Background(), "h1", payload); err == nil {
		t.Fatal("live publish error ignored")
	}
}
