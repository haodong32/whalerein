// Package storetest provides an isolated PostgreSQL schema for component
// tests. It is test support only and is never imported by production code.
//
// The test database is expected at WHALEREIN_TEST_DATABASE_URL, defaulting to
// a local trust-authenticated cluster on port 55432. Tests skip with a clear
// message when the cluster is unavailable.
package storetest

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"net/url"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/whalerein/whalerein/access/internal/store"
)

const defaultDSN = "postgres://postgres@127.0.0.1:55432/whalerein_test?sslmode=disable"

// DB identifies one isolated test schema.
type DB struct {
	BaseDSN string
	Schema  string
	DSN     string
}

// New creates a fresh schema, migrates it and returns the store.
func New(t *testing.T) (*store.Store, *DB) {
	t.Helper()
	db := NewSchema(t)
	return db.Open(t), db
}

var errUnavailable = errors.New("test database unavailable")

// NewSchema creates a fresh empty schema.
func NewSchema(t *testing.T) *DB {
	t.Helper()
	db, cleanup, err := newSchema()
	if err != nil {
		if errors.Is(err, errUnavailable) {
			t.Skipf("PostgreSQL test database unavailable: %v", err)
		}
		t.Fatalf("create test schema: %v", err)
	}
	t.Cleanup(cleanup)
	return db
}

func newSchema() (*DB, func(), error) {
	base := os.Getenv("WHALEREIN_TEST_DATABASE_URL")
	if base == "" {
		base = defaultDSN
	}
	return newSchemaWith(base)
}

type schemaOps struct {
	random func([]byte) (int, error)
	exec   func(context.Context, *pgxpool.Pool, string) error
}

func defaultSchemaOps() schemaOps {
	return schemaOps{
		random: rand.Read,
		exec: func(ctx context.Context, admin *pgxpool.Pool, statement string) error {
			_, err := admin.Exec(ctx, statement)
			return err
		},
	}
}

func newSchemaWith(base string) (*DB, func(), error) {
	return newSchemaWithOps(base, defaultSchemaOps())
}

func newSchemaWithOps(base string, ops schemaOps) (*DB, func(), error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	admin, err := pgxpool.New(ctx, base)
	if err != nil {
		return nil, nil, fmt.Errorf("%w at %s: %v", errUnavailable, redactDSN(base), err)
	}
	if err := admin.Ping(ctx); err != nil {
		admin.Close()
		return nil, nil, fmt.Errorf("%w at %s: %v", errUnavailable, redactDSN(base), err)
	}

	suffix := make([]byte, 6)
	if _, err := ops.random(suffix); err != nil {
		admin.Close()
		return nil, nil, fmt.Errorf("random schema suffix: %w", err)
	}
	schema := "test_" + hex.EncodeToString(suffix)
	if err := ops.exec(ctx, admin, "CREATE SCHEMA "+quoteIdent(schema)); err != nil {
		admin.Close()
		return nil, nil, fmt.Errorf("create schema: %w", err)
	}
	cleanup := func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		_, _ = admin.Exec(cleanupCtx, "DROP SCHEMA IF EXISTS "+quoteIdent(schema)+" CASCADE")
		admin.Close()
	}
	return &DB{BaseDSN: base, Schema: schema, DSN: withSearchPath(base, schema)}, cleanup, nil
}

// Open opens a new store and runs migrations. Each call creates an independent
// connection pool, which lets a test exercise Access restarts against the same
// persisted schema.
func (db *DB) Open(t *testing.T) *store.Store {
	t.Helper()
	s, cleanup, err := db.open()
	if err != nil {
		t.Fatalf("open test store: %v", err)
	}
	t.Cleanup(cleanup)
	return s
}

func (db *DB) open() (*store.Store, func(), error) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	cfg, err := pgxpool.ParseConfig(db.DSN)
	if err != nil {
		return nil, nil, fmt.Errorf("parse test DSN: %w", err)
	}
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		return nil, nil, fmt.Errorf("open test pool: %w", err)
	}
	s := store.NewWithPool(pool)
	if err := s.Migrate(ctx); err != nil {
		pool.Close()
		return nil, nil, fmt.Errorf("migrate test schema: %w", err)
	}
	return s, pool.Close, nil
}

func withSearchPath(base, schema string) string {
	u, err := url.Parse(base)
	if err != nil {
		return base
	}
	q := u.Query()
	q.Set("search_path", schema)
	u.RawQuery = q.Encode()
	return u.String()
}

func quoteIdent(value string) string {
	return `"` + strings.ReplaceAll(value, `"`, `""`) + `"`
}

func redactDSN(dsn string) string {
	u, err := url.Parse(dsn)
	if err != nil {
		return "configured database"
	}
	u.User = nil
	u.RawQuery = ""
	return u.String()
}
