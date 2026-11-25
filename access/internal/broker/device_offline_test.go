package broker

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
)

func offlineService(t *testing.T, hosts func(context.Context, string) ([]string, error)) (*Service, *fakeSession) {
	t.Helper()
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.Presence().SetBrokerEpoch("0123456789abcdef")
	service.deviceHosts = hosts
	service.setSession(session, true)
	return service, session
}

func TestDeviceOfflinePublishesHostScopedJsonToEveryAuthorizedHost(t *testing.T) {
	service, session := offlineService(t, func(_ context.Context, deviceID string) ([]string, error) {
		if deviceID != "d1" {
			t.Fatalf("device = %q", deviceID)
		}
		return []string{"h1", "h2"}, nil
	})
	observed := time.Date(2026, 11, 13, 12, 0, 0, 0, time.UTC)
	service.handleDeviceOffline("d1", PresenceSourceBroker, observed, ConnectionRef{})

	session.mu.Lock()
	defer session.mu.Unlock()
	if len(session.published) != 2 {
		t.Fatalf("published = %v", session.published)
	}
	if session.published[0] != "whalerein/v1/h/h1/presence" || session.published[1] != "whalerein/v1/h/h2/presence" {
		t.Fatalf("topics = %v", session.published)
	}
	if session.qos[0] != 1 || session.qos[1] != 1 {
		t.Fatalf("qos = %v", session.qos)
	}
	var event struct {
		Event      string `json:"event"`
		DeviceID   string `json:"device_id"`
		Reason     string `json:"reason"`
		ObservedAt string `json:"observed_at"`
	}
	if err := json.Unmarshal(session.payloads[0], &event); err != nil {
		t.Fatal(err)
	}
	if event.Event != "connection_closed" || event.DeviceID != "d1" || event.Reason != PresenceSourceBroker {
		t.Fatalf("event = %+v", event)
	}
	if _, err := time.Parse(time.RFC3339Nano, event.ObservedAt); err != nil {
		t.Fatalf("observed_at = %q: %v", event.ObservedAt, err)
	}
	if string(session.payloads[0]) != string(session.payloads[1]) {
		t.Fatal("host payloads differ")
	}
}

func TestDeviceOfflineUnknownDeviceHasNoHostsAndPublishesNothing(t *testing.T) {
	service, session := offlineService(t, func(context.Context, string) ([]string, error) {
		return nil, nil
	})
	service.handleDeviceOffline("missing", PresenceSourceBroker, time.Now(), ConnectionRef{})
	session.mu.Lock()
	defer session.mu.Unlock()
	if len(session.published) != 0 {
		t.Fatalf("published = %v", session.published)
	}
}

func TestDeviceOfflineFailurePathsAreBoundedAndCounted(t *testing.T) {
	lookupErr, _ := offlineService(t, func(context.Context, string) ([]string, error) {
		return nil, errors.New("database unavailable")
	})
	lookupErr.handleDeviceOffline("d1", PresenceSourceBroker, time.Now(), ConnectionRef{})
	if got := testutil.ToFloat64(lookupErr.metrics.DeviceOfflinePublishFail); got != 1 {
		t.Fatalf("lookup failure metric = %v", got)
	}

	noResolver, noResolverSession := offlineService(t, nil)
	noResolver.handleDeviceOffline("d1", PresenceSourceBroker, time.Now(), ConnectionRef{})
	if got := testutil.ToFloat64(noResolver.metrics.DeviceOfflinePublishFail); got != 1 {
		t.Fatalf("missing resolver metric = %v", got)
	}
	noResolverSession.mu.Lock()
	if len(noResolverSession.published) != 0 {
		t.Fatalf("missing resolver published = %v", noResolverSession.published)
	}
	noResolverSession.mu.Unlock()

	invalidID, invalidSession := offlineService(t, nil)
	invalidID.handleDeviceOffline("bad/id", PresenceSourceBroker, time.Now(), ConnectionRef{})
	if got := testutil.ToFloat64(invalidID.metrics.PresenceInvalidPayloads); got != 1 {
		t.Fatalf("invalid device metric = %v", got)
	}
	invalidSession.mu.Lock()
	if len(invalidSession.published) != 0 {
		t.Fatalf("invalid device published = %v", invalidSession.published)
	}
	invalidSession.mu.Unlock()
}

func TestDeviceOfflinePublishFailureIsCountedAndSkipsInvalidHosts(t *testing.T) {
	service, session := offlineService(t, func(context.Context, string) ([]string, error) {
		return []string{"bad/host", "h1"}, nil
	})
	session.publishErr = errors.New("broker unavailable")
	service.handleDeviceOffline("d1", PresenceSourceBroker, time.Now(), ConnectionRef{})
	if got := testutil.ToFloat64(service.metrics.DeviceOfflinePublishFail); got != 2 {
		t.Fatalf("publish failure metric = %v", got)
	}
}

func TestRetiredDeviceWillCannotPublishHostLifecycle(t *testing.T) {
	service, session := offlineService(t, func(context.Context, string) ([]string, error) {
		return []string{"h1"}, nil
	})
	service.HandleMessage(Message{Topic: "whalerein/v1/presence/device/d1"})
	if len(session.published) != 0 {
		t.Fatal("retired topic entered lifecycle routing")
	}
}

func TestBrokerDisconnectNotificationPublishesHostScopedJsonOnce(t *testing.T) {
	service, session := offlineService(t, func(context.Context, string) ([]string, error) {
		return []string{"h1"}, nil
	})
	connected := []byte("whalerein/1\nseq 10\nevent connected\nclient whalerein-device-d1\nkind device\ndevice d1\nconn 7\nat 1000\n")
	disconnected := []byte("whalerein/1\nseq 11\nevent disconnected\nclient whalerein-device-d1\nkind device\ndevice d1\nconn 7\nat 2000\n")
	service.HandleMessage(Message{Topic: ControlEventFilter, Payload: connected})
	session.mu.Lock()
	if len(session.published) != 0 {
		t.Fatalf("connected published = %v", session.published)
	}
	session.mu.Unlock()

	service.HandleMessage(Message{Topic: ControlEventFilter, Payload: disconnected})
	session.mu.Lock()
	if len(session.published) != 1 || session.published[0] != "whalerein/v1/h/h1/presence" {
		t.Fatalf("published = %v", session.published)
	}
	var event deviceOfflineEvent
	if err := json.Unmarshal(session.payloads[0], &event); err != nil {
		t.Fatal(err)
	}
	session.mu.Unlock()
	if event.Event != "connection_closed" || event.DeviceID != "d1" || event.Reason != PresenceSourceBroker {
		t.Fatalf("event = %+v", event)
	}

	service.HandleMessage(Message{Topic: ControlEventFilter, Payload: []byte("whalerein/1\nseq 12\nevent disconnected\nclient whalerein-device-d1\nkind device\ndevice d1\nconn 7\nat 3000\n")})
	session.mu.Lock()
	defer session.mu.Unlock()
	if len(session.published) != 1 {
		t.Fatalf("duplicate disconnect published = %v", session.published)
	}
	if got := testutil.ToFloat64(service.Presence().metrics.PresenceDuplicatesIgnored); got != 1 {
		t.Fatalf("duplicate metric = %v", got)
	}
}
