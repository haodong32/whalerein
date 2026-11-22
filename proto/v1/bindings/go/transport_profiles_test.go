package whalereinv1

import (
	"encoding/json"
	"os"
	"testing"
)

type profileSource struct {
	Schema                 string `json:"schema"`
	DefaultProfile         string `json:"defaultProfile"`
	BrokerPacketLimitBytes uint64 `json:"brokerPacketLimitBytes"`
	Profiles               map[string]struct {
		WireName            string `json:"wireName"`
		PacketBytes         uint32 `json:"packetBytes"`
		PacketOverheadBytes uint32 `json:"packetOverheadBytes"`
		EnvelopeBytes       uint64 `json:"envelopeBytes"`
		BulkConcurrency     uint32 `json:"bulkConcurrency"`
		ReservedBytes       uint64 `json:"reservedBytes"`
		TransferTimeoutMs   uint32 `json:"transferTimeoutMs"`
	} `json:"profiles"`
}

func TestGeneratedTransportProfilesMatchSource(t *testing.T) {
	raw, err := os.ReadFile("../../transport_profiles.json")
	if err != nil {
		t.Fatal(err)
	}
	var source profileSource
	if err := json.Unmarshal(raw, &source); err != nil {
		t.Fatal(err)
	}
	if source.Schema != TransportProfilesSchema {
		t.Fatalf("schema %q != %q", source.Schema, TransportProfilesSchema)
	}
	if source.DefaultProfile != DefaultTransportProfile {
		t.Fatalf("default %q != %q", source.DefaultProfile, DefaultTransportProfile)
	}
	if source.BrokerPacketLimitBytes != BrokerPacketLimitBytes {
		t.Fatalf("broker limit %d != %d", source.BrokerPacketLimitBytes, BrokerPacketLimitBytes)
	}
	if len(source.Profiles) != len(TransportProfileNames()) {
		t.Fatalf("profile count %d != %d", len(source.Profiles), len(TransportProfileNames()))
	}
	for name, want := range source.Profiles {
		got, ok := TransportProfileValuesFor(name)
		if !ok {
			t.Fatalf("generated profile %s missing", name)
		}
		if got.WireName != want.WireName ||
			got.PacketBytes != want.PacketBytes ||
			got.PacketOverheadBytes != want.PacketOverheadBytes ||
			got.EnvelopeBytes != want.EnvelopeBytes ||
			got.BulkConcurrency != want.BulkConcurrency ||
			got.ReservedBytes != want.ReservedBytes ||
			got.TransferTimeoutMs != want.TransferTimeoutMs {
			t.Fatalf("generated profile %s drifted from transport_profiles.json", name)
		}
	}
}

func TestGeneratedTransportProfilesValidate(t *testing.T) {
	if err := ValidateTransportProfiles(); err != nil {
		t.Fatal(err)
	}
	defaults, ok := TransportProfileValuesFor(DefaultTransportProfile)
	if !ok {
		t.Fatal("default profile missing")
	}
	if defaults.PacketBytes != 65536 ||
		defaults.EnvelopeBytes != 16*1024*1024 {
		t.Fatal("default profile no longer retains 64 KiB / 16 MiB")
	}
	invalid := defaults
	invalid.PacketBytes = 0
	if err := ValidateTransportProfile(invalid); err == nil {
		t.Fatal("zero packet budget must fail")
	}
	invalid = defaults
	invalid.EnvelopeBytes = uint64(invalid.PacketBytes) - 1
	if err := ValidateTransportProfile(invalid); err == nil {
		t.Fatal("envelope below one packet must fail")
	}
	invalid = defaults
	invalid.ReservedBytes = invalid.EnvelopeBytes - 1
	if err := ValidateTransportProfile(invalid); err == nil {
		t.Fatal("reserved bytes below one envelope must fail")
	}
	invalid = defaults
	invalid.PacketOverheadBytes = invalid.PacketBytes
	if err := ValidateTransportProfile(invalid); err == nil {
		t.Fatal("packet overhead at the packet budget must fail")
	}
}
