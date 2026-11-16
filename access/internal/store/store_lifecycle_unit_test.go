package store

import (
	"context"
	"errors"
	"testing"
)

// Lifecycle helpers fail closed without a pool and surface seam failures
// instead of panicking, which is also what a partially configured deployment
// sees during startup.
func TestStoreLifecycleHelpersFailClosed(t *testing.T) {
	ctx := context.Background()
	if err := (&Store{}).Ping(ctx); err == nil {
		t.Fatal("Ping without a pool must fail")
	}
	if err := newStore(&fakeDB{t: t, pingErr: errors.New("down")}).Ping(ctx); err == nil {
		t.Fatal("a ping failure must be reported")
	}
	if err := (&Store{}).Migrate(ctx); err == nil {
		t.Fatal("Migrate without a pool must fail")
	}
	if store := NewWithPool(nil); store.db != nil {
		t.Fatal("a nil pool must yield an empty store")
	}
	if (&Store{}).Pool() != nil {
		t.Fatal("an empty store has no pool")
	}
	if _, err := Open(ctx, "not a connection string"); err == nil {
		t.Fatal("an invalid DSN must be refused")
	}
}

func TestStorePoolHelpersFailClosedWithoutAPool(t *testing.T) {
	ctx := context.Background()
	if _, err := (&Store{}).poolBegin(ctx); err == nil {
		t.Fatal("poolBegin without a pool must fail")
	}
	if _, err := (&Store{}).poolQuery(ctx, "SELECT 1"); err == nil {
		t.Fatal("poolQuery without a pool must fail")
	}
	if row := (&Store{}).poolQueryRow(ctx, "SELECT 1"); row.Scan() == nil {
		t.Fatal("poolQueryRow without a pool must return an error row")
	}
}
