package broker

import (
	"context"
	"testing"
	"time"
)

func TestOlderPresenceEventsCannotReplaceNewerConnections(t *testing.T) {
	p := NewPresence(time.Second, nil, nil, nil)
	p.now = func() time.Time { return time.UnixMilli(30000) }
	p.SetHostOnline("review-host", true, time.UnixMilli(30000))
	if err := p.HandlePresenceWebhook([]byte(`{"event":"client.disconnected","clientid":"whalerein-host-review-host","timestamp":10000}`)); err != nil {
		t.Fatal(err)
	}
	if !p.IsHostOnline(context.Background(), "review-host") {
		t.Fatal("older disconnect replaced a live connection")
	}
	p.SetDeviceOnline("device", true, time.UnixMilli(30000))
	p.SetDeviceOnline("device", false, time.UnixMilli(10000))
	if !p.devices["device"].online {
		t.Fatal("older device disconnect replaced new connection")
	}
}
