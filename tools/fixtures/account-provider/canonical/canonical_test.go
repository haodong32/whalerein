package canonical

import (
	"encoding/base64"
	"strings"
	"testing"
)

func keys(values ...string) [][]byte {
	result := make([][]byte, 0, len(values))
	for _, value := range values {
		result = append(result, []byte(value))
	}
	return result
}

func baseFields() Fields {
	return Fields{
		AuthorizationID:    "auth-1",
		Issuer:             "fixture://issuer/a",
		Audience:           "https://access.fixture.test",
		Purpose:            "host_enrollment",
		WorkspaceID:        "ws-fixture-1",
		RegionID:           "fixture-a",
		OperationID:        "op-1",
		EnrollmentRevision: "",
		Precondition:       "initial_enrollment",
		HostID:             "host-1",
		HostPubkeys:        keys("host-signing", "host-noise"),
		IssuedAtUnixMS:     1000,
		ExpiresAtUnixMS:    2000,
		Nonce:              []byte("0123456789abcdef"),
	}
}

func TestInputIsDeterministic(t *testing.T) {
	fields := baseFields()
	first := Input(fields)
	for attempt := 0; attempt < 5; attempt++ {
		if got := Input(fields); got != first {
			t.Fatalf("input changed between calls:\n%s\n%s", first, got)
		}
	}
	if first == "" {
		t.Fatal("input must not be empty")
	}
}

func TestInputBindsEveryField(t *testing.T) {
	base := Input(baseFields())
	for name, mutate := range map[string]func(*Fields){
		"purpose":             func(f *Fields) { f.Purpose = "device_pairing" },
		"workspace":           func(f *Fields) { f.WorkspaceID = "ws-fixture-2" },
		"region":              func(f *Fields) { f.RegionID = "fixture-b" },
		"audience":            func(f *Fields) { f.Audience = "https://other.fixture.test" },
		"issuer":              func(f *Fields) { f.Issuer = "fixture://issuer/unknown" },
		"authorization id":    func(f *Fields) { f.AuthorizationID = "auth-2" },
		"operation id":        func(f *Fields) { f.OperationID = "op-2" },
		"precondition":        func(f *Fields) { f.Precondition = "" },
		"expected revision":   func(f *Fields) { f.ExpectedRevision = "7" },
		"host id":             func(f *Fields) { f.HostID = "host-2" },
		"host keys":           func(f *Fields) { f.HostPubkeys = keys("host-signing", "host-noise-2") },
		"device id":           func(f *Fields) { f.DeviceID = "device-1" },
		"issued at":           func(f *Fields) { f.IssuedAtUnixMS = 1001 },
		"expires at":          func(f *Fields) { f.ExpiresAtUnixMS = 2001 },
		"nonce":               func(f *Fields) { f.Nonce = []byte("fedcba9876543210") },
		"enrollment revision": func(f *Fields) { f.EnrollmentRevision = "3" },
	} {
		t.Run(name, func(t *testing.T) {
			fields := baseFields()
			mutate(&fields)
			if Input(fields) == base {
				t.Fatalf("changing the %s must change the signing input", name)
			}
		})
	}
}

func TestKeyHashIgnoresOrdering(t *testing.T) {
	forward := HashKeys(keys("a", "b", "c"))
	reversed := HashKeys(keys("c", "b", "a"))
	if forward != reversed {
		t.Fatalf("key hashing must not depend on order: %s vs %s", forward, reversed)
	}
}

func TestKeyHashSeparatesConcatenations(t *testing.T) {
	// Without length prefixes, ["ab","c"] and ["a","bc"] would collide.
	if HashKeys(keys("ab", "c")) == HashKeys(keys("a", "bc")) {
		t.Fatal("length prefixes must separate different key sets")
	}
}

func TestEmptyKeySetHashesToTheEmptyDigest(t *testing.T) {
	empty := HashKeys(nil)
	if empty != HashKeys([][]byte{}) {
		t.Fatal("nil and empty key sets must hash equally")
	}
	decoded, err := base64.RawURLEncoding.DecodeString(empty)
	if err != nil {
		t.Fatal(err)
	}
	if len(decoded) != 32 {
		t.Fatalf("expected a SHA-256 digest, got %d bytes", len(decoded))
	}
}

func TestEncodedValuesUseBase64URLWithoutPadding(t *testing.T) {
	// Only the encoded values are base64; the input also contains plain URLs.
	nonce := []byte{0xff, 0xfe, 0xfd}
	fields := baseFields()
	fields.HostPubkeys = nil
	fields.Nonce = nonce
	input := Input(fields)

	encodedNonce := base64.RawURLEncoding.EncodeToString(nonce)
	if !strings.HasSuffix(input, "|"+encodedNonce) {
		t.Fatalf("the nonce must be encoded as raw URL base64 at the end of the input: %q", input)
	}
	digest := HashKeys(nil)
	for _, encoded := range []string{encodedNonce, digest} {
		for _, character := range encoded {
			if character == '+' || character == '/' || character == '=' {
				t.Fatalf("encoded value %q must not use padding or the standard alphabet", encoded)
			}
		}
	}
}
