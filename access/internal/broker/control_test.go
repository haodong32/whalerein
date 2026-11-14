package broker

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

func controlRequestID(t *testing.T, payload []byte) string {
	t.Helper()
	for _, line := range strings.Split(string(payload), "\n") {
		if strings.HasPrefix(line, "id ") {
			return strings.TrimPrefix(line, "id ")
		}
	}
	t.Fatalf("control request has no id: %q", payload)
	return ""
}

func TestControlRequestPublishesAndMatchesTheReply(t *testing.T) {
	published := make(chan []byte, 1)
	control, err := NewControl(func(_ context.Context, topic string, payload []byte) error {
		if topic != ControlRequestTopic {
			t.Fatalf("topic = %q", topic)
		}
		published <- append([]byte(nil), payload...)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	type result struct {
		reply *ControlReply
		err   error
	}
	results := make(chan result, 1)
	go func() {
		reply, err := control.Request(context.Background(), "snapshot", []string{"ttl 1000"})
		results <- result{reply: reply, err: err}
	}()
	request := <-published
	id := controlRequestID(t, request)
	if !strings.Contains(string(request), "op snapshot\nttl 1000\n") {
		t.Fatalf("request payload = %q", request)
	}
	reply := "whalerein/1\nid " + id + "\nok\nseq 42\nrevision 9\n" +
		"client whalerein-host-h1 host h1 - 7 5 connected 8 1000\n" +
		"client whalerein-device-d1 device - d1 3 4 disconnected 9 2000\n"
	control.HandleMessage(ControlReplyFilter, []byte(reply))
	got := <-results
	if got.err != nil {
		t.Fatal(got.err)
	}
	if got.reply.Sequence != 42 || got.reply.Fields["revision"] != "9" {
		t.Fatalf("reply = %+v", got.reply)
	}
	if len(got.reply.Clients) != 2 {
		t.Fatalf("clients = %+v", got.reply.Clients)
	}
	host := got.reply.Clients[0]
	if host.ClientID != "whalerein-host-h1" || host.Kind != "host" || host.HostID != "h1" || host.DeviceID != "" ||
		host.ConnID != 7 || host.Revision != 5 || host.State != "connected" || host.Sequence != 8 || host.Connected != 1000 {
		t.Fatalf("host client = %+v", host)
	}
	device := got.reply.Clients[1]
	if device.HostID != "" || device.DeviceID != "d1" || device.State != "disconnected" {
		t.Fatalf("device client = %+v", device)
	}
}

func TestControlRequestReportsErrorRepliesPublishFailuresAndTimeouts(t *testing.T) {
	published := make(chan []byte, 1)
	control, err := NewControl(func(_ context.Context, _ string, payload []byte) error {
		published <- append([]byte(nil), payload...)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	control.timeout = 50 * time.Millisecond

	type result struct {
		reply *ControlReply
		err   error
	}
	results := make(chan result, 1)
	go func() {
		reply, err := control.Request(context.Background(), "deny", nil)
		results <- result{reply: reply, err: err}
	}()
	request := <-published
	id := controlRequestID(t, request)
	control.HandleMessage(ControlReplyFilter, []byte("whalerein/1\nid "+id+"\nerror not_found\n"))
	got := <-results
	if got.err == nil || got.reply == nil || got.reply.OK || got.reply.Error != "not_found" {
		t.Fatalf("reply = %+v err = %v", got.reply, got.err)
	}

	if _, err := control.Request(context.Background(), "ping", nil); err == nil || !strings.Contains(err.Error(), "timeout") {
		t.Fatalf("timeout = %v", err)
	}

	waitForDelivery := func() {
		t.Helper()
		select {
		case <-published:
		default:
		}
	}
	waitForDelivery()

	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := control.Request(cancelled, "ping", nil); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancel = %v", err)
	}
	waitForDelivery()

	failing, err := NewControl(func(context.Context, string, []byte) error { return errors.New("mqtt down") })
	if err != nil {
		t.Fatal(err)
	}
	if _, err := failing.Request(context.Background(), "ping", nil); err == nil || !strings.Contains(err.Error(), "mqtt down") {
		t.Fatalf("publish error = %v", err)
	}
	if _, err := NewControl(nil); err == nil {
		t.Fatal("nil publish accepted")
	}
}

func TestControlHandleMessageParsesNotificationsAndFiltersTopics(t *testing.T) {
	control, err := NewControl(func(context.Context, string, []byte) error { return nil })
	if err != nil {
		t.Fatal(err)
	}
	var got *Notification
	control.SetNotification(func(notification *Notification) { got = notification })
	if control.HandleMessage("other/topic", nil) {
		t.Fatal("unknown topic handled")
	}
	payload := []byte("whalerein/1\nseq 12\nevent connected\nclient whalerein-device-d1\nkind device\nhost -\ndevice d1\nconn 9\nat 1700000000000\nreason 3\n")
	if !control.HandleMessage(ControlEventFilter, payload) {
		t.Fatal("event topic not handled")
	}
	if got == nil || got.Sequence != 12 || got.Event != "connected" || got.ClientID != "whalerein-device-d1" ||
		got.Kind != "device" || got.HostID != "" || got.DeviceID != "d1" || got.ConnID != 9 ||
		got.AtUnixMilli != 1_700_000_000_000 || got.Reason != 3 {
		t.Fatalf("notification = %+v", got)
	}
	// Notifications without a sequence are dropped before reaching the sink.
	got = nil
	control.HandleMessage(ControlEventFilter, []byte("whalerein/1\nevent disconnected\nkind host\nhost h1\n"))
	if got != nil {
		t.Fatalf("sequenceless notification delivered: %+v", got)
	}
}
