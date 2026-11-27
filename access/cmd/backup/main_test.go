package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/whalerein/whalerein/access/internal/store"
)

func writeKey(t *testing.T, dir string, raw []byte) string {
	t.Helper()
	path := filepath.Join(dir, "installation.key")
	if err := os.WriteFile(path, []byte(base64.StdEncoding.EncodeToString(raw)+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestRunUsageAndKeyParsing(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := run(nil, &stdout, &stderr); code != 2 {
		t.Fatalf("no args = %d", code)
	}
	if code := run([]string{"unknown"}, &stdout, &stderr); code != 2 {
		t.Fatalf("unknown = %d", code)
	}
	dir := t.TempDir()
	key := bytes.Repeat([]byte{1}, 32)
	keyFile := writeKey(t, dir, key)
	got, err := readKey(keyFile)
	if err != nil || !bytes.Equal(got, key) {
		t.Fatalf("decoded key = %x %v", got, err)
	}
	raw := filepath.Join(dir, "raw.key")
	if err := os.WriteFile(raw, key, 0o600); err != nil {
		t.Fatal(err)
	}
	if got, err := readKey(raw); err != nil || !bytes.Equal(got, key) {
		t.Fatalf("raw key = %x %v", got, err)
	}
	spaced := filepath.Join(dir, "spaced.key")
	if err := os.WriteFile(spaced, []byte(" \n"+base64.StdEncoding.EncodeToString(key)+"\t\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got, err := readKey(spaced); err != nil || !bytes.Equal(got, key) {
		t.Fatalf("spaced key = %x %v", got, err)
	}
	short := filepath.Join(dir, "short.key")
	if err := os.WriteFile(short, []byte("short"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := readKey(short); err == nil {
		t.Fatal("short key accepted")
	}
	if _, err := readKey(filepath.Join(dir, "missing")); err == nil {
		t.Fatal("missing key accepted")
	}
}

func TestCreateVerifyAndRestoreRoundTrip(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	database := filepath.Join(dir, "access.db")
	data, err := store.Open(ctx, database)
	if err != nil {
		t.Fatal(err)
	}
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	data.Close()
	key := bytes.Repeat([]byte{2}, 32)
	keyFile := writeKey(t, dir, key)
	backup := filepath.Join(dir, "backup.db")
	var stdout, stderr bytes.Buffer
	if code := run([]string{"create", "-database", database, "-out", backup, "-installation-id", "install-1", "-key-file", keyFile, "-revision", "7"}, &stdout, &stderr); code != 0 {
		t.Fatalf("create = %d %s", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "\"authority_revision\":7") {
		t.Fatalf("create output = %s", stdout.String())
	}
	stdout.Reset()
	if code := run([]string{"verify", "-database", backup, "-manifest", backup + ".json", "-installation-id", "install-1", "-key-file", keyFile, "-floor", "7"}, &stdout, &stderr); code != 0 {
		t.Fatalf("verify = %d %s", code, stderr.String())
	}
	target := filepath.Join(dir, "restored.db")
	stdout.Reset()
	if code := run([]string{"restore", "-backup", backup, "-manifest", backup + ".json", "-target", target, "-installation-id", "install-1", "-key-file", keyFile, "-floor", "7"}, &stdout, &stderr); code != 0 {
		t.Fatalf("restore = %d %s", code, stderr.String())
	}
	if _, err := os.Stat(target); err != nil {
		t.Fatal(err)
	}
	if code := run([]string{"restore", "-backup", backup, "-manifest", backup + ".json", "-target", "", "-installation-id", "install-1", "-key-file", keyFile}, &stdout, &stderr); code == 0 {
		t.Fatal("empty target accepted")
	}
}

func TestCommandFailures(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := run([]string{"create", "-database", "/missing", "-out", "/tmp/x", "-installation-id", "i", "-key-file", "/missing"}, &stdout, &stderr); code == 0 {
		t.Fatal("missing key accepted")
	}
	dir := t.TempDir()
	keyFile := writeKey(t, dir, bytes.Repeat([]byte{3}, 32))
	if code := run([]string{"create", "-database", "/missing", "-out", filepath.Join(dir, "b.db"), "-installation-id", "i", "-key-file", keyFile}, &stdout, &stderr); code == 0 {
		t.Fatal("missing database accepted")
	}
	if code := run([]string{"verify", "-database", "/missing", "-manifest", "/missing", "-installation-id", "i", "-key-file", keyFile}, &stdout, &stderr); code == 0 {
		t.Fatal("missing backup accepted")
	}
}
