package store

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

type migrationRows struct {
	pgx.Rows
	names []string
	index int
	fail  string
}

func (r *migrationRows) Next() bool { r.index++; return r.index <= len(r.names) }
func (r *migrationRows) Scan(dest ...any) error {
	if r.fail == "scan" {
		return errStoreUnit
	}
	*dest[0].(*string) = r.names[r.index-1]
	return nil
}
func (r *migrationRows) Close() {}
func (r *migrationRows) Err() error {
	if r.fail == "rows" {
		return errStoreUnit
	}
	return nil
}

type checksumRow struct {
	checksum []byte
	err      error
}

func (r checksumRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	*dest[0].(*[]byte) = r.checksum
	return nil
}

type migrationTx struct {
	pgx.Tx
	t         *testing.T
	fail      string
	names     []string
	applied   map[string][]byte
	steps     []string
	inserted  map[string][]byte
	committed bool
}

func (tx *migrationTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	step := "statement"
	switch {
	case strings.Contains(sql, "pg_advisory_xact_lock"):
		step = "lock"
	case strings.Contains(sql, "CREATE TABLE IF NOT EXISTS schema_migrations"):
		step = "ledger"
	case strings.Contains(sql, "INSERT INTO schema_migrations"):
		step = "record"
	}
	tx.steps = append(tx.steps, step)
	if tx.fail == step {
		return pgconn.CommandTag{}, errStoreUnit
	}
	if step == "lock" && (len(args) != 1 || args[0] != migrationLockKey) {
		tx.t.Fatal("wrong migration lock")
	}
	if step == "record" {
		name := args[0].(string)
		tx.inserted[name] = append([]byte(nil), args[1].([]byte)...)
	}
	return pgconn.NewCommandTag("UPDATE 1"), nil
}
func (tx *migrationTx) Query(_ context.Context, _ string, _ ...any) (pgx.Rows, error) {
	tx.steps = append(tx.steps, "list")
	if tx.fail == "list" {
		return nil, errStoreUnit
	}
	return &migrationRows{names: tx.names, fail: tx.fail}, nil
}
func (tx *migrationTx) QueryRow(_ context.Context, _ string, args ...any) pgx.Row {
	tx.steps = append(tx.steps, "checksum")
	if tx.fail == "checksum" {
		return checksumRow{err: errStoreUnit}
	}
	value, ok := tx.applied[args[0].(string)]
	if !ok {
		return checksumRow{err: pgx.ErrNoRows}
	}
	return checksumRow{checksum: value}
}
func (tx *migrationTx) Commit(context.Context) error {
	tx.steps = append(tx.steps, "commit")
	if tx.fail == "commit" {
		return errStoreUnit
	}
	tx.committed = true
	return nil
}
func (tx *migrationTx) Rollback(context.Context) error {
	tx.steps = append(tx.steps, "rollback")
	return nil
}
func TestMigrationLedgerRejectsChangedAndFutureHistoryBeforeCommit(t *testing.T) {
	entries, err := migrationFS.ReadDir("migrations")
	if err != nil {
		t.Fatal(err)
	}
	known := map[string][]byte{}
	names := []string{}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		raw, err := migrationFS.ReadFile("migrations/" + entry.Name())
		if err != nil {
			t.Fatal(err)
		}
		hash := sha256.Sum256(raw)
		known[entry.Name()] = append([]byte(nil), hash[:]...)
		names = append(names, entry.Name())
	}
	for _, mode := range []string{"current", "empty", "future", "checksum-changed", "lock", "ledger", "list", "scan", "rows", "checksum", "statement", "record", "commit"} {
		t.Run(mode, func(t *testing.T) {
			tx := &migrationTx{t: t, applied: map[string][]byte{}, inserted: map[string][]byte{}}
			if mode == "current" || mode == "checksum-changed" {
				tx.names = append([]string(nil), names...)
				for k, v := range known {
					tx.applied[k] = append([]byte(nil), v...)
				}
				if mode == "checksum-changed" {
					tx.applied[names[0]] = []byte{0}
				}
			}
			if mode == "future" {
				tx.names = []string{"999_future.sql"}
			}
			if mode == "scan" || mode == "rows" {
				tx.names = []string{names[0]}
			}
			if mode != "current" && mode != "empty" && mode != "future" && mode != "checksum-changed" {
				tx.fail = mode
			}
			err := migrate(context.Background(), func(context.Context) (pgx.Tx, error) { return tx, nil })
			success := mode == "current" || mode == "empty"
			if (err == nil) != success || tx.committed != success || tx.steps[len(tx.steps)-1] != "rollback" {
				t.Fatalf("err=%v committed=%v steps=%v", err, tx.committed, tx.steps)
			}
			if mode == "future" || mode == "checksum-changed" {
				for _, step := range tx.steps {
					if step == "statement" || step == "record" {
						t.Fatal("invalid ledger executed migration")
					}
				}
			}
			if mode == "current" && len(tx.inserted) != 0 {
				t.Fatal("existing migrations reinserted")
			}
			if mode == "empty" {
				if len(tx.inserted) != len(known) {
					t.Fatal("not all migrations recorded")
				}
				for name, want := range known {
					if !bytes.Equal(tx.inserted[name], want) {
						t.Fatalf("checksum mismatch for %s", name)
					}
				}
			}
		})
	}
	t.Run("begin failure", func(t *testing.T) {
		err := migrate(context.Background(), func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit })
		if !errors.Is(err, errStoreUnit) {
			t.Fatal("begin failure lost")
		}
	})
}
