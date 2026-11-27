package store

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"os"
	"strings"
	"testing"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestTranslateSQLPostgresShapes(t *testing.T) {
	translated, args, err := translateSQL(`SELECT $2::text, $1::bigint FROM requests
		WHERE request_id=$1 FOR UPDATE SKIP LOCKED`, []any{"id", int64(7)})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(strings.ToLower(translated), "for update") || strings.Contains(strings.ToLower(translated), "skip locked") {
		t.Fatalf("row locks survived: %s", translated)
	}
	if strings.Contains(translated, "::") {
		t.Fatalf("cast survived: %s", translated)
	}
	if len(args) != 3 || args[0] != int64(7) || args[1] != "id" || args[2] != "id" {
		t.Fatalf("args = %#v", args)
	}
}

func TestTranslateSQLExpandsAnyAndRewritesFunctions(t *testing.T) {
	translated, args, err := translateSQL(`SELECT GREATEST(a,b), LEAST(a,b)
		FROM device_credentials WHERE device_id = ANY($1)
		AND expires_at > now() - interval '2 hours'`, []any{[]string{"a", "b"}})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(strings.ToUpper(translated), "GREATEST") || strings.Contains(strings.ToUpper(translated), "LEAST") {
		t.Fatalf("greatest/least survived: %s", translated)
	}
	if !strings.Contains(translated, "IN (?,?)") {
		t.Fatalf("ANY not expanded: %s", translated)
	}
	if !strings.Contains(translated, "unixepoch()*1000") || !strings.Contains(translated, "7200000") {
		t.Fatalf("time expressions not rewritten: %s", translated)
	}
	if len(args) != 2 || args[0] != "a" || args[1] != "b" {
		t.Fatalf("args = %#v", args)
	}
}

func TestTranslateSQLRewritesExtractEpoch(t *testing.T) {
	translated, _, err := translateSQL(`SELECT EXTRACT(EPOCH FROM (clock_timestamp() - MIN(expires_at)))`, nil)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(strings.ToUpper(translated), "EXTRACT") {
		t.Fatalf("extract survived: %s", translated)
	}
	if !strings.Contains(translated, "/1000.0") {
		t.Fatalf("epoch conversion missing: %s", translated)
	}
}

func TestTranslateSQLSessionAuthorityAndAdvisoryLock(t *testing.T) {
	translated, args, err := translateSQL(`SELECT set_config('whalerein.session_authority',$1,true)`, []any{"rev"})
	if err != nil || !strings.Contains(translated, "INSERT OR REPLACE INTO app_settings") || args[0] != "rev" {
		t.Fatalf("set_config = %q %#v %v", translated, args, err)
	}
	translated, args, err = translateSQL(`SELECT pg_advisory_xact_lock($1)`, []any{int64(1)})
	if err != nil || translated != "SELECT 1" || len(args) != 0 {
		t.Fatalf("advisory = %q %#v %v", translated, args, err)
	}
}

func TestTranslateSQLNormalizesArguments(t *testing.T) {
	translated, args, err := translateSQL(`SELECT $1,$2,$3`, []any{true, uint64(4), []byte{1}})
	if err != nil || translated != "SELECT ?,?,?" {
		t.Fatalf("translate = %q %v", translated, err)
	}
	if args[0] != int64(1) || args[1] != int64(4) {
		t.Fatalf("args = %#v", args)
	}
	if _, _, err := translateSQL(`SELECT $2`, []any{"only"}); err == nil {
		t.Fatal("missing placeholder argument was accepted")
	}
}

func TestNormalizeSQLiteErrorsAreTyped(t *testing.T) {
	cases := []struct {
		message string
		want    error
	}{
		{"database is locked", ErrUnavailable},
		{"database or disk is full", ErrStorageFull},
		{"database disk image is malformed", ErrCorrupt},
		{"UNIQUE constraint failed: devices.device_id", ErrConflict},
	}
	for _, tc := range cases {
		err := normalizeSQLiteError(errors.New(tc.message))
		if !errors.Is(err, tc.want) {
			t.Fatalf("%q mapped to %v, want %v", tc.message, err, tc.want)
		}
	}
	if normalizeSQLiteError(nil) != nil {
		t.Fatal("nil error normalized")
	}
	if normalizeSQLiteError(errors.New("other")) == nil {
		t.Fatal("unknown error lost")
	}
	for _, message := range []string{"database table is locked", "disk i/o error", "file is not a database"} {
		if normalizeSQLiteError(errors.New(message)) == nil {
			t.Fatalf("%q lost", message)
		}
	}
	if _, err := Open(context.Background(), "/"); err == nil {
		t.Fatal("directory path opened")
	}
	var at time.Time
	if err := assignValue(&at, "not-a-time"); err == nil {
		t.Fatal("bad timestamp accepted")
	}
}

func TestSessionAuthorityDoesNotLeakAcrossTransactions(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	tx, err := data.poolBegin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec(ctx, `SELECT set_config('whalerein.session_authority',$1,true)`, "revision-1"); err != nil {
		t.Fatal(err)
	}
	if err := tx.Commit(ctx); err != nil {
		t.Fatal(err)
	}
	var count int
	if err := data.db.QueryRow(ctx, `SELECT count(*) FROM app_settings WHERE key='whalerein.session_authority'`).Scan(&count); err != nil {
		t.Fatal(err)
	}
	if count != 0 {
		t.Fatalf("session authority leaked across transactions: %d", count)
	}
}

func TestSQLiteSeamQueriesRowsAndSnapshot(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	rows, err := data.db.Query(ctx, `SELECT $1, $2, $3, $4`, int64(7), true, time.Now().UTC(), []byte{1, 2})
	if err != nil {
		t.Fatal(err)
	}
	if !rows.Next() {
		t.Fatal("no row")
	}
	var number int64
	var flag bool
	var at time.Time
	var raw []byte
	if err := rows.Scan(&number, &flag, &at, &raw); err != nil {
		t.Fatal(err)
	}
	if number != 7 || !flag || at.IsZero() || len(raw) != 2 {
		t.Fatalf("scan = %d %t %v %v", number, flag, at, raw)
	}
	if rows.Next() {
		t.Fatal("unexpected second row")
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	rows.Close()

	var value string
	if err := data.db.QueryRow(ctx, `SELECT 'one'`).Scan(&value); err != nil || value != "one" {
		t.Fatalf("query row = %q %v", value, err)
	}
	if err := data.db.QueryRow(ctx, `SELECT 1 FROM requests WHERE request_id='missing'`).Scan(&value); err == nil {
		t.Fatal("missing row did not return an error")
	}
	if _, err := data.db.Exec(ctx, `INSERT INTO hosts(host_id) VALUES (NULL)`); err == nil {
		t.Fatal("invalid insert was accepted")
	}

	backup := t.TempDir() + "/backup.db"
	if err := data.Snapshot(ctx, backup); err != nil {
		t.Fatal(err)
	}
	if err := data.Snapshot(ctx, ""); err == nil {
		t.Fatal("empty snapshot destination accepted")
	}
	if err := (&Store{}).Snapshot(ctx, backup); err == nil {
		t.Fatal("non-SQLite store snapshot accepted")
	}
}

func TestOpenRejectsNetworkDSN(t *testing.T) {
	if _, err := Open(context.Background(), ""); err == nil {
		t.Fatal("empty path accepted")
	}
	if _, err := Open(context.Background(), "postgres://host/db"); err == nil {
		t.Fatal("network DSN accepted")
	}
}

func TestValueConversions(t *testing.T) {
	for value, want := range map[string]string{"x": "x", "": ""} {
		if got := stringValue(value); got != want {
			t.Fatalf("stringValue(%q) = %q", value, got)
		}
	}
	if stringValue([]byte("b")) != "b" || stringValue(nil) != "" || stringValue(int64(3)) != "3" {
		t.Fatal("stringValue variants")
	}
	if intValue("42") != 42 || intValue(true) != 1 || intValue(3.5) != 3 || intValue(nil) != 0 {
		t.Fatal("intValue variants")
	}
	if uintValue(-1) != 0 || uintValue(4) != 4 {
		t.Fatal("uintValue variants")
	}
	if floatValue("1.5") != 1.5 || floatValue(2) != 2 || floatValue(nil) != 0 {
		t.Fatal("floatValue variants")
	}
	if !boolValue("true") || !boolValue([]byte("1")) || boolValue("no") || !boolValue(int64(2)) {
		t.Fatal("boolValue variants")
	}
	if parsed, ok := parseTimeString("2026-11-26T12:00:00Z"); !ok || parsed.IsZero() {
		t.Fatal("RFC3339 parse")
	}
	if parsed, ok := parseTimeString("1760000000000"); !ok || parsed.IsZero() {
		t.Fatal("millisecond parse")
	}
	if _, ok := parseTimeString("not-a-time"); ok {
		t.Fatal("bad time accepted")
	}
	if _, ok := timeValue(nil); ok {
		t.Fatal("nil time accepted")
	}
	if parsed, ok := timeValue(int64(1000)); !ok || parsed.IsZero() {
		t.Fatal("int time")
	}
	if parsed, ok := timeValue("1760000000000"); !ok || parsed.IsZero() {
		t.Fatal("string time")
	}
	if _, ok := timeValue(struct{}{}); ok {
		t.Fatal("unknown time accepted")
	}
}

func TestAssignValueCoverage(t *testing.T) {
	var anyValue any
	if err := assignValue(&anyValue, 1); err != nil || anyValue != 1 {
		t.Fatal("any")
	}
	var str string
	if err := assignValue(&str, []byte("x")); err != nil || str != "x" {
		t.Fatal("string")
	}
	var raw []byte
	if err := assignValue(&raw, "x"); err != nil || string(raw) != "x" {
		t.Fatal("bytes")
	}
	var at *time.Time
	if err := assignValue(&at, nil); err != nil || at != nil {
		t.Fatal("nil time")
	}
	if err := assignValue(&at, int64(1000)); err != nil || at == nil {
		t.Fatal("time")
	}
	var number int
	var number32 int32
	var number64 int64
	var unsigned uint32
	var unsigned64 uint64
	var floating float64
	var floating32 float32
	var flag bool
	for _, target := range []any{&number, &number32, &number64, &unsigned, &unsigned64, &floating, &floating32, &flag} {
		if err := assignValue(target, int64(2)); err != nil {
			t.Fatalf("numeric %T: %v", target, err)
		}
	}
	if number != 2 || number32 != 2 || number64 != 2 || unsigned != 2 || unsigned64 != 2 || floating != 2 || floating32 != 2 || !flag {
		t.Fatal("numeric conversions")
	}
	var nullable *int64
	if err := assignValue(&nullable, nil); err != nil || nullable != nil {
		t.Fatal("nil pointer")
	}
	if err := assignValue(&nullable, int64(3)); err != nil || nullable == nil || *nullable != 3 {
		t.Fatal("pointer")
	}
	var bad struct{}
	if err := assignValue(&bad, 1); err == nil {
		t.Fatal("unsupported destination accepted")
	}
	if err := assignValue(&str, 1); err != nil || str != "1" {
		t.Fatal("fallback string")
	}
}

func TestTranslateSQLEdgeCases(t *testing.T) {
	for unit, want := range map[string]string{"seconds": "1000", "minutes": "60000", "hours": "3600000", "days": "86400000"} {
		translated, _, err := translateSQL(`SELECT now() - interval '1 `+unit+`'`, nil)
		if err != nil || !strings.Contains(translated, want) {
			t.Fatalf("%s = %q %v", unit, translated, err)
		}
	}
	if _, _, err := translateSQL(`SELECT set_config('whalerein.session_authority')`, nil); err == nil {
		t.Fatal("set_config without an argument accepted")
	}
	if _, _, err := translateSQL(`SELECT $0`, []any{1}); err == nil {
		t.Fatal("zero placeholder accepted")
	}
}

func TestNormalizeArgVariants(t *testing.T) {
	now := time.Unix(1, 0)
	if normalizeArg(now) != now.UnixMilli() || normalizeArg(&now) != now.UnixMilli() {
		t.Fatal("time conversion")
	}
	if normalizeArg((*time.Time)(nil)) != nil {
		t.Fatal("nil time pointer")
	}
	if normalizeArg(false) != int64(0) || normalizeArg(uint32(2)) != int64(2) || normalizeArg(float32(1.5)) != 1.5 {
		t.Fatal("scalar conversion")
	}
	if values, ok := normalizeArg([]any{true, uint64(2)}).([]any); !ok || len(values) != 2 {
		t.Fatal("any slice")
	}
	if values, ok := normalizeArg([]string{"a"}).([]any); !ok || len(values) != 1 {
		t.Fatal("string slice")
	}
}

func TestMigrateRefusesChangedChecksum(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := data.db.Exec(ctx, `UPDATE schema_migrations SET checksum=x'00' WHERE name='001_schema.sql'`); err != nil {
		t.Fatal(err)
	}
	if err := data.Migrate(ctx); err == nil || !strings.Contains(err.Error(), "checksum changed") {
		t.Fatalf("changed checksum accepted: %v", err)
	}
}

func TestSQLiteTransactionSeamAndPing(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if err := data.Ping(ctx); err != nil {
		t.Fatal(err)
	}
	tx, err := data.poolBegin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	rows, err := tx.Query(ctx, `SELECT $1`, int64(1))
	if err != nil {
		t.Fatal(err)
	}
	if !rows.Next() {
		t.Fatal("no transaction row")
	}
	var value int
	if err := rows.Scan(&value); err != nil || value != 1 {
		t.Fatalf("transaction scan = %d %v", value, err)
	}
	rows.Close()
	var other string
	if err := tx.QueryRow(ctx, `SELECT 'tx'`).Scan(&other); err != nil || other != "tx" {
		t.Fatalf("transaction row = %q %v", other, err)
	}
	if _, err := tx.Exec(ctx, `INSERT INTO app_settings(key,value) VALUES ($1,$2)`, "k", "v"); err != nil {
		t.Fatal(err)
	}
	if err := tx.Rollback(ctx); err != nil {
		t.Fatal(err)
	}
}

func TestMoreValueConversions(t *testing.T) {
	if parsed, ok := timeValue(float64(1000)); !ok || parsed.IsZero() {
		t.Fatal("float time")
	}
	if parsed, ok := timeValue([]byte("1760000000000")); !ok || parsed.IsZero() {
		t.Fatal("bytes time")
	}
	if parsed, ok := timeValue(1000); !ok || parsed.IsZero() {
		t.Fatal("int time")
	}
	if !boolValue("1") || !boolValue([]byte("true")) || boolValue(nil) {
		t.Fatal("bool variants")
	}
	if stringValue([]byte{}) != "" || stringValue(false) != "false" {
		t.Fatal("string variants")
	}
	var slice []byte
	if err := assignValue(&slice, int64(1)); err != nil || string(slice) != "1" {
		t.Fatal("fallback bytes")
	}
	var pointer **int64
	if err := assignValue(&pointer, nil); err != nil || pointer != nil {
		t.Fatal("double nil pointer")
	}
}

func TestErrorRowReturnsItsError(t *testing.T) {
	if err := (errorRow{err: ErrNotFound}).Scan(); err != ErrNotFound {
		t.Fatalf("error row = %v", err)
	}
}

func TestSnapshotRefusesExistingDestination(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if err := data.Snapshot(ctx, path); err == nil {
		t.Fatal("snapshot overwrote a live database")
	}
	if translated, _, err := translateSQL(`SELECT 1 WHERE x = ANY($1)`, []any{"scalar"}); err != nil || !strings.Contains(translated, "ANY(?)") {
		t.Fatalf("scalar ANY = %q %v", translated, err)
	}
	if _, err := Open(ctx, "/missing-parent/access.db"); err == nil {
		t.Fatal("unwritable path opened")
	}
}

func TestRevocationScopeHelpers(t *testing.T) {
	ref := ConnectionRef{Epoch: "e", ID: 1}
	if !ref.Valid() || ref.String() != "e:1" {
		t.Fatal("valid connection ref")
	}
	if (ConnectionRef{}).String() != "" {
		t.Fatal("empty connection ref string")
	}
	if (ConnectionRef{Epoch: "bad/epoch", ID: 1}).Valid() || (ConnectionRef{}).Valid() {
		t.Fatal("invalid connection ref accepted")
	}
	if parsed, ok := ParseConnectionRef("e:2"); !ok || parsed.ID != 2 {
		t.Fatal("parse connection ref")
	}
	if _, ok := ParseConnectionRef("e:0"); ok {
		t.Fatal("zero connection accepted")
	}
	if _, ok := ParseConnectionRef("bad/epoch:1"); ok {
		t.Fatal("bad epoch accepted")
	}
	if (ScopeConnection{Ref: ref, ClientID: "client"}).Valid() != true || (ScopeConnection{Ref: ref}).Valid() {
		t.Fatal("scope connection validity")
	}
	grant := ScopeGrant{GrantGeneration: 1, RevocationGeneration: 2, DeviceRevocationGeneration: 3}
	if grant.Revision() == 0 {
		t.Fatal("revision")
	}
	scope := RevocationScope{HostID: "h", Grants: []ScopeGrant{{Connections: []ScopeConnection{{Ref: ref, ClientID: "c"}}}}}
	frozen := scope.Frozen()
	frozen.Grants[0].Connections[0].ClientID = "changed"
	if scope.Grants[0].Connections[0].ClientID != "c" {
		t.Fatal("frozen scope shares connections")
	}
}

func TestNextNodeSequenceIsDurableAndMonotonic(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	for want := uint64(1); want <= 3; want++ {
		got, err := data.NextNodeSequence(ctx)
		if err != nil || got != want {
			t.Fatalf("sequence = %d %v, want %d", got, err, want)
		}
	}
	reopened, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	if got, err := reopened.NextNodeSequence(ctx); err != nil || got != 4 {
		t.Fatalf("reopened sequence = %d %v, want 4", got, err)
	}
	if _, err := (&Store{}).NextNodeSequence(ctx); err == nil {
		t.Fatal("empty store returned a sequence")
	}
	if err := (&Store{sqlite: true}).Migrate(ctx); err == nil {
		t.Fatal("SQLite store without a database migrated")
	}
}

func TestCorruptStorageFailsBeforeServing(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	if err := os.WriteFile(path, []byte("this is not a sqlite database"), 0o600); err != nil {
		t.Fatal(err)
	}
	data, err := Open(ctx, path)
	if err != nil {
		return // refusing at open is also correct
	}
	defer data.Close()
	if err := data.Migrate(ctx); err == nil {
		t.Fatal("corrupt database migrated successfully")
	}
}

func TestSQLiteDiskFullReturnsExplicitFailureAndRecovers(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	now := time.Now().UTC().Truncate(time.Millisecond)
	if err := data.CreatePairedDevice(ctx, "host", "device", "fingerprint", []byte("auth"), now); err != nil {
		t.Fatal(err)
	}
	if _, err := data.db.Exec(ctx, `PRAGMA max_page_count=256`); err != nil {
		t.Fatal(err)
	}
	body := make([]byte, 512<<10)
	failed := false
	for attempt := 0; attempt < 8; attempt++ {
		request := Request{
			RequestID: "disk-" + string(rune('a'+attempt)), HostID: "host", DeviceID: "device",
			CommandID: "cmd-" + string(rune('a'+attempt)), Kind: 3, ProtocolVersion: 1, RelayVersion: 5,
			Status: whalereinv1.OnlineStatus_received, RequestBody: body,
			ExpiresAt: now.Add(time.Hour), CreatedAt: now, UpdatedAt: now,
			Stamp: &GrantStamp{ResultLimitBytes: 1 << 20},
		}
		_, err := data.CreateRequest(ctx, request)
		if err != nil {
			failed = true
			if !errors.Is(err, ErrStorageFull) && !errors.Is(err, ErrUnavailable) {
				t.Fatalf("disk full error = %v", err)
			}
			break
		}
	}
	if !failed {
		t.Fatal("writes succeeded beyond the page limit")
	}
	if _, err := data.db.Exec(ctx, `PRAGMA max_page_count=1000000`); err != nil {
		t.Fatal(err)
	}
	if _, err := data.db.Exec(ctx, `DELETE FROM requests`); err != nil {
		t.Fatal(err)
	}
	if _, err := data.EnsureHost(ctx, "recovered"); err != nil {
		t.Fatalf("store did not recover after freeing space: %v", err)
	}
}

func TestSQLiteBusyWriterReturnsRetryableUnavailable(t *testing.T) {
	ctx := context.Background()
	path := t.TempDir() + "/access.db"
	data, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	blocker, err := sql.Open("sqlite", "file:"+path+"?_pragma=busy_timeout(100)")
	if err != nil {
		t.Fatal(err)
	}
	defer blocker.Close()
	tx, err := blocker.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	if _, err := tx.Exec(`UPDATE app_settings SET value=value WHERE key='control.sequence'`); err != nil {
		// The row may not exist yet; insert a write lock through the table.
		if _, err := tx.Exec(`INSERT INTO app_settings(key,value) VALUES ('busy-probe','1')`); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := data.EnsureHost(ctx, "blocked"); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("busy write error = %v, want storage unavailable", err)
	}
	if data.BusyErrors() == 0 {
		t.Fatal("busy error was not counted")
	}
}

func TestEarliestSessionExpiryIsAnAggregate(t *testing.T) {
	ctx := context.Background()
	data, err := Open(ctx, t.TempDir()+"/access.db")
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if _, ok, err := data.EarliestSessionExpiry(ctx); err != nil || ok {
		t.Fatalf("empty expiry = %v %v", ok, err)
	}
	now := time.Now().UTC().Truncate(time.Millisecond)
	for index, expiry := range []time.Time{now.Add(time.Hour), now.Add(2 * time.Hour)} {
		if err := data.InstallDeviceSession(ctx, DeviceSession{
			ID: "session-" + string(rune('a'+index)), WorkspaceID: "workspace", RegionID: "region",
			DeviceID: "device", KeyFingerprint: "fingerprint", AuthorityRevision: "revision",
			CredentialHash: bytes.Repeat([]byte{byte(index + 1)}, 32), Revision: int64(index + 1),
			ExpiresAt: expiry,
		}); err != nil {
			t.Fatal(err)
		}
	}
	expires, ok, err := data.EarliestSessionExpiry(ctx)
	if err != nil || !ok || !expires.Equal(now.Add(time.Hour)) {
		t.Fatalf("earliest = %v %v %v", expires, ok, err)
	}
}

func TestPendingRoutesAreDurableAndGenerationScoped(t *testing.T) {
	ctx := context.Background()
	data, err := Open(ctx, t.TempDir()+"/access.db")
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if err := data.SavePendingRoute(ctx, PendingRoute{
		HostID: "host", Kind: PendingAttach, Generation: 2, Payload: []byte("attach"),
	}); err != nil {
		t.Fatal(err)
	}
	routes, err := data.ListPendingRoutes(ctx)
	if err != nil || len(routes) != 1 || routes[0].Kind != PendingAttach || routes[0].Generation != 2 {
		t.Fatalf("pending = %#v %v", routes, err)
	}
	if err := data.SavePendingRoute(ctx, PendingRoute{
		HostID: "host", Kind: PendingClose, Generation: 3, Payload: []byte("close"),
	}); err != nil {
		t.Fatal(err)
	}
	if err := data.DeletePendingRoute(ctx, "host", 2); err != nil {
		t.Fatal(err)
	}
	routes, err = data.ListPendingRoutes(ctx)
	if err != nil || len(routes) != 1 || routes[0].Generation != 3 {
		t.Fatalf("generation-scoped delete = %#v %v", routes, err)
	}
	if err := data.DeletePendingRoute(ctx, "host", 3); err != nil {
		t.Fatal(err)
	}
	routes, err = data.ListPendingRoutes(ctx)
	if err != nil || len(routes) != 0 {
		t.Fatalf("remaining = %#v %v", routes, err)
	}
	if err := data.SavePendingRoute(ctx, PendingRoute{HostID: "host", Kind: PendingAttach, Payload: []byte("x")}); err == nil {
		t.Fatal("invalid pending route accepted")
	}
}

func TestPendingRouteSeamFailsClosedWithoutADatabase(t *testing.T) {
	var data *Store
	if err := data.SavePendingRoute(context.Background(), PendingRoute{HostID: "h", Kind: PendingAttach, Generation: 1, Payload: []byte("x")}); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("save = %v", err)
	}
	if err := data.DeletePendingRoute(context.Background(), "h", 1); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("delete = %v", err)
	}
	if _, err := data.ListPendingRoutes(context.Background()); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("list = %v", err)
	}
}

func TestPendingRoutesReportAClosedStore(t *testing.T) {
	data, err := Open(context.Background(), t.TempDir()+"/access.db")
	if err != nil {
		t.Fatal(err)
	}
	if err := data.Migrate(context.Background()); err != nil {
		t.Fatal(err)
	}
	data.Close()
	if _, err := data.ListPendingRoutes(context.Background()); err == nil {
		t.Fatal("closed store listed pending routes")
	}
}

func TestDeviceSessionPersistsTheSessionProofKey(t *testing.T) {
	ctx := context.Background()
	data, err := Open(ctx, t.TempDir()+"/access.db")
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	key := bytes.Repeat([]byte{7}, 32)
	expires := time.Now().UTC().Add(time.Hour)
	if err := data.InstallDeviceSession(ctx, DeviceSession{
		ID: "session", WorkspaceID: "workspace", RegionID: "region", DeviceID: "device",
		KeyFingerprint: "fingerprint", AuthorityRevision: "revision",
		CredentialHash: bytes.Repeat([]byte{1}, 32), ProofPublicKey: key,
		Revision: 1, ExpiresAt: expires,
	}); err != nil {
		t.Fatal(err)
	}
	loaded, err := data.ActiveDeviceSession(ctx, "session", time.Now().UTC())
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(loaded.ProofPublicKey, key) {
		t.Fatalf("proof key = %x", loaded.ProofPublicKey)
	}
}

func TestAuthorityScopesListLiveGrantedSessionsOnly(t *testing.T) {
	ctx := context.Background()
	data, err := Open(ctx, t.TempDir()+"/access.db")
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := data.db.Exec(ctx, `INSERT INTO grants (host_id,device_id,grant_generation) VALUES ('host','device',4)`); err != nil {
		t.Fatal(err)
	}
	key := bytes.Repeat([]byte{9}, 32)
	if err := data.InstallDeviceSession(ctx, DeviceSession{
		ID: "session", WorkspaceID: "workspace", RegionID: "region", DeviceID: "device",
		KeyFingerprint: "fingerprint", AuthorityRevision: "revision",
		CredentialHash: bytes.Repeat([]byte{2}, 32), ProofPublicKey: key,
		Revision: 1, ExpiresAt: time.Now().UTC().Add(time.Hour),
	}); err != nil {
		t.Fatal(err)
	}
	scopes, err := data.AuthorityScopes(ctx, time.Now().UTC())
	if err != nil || len(scopes) != 1 {
		t.Fatalf("scopes = %#v %v", scopes, err)
	}
	if scopes[0].HostID != "host" || scopes[0].GrantGeneration != 4 || !bytes.Equal(scopes[0].ProofPublicKey, key) {
		t.Fatalf("scope = %#v", scopes[0])
	}
	if _, err := data.db.Exec(ctx, `UPDATE grants SET revoked_at=unixepoch()*1000 WHERE host_id='host'`); err != nil {
		t.Fatal(err)
	}
	scopes, err = data.AuthorityScopes(ctx, time.Now().UTC())
	if err != nil || len(scopes) != 0 {
		t.Fatalf("revoked scopes = %#v %v", scopes, err)
	}
}

func TestAuthorityScopesFailClosedWithoutAStore(t *testing.T) {
	var data *Store
	if _, err := data.AuthorityScopes(context.Background(), time.Now()); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("nil store = %v", err)
	}
}

func TestDeviceSessionHasProofTracksTheBoundKey(t *testing.T) {
	ctx := context.Background()
	data, err := Open(ctx, t.TempDir()+"/access.db")
	if err != nil {
		t.Fatal(err)
	}
	defer data.Close()
	if err := data.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if err := data.InstallDeviceSession(ctx, DeviceSession{
		ID: "session", WorkspaceID: "workspace", RegionID: "region", DeviceID: "device",
		KeyFingerprint: "fingerprint", AuthorityRevision: "revision",
		CredentialHash: bytes.Repeat([]byte{1}, 32), ProofPublicKey: bytes.Repeat([]byte{2}, 32),
		Revision: 1, ExpiresAt: time.Now().UTC().Add(time.Hour),
	}); err != nil {
		t.Fatal(err)
	}
	expected, err := data.DeviceSessionHasProof(ctx, "session")
	if err != nil || !expected {
		t.Fatalf("proof expectation = %v %v", expected, err)
	}
	if _, err := data.db.Exec(ctx, `UPDATE device_sessions SET proof_public_key=NULL WHERE session_id='session'`); err != nil {
		t.Fatal(err)
	}
	expected, err = data.DeviceSessionHasProof(ctx, "session")
	if err != nil || expected {
		t.Fatalf("cleared proof expectation = %v %v", expected, err)
	}
	if _, err := data.DeviceSessionHasProof(ctx, "missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing session = %v", err)
	}
}
