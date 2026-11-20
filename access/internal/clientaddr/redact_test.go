package clientaddr

import (
	"bytes"
	"errors"
	"net/netip"
	"strings"
	"testing"
	"time"
)

func newTestRedactor(t *testing.T, writer *bytes.Buffer) *Redactor {
	t.Helper()
	referencer, err := NewReferencer(testKey, WindowMonthly)
	if err != nil {
		t.Fatal(err)
	}
	return NewRedactor(writer, referencer)
}

func TestRedactorRemovesAddressLiterals(t *testing.T) {
	var logs bytes.Buffer
	redactor := newTestRedactor(t, &logs)
	for _, input := range []string{
		"client 203.0.113.7:443 failed\n",
		"xff=203.0.113.7, 198.51.100.9\n",
		"peer [2001:db8::1]:443\n",
		"zone fe80::1%eth0\n",
		"loopback 127.0.0.1:8080\n",
	} {
		if _, err := redactor.Write([]byte(input)); err != nil {
			t.Fatal(err)
		}
	}
	output := logs.String()
	for _, plaintext := range []string{"203.0.113.7", "198.51.100.9", "2001:db8::1", "fe80::1", "127.0.0.1"} {
		if strings.Contains(output, plaintext) {
			t.Fatalf("output leaked %q: %s", plaintext, output)
		}
	}
	if !strings.Contains(output, "c1_") {
		t.Fatalf("output lacks pseudonyms: %s", output)
	}
}

func TestRedactorLeavesTimesAndNonAddressesAlone(t *testing.T) {
	var logs bytes.Buffer
	redactor := newTestRedactor(t, &logs)
	input := "2026-11-20T10:00:00Z version 1.2.3 listen 0.0.0.0:8080\n"
	if _, err := redactor.Write([]byte(input)); err != nil {
		t.Fatal(err)
	}
	if got := logs.String(); got != input {
		t.Fatalf("output changed non-address content: %q", got)
	}
}

func TestRedactorWithoutReferenceUsesPlaceholder(t *testing.T) {
	var logs bytes.Buffer
	redactor := NewRedactor(&logs, nil)
	if _, err := redactor.Write([]byte("client 203.0.113.7\n")); err != nil {
		t.Fatal(err)
	}
	if got := logs.String(); strings.Contains(got, "203.0.113.7") || !strings.Contains(got, "[client]") {
		t.Fatalf("output = %q", got)
	}
}

func TestRedactorReturnsTheInputSizeAndPropagatesWriteErrors(t *testing.T) {
	var logs bytes.Buffer
	redactor := NewRedactor(&logs, nil)
	input := []byte("client 203.0.113.7\n")
	n, err := redactor.Write(input)
	if err != nil || n != len(input) {
		t.Fatalf("Write = %d, %v", n, err)
	}
	if got := logs.String(); strings.Contains(got, "203.0.113.7") {
		t.Fatalf("output = %q", got)
	}
	failing := &errorWriter{}
	if _, err := NewRedactor(failing, nil).Write(input); err == nil {
		t.Fatal("write error was swallowed")
	}
}

type errorWriter struct{}

func (errorWriter) Write([]byte) (int, error) { return 0, errors.New("write failed") }

func TestRedactorUsesTheRequestTimeWindow(t *testing.T) {
	referencer, err := NewReferencer(testKey, WindowMonthly)
	if err != nil {
		t.Fatal(err)
	}
	redactor := NewRedactor(&bytes.Buffer{}, referencer)
	now := time.Date(2026, time.November, 20, 0, 0, 0, 0, time.UTC)
	got := redactor.redact("203.0.113.7", now)
	want, _ := referencer.Reference(netip.MustParseAddr("203.0.113.7"), now)
	if got != want {
		t.Fatalf("redacted = %q, want %q", got, want)
	}
}
