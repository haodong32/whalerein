//go:build integration

package storetest

import (
	"context"
	"errors"

	"strings"
	"testing"
)

// When a test schema is created and reopened, each store connects to that same isolated schema.
func TestSchemaLifecycleCreatesAndReopensIsolatedSchema(t *testing.T) {
	db := NewSchema(t)
	if db.Schema == "" || !strings.Contains(db.DSN, "search_path="+db.Schema) {
		t.Fatalf("db = %+v", db)
	}
	s := db.Open(t)
	if err := s.Ping(context.Background()); err != nil {
		t.Fatal(err)
	}
	var schema string
	if err := s.Pool().QueryRow(context.Background(), "SELECT current_schema()").Scan(&schema); err != nil {
		t.Fatal(err)
	}
	if schema != db.Schema {
		t.Fatalf("schema = %q want %q", schema, db.Schema)
	}
	second := db.Open(t)
	if err := second.Ping(context.Background()); err != nil {
		t.Fatal(err)
	}
}

// When the test store factory succeeds, it returns both an initialized store and its isolated
// database schema.
func TestNewCreatesSchemaAndStore(t *testing.T) {
	s, db := New(t)
	if s == nil || db == nil {
		t.Fatal("New returned nil")
	}
}

// When database configuration is malformed, unreachable or references a missing schema, setup
// returns the corresponding error.
func TestSchemaSetupRejectsInvalidDSNAndUnreachableDatabase(t *testing.T) {
	if _, _, err := newSchemaWith("postgres://%zz"); err == nil || !errors.Is(err, errUnavailable) {
		t.Fatalf("invalid DSN = %v", err)
	}
	if _, _, err := newSchemaWith("postgres://postgres@127.0.0.1:1/none?sslmode=disable&connect_timeout=1"); err == nil || !errors.Is(err, errUnavailable) {
		t.Fatalf("unreachable = %v", err)
	}
	db := NewSchema(t)
	if _, _, err := (&DB{DSN: "postgres://%zz"}).open(); err == nil {
		t.Fatal("expected parse error")
	}
	missing := strings.Replace(db.DSN, "search_path="+db.Schema, "search_path=missing_schema", 1)
	if _, _, err := (&DB{DSN: missing}).open(); err == nil {
		t.Fatal("expected migration error")
	}
	if _, cleanup, err := db.open(); err != nil {
		t.Fatal(err)
	} else {
		cleanup()
	}
}

// When schema identity generation or schema creation fails, setup returns the failure instead of a
// usable fixture.
func TestSchemaSetupReturnsRandomAndExecFailures(t *testing.T) {
	ops := defaultSchemaOps()
	ops.random = func([]byte) (int, error) { return 0, errors.New("random failed") }
	if _, _, err := newSchemaWithOps(defaultDSNForTest(), ops); err == nil || !strings.Contains(err.Error(), "random") {
		t.Fatalf("random = %v", err)
	}
	ops = defaultSchemaOps()
	ops.exec = func(context.Context, schemaAdmin, string) error { return errors.New("exec failed") }
	if _, _, err := newSchemaWithOps(defaultDSNForTest(), ops); err == nil || !strings.Contains(err.Error(), "create schema") {
		t.Fatalf("exec = %v", err)
	}
}
