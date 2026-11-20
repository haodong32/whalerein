package clientaddr

import (
	"encoding/base64"
	"net/netip"
	"testing"
	"time"
)

var testKey = []byte{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
	16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31}

func TestParseWindow(t *testing.T) {
	for value, want := range map[string]Window{"": WindowMonthly, "monthly": WindowMonthly,
		"MONTHLY": WindowMonthly, "daily": WindowDaily, " Daily ": WindowDaily} {
		got, err := ParseWindow(value)
		if err != nil || got != want {
			t.Fatalf("ParseWindow(%q) = %q, %v", value, got, err)
		}
	}
	if _, err := ParseWindow("hourly"); err == nil {
		t.Fatal("accepted an unsupported window")
	}
}

func TestDecodeKey(t *testing.T) {
	encoded := base64.StdEncoding.EncodeToString(testKey)
	if material, err := DecodeKey(encoded); err != nil || string(material) != string(testKey) {
		t.Fatalf("decode = %v, %v", material, err)
	}
	if material, err := DecodeKey(" " + encoded + " "); err != nil || string(material) != string(testKey) {
		t.Fatalf("padded decode = %v, %v", material, err)
	}
	raw := base64.RawStdEncoding.EncodeToString(testKey)
	if material, err := DecodeKey(raw); err != nil || string(material) != string(testKey) {
		t.Fatalf("raw decode = %v, %v", material, err)
	}
	for _, value := range []string{"", "not-base64!", base64.StdEncoding.EncodeToString(testKey[:31])} {
		if _, err := DecodeKey(value); err == nil {
			t.Fatalf("accepted key %q", value)
		}
	}
}

func TestReferencerWindowedPseudonym(t *testing.T) {
	referencer, err := NewReferencer(testKey, WindowMonthly)
	if err != nil {
		t.Fatal(err)
	}
	addr := netip.MustParseAddr("203.0.113.7")
	monthly := time.Date(2026, time.November, 20, 10, 0, 0, 0, time.UTC)
	ref, window := referencer.Reference(addr, monthly)
	if window != "2026-11" {
		t.Fatalf("window = %q", window)
	}
	if ref != "c1_6f5ff3961d33597f4676712b4f397144" {
		t.Fatalf("ref = %q", ref)
	}
	next, nextWindow := referencer.Reference(addr, time.Date(2026, time.December, 1, 0, 0, 1, 0, time.UTC))
	if next == ref || nextWindow != "2026-12" {
		t.Fatalf("next = %q window=%q", next, nextWindow)
	}
	other, _ := referencer.Reference(netip.MustParseAddr("203.0.113.8"), monthly)
	if other == ref {
		t.Fatal("different addresses share a pseudonym")
	}
}

func TestReferencerDailyWindow(t *testing.T) {
	referencer, err := NewReferencer(testKey, WindowDaily)
	if err != nil {
		t.Fatal(err)
	}
	ref, window := referencer.Reference(netip.MustParseAddr("203.0.113.7"),
		time.Date(2026, time.November, 20, 10, 0, 0, 0, time.UTC))
	if window != "2026-11-20" || ref != "c1_761ea4ea3f761b1587b13a8c54447f6c" {
		t.Fatalf("ref = %q window = %q", ref, window)
	}
	next, nextWindow := referencer.Reference(netip.MustParseAddr("203.0.113.7"),
		time.Date(2026, time.November, 21, 0, 0, 0, 0, time.UTC))
	if next == ref || nextWindow != "2026-11-21" {
		t.Fatalf("next = %q window = %q", next, nextWindow)
	}
}

func TestReferencerNormalisesMappedAndZonedAddresses(t *testing.T) {
	referencer, err := NewReferencer(testKey, WindowMonthly)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Date(2026, time.November, 20, 0, 0, 0, 0, time.UTC)
	base, _ := referencer.Reference(netip.MustParseAddr("203.0.113.7"), now)
	mapped, _ := referencer.Reference(netip.MustParseAddr("::ffff:203.0.113.7"), now)
	if base != mapped {
		t.Fatalf("mapped ref = %q, base = %q", mapped, base)
	}
	zoned, _ := referencer.Reference(netip.MustParseAddr("fe80::1%eth0"), now)
	plain, _ := referencer.Reference(netip.MustParseAddr("fe80::1"), now)
	if zoned != plain {
		t.Fatalf("zoned ref = %q, plain = %q", zoned, plain)
	}
}

func TestReferencerRejectsInvalidConfigurationAndInput(t *testing.T) {
	if _, err := NewReferencer(testKey[:31], WindowMonthly); err == nil {
		t.Fatal("accepted a short key")
	}
	if _, err := NewReferencer(testKey, "weekly"); err == nil {
		t.Fatal("accepted an unsupported window")
	}
	if _, err := NewReferencerFromConfig("", "monthly"); err != nil {
		t.Fatal(err)
	}
	if referencer, err := NewReferencerFromConfig("", "monthly"); err != nil || referencer != nil {
		t.Fatalf("empty key = %v, %v", referencer, err)
	}
	if _, err := NewReferencerFromConfig("not-base64!", "monthly"); err == nil {
		t.Fatal("accepted an invalid key")
	}
	if _, err := NewReferencerFromConfig(base64.StdEncoding.EncodeToString(testKey), "weekly"); err == nil {
		t.Fatal("accepted an invalid window")
	}
	var nilReferencer *Referencer
	if ref, window := nilReferencer.Reference(netip.MustParseAddr("203.0.113.7"), time.Now()); ref != "" || window != "" {
		t.Fatalf("nil referencer = %q %q", ref, window)
	}
	if window := nilReferencer.WindowID(time.Now()); window != "" {
		t.Fatalf("nil window = %q", window)
	}
	if ref, window := nilReferencer.Reference(netip.Addr{}, time.Now()); ref != "" || window != "" {
		t.Fatalf("invalid address = %q %q", ref, window)
	}
}
