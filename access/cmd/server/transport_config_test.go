package main

import (
	"bytes"
	"context"
	"os"
	"strings"
	"testing"
)

func TestCheckConfigPrintsResolvedTransportProfile(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture?sslmode=disable"})
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	updated := bytes.Replace(body, []byte("internal:"), []byte("transport:\n  profile: roomy\ninternal:"), 1)
	if err := os.WriteFile(path, updated, 0o600); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr); code != 0 {
		t.Fatalf("exit=%d stderr=%q", code, stderr.String())
	}
	for _, expected := range []string{"profile=roomy", "envelope_bytes=33554432", "bulk_concurrency=8", "http_max_body_bytes=47185920"} {
		if !strings.Contains(stdout.String(), expected) {
			t.Fatalf("resolved transport line missing %q: %s", expected, stdout.String())
		}
	}
}

func TestCheckConfigRejectsAnInvalidTransportProfileBeforeReadiness(t *testing.T) {
	path := writeConfig(t, configValues{publicPort: 1, internalPort: 2, databaseURL: "postgres://localhost:1/fixture?sslmode=disable"})
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	updated := bytes.Replace(body, []byte("internal:"), []byte("transport:\n  profile: not-a-profile\ninternal:"), 1)
	if err := os.WriteFile(path, updated, 0o600); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := run(context.Background(), []string{"-config", path, "-check-config"}, &stdout, &stderr); code != 2 {
		t.Fatalf("exit=%d stderr=%q", code, stderr.String())
	}
	if !strings.Contains(stderr.String(), "not a packaged profile") {
		t.Fatalf("transport error not explicit: %s", stderr.String())
	}
	if strings.Contains(stdout.String(), "config ok") {
		t.Fatal("invalid transport profile accepted")
	}
}
