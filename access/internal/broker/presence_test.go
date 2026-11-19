package broker

import (
	"context"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
)

func TestPresenceIsUnavailableUntilBrokerStateIsEstablished(t *testing.T) {
	p := newPresence()
	if p.Available() {
		t.Fatal("new presence should be unavailable")
	}
	p.SetHostOnline("h1", true, time.UnixMilli(1000))
	if !p.Available() || !p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("live presence should be available once observed")
	}

	empty := newPresence()
	empty.ApplySnapshot(nil, 7)
	if !empty.Available() {
		t.Fatal("snapshot should establish availability")
	}
	if empty.lastSeq != 7 {
		t.Fatalf("snapshot boundary = %d", empty.lastSeq)
	}
	if empty.IsHostOnline(context.Background(), "h1") {
		t.Fatal("empty snapshot invented a host")
	}
	if hosts, devices := empty.ConnectedCounts(); hosts != 0 || devices != 0 {
		t.Fatalf("counts = %d/%d", hosts, devices)
	}
}

func TestHandleNotificationAppliesOrderedHostPresence(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 100)
	p.HandleNotification(&Notification{Sequence: 101, Event: "connected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: 2000})
	// A broker CONNECT establishes the connection, not the request
	// subscription. Until HostReady confirms readiness a publish could be
	// accepted by the broker with no subscriber.
	if p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("connected-but-unready host reported online")
	}
	p.MarkHostReady("h1", time.UnixMilli(2001))
	if !p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("ready host was not applied")
	}
	// Older sequences are ignored even when they arrive late.
	p.HandleNotification(&Notification{Sequence: 100, Event: "disconnected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: 3000})
	if !p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("stale sequence changed presence")
	}
	// A newer sequence with an older source timestamp is also ignored.
	p.HandleNotification(&Notification{Sequence: 102, Event: "disconnected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: 1000})
	if !p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("stale timestamp changed presence")
	}
	p.HandleNotification(&Notification{Sequence: 103, Event: "disconnected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: 4000})
	if p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("disconnected notification was not applied")
	}
	if p.lastSeq != 103 {
		t.Fatalf("last sequence = %d", p.lastSeq)
	}
}

func TestHostPresenceRequiresReadyConfirmationForANewConnection(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 10)
	p.HandleNotification(&Notification{Sequence: 11, Event: "connected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: 1000})
	p.MarkHostReady("h1", time.UnixMilli(1100))
	if !p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("ready host missing")
	}
	// Reconnecting assigns a new connection identity; the old readiness proof
	// cannot authorize the new connection before its own HostReady.
	p.HandleNotification(&Notification{Sequence: 12, Event: "connected", Kind: "host", HostID: "h1", ConnID: 2, AtUnixMilli: 2000})
	if p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("new connection inherited readiness")
	}
	p.ApplySnapshot([]SnapshotClient{{Kind: "host", HostID: "h1", ConnID: 2, State: "connected", Connected: 2000}}, 12)
	if p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("reconciliation made a known unready connection look ready")
	}
	p.MarkHostReady("h1", time.UnixMilli(2100))
	if !p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("new connection readiness was not applied")
	}
}

func TestHandleNotificationIgnoresSupersededDeviceConnection(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 1)
	emitted := []string{}
	p.SetDeviceOfflineHandler(func(deviceID, source string, _ time.Time) {
		emitted = append(emitted, deviceID+":"+source)
	})
	p.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 5, AtUnixMilli: 2000})
	p.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 4, AtUnixMilli: 3000})
	if len(emitted) != 0 {
		t.Fatalf("superseded disconnect emitted: %v", emitted)
	}
	if !p.devices["d1"].online {
		t.Fatal("superseded disconnect changed presence")
	}
	p.HandleNotification(&Notification{Sequence: 4, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 5, AtUnixMilli: 4000})
	if len(emitted) != 1 || emitted[0] != "d1:"+PresenceSourceBroker {
		t.Fatalf("emitted = %v", emitted)
	}
	// A repeated disconnect for the same connection is deduplicated.
	p.HandleNotification(&Notification{Sequence: 5, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 5, AtUnixMilli: 5000})
	if len(emitted) != 1 {
		t.Fatalf("duplicate emitted = %v", emitted)
	}
	if got := testutil.ToFloat64(p.metrics.PresenceDuplicatesIgnored); got != 1 {
		t.Fatalf("duplicate metric = %v", got)
	}
}

func TestApplySnapshotReplacesPresenceAtTheBoundary(t *testing.T) {
	p := newPresence()
	p.SetHostOnline("stale-host", true, time.UnixMilli(1000))
	p.SetDeviceOnline("stale-device", true, time.UnixMilli(1000))

	p.ApplySnapshot([]SnapshotClient{
		{ClientID: "whalerein-host-h1", Kind: "host", HostID: "h1", ConnID: 1, State: "connected", Connected: 10_000},
		{ClientID: "whalerein-host-h2", Kind: "host", HostID: "h2", ConnID: 2, State: "disconnected", Connected: 10_000},
		{ClientID: "whalerein-device-d1", Kind: "device", DeviceID: "d1", ConnID: 3, State: "connected", Connected: 10_000},
		{ClientID: "whalerein-device-d2", Kind: "device", DeviceID: "d2", ConnID: 4, State: "disconnected", Connected: 10_000},
		{ClientID: "whalerein-probe-x", Kind: "probe", State: "connected"},
	}, 50)

	if !p.Available() || p.lastSeq != 50 {
		t.Fatalf("available=%v lastSeq=%d", p.Available(), p.lastSeq)
	}
	if p.IsHostOnline(context.Background(), "stale-host") || p.IsHostOnline(context.Background(), "h2") {
		t.Fatal("offline snapshot entries or stale hosts reported online")
	}
	if !p.IsHostOnline(context.Background(), "h1") {
		t.Fatal("connected host missing from snapshot")
	}
	if hosts, devices := p.ConnectedCounts(); hosts != 1 || devices != 1 {
		t.Fatalf("counts = %d/%d", hosts, devices)
	}
	if _, ok := p.devices["stale-device"]; ok {
		t.Fatal("snapshot did not replace stale device state")
	}
	// A snapshot with an older boundary cannot rewind the ordered sequence.
	p.ApplySnapshot(nil, 10)
	if p.lastSeq != 50 {
		t.Fatalf("boundary rewound to %d", p.lastSeq)
	}
	p.HandleNotification(&Notification{Sequence: 49, Event: "connected", Kind: "host", HostID: "h3", AtUnixMilli: 20_000})
	if p.IsHostOnline(context.Background(), "h3") {
		t.Fatal("notification before the snapshot boundary was applied")
	}
	p.HandleNotification(&Notification{Sequence: 51, Event: "connected", Kind: "host", HostID: "h3", AtUnixMilli: 20_000})
	if p.IsHostOnline(context.Background(), "h3") {
		t.Fatal("connected host reported online before HostReady")
	}
	p.MarkHostReady("h3", time.UnixMilli(20_001))
	if !p.IsHostOnline(context.Background(), "h3") {
		t.Fatal("notification after the snapshot boundary was ignored")
	}
}

func TestHandleNotificationIgnoresMalformedEntries(t *testing.T) {
	p := newPresence()
	p.HandleNotification(nil)
	p.HandleNotification(&Notification{Sequence: 0, Event: "connected", Kind: "host", HostID: "h1"})
	if p.Available() || p.lastSeq != 0 {
		t.Fatal("invalid notification changed presence")
	}
	p.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host"})
	p.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device"})
	p.HandleNotification(&Notification{Sequence: 3, Event: "connected", Kind: "probe", HostID: "h1"})
	if hosts, devices := p.ConnectedCounts(); hosts != 0 || devices != 0 {
		t.Fatalf("counts = %d/%d", hosts, devices)
	}
}
