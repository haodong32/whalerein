package broker

import (
	"context"
	"strings"
	"testing"
	"time"
)

func TestRunInvokesReadinessHookAfterTheSessionIsEstablished(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	hookCalls := make(chan bool, 1)
	service.SetReadyHook(func(context.Context) error {
		hookCalls <- service.Ready()
		return nil
	})
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- service.Run(ctx) }()
	select {
	case ready := <-hookCalls:
		if !ready {
			t.Fatal("readiness hook ran before the session was ready")
		}
	case <-time.After(time.Second):
		cancel()
		t.Fatal("readiness hook not invoked")
	}
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("run did not stop")
	}
	if service.Control() == nil {
		t.Fatal("control accessor returned nil")
	}
}

func TestDisconnectDeviceSendsAControlKickByUsername(t *testing.T) {
	session := newFakeSession()
	service := testService(t, &fakeConnector{sessions: []*fakeSession{session}})
	service.setSession(session, true)
	session.onPublish = func(topic string, payload []byte) {
		if topic != ControlRequestTopic {
			t.Errorf("topic = %q", topic)
			return
		}
		id := controlRequestID(t, payload)
		if !strings.Contains(string(payload), "op kick\nusername device:d1\n") {
			t.Errorf("payload = %q", payload)
		}
		service.Control().HandleMessage(ControlReplyFilter, []byte("whalerein/1\nid "+id+"\nok\n"))
	}
	if err := service.DisconnectDevice(context.Background(), "d1"); err != nil {
		t.Fatal(err)
	}
	if err := service.DisconnectDevice(context.Background(), ""); err == nil {
		t.Fatal("empty device id accepted")
	}
}
