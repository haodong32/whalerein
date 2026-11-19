//go:build integration

package store_test

import (
	"context"
	"crypto/sha256"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
)

// openStoreAtMigration builds a fresh schema with the embedded ledger applied
// only through the named migration. The production Migrate path can then run
// against a genuinely older schema instead of dropping objects from a current
// one.
func openStoreAtMigration(t *testing.T, through string) *store.Store {
	t.Helper()
	db := storetest.NewSchema(t)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	t.Cleanup(cancel)

	cfg, err := pgxpool.ParseConfig(db.DSN)
	if err != nil {
		t.Fatal(err)
	}
	// Older schemas contain multi-statement files and must apply exactly as an
	// older build did.
	cfg.ConnConfig.DefaultQueryExecMode = pgx.QueryExecModeSimpleProtocol
	raw, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(raw.Close)
	if _, err := raw.Exec(ctx, `CREATE TABLE schema_migrations (
		name text PRIMARY KEY, checksum bytea NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())`); err != nil {
		t.Fatalf("create migration ledger: %v", err)
	}
	entries, err := embeddedMigrations.ReadDir("migrations")
	if err != nil {
		t.Fatal(err)
	}
	applied := false
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		content, err := embeddedMigrations.ReadFile("migrations/" + entry.Name())
		if err != nil {
			t.Fatal(err)
		}
		if _, err := raw.Exec(ctx, string(content)); err != nil {
			t.Fatalf("apply older migration %s: %v", entry.Name(), err)
		}
		checksum := sha256.Sum256(content)
		if _, err := raw.Exec(ctx, "INSERT INTO schema_migrations(name,checksum) VALUES ($1,$2)", entry.Name(), checksum[:]); err != nil {
			t.Fatalf("record older migration %s: %v", entry.Name(), err)
		}
		if entry.Name() == through {
			applied = true
			break
		}
	}
	if !applied {
		t.Fatalf("migration %s not found", through)
	}
	pool, err := pgxpool.New(ctx, db.DSN)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(pool.Close)
	return store.NewWithPool(pool)
}
