package broker

import (
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

func TestSysDisconnectIsPrimaryAndDeduplicatesTheFollowingWill(t *testing.T) {
	presence := newPresence()
	presence.now = func() time.Time { return time.Unix(3000, 0) }
	var transitions []presenceTransition
	presence.SetDeviceOfflineHandler(func(deviceID, source string, at time.Time) {
		transitions = append(transitions, presenceTransition{deviceID: deviceID, source: source, at: at})
	})

	presence.HandleSysMessage("$SYS/brokers/n/clients/whalerein-device-d1/connected", nil)
	presence.HandleSysMessage("$SYS/brokers/n/clients/whalerein-device-d1/disconnected", nil)
	if len(transitions) != 1 || transitions[0].source != PresenceSourceSys {
		t.Fatalf("sys transition = %+v", transitions)
	}
	presence.HandleDeviceWill(DeviceWillTopic("d1"), []byte(`{"event":"device_offline","device_id":"d1"}`))
	if len(transitions) != 1 {
		t.Fatalf("will duplicated sys transition: %+v", transitions)
	}
	if got := testutil.ToFloat64(presence.metrics.DevicePresenceEvents.WithLabelValues(PresenceSourceSys, "offline")); got != 1 {
		t.Fatalf("sys offline metric = %v", got)
	}
	if got := testutil.ToFloat64(presence.metrics.DevicePresenceEvents.WithLabelValues(PresenceSourceWill, "offline")); got != 0 {
		t.Fatalf("will offline metric = %v", got)
	}
}
