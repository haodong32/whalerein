package storetest

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5/pgconn"
)

type fakeSchemaAdmin struct {
	pingErr, execErr error
	statements       []string
	closed           int
}

func (a *fakeSchemaAdmin) Ping(context.Context) error { return a.pingErr }
func (a *fakeSchemaAdmin) Exec(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
	a.statements = append(a.statements, sql)
	return pgconn.NewCommandTag("CREATE SCHEMA"), a.execErr
}
func (a *fakeSchemaAdmin) Close() { a.closed++ }
func TestSchemaSetupOwnsAndCleansOnlyItsGeneratedSchema(t *testing.T) {
	for _, tc := range []struct{ name, fail string }{{"success", ""}, {"open failure", "open"}, {"ping failure", "ping"}, {"random failure", "random"}, {"create failure", "create"}} {
		t.Run(tc.name, func(t *testing.T) {
			admin := &fakeSchemaAdmin{}
			if tc.fail == "ping" {
				admin.pingErr = errors.New("fixture unavailable")
			}
			if tc.fail == "create" {
				admin.execErr = errors.New("fixture create failure")
			}
			ops := schemaOps{open: func(context.Context, string) (schemaAdmin, error) {
				if tc.fail == "open" {
					return nil, errors.New("fixture open failure")
				}
				return admin, nil
			}, random: func(data []byte) (int, error) {
				if tc.fail == "random" {
					return 0, errors.New("fixture random failure")
				}
				for i := range data {
					data[i] = byte(i)
				}
				return len(data), nil
			}, exec: func(ctx context.Context, a schemaAdmin, statement string) error {
				_, err := a.Exec(ctx, statement)
				return err
			}}
			db, cleanup, err := newSchemaWithOps("postgres://localhost/fixture?sslmode=disable", ops)
			if tc.fail != "" {
				if err == nil || db != nil || cleanup != nil {
					t.Fatal("failure returned usable schema")
				}
				if tc.fail != "open" && admin.closed != 1 {
					t.Fatal("failed setup did not close admin")
				}
				return
			}
			if err != nil || db.Schema != "test_000102030405" || !strings.Contains(db.DSN, "search_path=test_000102030405") {
				t.Fatal("incorrect isolated schema")
			}
			cleanup()
			if admin.closed != 1 || len(admin.statements) != 2 || admin.statements[0] != "CREATE SCHEMA \"test_000102030405\"" || admin.statements[1] != "DROP SCHEMA IF EXISTS \"test_000102030405\" CASCADE" {
				t.Fatal("cleanup did not target the created schema")
			}
		})
	}
}

func TestEnvironmentSchemaSetupRejectsMalformedURLWithoutConnecting(t *testing.T) {
	t.Setenv("WHALEREIN_TEST_DATABASE_URL", "postgres://%zz")
	value, cleanup, err := newSchema()
	if value != nil || cleanup != nil || !errors.Is(err, errUnavailable) {
		t.Fatal("malformed environment DSN did not fail closed")
	}
}
