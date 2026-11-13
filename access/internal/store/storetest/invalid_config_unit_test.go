package storetest

import (
	"errors"
	"strings"
	"testing"
)

func TestInvalidDatabaseConfigurationFailsBeforeOpeningTestSchema(t *testing.T) {
	_, cleanup, err := newSchemaWith("postgres://%zz")
	if !errors.Is(err, errUnavailable) || cleanup != nil {
		t.Fatalf("expected unavailable for malformed DSN; err=%v", err)
	}
	_, cleanup, err = (&DB{DSN: "postgres://%zz"}).open()
	if err == nil || cleanup != nil {
		t.Fatal("malformed store DSN was accepted")
	}
}

func TestDatabaseDiagnosticRedactsURLFragmentAndCredentials(t *testing.T) {
	value := redactDSN("postgres://fixture-user:fixture-password@localhost/db?credential=fixture-query#fixture-fragment")
	for _, part := range []string{"fixture-user", "fixture-password", "fixture-query", "fixture-fragment"} {
		if strings.Contains(value, part) {
			t.Fatal("database diagnostic retained private URL material")
		}
	}
}

func TestInvalidSchemaConfigurationDoesNotExposeConnectionIdentity(t *testing.T) {
	_, _, err := newSchemaWith("postgres://fixture-user:fixture-password@%zz/fixture")
	if err == nil {
		t.Fatal("invalid schema URL was accepted")
	}
	if strings.Contains(err.Error(), "fixture-user") || strings.Contains(err.Error(), "fixture-password") {
		t.Fatal("schema error exposed connection identity")
	}
}

func TestOpeningTestStoreDoesNotExposeMalformedConnectionIdentity(t *testing.T) {
	_, _, err := (&DB{DSN: "postgres://fixture-user:fixture-password@%zz/fixture"}).open()
	if err == nil {
		t.Fatal("invalid store URL was accepted")
	}
	if strings.Contains(err.Error(), "fixture-user") || strings.Contains(err.Error(), "fixture-password") {
		t.Fatal("store error exposed connection identity")
	}
}
