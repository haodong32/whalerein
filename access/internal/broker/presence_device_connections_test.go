package broker

import (
	"testing"
	"time"
)

type deviceOfflineEdge struct {
	deviceID string
	source   string
}

func newDeviceOfflineRecorder(p *Presence) *[]deviceOfflineEdge {
	edges := &[]deviceOfflineEdge{}
	p.SetDeviceOfflineHandler(func(deviceID, source string, _ time.Time, _ ConnectionRef) {
		*edges = append(*edges, deviceOfflineEdge{deviceID: deviceID, source: source})
	})
	return edges
}

func TestDevicePresenceKeepsOnlineWhileASiblingConnectionRemains(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 0)
	edges := newDeviceOfflineRecorder(p)

	p.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 10, AtUnixMilli: 1000})
	p.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 11, AtUnixMilli: 2000})
	if len(p.devices["d1"].connections) != 2 {
		t.Fatalf("connections = %v", p.devices["d1"].connections)
	}

	// Disconnecting the older connection must not remove the newer live one.
	p.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 10, AtUnixMilli: 3000})
	if len(*edges) != 0 {
		t.Fatalf("sibling disconnect emitted offline: %+v", *edges)
	}
	entry := p.devices["d1"]
	if !entry.online || entry.connID != 11 || len(entry.connections) != 1 {
		t.Fatalf("presence after sibling disconnect = %+v", entry)
	}
	if _, tracked := entry.connections[10]; tracked {
		t.Fatal("disconnected connection remained tracked")
	}

	// The final connection is the only offline edge.
	p.HandleNotification(&Notification{Sequence: 4, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 11, AtUnixMilli: 4000})
	if len(*edges) != 1 || (*edges)[0].deviceID != "d1" || (*edges)[0].source != PresenceSourceBroker {
		t.Fatalf("final disconnect edges = %+v", *edges)
	}
	if p.devices["d1"].online || len(p.devices["d1"].connections) != 0 {
		t.Fatalf("final presence = %+v", p.devices["d1"])
	}
}

func TestDevicePresenceEmitsExactlyOneOfflineEdgeForTheLastConnection(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 0)
	edges := newDeviceOfflineRecorder(p)

	p.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 7, AtUnixMilli: 1000})
	p.HandleNotification(&Notification{Sequence: 2, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 7, AtUnixMilli: 2000})
	p.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 7, AtUnixMilli: 3000})
	if len(*edges) != 1 {
		t.Fatalf("offline edges = %+v", *edges)
	}
	if p.devices["d1"].online {
		t.Fatal("device still online after its last connection disconnected")
	}
}

func TestShortLivedSiblingConnectionDoesNotRemoveALongLivedWatch(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 0)
	edges := newDeviceOfflineRecorder(p)

	// AC-16: a foreground send opens a second short-lived device connection
	// while the watch connection stays live.
	p.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 20, AtUnixMilli: 1000})
	p.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 21, AtUnixMilli: 2000})
	p.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 21, AtUnixMilli: 3000})
	if len(*edges) != 0 {
		t.Fatalf("short-lived sibling emitted offline: %+v", *edges)
	}
	if !p.devices["d1"].online || p.devices["d1"].connID != 20 {
		t.Fatalf("watch connection = %+v", p.devices["d1"])
	}
}

func TestDeviceSnapshotAccumulatesEveryConnectedClient(t *testing.T) {
	p := newPresence()
	edges := newDeviceOfflineRecorder(p)
	p.ApplySnapshot([]SnapshotClient{
		{ClientID: "watch", Kind: "device", DeviceID: "d1", ConnID: 3, State: "connected", Connected: 1000},
		{ClientID: "send", Kind: "device", DeviceID: "d1", ConnID: 4, State: "connected", Connected: 2000},
	}, 10)
	entry := p.devices["d1"]
	if !entry.online || entry.connID != 4 || len(entry.connections) != 2 {
		t.Fatalf("snapshot entry = %+v", entry)
	}
	if _, ok := entry.connections[3]; !ok {
		t.Fatal("watch connection missing from snapshot")
	}
	if _, ok := entry.connections[4]; !ok {
		t.Fatal("send connection missing from snapshot")
	}

	p.HandleNotification(&Notification{Sequence: 11, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 3, AtUnixMilli: 3000})
	if len(*edges) != 0 || !p.devices["d1"].online {
		t.Fatalf("snapshot sibling disconnect = edges %+v entry %+v", *edges, p.devices["d1"])
	}
	p.HandleNotification(&Notification{Sequence: 12, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 4, AtUnixMilli: 4000})
	if len(*edges) != 1 {
		t.Fatalf("snapshot last disconnect edges = %+v", *edges)
	}
}

func TestDeviceWillFromASiblingCannotRemoveALiveConnection(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 0)
	edges := newDeviceOfflineRecorder(p)
	p.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 30, AtUnixMilli: 1000})
	p.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 31, AtUnixMilli: 2000})

	p.HandleDeviceWill(DeviceWillTopic("d1"), []byte(`{"event":"device_offline","device_id":"d1"}`))
	if len(*edges) != 0 || !p.devices["d1"].online {
		t.Fatalf("sibling will changed presence: edges %+v entry %+v", *edges, p.devices["d1"])
	}
	p.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 30, AtUnixMilli: 3000})
	if len(*edges) != 0 {
		t.Fatalf("sibling disconnect after will edges = %+v", *edges)
	}
	p.HandleNotification(&Notification{Sequence: 4, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 31, AtUnixMilli: 4000})
	if len(*edges) != 1 {
		t.Fatalf("last disconnect after will edges = %+v", *edges)
	}
}

func TestSupersededDeviceDisconnectKeepsTimestampAndSequenceFencing(t *testing.T) {
	p := newPresence()
	p.ApplySnapshot(nil, 0)
	edges := newDeviceOfflineRecorder(p)
	p.HandleNotification(&Notification{Sequence: 1, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 1, AtUnixMilli: 2000})
	p.HandleNotification(&Notification{Sequence: 2, Event: "connected", Kind: "device", DeviceID: "d1", ConnID: 2, AtUnixMilli: 3000})

	// A delayed disconnect for the superseded connection with an older source
	// timestamp must not remove either live connection.
	p.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 1, AtUnixMilli: 2500})
	if len(*edges) != 0 || len(p.devices["d1"].connections) != 2 {
		t.Fatalf("timestamp-fenced disconnect = edges %+v entry %+v", *edges, p.devices["d1"])
	}
	// It also cannot remove the newer connection when it is fenced only by
	// sequence: the same sequence is ignored.
	p.HandleNotification(&Notification{Sequence: 3, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 2, AtUnixMilli: 3500})
	if len(*edges) != 0 || !p.devices["d1"].online {
		t.Fatalf("sequence-fenced disconnect = edges %+v entry %+v", *edges, p.devices["d1"])
	}
	// Disconnecting the newer connection leaves the older live sibling online.
	p.HandleNotification(&Notification{Sequence: 4, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 2, AtUnixMilli: 4000})
	if len(*edges) != 0 || !p.devices["d1"].online {
		t.Fatalf("newer disconnect with a sibling = edges %+v entry %+v", *edges, p.devices["d1"])
	}
	// The last tracked connection produces the only offline edge.
	p.HandleNotification(&Notification{Sequence: 5, Event: "disconnected", Kind: "device", DeviceID: "d1", ConnID: 1, AtUnixMilli: 5000})
	if len(*edges) != 1 {
		t.Fatalf("last connection edges = %+v", *edges)
	}
}
