package store

import (
	"errors"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestSplitStatementsRespectCommentsAndQuotedSemicolons(t *testing.T) {
	raw := `-- a comment; with a semicolon
CREATE TABLE a (id text);
-- another; comment
CREATE TABLE b (value text DEFAULT 'x;y');
`
	statements := splitStatements(raw)
	if len(statements) != 2 {
		t.Fatalf("statements = %d: %#v", len(statements), statements)
	}
	if !strings.Contains(statements[0], "CREATE TABLE a") || strings.Contains(statements[0], "another") {
		t.Fatalf("first = %q", statements[0])
	}
	if !strings.Contains(statements[1], "'x;y'") {
		t.Fatalf("second = %q", statements[1])
	}
	if extra := splitStatements("-- only comment; here\n"); len(extra) != 1 {
		t.Fatalf("comment-only trailing statement = %#v", extra)
	}
	edge := splitStatements("SELECT 'it''s;fine'; -- trailing comment")
	if len(edge) != 2 || !strings.Contains(edge[0], "it''s;fine") || !strings.Contains(edge[1], "trailing comment") {
		t.Fatalf("edge = %#v", edge)
	}
	if got := splitStatements("SELECT 1"); len(got) != 1 || got[0] != "SELECT 1" {
		t.Fatalf("no semicolon = %#v", got)
	}
	if none := splitStatements(""); len(none) != 0 {
		t.Fatalf("empty = %#v", none)
	}
}

func TestStatusHelpersRoundTripKnownAndUnknownValues(t *testing.T) {
	if got := parseStatus("succeeded"); got != whalereinv1.OnlineStatus_succeeded {
		t.Fatalf("parse = %v", got)
	}
	if got := parseStatus("bogus"); got != whalereinv1.OnlineStatus_online_status_unspecified {
		t.Fatalf("parse bogus = %v", got)
	}
	if got := StatusName(whalereinv1.OnlineStatus_host_accepted); got != "host_accepted" {
		t.Fatalf("status name = %q", got)
	}
	if got := StatusName(whalereinv1.OnlineStatus_online_status_unspecified); got != "online_status_unspecified" {
		t.Fatalf("unspecified name = %q", got)
	}
	if ValidStatus(whalereinv1.OnlineStatus_online_status_unspecified) {
		t.Fatal("unspecified must be invalid")
	}
	if !ValidStatus(whalereinv1.OnlineStatus_unknown) {
		t.Fatal("unknown must be valid")
	}
}

func TestPrefixColumnsFormatsMultilineLists(t *testing.T) {
	got := prefixColumns("a, b,\n c", "r.")
	if got != "r.a, r.b, r.c" {
		t.Fatalf("prefix = %q", got)
	}
}

func TestErrorHelpersMapMissingRowsAndUniqueViolations(t *testing.T) {
	if !errors.Is(wrapRequestScan(pgx.ErrNoRows), ErrNotFound) {
		t.Fatal("no rows should map to ErrNotFound")
	}
	original := errors.New("boom")
	if !errors.Is(wrapRequestScan(original), original) {
		t.Fatal("other errors should pass through")
	}
	if !isUniqueViolation(&pgconn.PgError{Code: "23505"}) {
		t.Fatal("unique violation not recognized")
	}
	if isUniqueViolation(original) {
		t.Fatal("unexpected unique violation")
	}
}
