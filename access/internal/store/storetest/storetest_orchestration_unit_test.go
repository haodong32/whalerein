package storetest

import (
	"context"
	"errors"
	"testing"

	"github.com/whalerein/whalerein/access/internal/store"
)

// newSchemaBaseWithOps is the seam NewSchema uses. An empty base must fall back
// to the documented default, and the supplied operations must drive setup.
func TestSchemaBaseResolvesDefaultAndInjectedOperations(t *testing.T) {
	for _, tc := range []struct {
		name string
		base string
		want string
	}{
		{"configured base", "postgres://localhost/fixture?sslmode=disable", "postgres://localhost/fixture?sslmode=disable"},
		{"default base", "", defaultDSN},
	} {
		t.Run(tc.name, func(t *testing.T) {
			admin := &fakeSchemaAdmin{}
			ops := schemaOps{open: func(context.Context, string) (schemaAdmin, error) { return admin, nil },
				random: func(data []byte) (int, error) { return len(data), nil },
				exec: func(_ context.Context, admin schemaAdmin, statement string) error {
					_, err := admin.Exec(context.Background(), statement)
					return err
				},
			}
			db, cleanup, err := newSchemaBaseWithOps(tc.base, ops)
			if err != nil || db == nil || db.BaseDSN != tc.want || cleanup == nil {
				t.Fatalf("db=%+v err=%v", db, err)
			}
			cleanup()
		})
	}
}

func TestDefaultSchemaOperationsSurfaceSetupFailures(t *testing.T) {
	ops := defaultSchemaOps()
	admin := &fakeSchemaAdmin{}
	if err := ops.exec(context.Background(), admin, "CREATE SCHEMA fixture"); err != nil {
		t.Fatalf("exec = %v", err)
	}
	if len(admin.statements) != 1 || admin.statements[0] != "CREATE SCHEMA fixture" {
		t.Fatalf("statements = %v", admin.statements)
	}
	admin.execErr = errUnavailable
	if err := ops.exec(context.Background(), admin, "CREATE SCHEMA fixture"); !errors.Is(err, errUnavailable) {
		t.Fatalf("exec failure = %v", err)
	}
	if _, err := ops.open(context.Background(), "postgres://%zz/fixture"); err == nil {
		t.Fatal("malformed DSN was accepted by the default opener")
	}
}

// The store opener seam must hand back the store and its cleanup exactly once,
// and must not invent a usable store when the schema was never migrated.
func TestStoreOpeningSeamReturnsCleanupOrFailsClosed(t *testing.T) {
	cleanupCalls := 0
	remembered := store.NewWithPool(nil)
	db := &DB{DSN: "postgres://localhost/fixture", openStore: func(context.Context, string) (*store.Store, func(), error) {
		return remembered, func() { cleanupCalls++ }, nil
	}}
	s, cleanup, err := db.open()
	if err != nil || s != remembered || cleanup == nil {
		t.Fatalf("store=%p err=%v", s, err)
	}
	cleanup()
	if cleanupCalls != 1 {
		t.Fatalf("cleanup calls = %d", cleanupCalls)
	}

	db.openStore = func(context.Context, string) (*store.Store, func(), error) {
		return nil, nil, errUnavailable
	}
	if s, cleanup, err := db.open(); s != nil || cleanup != nil || !errors.Is(err, errUnavailable) {
		t.Fatalf("failed open returned store=%p cleanup-nil=%t err=%v", s, cleanup == nil, err)
	}
}

// A test store is never handed out before its schema migrated. Letting the
// migration fail must close the pool and return no store or cleanup.
func TestPoolBackedStoreOpeningFailsClosedWhenMigrationFails(t *testing.T) {
	for _, tc := range []struct{ name, dsn string }{
		{"unreachable cluster", "postgres://127.0.0.1:1/fixture?sslmode=disable&connect_timeout=1"},
		{"malformed dsn", "postgres://%zz/fixture"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s, cleanup, err := openStoreWithPool(context.Background(), tc.dsn)
			if err == nil || s != nil || cleanup != nil {
				t.Fatalf("store=%p cleanup-nil=%t err=%v", s, cleanup == nil, err)
			}
		})
	}
}

// newStore is the shared body of New: it must register the schema cleanup and
// open a store against the created schema.
func TestNewStoreOpensTheCreatedSchema(t *testing.T) {
	opened := store.NewWithPool(nil)
	cleanupCalls := 0
	factory := func() (*DB, func(), error) {
		return &DB{BaseDSN: defaultDSN, Schema: "test_fixture", DSN: "postgres://localhost/fixture", openStore: func(context.Context, string) (*store.Store, func(), error) {
			return opened, func() { cleanupCalls++ }, nil
		}}, func() { cleanupCalls++ }, nil
	}
	s, db := newStore(t, factory)
	if s != opened || db == nil || db.Schema != "test_fixture" {
		t.Fatalf("store=%p db=%+v", s, db)
	}
}
