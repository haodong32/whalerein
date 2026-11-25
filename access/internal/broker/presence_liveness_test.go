package broker

import (
	"context"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
)

type presenceTransition struct {
	deviceID string
	source   string
	at       time.Time
}

func observeDevice(p *Presence, device string, connected bool, id uint64, at time.Time) {
	event := "disconnected"
	if connected {
		event = "connected"
	}
	p.HandleNotification(&Notification{Sequence: p.lastSeq + 1, Epoch: p.BrokerEpoch(), Event: event, Kind: "device", DeviceID: device, ConnID: id, AtUnixMilli: at.UnixMilli()})
}

func TestDevicePresenceOfflineCallbackFiresOncePerTransitionAndResets(t *testing.T) {
	presence := newPresence()
	var transitions []presenceTransition
	presence.SetConnectionClosedHandler(func(deviceID, source string, at time.Time, _ ConnectionRef) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})
	base := time.Unix(1000, 0)

	observeDevice(presence, "d1", true, 1, base)
	if len(transitions) != 0 {
		t.Fatalf("online event emitted offline: %+v", transitions)
	}
	observeDevice(presence, "d1", false, 1, base.Add(time.Second))
	if len(transitions) != 1 || transitions[0].deviceID != "d1" || transitions[0].source != PresenceSourceBroker {
		t.Fatalf("transitions = %+v", transitions)
	}

	// A repeated offline event is deduplicated, even with a newer timestamp.
	observeDevice(presence, "d1", false, 1, base.Add(2*time.Second))
	if len(transitions) != 1 {
		t.Fatalf("duplicate offline emitted: %+v", transitions)
	}
	if got := testutil.ToFloat64(presence.metrics.PresenceDuplicatesIgnored); got != 1 {
		t.Fatalf("duplicate metric = %v", got)
	}

	// Reconnecting resets presence so a later disconnect emits again.
	observeDevice(presence, "d1", true, 2, base.Add(3*time.Second))
	observeDevice(presence, "d1", false, 2, base.Add(4*time.Second))
	if len(transitions) != 2 {
		t.Fatalf("transitions after reconnect = %+v", transitions)
	}
	if got := testutil.ToFloat64(presence.metrics.DevicePresenceEvents.WithLabelValues(PresenceSourceBroker, "offline")); got != 2 {
		t.Fatalf("offline metric = %v", got)
	}
	if got := testutil.ToFloat64(presence.metrics.DevicePresenceEvents.WithLabelValues(PresenceSourceBroker, "online")); got != 2 {
		t.Fatalf("online metric = %v", got)
	}
}

func TestPresenceCacheTTLExpiresAHostWithALostDisconnect(t *testing.T) {
	presence := newPresence()
	base := time.Unix(5000, 0)
	presence.now = func() time.Time { return base }
	presence.ApplySnapshot(nil, 0)
	presence.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: 1000})
	presence.MarkHostReady("h1", time.UnixMilli(1100))
	if !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("host should be online")
	}

	base = base.Add(40 * time.Millisecond)
	if !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("host expired before the cache TTL")
	}
	base = base.Add(20 * time.Millisecond)
	if presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("host outlived the cache TTL")
	}
	if hosts, _ := presence.ConnectedCounts(); hosts != 0 {
		t.Fatalf("expired host counted online: %d", hosts)
	}
	if expired := presence.Sweep(); expired != 1 {
		t.Fatalf("sweep expired %d entries", expired)
	}
	if presence.hosts["h1"].online {
		t.Fatal("sweep left the host online")
	}
}

func TestPresenceCacheTTLExpiresADeviceAndFansOutOffline(t *testing.T) {
	presence := newPresence()
	base := time.Unix(6000, 0)
	presence.now = func() time.Time { return base }
	var transitions []presenceTransition
	presence.SetConnectionClosedHandler(func(deviceID, source string, at time.Time, _ ConnectionRef) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})
	observeDevice(presence, "d1", true, 1, base)
	base = base.Add(51 * time.Millisecond) // one millisecond past the 50 ms test TTL
	if expired := presence.Sweep(); expired != 1 {
		t.Fatalf("sweep expired %d entries", expired)
	}
	if len(transitions) != 1 || transitions[0].deviceID != "d1" || transitions[0].source != PresenceSourceExpired {
		t.Fatalf("expiry transition = %+v", transitions)
	}
	// A repeated sweep is idempotent.
	if expired := presence.Sweep(); expired != 0 || len(transitions) != 1 {
		t.Fatalf("repeated sweep = %d/%+v", expired, transitions)
	}
	// A reconnect refreshes the observation and becomes expirable again.
	observeDevice(presence, "d1", true, 1, base)
	base = base.Add(51 * time.Millisecond)
	presence.Sweep()
	if len(transitions) != 2 {
		t.Fatalf("reconnect was not expired: %+v", transitions)
	}
}

func TestSnapshotRepairsALostDeviceDisconnect(t *testing.T) {
	presence := newPresence()
	base := time.Unix(7000, 0)
	presence.now = func() time.Time { return base }
	var transitions []presenceTransition
	presence.SetConnectionClosedHandler(func(deviceID, source string, at time.Time, _ ConnectionRef) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})
	observeDevice(presence, "lost", true, 1, base)
	observeDevice(presence, "live", true, 2, base)

	presence.ApplySnapshot([]SnapshotClient{
		{ClientID: "whalerein-device-live", Kind: "device", DeviceID: "live", State: "connected", ConnID: 2, Connected: 1000},
	}, 20)
	if len(transitions) != 1 || transitions[0].deviceID != "lost" || transitions[0].source != PresenceSourceSnapshot {
		t.Fatalf("snapshot repair = %+v", transitions)
	}
	if !presence.devices["live"].online {
		t.Fatal("live device was lost")
	}
	// An unchanged snapshot must not repeat the offline edge.
	presence.ApplySnapshot([]SnapshotClient{
		{ClientID: "whalerein-device-live", Kind: "device", DeviceID: "live", State: "connected", ConnID: 2, Connected: 1000},
	}, 21)
	if len(transitions) != 1 {
		t.Fatalf("snapshot duplicated the offline edge: %+v", transitions)
	}
}

func TestSnapshotEstablishesHostReadinessAfterAccessRestart(t *testing.T) {
	presence := newPresence()
	presence.ApplySnapshot([]SnapshotClient{
		{ClientID: "whalerein-host-h1", Kind: "host", HostID: "h1", State: "connected", ConnID: 4, Connected: 1000},
	}, 30)
	if !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("snapshot did not establish host readiness")
	}
}

func TestLiveNotificationsCannotEstablishPresenceBeforeASnapshot(t *testing.T) {
	presence := newPresence()
	presence.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "host", HostID: "h1", ConnID: 1, AtUnixMilli: 1000})
	presence.MarkHostReady("h1", time.UnixMilli(1100))
	if presence.Available() || presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("live notifications established presence before the startup snapshot")
	}
	presence.ApplySnapshot([]SnapshotClient{
		{ClientID: "whalerein-host-h1", Kind: "host", HostID: "h1", ConnID: 1, State: "connected", Connected: 1000},
	}, 2)
	if !presence.Available() || !presence.IsHostOnline(context.Background(), "h1") {
		t.Fatal("snapshot did not establish presence")
	}
}

// Sequence and connection numbers may both repeat in the new broker process.
func TestNewBrokerEpochRestartsPresenceSequenceAndClosesOldScopes(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot([]SnapshotClient{{Kind: "host", HostID: "host", State: "connected", ConnID: 8}}, 500)
	p.SetBrokerEpoch("2222222222222222")
	p.ApplySnapshot(nil, 1)
	if p.IsHostOnline(context.Background(), "host") {
		t.Fatal("old Host survived epoch change")
	}
	p.HandleNotification(&Notification{Epoch: p.BrokerEpoch(), Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d", ConnID: 8})
	if !p.DeviceConnectionActive("d", ConnectionRef{Epoch: p.BrokerEpoch(), ID: 8}) {
		t.Fatal("new broker sequence was rejected")
	}
}

func TestSnapshotClosesOnlyMissingDeviceAndSessionConnections(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot([]SnapshotClient{
		{Kind: "device", DeviceID: "d", ConnID: 1, State: "connected"},
		{Kind: "session", DeviceID: "d", SessionID: "managed-login", ConnID: 2, State: "connected"},
	}, 10)
	if session, active := p.DeviceConnectionSession("d", ConnectionRef{Epoch: p.BrokerEpoch(), ID: 2}); !active || session != "managed-login" {
		t.Fatal("snapshot lost the managed login scope")
	}
	var closed []ConnectionRef
	p.SetConnectionClosedHandler(func(_ string, _ string, _ time.Time, ref ConnectionRef) { closed = append(closed, ref) })
	p.HandleNotification(&Notification{Epoch: p.BrokerEpoch(), Sequence: 12, Event: "disconnected", Kind: "session", DeviceID: "d", ConnID: 2})
	p.ApplySnapshot([]SnapshotClient{
		{Kind: "session", DeviceID: "d", SessionID: "managed-login", ConnID: 2, State: "connected"},
	}, 11)
	if len(closed) != 2 || closed[0].ID != 2 || closed[1].ID != 1 || len(p.DeviceConnections("d")) != 0 {
		t.Fatal("snapshot lost a closure or revived a disconnected session")
	}
}
