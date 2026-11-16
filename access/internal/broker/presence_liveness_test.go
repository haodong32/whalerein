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

func TestDevicePresenceOfflineCallbackFiresOncePerTransitionAndResets(t *testing.T) {
	presence := newPresence()
	var transitions []presenceTransition
	presence.SetDeviceOfflineHandler(func(deviceID, source string, at time.Time) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})
	base := time.Unix(1000, 0)

	presence.SetDeviceOnline("d1", true, base)
	if len(transitions) != 0 {
		t.Fatalf("online event emitted offline: %+v", transitions)
	}
	presence.SetDeviceOnline("d1", false, base.Add(time.Second))
	if len(transitions) != 1 || transitions[0].deviceID != "d1" || transitions[0].source != PresenceSourceBroker {
		t.Fatalf("transitions = %+v", transitions)
	}

	// A repeated offline event is deduplicated, even with a newer timestamp.
	presence.SetDeviceOnline("d1", false, base.Add(2*time.Second))
	if len(transitions) != 1 {
		t.Fatalf("duplicate offline emitted: %+v", transitions)
	}
	if got := testutil.ToFloat64(presence.metrics.PresenceDuplicatesIgnored); got != 1 {
		t.Fatalf("duplicate metric = %v", got)
	}

	// Reconnecting resets presence so a later disconnect emits again.
	presence.SetDeviceOnline("d1", true, base.Add(3*time.Second))
	presence.SetDeviceOnline("d1", false, base.Add(4*time.Second))
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

func TestDeviceWillUnknownEntryFiresAndInvalidPayloadsAreDropped(t *testing.T) {
	presence := newPresence()
	presence.now = func() time.Time { return time.Unix(2000, 0) }
	var transitions []presenceTransition
	presence.SetDeviceOfflineHandler(func(deviceID, source string, at time.Time) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})

	valid := []byte(`{"event":"device_offline","device_id":"d9"}`)
	presence.HandleDeviceWill(DeviceWillTopic("d9"), valid)
	if len(transitions) != 1 || transitions[0].deviceID != "d9" || transitions[0].source != PresenceSourceWill {
		t.Fatalf("unknown-device will = %+v", transitions)
	}
	presence.HandleDeviceWill(DeviceWillTopic("d9"), valid)
	if len(transitions) != 1 {
		t.Fatalf("duplicate will emitted: %+v", transitions)
	}
	if got := testutil.ToFloat64(presence.metrics.PresenceDuplicatesIgnored); got != 1 {
		t.Fatalf("duplicate metric = %v", got)
	}

	for _, tc := range []struct {
		name    string
		topic   string
		payload []byte
	}{
		{"filter topic", DeviceWillFilter, valid},
		{"malformed payload", DeviceWillTopic("d10"), []byte(`{`)},
		{"wrong event", DeviceWillTopic("d10"), []byte(`{"event":"device_online","device_id":"d10"}`)},
		{"identity mismatch", DeviceWillTopic("d10"), []byte(`{"event":"device_offline","device_id":"d11"}`)},
	} {
		presence.HandleDeviceWill(tc.topic, tc.payload)
		if len(transitions) != 1 {
			t.Fatalf("%s emitted: %+v", tc.name, transitions)
		}
	}
	if got := testutil.ToFloat64(presence.metrics.PresenceInvalidPayloads); got != 4 {
		t.Fatalf("invalid metric = %v", got)
	}
}

func TestDeviceWillCannotOverrideANewerConnection(t *testing.T) {
	presence := newPresence()
	base := time.Unix(4000, 0)
	presence.now = func() time.Time { return base }
	var transitions []presenceTransition
	presence.SetDeviceOfflineHandler(func(deviceID, source string, at time.Time) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})

	presence.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 1, AtUnixMilli: 1000})
	// A duplicate client id takes over the device identity; the older
	// connection's delayed Will arrives afterwards.
	presence.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 2, AtUnixMilli: 2000})
	presence.HandleDeviceWill(DeviceWillTopic("d1"), []byte(`{"event":"device_offline","device_id":"d1"}`))
	if len(transitions) != 0 {
		t.Fatalf("stale will marked the newer connection offline: %+v", transitions)
	}
	if !presence.devices["d1"].online || presence.devices["d1"].connID != 2 {
		t.Fatalf("newer connection state = %+v", presence.devices["d1"])
	}
	// The real disconnect for the current connection still transitions.
	presence.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 2, AtUnixMilli: 3000})
	if len(transitions) != 1 || transitions[0].source != PresenceSourceBroker {
		t.Fatalf("current disconnect = %+v", transitions)
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
	presence.SetDeviceOfflineHandler(func(deviceID, source string, at time.Time) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})
	presence.SetDeviceOnline("d1", true, base)
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
	presence.SetDeviceOnline("d1", true, base)
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
	presence.SetDeviceOfflineHandler(func(deviceID, source string, at time.Time) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})
	presence.SetDeviceOnline("lost", true, base)
	presence.SetDeviceOnline("live", true, base)

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

func TestBrokerDisconnectIsPrimaryAndDeduplicatesTheFollowingWill(t *testing.T) {
	presence := newPresence()
	presence.now = func() time.Time { return time.Unix(3000, 0) }
	var transitions []presenceTransition
	presence.SetDeviceOfflineHandler(func(deviceID, source string, at time.Time) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})

	presence.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 1, AtUnixMilli: 1000})
	presence.HandleNotification(&Notification{Sequence: 2, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 1, AtUnixMilli: 2000})
	if len(transitions) != 1 || transitions[0].source != PresenceSourceBroker {
		t.Fatalf("broker transition = %+v", transitions)
	}
	presence.HandleDeviceWill(DeviceWillTopic("d1"), []byte(`{"event":"device_offline","device_id":"d1"}`))
	if len(transitions) != 1 {
		t.Fatalf("will duplicated broker transition: %+v", transitions)
	}
	if got := testutil.ToFloat64(presence.metrics.DevicePresenceEvents.WithLabelValues(PresenceSourceBroker, "offline")); got != 1 {
		t.Fatalf("broker offline metric = %v", got)
	}
	if got := testutil.ToFloat64(presence.metrics.DevicePresenceEvents.WithLabelValues(PresenceSourceWill, "offline")); got != 0 {
		t.Fatalf("will offline metric = %v", got)
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
