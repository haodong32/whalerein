package policy

import (
	"reflect"
	"testing"

	"github.com/whalerein/whalerein/access/internal/store"
)

func TestBuildHostPolicyListsOnlyItsOwnScopedTopics(t *testing.T) {
	connection := Build(KindHost, "h1", "", nil)
	if connection.Kind != KindHost || connection.HostID != "h1" || connection.DeviceID != "" || connection.Will != "" || connection.Revision != 0 {
		t.Fatalf("connection = %+v", connection)
	}
	want := []Entry{
		{Access: "read", Filter: "whalerein/v1/h/h1/presence"},
		{Access: "read", Filter: "whalerein/v1/h/h1/request"},
		{Access: "subscribe", Filter: "whalerein/v1/h/h1/presence"},
		{Access: "subscribe", Filter: "whalerein/v1/h/h1/request"},
		{Access: "write", Filter: "whalerein/v1/h/h1/event"},
		{Access: "write", Filter: "whalerein/v1/h/h1/result"},
	}
	if !reflect.DeepEqual(connection.Entries, want) {
		t.Fatalf("entries = %+v", connection.Entries)
	}
}

func TestBuildDevicePolicyUsesOnlyActiveGrantsAndSetsTheWill(t *testing.T) {
	connection := Build(KindDevice, "", "d1", []store.AdmissionGrant{
		{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 1},
		{HostID: "h2", DeviceID: "d1", Active: false, GrantGeneration: 2},
		{HostID: "", DeviceID: "d1", Active: true, GrantGeneration: 3},
		{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 4},
	})
	if connection.Kind != KindDevice || connection.DeviceID != "d1" || connection.Will != "whalerein/v1/presence/device/d1" || connection.Revision != 1 {
		t.Fatalf("connection = %+v", connection)
	}
	want := []Entry{
		{Access: "read", Revision: 1, HostID: "h1", Filter: "whalerein/v1/h/h1/d/d1/event"},
		{Access: "subscribe", Revision: 1, HostID: "h1", Filter: "whalerein/v1/h/h1/d/d1/event"},
		{Access: "write", Revision: 1, Filter: "whalerein/v1/presence/device/d1"},
		{Access: "write", Revision: 1, HostID: "h1", Filter: "whalerein/v1/h/h1/d/d1/ack"},
	}
	if !reflect.DeepEqual(connection.Entries, want) {
		t.Fatalf("entries = %+v", connection.Entries)
	}
}

func TestBuildDevicePolicyWithoutGrantsStillAllowsItsOwnWill(t *testing.T) {
	connection := Build(KindDevice, "", "d1", nil)
	if connection.Revision != 0 || len(connection.Entries) != 1 {
		t.Fatalf("connection = %+v", connection)
	}
	want := Entry{Access: "write", Filter: "whalerein/v1/presence/device/d1"}
	if connection.Entries[0] != want {
		t.Fatalf("entries = %+v", connection.Entries)
	}
}

func TestBuildAccessAndProbePoliciesHaveNoTopics(t *testing.T) {
	for _, kind := range []string{KindAccess, KindProbe} {
		connection := Build(kind, "h1", "d1", []store.AdmissionGrant{{HostID: "h1", DeviceID: "d1", Active: true}})
		if connection.Kind != kind || len(connection.Entries) != 0 || connection.Will != "" {
			t.Fatalf("%s connection = %+v", kind, connection)
		}
	}
}

func TestRevisionPacksGenerationComponentsMonotonically(t *testing.T) {
	cases := []struct {
		grant store.AdmissionGrant
		want  uint64
	}{
		{store.AdmissionGrant{GrantGeneration: 1}, 1},
		{store.AdmissionGrant{RevocationGeneration: 1}, 1 << 20},
		{store.AdmissionGrant{DeviceRevocationGeneration: 1}, 1 << 40},
		{store.AdmissionGrant{GrantGeneration: 5, RevocationGeneration: 2, DeviceRevocationGeneration: 1}, 1<<40 | 2<<20 | 5},
	}
	for _, tc := range cases {
		if got := Revision(tc.grant); got != tc.want {
			t.Fatalf("Revision(%+v) = %d, want %d", tc.grant, got, tc.want)
		}
	}
	if Revision(store.AdmissionGrant{DeviceRevocationGeneration: 1}) <= Revision(store.AdmissionGrant{RevocationGeneration: 1, GrantGeneration: 1 << 20}) {
		t.Fatal("device-wide revocation did not dominate host revocation")
	}
	if Revision(store.AdmissionGrant{RevocationGeneration: 1}) <= Revision(store.AdmissionGrant{GrantGeneration: 1}) {
		t.Fatal("host revocation did not dominate re-pairing")
	}
}

func TestFormatRendersTheVersionedHostAndDevicePolicy(t *testing.T) {
	cases := []struct {
		name       string
		connection Connection
		want       string
	}{
		{
			name:       "host",
			connection: Build(KindHost, "h1", "", nil),
			want: "whalerein/1\nok\nkind host\nhost h1\nrevision 0\nready 1\n" +
				"acl read 0 - whalerein/v1/h/h1/presence\n" +
				"acl read 0 - whalerein/v1/h/h1/request\n" +
				"acl subscribe 0 - whalerein/v1/h/h1/presence\n" +
				"acl subscribe 0 - whalerein/v1/h/h1/request\n" +
				"acl write 0 - whalerein/v1/h/h1/event\n" +
				"acl write 0 - whalerein/v1/h/h1/result\n",
		},
		{
			name: "device",
			connection: Build(KindDevice, "", "d1", []store.AdmissionGrant{
				{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 1},
			}),
			want: "whalerein/1\nok\nkind device\ndevice d1\nrevision 1\nready 1\nwill whalerein/v1/presence/device/d1\n" +
				"acl read 1 h1 whalerein/v1/h/h1/d/d1/event\n" +
				"acl subscribe 1 h1 whalerein/v1/h/h1/d/d1/event\n" +
				"acl write 1 - whalerein/v1/presence/device/d1\n" +
				"acl write 1 h1 whalerein/v1/h/h1/d/d1/ack\n",
		},
		{
			name:       "probe",
			connection: Build(KindProbe, "h1", "", nil),
			want:       "whalerein/1\nok\nkind probe\nhost h1\nrevision 0\nready 1\n",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := string(Format(tc.connection)); got != tc.want {
				t.Fatalf("Format() =\n%q\nwant\n%q", got, tc.want)
			}
		})
	}
}

func TestWillAllowedOnlyAcceptsTheDevicesOwnWill(t *testing.T) {
	device := Build(KindDevice, "", "d1", nil)
	if !device.WillAllowed("") {
		t.Fatal("empty will rejected")
	}
	if !device.WillAllowed("whalerein/v1/presence/device/d1") {
		t.Fatal("own will rejected")
	}
	for _, topic := range []string{"whalerein/v1/presence/device/d2", "whalerein/v1/presence/device/+", "other"} {
		if device.WillAllowed(topic) {
			t.Fatalf("foreign will %q accepted", topic)
		}
	}
	host := Build(KindHost, "h1", "", nil)
	if !host.WillAllowed("") || host.WillAllowed("whalerein/v1/presence/device/d1") {
		t.Fatal("host will policy")
	}
}

func TestControlMessagesRenderTheSetRequest(t *testing.T) {
	connection := Build(KindDevice, "", "d1", []store.AdmissionGrant{
		{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 2},
	})
	want := []string{
		"client whalerein-device-d1",
		"kind device",
		"device d1",
		"host ",
		"revision 2",
		"ttl 10000",
		"acl read 2 h1 whalerein/v1/h/h1/d/d1/event",
		"acl subscribe 2 h1 whalerein/v1/h/h1/d/d1/event",
		"acl write 2 - whalerein/v1/presence/device/d1",
		"acl write 2 h1 whalerein/v1/h/h1/d/d1/ack",
	}
	if got := ControlMessages(connection, "whalerein-device-d1", 10000); !reflect.DeepEqual(got, want) {
		t.Fatalf("ControlMessages() = %#v", got)
	}
}
