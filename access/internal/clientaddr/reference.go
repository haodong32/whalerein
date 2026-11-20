package clientaddr

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"net/netip"
	"strings"
	"time"
)

const (
	referencePrefix = "c1_"
	referenceDomain = "whalerein/client/v1"
	// ReferenceBytes bounds the pseudonym to 128 bits. IPv4 has only 32 bits,
	// so this is not the collision limit; the secret is.
	ReferenceBytes = 16
	keyBytes       = 32
)

// Window bounds how long the same client keeps the same pseudonym.
type Window string

const (
	// WindowMonthly keeps a pseudonym stable for the UTC calendar month.
	WindowMonthly Window = "monthly"
	// WindowDaily keeps a pseudonym stable for the UTC day.
	WindowDaily Window = "daily"
)

// ParseWindow accepts the configured window; an empty value defaults to
// monthly.
func ParseWindow(value string) (Window, error) {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", string(WindowMonthly):
		return WindowMonthly, nil
	case string(WindowDaily):
		return WindowDaily, nil
	default:
		return "", fmt.Errorf("window must be %q or %q", WindowMonthly, WindowDaily)
	}
}

// DecodeKey decodes a standard-base64 256-bit master key. Padding is
// optional; the decoded material must be exactly 32 bytes.
func DecodeKey(value string) ([]byte, error) {
	value = strings.TrimSpace(value)
	if value == "" {
		return nil, fmt.Errorf("client reference key is empty")
	}
	var material []byte
	var err error
	for _, encoding := range []*base64.Encoding{base64.StdEncoding, base64.RawStdEncoding} {
		material, err = encoding.DecodeString(value)
		if err == nil {
			break
		}
	}
	if err != nil {
		return nil, fmt.Errorf("client reference key is not standard base64")
	}
	if len(material) != keyBytes {
		return nil, fmt.Errorf("client reference key must be %d bytes", keyBytes)
	}
	return material, nil
}

// Referencer produces windowed HMAC pseudonyms. A nil Referencer is valid and
// produces no reference; logging then falls back to the redacted placeholder.
type Referencer struct {
	key    []byte
	window Window
}

// NewReferencer validates the material and window.
func NewReferencer(key []byte, window Window) (*Referencer, error) {
	if len(key) != keyBytes {
		return nil, fmt.Errorf("client reference key must be %d bytes", keyBytes)
	}
	if window != WindowMonthly && window != WindowDaily {
		return nil, fmt.Errorf("unsupported client reference window %q", window)
	}
	return &Referencer{key: append([]byte(nil), key...), window: window}, nil
}

// NewReferencerFromConfig returns nil when no key is configured, so a
// deployment without pseudonyms keeps starting while never logging a raw
// address.
func NewReferencerFromConfig(key, window string) (*Referencer, error) {
	if strings.TrimSpace(key) == "" {
		return nil, nil
	}
	material, err := DecodeKey(key)
	if err != nil {
		return nil, err
	}
	parsed, err := ParseWindow(window)
	if err != nil {
		return nil, err
	}
	return NewReferencer(material, parsed)
}

// WindowID returns the UTC window identifier included in the HMAC input and in
// access records.
func (r *Referencer) WindowID(now time.Time) string {
	if r == nil {
		return ""
	}
	switch r.window {
	case WindowDaily:
		return now.UTC().Format("2006-01-02")
	default:
		return now.UTC().Format("2006-01")
	}
}

// Reference returns the pseudonym and window for an address. It returns empty
// strings for a nil referencer or an invalid address.
func (r *Referencer) Reference(addr netip.Addr, now time.Time) (string, string) {
	if r == nil || !addr.IsValid() {
		return "", ""
	}
	addr = addr.WithZone("").Unmap()
	if !addr.IsValid() || addr.IsUnspecified() || addr.IsMulticast() {
		return "", ""
	}
	window := r.WindowID(now)
	mac := hmac.New(sha256.New, r.key)
	_, _ = mac.Write([]byte(referenceDomain))
	_, _ = mac.Write([]byte("\n"))
	_, _ = mac.Write([]byte(window))
	_, _ = mac.Write([]byte("\n"))
	_, _ = mac.Write([]byte(addr.String()))
	sum := mac.Sum(nil)
	return referencePrefix + hex.EncodeToString(sum[:ReferenceBytes]), window
}
