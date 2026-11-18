package store_test

// Real-PostgreSQL acceptance for the documented migration guarantee: missing
// migrations are applied once, in one transaction, under an advisory lock, so a
// process that dies mid-upgrade cannot leave a partially committed schema.
//
// The harness re-executes this test binary in migration mode (see TestMain).
// One harness applies the upgrade and blocks inside its open transaction on a
// table lock held by the test, a second harness waits for the advisory lock,
// and the test terminates the first harness's backend. PostgreSQL must roll the
// open transaction back; the waiting harness then completes the upgrade.
//
// The older state is the real embedded ledger truncated to migration 013's
// predecessor. It is built with the same SQL and checksum bytes as production,
// so the target build sees a genuinely incomplete ledger and real pending DDL.

import (
	"bytes"
	"context"
	"crypto/sha256"
	"embed"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"os/exec"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/whalerein/whalerein/access/internal/store"
	"github.com/whalerein/whalerein/access/internal/store/storetest"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

//go:embed migrations/*.sql
var e02MigrationsFS embed.FS

const (
	e02HarnessEnv      = "WHALEREIN_MIGRATION_HARNESS_DSN"
	e02OlderMigrations = 12
	e02HostID          = "e02-migration-host"
	e02DeviceID        = "e02-migration-device"
	e02AuthHash        = "e02-credential-hash"
	e02RequestID       = "e02-migration-request"
	e02CommandID       = "e02-migration-command"
)

// When the binary is started in harness mode it opens the configured schema and
// runs the production migration path once, then exits with the real outcome.
func TestMain(m *testing.M) {
	if dsn := os.Getenv(e02HarnessEnv); dsn != "" {
		os.Exit(e02RunMigrationHarness(dsn))
	}
	os.Exit(m.Run())
}

func e02RunMigrationHarness(dsn string) int {
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	opened, err := store.Open(ctx, dsn)
	if err != nil {
		fmt.Fprintln(os.Stderr, "migration harness open failed:", err)
		return 1
	}
	defer opened.Close()
	if err := opened.Migrate(ctx); err != nil {
		fmt.Fprintln(os.Stderr, "migration harness migrate failed:", err)
		return 1
	}
	return 0
}

// When a fresh schema receives one normal migration and then repeated startups,
// the ledger and every persisted business identity stay unchanged.
func TestMigrationSingleProcessUpgradeAndRepeatedStartup(t *testing.T) {
	ctx := e02Context(t)
	db := storetest.NewSchema(t)
	raw := e02SimplePool(t, ctx, db.DSN)
	fixture := e02Pool(t, ctx, db.DSN)
	e02ApplyOlderSchema(t, ctx, raw, fixture)
	older := store.NewWithPool(fixture)
	e02SeedOlderBusinessState(t, ctx, older)
	before := e02ReadIdentity(t, ctx, fixture, older)
	e02AssertOlderLedger(t, ctx, fixture)

	opened, err := store.Open(ctx, db.DSN)
	if err != nil {
		t.Fatalf("open upgraded store: %v", err)
	}
	defer opened.Close()
	if err := opened.Migrate(ctx); err != nil {
		t.Fatalf("normal single-process migration: %v", err)
	}
	e02AssertCompleteLedger(t, ctx, fixture)
	e02AssertUpgradeObjects(t, ctx, fixture)
	if after := e02ReadIdentity(t, ctx, fixture, opened); !reflect.DeepEqual(before, after) {
		t.Fatalf("migration changed business identity:\nbefore=%+v\nafter=%+v", before, after)
	}

	// Repeated startup is the positive control: an already-current ledger is
	// accepted (no rewrite, no unknown-version rejection) and durable state stays
	// identical.
	complete := e02LedgerRows(t, ctx, fixture)
	second, err := store.Open(ctx, db.DSN)
	if err != nil {
		t.Fatalf("open store for repeated startup: %v", err)
	}
	defer second.Close()
	if err := second.Migrate(ctx); err != nil {
		t.Fatalf("repeated startup rejected a current ledger: %v", err)
	}
	if rows := e02LedgerRows(t, ctx, fixture); !reflect.DeepEqual(complete, rows) {
		t.Fatalf("repeated startup changed the migration ledger:\nfirst=%v\nsecond=%v", complete, rows)
	}
	if after := e02ReadIdentity(t, ctx, fixture, second); !reflect.DeepEqual(before, after) {
		t.Fatalf("repeated startup changed business identity:\nbefore=%+v\nafter=%+v", before, after)
	}
}

// When the transaction holding the migration advisory lock is terminated while
// another process waits, the interrupted migration must not commit any of its
// statements and the waiting process must complete the upgrade.
func TestMigrationInterruptionPreservesAtomicUpgrade(t *testing.T) {
	ctx := e02Context(t)
	db := storetest.NewSchema(t)
	raw := e02SimplePool(t, ctx, db.DSN)
	fixture := e02Pool(t, ctx, db.DSN)
	e02ApplyOlderSchema(t, ctx, raw, fixture)
	older := store.NewWithPool(fixture)
	e02SeedOlderBusinessState(t, ctx, older)
	before := e02ReadIdentity(t, ctx, fixture, older)
	e02AssertOlderLedger(t, ctx, fixture)
	if e02RelationExists(t, ctx, fixture, "host_signature_replays") {
		t.Fatal("older fixture unexpectedly contains migration 013 state")
	}

	// Hold a conflicting lock on the table altered by migration 014. The upgrade
	// transaction can apply all of migration 013 and then blocks deterministically
	// at the first statement of migration 014, before either ledger row commits.
	lockConn := e02Connect(t, ctx, db.DSN)
	defer lockConn.Close(context.Background())
	if _, err := lockConn.Exec(ctx, "BEGIN"); err != nil {
		t.Fatalf("begin migration barrier: %v", err)
	}
	released := false
	defer func() {
		if !released {
			_, _ = lockConn.Exec(context.Background(), "ROLLBACK")
		}
	}()
	if _, err := lockConn.Exec(ctx, "LOCK TABLE external_operations IN ACCESS SHARE MODE"); err != nil {
		t.Fatalf("lock external_operations: %v", err)
	}

	admin := e02Connect(t, ctx, db.DSN)
	defer admin.Close(context.Background())

	first := e02StartHarness(t, db.DSN, "e02_first_"+db.Schema)
	firstPID := e02WaitForBarrier(t, ctx, admin, first.name, "first migration holds the advisory lock mid-transaction", func(state e02BackendState) bool {
		return state.HasAdvisoryLock && state.WaitingRelationLock
	})
	// The first harness has executed migration 013's statements and its ledger
	// insert inside its transaction. Nothing is visible to other sessions yet.
	e02AssertOlderLedger(t, ctx, fixture)
	if e02RelationExists(t, ctx, fixture, "host_signature_replays") {
		t.Fatal("interrupted migration committed part of migration 013 before termination")
	}

	second := e02StartHarness(t, db.DSN, "e02_second_"+db.Schema)
	e02WaitForBarrier(t, ctx, admin, second.name, "second migration waits for the advisory lock", func(state e02BackendState) bool {
		return state.WaitingAdvisoryLock && !state.HasAdvisoryLock
	})

	if _, err := admin.Exec(ctx, "SELECT pg_terminate_backend($1)", firstPID); err != nil {
		t.Fatalf("terminate first migration backend: %v", err)
	}
	if err := first.wait(30 * time.Second); err == nil {
		t.Fatal("interrupted migration process reported success after its backend was terminated")
	}
	secondPID := e02WaitForBarrier(t, ctx, admin, second.name, "second migration re-applies the upgrade after the termination", func(state e02BackendState) bool {
		return state.HasAdvisoryLock && state.WaitingRelationLock
	})
	if secondPID == firstPID {
		t.Fatal("second migration reused the terminated backend identity")
	}

	// The second transaction is now at the same barrier. If the first process had
	// committed any statement or ledger row, it would be visible here; it is not.
	e02AssertOlderLedger(t, ctx, fixture)
	if e02RelationExists(t, ctx, fixture, "host_signature_replays") {
		t.Fatal("terminated migration left a partially committed relation")
	}
	if e02ColumnExists(t, ctx, fixture, "external_operations", "outcome") {
		t.Fatal("terminated migration left a partially committed column")
	}
	if e02RelationExists(t, ctx, fixture, "host_proof_nonces") {
		t.Fatal("terminated migration left a partially committed table")
	}

	// Release the barrier; the waiting harness must complete and commit atomically.
	if _, err := lockConn.Exec(ctx, "ROLLBACK"); err != nil {
		t.Fatalf("release migration barrier: %v", err)
	}
	released = true
	if err := second.wait(60 * time.Second); err != nil {
		t.Fatalf("waiting migration did not complete the upgrade: %v", err)
	}

	e02AssertCompleteLedger(t, ctx, fixture)
	e02AssertUpgradeObjects(t, ctx, fixture)
	completed, err := store.Open(ctx, db.DSN)
	if err != nil {
		t.Fatalf("open completed store: %v", err)
	}
	defer completed.Close()
	if after := e02ReadIdentity(t, ctx, fixture, completed); !reflect.DeepEqual(before, after) {
		t.Fatalf("interrupted upgrade changed business identity:\nbefore=%+v\nafter=%+v", before, after)
	}
	if err := completed.Migrate(ctx); err != nil {
		t.Fatalf("startup after recovered upgrade: %v", err)
	}
	if after := e02ReadIdentity(t, ctx, fixture, completed); !reflect.DeepEqual(before, after) {
		t.Fatalf("startup after recovered upgrade changed business identity:\nbefore=%+v\nafter=%+v", before, after)
	}
}

type e02Migration struct {
	name     string
	raw      []byte
	checksum []byte
}

func e02MigrationList(t *testing.T) []e02Migration {
	t.Helper()
	entries, err := e02MigrationsFS.ReadDir("migrations")
	if err != nil {
		t.Fatalf("read embedded migrations: %v", err)
	}
	var migrations []e02Migration
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		raw, err := e02MigrationsFS.ReadFile("migrations/" + entry.Name())
		if err != nil {
			t.Fatalf("read migration %s: %v", entry.Name(), err)
		}
		sum := sha256.Sum256(raw)
		migrations = append(migrations, e02Migration{name: entry.Name(), raw: raw, checksum: sum[:]})
	}
	if len(migrations) <= e02OlderMigrations {
		t.Fatalf("need a migration newer than %d to exercise an upgrade; have %d", e02OlderMigrations, len(migrations))
	}
	return migrations
}

func e02SimplePool(t *testing.T, ctx context.Context, dsn string) *pgxpool.Pool {
	t.Helper()
	cfg, err := pgxpool.ParseConfig(dsn)
	if err != nil {
		t.Fatalf("parse fixture DSN: %v", err)
	}
	// Migration files contain several statements; the simple protocol executes
	// the file as PostgreSQL received it, matching the production apply path.
	cfg.ConnConfig.DefaultQueryExecMode = pgx.QueryExecModeSimpleProtocol
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		t.Fatalf("open fixture pool: %v", err)
	}
	t.Cleanup(pool.Close)
	return pool
}

func e02Pool(t *testing.T, ctx context.Context, dsn string) *pgxpool.Pool {
	t.Helper()
	pool, err := pgxpool.New(ctx, dsn)
	if err != nil {
		t.Fatalf("open fixture pool: %v", err)
	}
	t.Cleanup(pool.Close)
	return pool
}

func e02Context(t *testing.T) context.Context {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 180*time.Second)
	t.Cleanup(cancel)
	return ctx
}

func e02Connect(t *testing.T, ctx context.Context, dsn string) *pgx.Conn {
	t.Helper()
	conn, err := pgx.Connect(ctx, dsn)
	if err != nil {
		t.Fatalf("connect fixture session: %v", err)
	}
	return conn
}

// e02ApplyOlderSchema builds the exact schema and ledger an older build would
// have left: the embedded migrations through 012 with their production checksums.
func e02ApplyOlderSchema(t *testing.T, ctx context.Context, raw, fixture *pgxpool.Pool) {
	t.Helper()
	if _, err := fixture.Exec(ctx, `CREATE TABLE schema_migrations (
 name text PRIMARY KEY, checksum bytea NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())`); err != nil {
		t.Fatalf("create migration ledger: %v", err)
	}
	for _, migration := range e02MigrationList(t)[:e02OlderMigrations] {
		if _, err := raw.Exec(ctx, string(migration.raw)); err != nil {
			t.Fatalf("apply older migration %s: %v", migration.name, err)
		}
		if _, err := fixture.Exec(ctx, "INSERT INTO schema_migrations(name,checksum) VALUES ($1,$2)", migration.name, migration.checksum); err != nil {
			t.Fatalf("record older migration %s: %v", migration.name, err)
		}
	}
}

type e02Identity struct {
	HostID                     string
	HostFingerprint            string
	HostProtocolVersion        int32
	DeviceID                   string
	DeviceFingerprint          string
	DeviceRevocationGeneration int64
	CredentialDevice           string
	ActiveGrant                bool
	GrantGeneration            int64
	RequestID                  string
	CommandID                  string
	PayloadDigest              []byte
	Status                     whalereinv1.OnlineStatus
	ResultBody                 []byte
	BodiesCleared              *bool
	ForwardProvenanceKnown     bool
}

func parseE02Status(value string) whalereinv1.OnlineStatus {
	number, ok := whalereinv1.OnlineStatus_value[value]
	if !ok {
		return whalereinv1.OnlineStatus_online_status_unspecified
	}
	return whalereinv1.OnlineStatus(number)
}

func e02SeedOlderBusinessState(t *testing.T, ctx context.Context, s *store.Store) {
	t.Helper()
	now := time.Now().UTC()
	if _, err := s.EnsureHost(ctx, e02HostID); err != nil {
		t.Fatalf("seed host: %v", err)
	}
	if err := s.UpdateHostMetadata(ctx, e02HostID, "e02-host-fingerprint", 2, now); err != nil {
		t.Fatalf("seed host metadata: %v", err)
	}
	if err := s.CreatePairedDevice(ctx, e02HostID, e02DeviceID, "e02-device-fingerprint", []byte(e02AuthHash), now); err != nil {
		t.Fatalf("seed paired device: %v", err)
	}
	// Seed with the historical column set: this schema predates the request
	// lease columns, so today's CreateRequest/GetRequest cannot be used as the
	// older-build stand-in.
	if _, err := s.Pool().Exec(ctx, `
		INSERT INTO requests(request_id,host_id,device_id,command_id,payload_digest,kind,
			protocol_version,status,request_body,result_body,expires_at,forward_state,
			forward_after,forward_attempts,published_once,created_at,updated_at,
			forward_provenance_known)
		VALUES($1,$2,$3,$4,$5,$6,1,'succeeded',$7,$8,$9,'none',$10,0,false,$10,$10,true)`,
		e02RequestID, e02HostID, e02DeviceID, e02CommandID, []byte("e02-command-digest"),
		int32(whalereinv1.RequestKind_REQUEST_KIND_COMMAND),
		[]byte("e02-request-body"), []byte("e02-result-body"), now.Add(time.Hour), now); err != nil {
		t.Fatalf("seed completed command: %v", err)
	}
	if _, err := s.Pool().Exec(ctx, "UPDATE grants SET grant_generation = 7 WHERE host_id = $1 AND device_id = $2", e02HostID, e02DeviceID); err != nil {
		t.Fatalf("seed grant generation: %v", err)
	}
}

func e02ReadIdentity(t *testing.T, ctx context.Context, pool *pgxpool.Pool, s *store.Store) e02Identity {
	t.Helper()
	host, err := s.GetHost(ctx, e02HostID)
	if err != nil {
		t.Fatalf("read host after migration: %v", err)
	}
	device, err := s.DeviceByID(ctx, e02DeviceID)
	if err != nil {
		t.Fatalf("read device after migration: %v", err)
	}
	credential, err := s.ActiveDeviceByAuthHash(ctx, []byte(e02AuthHash))
	if err != nil {
		t.Fatalf("read credential after migration: %v", err)
	}
	active, err := s.HasActiveGrant(ctx, e02HostID, e02DeviceID)
	if err != nil {
		t.Fatalf("read grant after migration: %v", err)
	}
	var requestID, commandID, status string
	var payloadDigest, resultBody []byte
	var bodiesCleared *bool
	var forwardProvenanceKnown bool
	if err := pool.QueryRow(ctx, `SELECT request_id,command_id,payload_digest,status,result_body,bodies_cleared,forward_provenance_known FROM requests WHERE request_id = $1`, e02RequestID).
		Scan(&requestID, &commandID, &payloadDigest, &status, &resultBody, &bodiesCleared, &forwardProvenanceKnown); err != nil {
		t.Fatalf("read command recovery row after migration: %v", err)
	}
	identity := e02Identity{
		HostID:                 host.HostID,
		HostFingerprint:        host.Fingerprint,
		HostProtocolVersion:    int32(host.ProtocolVersion),
		DeviceID:               device.DeviceID,
		DeviceFingerprint:      device.PublicKeyFingerprint,
		CredentialDevice:       credential.DeviceID,
		ActiveGrant:            active,
		RequestID:              requestID,
		CommandID:              commandID,
		PayloadDigest:          append([]byte(nil), payloadDigest...),
		Status:                 parseE02Status(status),
		ResultBody:             append([]byte(nil), resultBody...),
		BodiesCleared:          bodiesCleared,
		ForwardProvenanceKnown: forwardProvenanceKnown,
	}
	if err := pool.QueryRow(ctx, "SELECT revocation_generation FROM devices WHERE device_id = $1", e02DeviceID).Scan(&identity.DeviceRevocationGeneration); err != nil {
		t.Fatalf("read device revocation generation: %v", err)
	}
	if err := pool.QueryRow(ctx, "SELECT grant_generation FROM grants WHERE host_id = $1 AND device_id = $2", e02HostID, e02DeviceID).Scan(&identity.GrantGeneration); err != nil {
		t.Fatalf("read grant generation: %v", err)
	}
	if err := pool.QueryRow(ctx, "SELECT bodies_cleared, forward_provenance_known FROM requests WHERE request_id = $1", e02RequestID).Scan(&identity.BodiesCleared, &identity.ForwardProvenanceKnown); err != nil {
		t.Fatalf("read request retention identity: %v", err)
	}
	return identity
}

type e02LedgerRow struct {
	Name     string
	Checksum []byte
}

func e02LedgerRows(t *testing.T, ctx context.Context, pool *pgxpool.Pool) []e02LedgerRow {
	t.Helper()
	rows, err := pool.Query(ctx, "SELECT name, checksum FROM schema_migrations ORDER BY name")
	if err != nil {
		t.Fatalf("read migration ledger: %v", err)
	}
	defer rows.Close()
	var ledger []e02LedgerRow
	for rows.Next() {
		var row e02LedgerRow
		if err := rows.Scan(&row.Name, &row.Checksum); err != nil {
			t.Fatalf("scan migration ledger: %v", err)
		}
		ledger = append(ledger, row)
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("iterate migration ledger: %v", err)
	}
	return ledger
}

func e02AssertLedger(t *testing.T, ctx context.Context, pool *pgxpool.Pool, migrations []e02Migration) {
	t.Helper()
	ledger := e02LedgerRows(t, ctx, pool)
	if len(ledger) != len(migrations) {
		t.Fatalf("migration ledger has %d rows, want %d: %v", len(ledger), len(migrations), ledger)
	}
	for i, migration := range migrations {
		if ledger[i].Name != migration.name || !bytes.Equal(ledger[i].Checksum, migration.checksum) {
			t.Fatalf("ledger row %d = %q %x, want %q %x", i, ledger[i].Name, ledger[i].Checksum, migration.name, migration.checksum)
		}
	}
}

func e02AssertOlderLedger(t *testing.T, ctx context.Context, pool *pgxpool.Pool) {
	t.Helper()
	e02AssertLedger(t, ctx, pool, e02MigrationList(t)[:e02OlderMigrations])
}

func e02AssertCompleteLedger(t *testing.T, ctx context.Context, pool *pgxpool.Pool) {
	t.Helper()
	e02AssertLedger(t, ctx, pool, e02MigrationList(t))
}

func e02AssertUpgradeObjects(t *testing.T, ctx context.Context, pool *pgxpool.Pool) {
	t.Helper()
	for _, relation := range []string{"host_signature_replays", "host_proof_nonces"} {
		if !e02RelationExists(t, ctx, pool, relation) {
			t.Fatalf("completed upgrade is missing table %s", relation)
		}
	}
	if !e02ColumnExists(t, ctx, pool, "external_operations", "outcome") {
		t.Fatal("completed upgrade is missing external_operations.outcome")
	}
	var replays int
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM host_signature_replays`).Scan(&replays); err != nil {
		t.Fatalf("query completed upgrade table: %v", err)
	}
	if replays != 0 {
		t.Fatalf("completed upgrade table has %d unexpected rows", replays)
	}
}

func e02RelationExists(t *testing.T, ctx context.Context, pool *pgxpool.Pool, name string) bool {
	t.Helper()
	var exists bool
	if err := pool.QueryRow(ctx, "SELECT to_regclass($1) IS NOT NULL", name).Scan(&exists); err != nil {
		t.Fatalf("check relation %s: %v", name, err)
	}
	return exists
}

func e02ColumnExists(t *testing.T, ctx context.Context, pool *pgxpool.Pool, table, column string) bool {
	t.Helper()
	var exists bool
	if err := pool.QueryRow(ctx, `SELECT EXISTS (
 SELECT 1 FROM information_schema.columns
 WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2)`, table, column).Scan(&exists); err != nil {
		t.Fatalf("check column %s.%s: %v", table, column, err)
	}
	return exists
}

type e02BackendState struct {
	PID                 int32
	HasAdvisoryLock     bool
	WaitingAdvisoryLock bool
	WaitingRelationLock bool
}

func e02BackendStateFor(ctx context.Context, admin *pgx.Conn, applicationName string) (e02BackendState, error) {
	var state e02BackendState
	err := admin.QueryRow(ctx, `
SELECT COALESCE(max(a.pid), 0),
       COALESCE(bool_or(EXISTS (
         SELECT 1 FROM pg_locks l
         WHERE l.pid = a.pid AND l.locktype = 'advisory' AND l.granted)), false),
       COALESCE(bool_or(EXISTS (
         SELECT 1 FROM pg_locks l
         WHERE l.pid = a.pid AND l.locktype = 'advisory' AND NOT l.granted)), false),
       COALESCE(bool_or(EXISTS (
         SELECT 1 FROM pg_locks l
         WHERE l.pid = a.pid AND l.locktype = 'relation' AND NOT l.granted)), false)
FROM pg_stat_activity a
WHERE a.application_name = $1`, applicationName).Scan(
		&state.PID, &state.HasAdvisoryLock, &state.WaitingAdvisoryLock, &state.WaitingRelationLock)
	return state, err
}

// e02WaitForBarrier observes real PostgreSQL lock state until the harness is at
// the requested deterministic barrier. It never guesses at timing.
func e02WaitForBarrier(t *testing.T, ctx context.Context, admin *pgx.Conn, applicationName, description string, ready func(e02BackendState) bool) int32 {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	var last e02BackendState
	for {
		state, err := e02BackendStateFor(ctx, admin, applicationName)
		if err != nil {
			t.Fatalf("observe %s: %v", applicationName, err)
		}
		last = state
		if state.PID != 0 && ready(state) {
			return state.PID
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s; last state = %+v", description, last)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

type e02Harness struct {
	name   string
	cmd    *exec.Cmd
	done   chan error
	stderr bytes.Buffer
}

func e02StartHarness(t *testing.T, dsn, applicationName string) *e02Harness {
	t.Helper()
	cmd := exec.Command(os.Args[0])
	cmd.Env = append(os.Environ(), e02HarnessEnv+"="+e02WithApplicationName(dsn, applicationName))
	cmd.Stdout = io.Discard
	harness := &e02Harness{name: applicationName, cmd: cmd, done: make(chan error, 1)}
	cmd.Stderr = &harness.stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("start migration harness: %v", err)
	}
	go func() { harness.done <- cmd.Wait() }()
	t.Cleanup(func() {
		if cmd.Process != nil {
			_ = cmd.Process.Kill()
		}
	})
	return harness
}

func (h *e02Harness) wait(timeout time.Duration) error {
	select {
	case err := <-h.done:
		if err != nil {
			detail := strings.TrimSpace(h.stderr.String())
			if detail != "" {
				return fmt.Errorf("%w: %s", err, detail)
			}
		}
		return err
	case <-time.After(timeout):
		_ = h.cmd.Process.Kill()
		return errors.New("timed out waiting for migration harness")
	}
}

func e02WithApplicationName(dsn, applicationName string) string {
	parsed, err := url.Parse(dsn)
	if err != nil {
		return dsn
	}
	query := parsed.Query()
	query.Set("application_name", applicationName)
	parsed.RawQuery = query.Encode()
	return parsed.String()
}
