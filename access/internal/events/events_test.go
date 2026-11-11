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

type fakeStore struct {
	active map[string]bool
	err    error
}

func (f *fakeStore) HasActiveGrant(_ context.Context, hostID, deviceID string) (bool, error) {
	if f.err != nil {
		return false, f.err
	}
	return f.active[hostID+"|"+deviceID], nil
}

type fakeBroker struct {
	hostID   string
	deviceID string
	payload  []byte
	err      error
	calls    int
}

func (f *fakeBroker) PublishDeviceEvent(_ context.Context, hostID, deviceID string, payload []byte) error {
	f.calls++
	f.hostID = hostID
	f.deviceID = deviceID
	f.payload = append([]byte(nil), payload...)
	return f.err
}

func envelope(t *testing.T, hostID, deviceID string) []byte {
	t.Helper()
	data, err := proto.Marshal(&whalereinv1.EventEnvelope{HostId: hostID, DeviceId: deviceID, Ciphertext: []byte("ciphertext")})
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func newHandler(t *testing.T) (*Handler, *fakeStore, *fakeBroker) {
	t.Helper()
	store := &fakeStore{active: map[string]bool{"h1|d1": true}}
	broker := &fakeBroker{}
	handler, err := New(Dependencies{
		Store: store, Broker: broker, Metrics: diagnostics.NewMetrics(),
		Masker: diagnostics.NewMaskerWithKey([]byte("event")),
		Logger: slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	return handler, store, broker
}

func TestNewRejectsMissingRequiredDependencies(t *testing.T) {
	if _, err := New(Dependencies{}); err == nil {
		t.Fatal("expected error")
	}
	if _, err := New(Dependencies{Store: &fakeStore{}}); err == nil {
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

func TestHandleDropsEventsForRevokedOrMissingGrants(t *testing.T) {
	handler, store, broker := newHandler(t)
	store.active["h1|d1"] = false
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h1", "d1")); err != nil {
		t.Fatal(err)
	}
	if broker.calls != 0 {
		t.Fatal("dropped event was published")
	}
	store.active["h1|d1"] = true
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h1", "d2")); err != nil {
		t.Fatal(err)
	}
	if broker.calls != 0 {
		t.Fatal("missing grant event was published")
	}
}

func TestHandleRejectsMalformedEventEnvelopes(t *testing.T) {
	handler, store, broker := newHandler(t)
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
	store.err = errors.New("db down")
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h1", "d1")); err == nil {
		t.Fatal("store error")
	}
	store.err = nil
	broker.err = errors.New("mqtt down")
	if err := handler.Handle(context.Background(), "h1", envelope(t, "h1", "d1")); err == nil {
		t.Fatal("publish error")
	}
}
