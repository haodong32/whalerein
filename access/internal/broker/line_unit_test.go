package broker

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

func TestPublishPresenceUsesHostScopedTopicAndQoS(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)

	if err := service.PublishPresence(context.Background(), "h1", []byte(`{"event":"device_offline"}`)); err != nil {
		t.Fatalf("publish presence: %v", err)
	}
	if len(session.published) != 1 || session.published[0] != PresenceTopic("h1") {
		t.Fatalf("published %v", session.published)
	}
	if session.qos[0] != 1 {
		t.Fatalf("presence qos = %d", session.qos[0])
	}
	if err := service.PublishPresence(context.Background(), "", []byte("x")); err == nil {
		t.Fatal("invalid host accepted")
	}
	if err := service.PublishPresence(context.Background(), "h1", nil); err == nil {
		t.Fatal("empty payload accepted")
	}
	service.setSession(nil, false)
	if err := service.PublishPresence(context.Background(), "h1", []byte("x")); err == nil {
		t.Fatal("publish succeeded without a session")
	}
}

func TestSupportsPairingRequiresCurrentSessionAndConfirmedPeer(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	if service.SupportsPairing("h1") {
		t.Fatal("pairing supported without a session")
	}
	service.setSession(session, true)
	if !service.SupportsPairing("h1") {
		t.Fatal("session without peer detail should report support")
	}
}

func TestDisconnectDeviceRejectsEmptyIDAndRequiresControlPath(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	if err := service.DisconnectDevice(context.Background(), ""); err == nil {
		t.Fatal("empty device accepted")
	}
	service.setSession(session, true)
	// Without a reply the bounded control request times out and reports a
	// failure instead of claiming a confirmed disconnect.
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if err := service.DisconnectDevice(ctx, "d1"); err == nil {
		t.Fatal("disconnect succeeded without a broker acknowledgement")
	}
}

func TestPublishControlRejectsUnavailableSession(t *testing.T) {
	service := testService(t, &fakeConnector{sessions: []*fakeSession{newFakeSession()}})
	if err := service.publishControl(context.Background(), ControlRequestTopic, []byte("x")); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("publish control error = %v", err)
	}
	session := newFakeSession()
	session.publishErr = errors.New("synthetic failure")
	service.setSession(session, true)
	if err := service.publishControl(context.Background(), ControlRequestTopic, []byte("x")); err == nil || !strings.Contains(err.Error(), "publish control") {
		t.Fatalf("publish control error = %v", err)
	}
}
