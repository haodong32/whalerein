package storetest

import (
	"context"
	"os"
	"strings"
	"testing"
)

func TestDSNHelpersPreserveOptionsAndRedactCredentials(t *testing.T) {
	if got := withSearchPath("postgres://user:pass@host/db?sslmode=disable", "s1"); !strings.Contains(got, "search_path=s1") || !strings.Contains(got, "sslmode=disable") {
		t.Fatalf("withSearchPath = %q", got)
	}
	if got := withSearchPath("://bad", "s"); got != "://bad" {
		t.Fatalf("invalid base = %q", got)
	}
	if got := quoteIdent(`a"b`); got != `"a""b"` {
		t.Fatalf("quote = %q", got)
	}
	if got := redactDSN("postgres://user:password@host/db?x=1"); strings.Contains(got, "user") || strings.Contains(got, "password") || strings.Contains(got, "x=1") {
		t.Fatalf("redacted = %q", got)
	}
	if got := redactDSN("://bad"); got != "configured database" {
		t.Fatalf("bad redact = %q", got)
	}
}

func defaultDSNForTest() string {
	if value := os.Getenv("WHALEREIN_TEST_DATABASE_URL"); value != "" {
		return value
	}
	return defaultDSN
}

func TestNewSQLiteCreatesAnIsolatedReopenableStore(t *testing.T) {
	s, db := NewSQLite(t)
	if db.SQLitePath == "" {
		t.Fatal("no SQLite path")
	}
	if _, err := s.EnsureHost(context.Background(), "host"); err != nil {
		t.Fatal(err)
	}
	reopened := db.Open(t)
	host, err := reopened.GetHost(context.Background(), "host")
	if err != nil || host.HostID != "host" {
		t.Fatalf("reopened host = %#v %v", host, err)
	}
}

func TestNewSchemaAndNewSkipWithoutAPostgres(t *testing.T) {
	t.Setenv("WHALEREIN_TEST_DATABASE_URL", "postgres://127.0.0.1:1/test?sslmode=disable")
	NewSchema(t)
	New(t)
}

func TestSQLiteOpenFailureIsReported(t *testing.T) {
	db := &DB{SQLitePath: "/"}
	if _, _, err := db.open(); err == nil {
		t.Fatal("directory database path opened")
	}
}
