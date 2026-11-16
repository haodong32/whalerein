// Package store owns the WhaleRein Access PostgreSQL schema and all SQL.
//
// Versioned migrations own local workspace bindings, devices, grants, pairing
// operations and requests. Authorization is derived from the live grants rows; no other
// package keeps an in-memory authorization or request copy.
package store

import (
	"bytes"
	"context"
	"crypto/sha256"
	"embed"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// ErrNotFound is returned when a row does not exist.
var ErrNotFound = errors.New("not found")

// ErrConflict is returned for a uniqueness conflict that is not an idempotent
// command replay.
var ErrConflict = errors.New("conflict")

// ErrPairingFenced proves an unfinished pairing intent cannot commit.
var ErrPairingFenced = errors.New("pairing intent invalidated")

// Pairing deadlines are distinguished so callers do not confuse expiration
// with a consumed invitation or a proven revocation fence.
var ErrPairingInvitationExpired = errors.New("pairing invitation expired")
var ErrPairingIntentExpired = errors.New("pairing intent expired")

//go:embed migrations/*.sql
var migrationFS embed.FS

const migrationLockKey int64 = 0x5748524143434553 // "WHRACCES"

// CleanupBatchSize bounds every retention statement. Cleanup repeats batches
// until one comes back short, so a long pause drains as fast as the database
// allows without one unbounded delete or update.
const CleanupBatchSize = 2000

// database is the narrow storage seam the Store needs. It is satisfied by
// *pgxpool.Pool in production and by a hand-rolled double in unit tests so
// public Store methods can be exercised without PostgreSQL.
type database interface {
	Ping(context.Context) error
	Close()
	Query(context.Context, string, ...any) (pgx.Rows, error)
	QueryRow(context.Context, string, ...any) pgx.Row
	Exec(context.Context, string, ...any) (pgconn.CommandTag, error)
	Begin(context.Context) (pgx.Tx, error)
}

// Store is a PostgreSQL-backed state store. It is safe for concurrent use.
type Store struct {
	db database
}

// newStore binds a storage seam. Tests use it to install a scripted database.
func newStore(db database) *Store { return &Store{db: db} }

// unavailableRow reports that no database seam is configured. Operations keep
// their validation-before-database behavior and fail closed instead of panicking.
type unavailableRow struct{ err error }

func (r unavailableRow) Scan(...any) error { return r.err }

// The helpers below defer seam access until an operation has validated its
// inputs. They keep a zero Store failing closed rather than panicking.
func (s *Store) poolBegin(ctx context.Context) (pgx.Tx, error) {
	if s == nil || s.db == nil {
		return nil, errors.New("store: no database pool")
	}
	return s.db.Begin(ctx)
}

func (s *Store) poolQuery(ctx context.Context, sql string, args ...any) (pgx.Rows, error) {
	if s == nil || s.db == nil {
		return nil, errors.New("store: no database pool")
	}
	return s.db.Query(ctx, sql, args...)
}

func (s *Store) poolQueryRow(ctx context.Context, sql string, args ...any) pgx.Row {
	if s == nil || s.db == nil {
		return unavailableRow{err: errors.New("store: no database pool")}
	}
	return s.db.QueryRow(ctx, sql, args...)
}

// Open parses a DSN, opens a pool and verifies connectivity.
func Open(ctx context.Context, databaseURL string) (*Store, error) {
	cfg, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		// pgx parse errors can include user-supplied connection identity.
		return nil, errors.New("parse database_url: invalid connection configuration")
	}
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		return nil, fmt.Errorf("open database: %w", err)
	}
	store := newStore(pool)
	if err := store.Ping(ctx); err != nil {
		pool.Close()
		return nil, err
	}
	return store, nil
}

// NewWithPool wraps an existing pool. It is intended for component tests and
// for callers that already own the pool.
func NewWithPool(pool *pgxpool.Pool) *Store {
	if pool == nil {
		return &Store{}
	}
	return newStore(pool)
}

// Close releases the connection pool.
func (s *Store) Close() {
	if s != nil && s.db != nil {
		s.db.Close()
	}
}

// Ping verifies that the database accepts queries.
func (s *Store) Ping(ctx context.Context) error {
	if s == nil || s.db == nil {
		return errors.New("store: no database pool")
	}
	if err := s.db.Ping(ctx); err != nil {
		return fmt.Errorf("ping database: %w", err)
	}
	return nil
}

// Pool exposes the underlying pool for component tests.
func (s *Store) Pool() *pgxpool.Pool {
	pool, _ := s.db.(*pgxpool.Pool)
	return pool
}

// Migrate applies checksummed migrations once under a transaction-scoped lock.
// Existing unversioned installations are adopted without deleting their state.
func (s *Store) Migrate(ctx context.Context) error {
	return migrate(ctx, s.poolBegin)
}

// migrate applies the embedded ledger and schema atomically under a database lock.
func migrate(ctx context.Context, begin func(context.Context) (pgx.Tx, error)) error {
	entries, err := migrationFS.ReadDir("migrations")
	if err != nil {
		return fmt.Errorf("read migrations: %w", err)
	}
	tx, err := begin(ctx)
	if err != nil {
		return fmt.Errorf("begin migration: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock($1)", migrationLockKey); err != nil {
		return fmt.Errorf("lock migrations: %w", err)
	}
	if _, err := tx.Exec(ctx, `CREATE TABLE IF NOT EXISTS schema_migrations (
 name text PRIMARY KEY, checksum bytea NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())`); err != nil {
		return fmt.Errorf("create migration ledger: %w", err)
	}
	// A binary older than the database must not start against an unknown schema.
	rows, err := tx.Query(ctx, "SELECT name FROM schema_migrations")
	if err != nil {
		return err
	}
	known := make(map[string]bool)
	for _, entry := range entries {
		known[entry.Name()] = true
	}
	unknown := false
	for rows.Next() {
		var name string
		if err := rows.Scan(&name); err != nil {
			rows.Close()
			return err
		}
		unknown = unknown || !known[name]
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return err
	}
	if unknown {
		return errors.New("database contains a newer migration; upgrade Access")
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		raw, err := migrationFS.ReadFile("migrations/" + entry.Name())
		if err != nil {
			return fmt.Errorf("read migration %s: %w", entry.Name(), err)
		}
		checksum := sha256.Sum256(raw)
		var applied []byte
		err = tx.QueryRow(ctx, "SELECT checksum FROM schema_migrations WHERE name=$1", entry.Name()).Scan(&applied)
		if err == nil {
			if !bytes.Equal(applied, checksum[:]) {
				return fmt.Errorf("migration checksum changed: %s", entry.Name())
			}
			continue
		}
		if !errors.Is(err, pgx.ErrNoRows) {
			return err
		}
		for _, statement := range splitStatements(string(raw)) {
			if _, err := tx.Exec(ctx, statement); err != nil {
				return fmt.Errorf("apply migration %s: %w", entry.Name(), err)
			}
		}
		if _, err := tx.Exec(ctx, "INSERT INTO schema_migrations(name,checksum) VALUES ($1,$2)", entry.Name(), checksum[:]); err != nil {
			return err
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("commit migration: %w", err)
	}
	return nil
}

// splitStatements splits a controlled migration file into complete statements.
// It is aware of SQL single-quoted strings and line comments so semicolons in
// comments do not produce partial statements.
func splitStatements(raw string) []string {
	var out []string
	var b strings.Builder
	inSingle := false
	for i := 0; i < len(raw); {
		c := raw[i]
		if inSingle {
			b.WriteByte(c)
			if c == '\'' {
				if i+1 < len(raw) && raw[i+1] == '\'' {
					b.WriteByte(raw[i+1])
					i += 2
					continue
				}
				inSingle = false
			}
			i++
			continue
		}
		switch {
		case c == '\'':
			inSingle = true
			b.WriteByte(c)
			i++
		case c == '-' && i+1 < len(raw) && raw[i+1] == '-':
			end := strings.IndexByte(raw[i:], '\n')
			if end < 0 {
				b.WriteString(raw[i:])
				i = len(raw)
				continue
			}
			b.WriteString(raw[i : i+end])
			i += end
		case c == ';':
			if statement := strings.TrimSpace(b.String()); statement != "" {
				out = append(out, statement)
			}
			b.Reset()
			i++
		default:
			b.WriteByte(c)
			i++
		}
	}
	if statement := strings.TrimSpace(b.String()); statement != "" {
		out = append(out, statement)
	}
	return out
}

// Host is one registered host.
type Host struct {
	HostID          string
	Fingerprint     string
	ProtocolVersion uint32
	CreatedAt       time.Time
	UpdatedAt       time.Time
}

// Device is one registered device.
type Device struct {
	DeviceID             string
	PublicKeyFingerprint string
	AuthHash             []byte
	CreatedAt            time.Time
	UpdatedAt            time.Time
}

// Grant is one device's access to one host. A nil RevokedAt means active.
type Grant struct {
	HostID     string
	DeviceID   string
	CreatedAt  time.Time
	UpdatedAt  time.Time
	RevokedAt  *time.Time
	ReplayedAt *time.Time
}

// PairingToken is a one-time invitation. Only the hash is stored.
type PairingToken struct {
	TokenHash     []byte
	HostID        string
	ExpiresAt     time.Time
	UsedAt        *time.Time
	FailureCount  int
	LastFailureAt *time.Time
	CreatedAt     time.Time
	UpdatedAt     time.Time
}

// ForwardState tracks the MQTT publish lifecycle of a request row.
type ForwardState string

// Forward states stored in requests.forward_state.
const (
	ForwardNone      ForwardState = "none"
	ForwardPending   ForwardState = "pending"
	ForwardPublished ForwardState = "published"
)

// Request is the Access-side state copy for one HostRequest.
type Request struct {
	RequestID              string
	HostID                 string
	DeviceID               string
	CommandID              string
	PayloadDigest          []byte
	Kind                   whalereinv1.RequestKind
	ProtocolVersion        uint32
	ClientTransportVersion uint32
	Status                 whalereinv1.OnlineStatus
	RequestBody            []byte
	ResultBody             []byte
	ErrorCode              whalereinv1.ErrorCode
	ErrorMessage           string
	ChannelID              string
	TraceID                string
	ExpiresAt              time.Time
	ForwardState           ForwardState
	ForwardAfter           time.Time
	ForwardAttempts        int
	PublishedOnce          bool
	CreatedAt              time.Time
	UpdatedAt              time.Time
}

// CreateOutcome reports the result of inserting or deduplicating a request.
type CreateOutcome struct {
	Request        Request
	Created        bool
	DigestMismatch bool
}

// HostResultUpdate is a validated host result targeting one request row.
type HostResultUpdate struct {
	RequestID          string
	HostID             string
	DeviceID           string
	Status             whalereinv1.OnlineStatus
	TransportPreflight bool
	ResultBody         []byte
	ErrorCode          whalereinv1.ErrorCode
	ErrorMessage       string
	ChannelID          string
	At                 time.Time
}

// Revocation is one pending revocation row.
type Revocation struct {
	HostID          string
	GrantGeneration uint64
	DeviceID        string
	RevokedAt       time.Time
}

var requestColumns = `request_id, host_id, device_id, command_id, payload_digest, kind,
	protocol_version, status, request_body, result_body, error_code, error_message,
	channel_id, trace_id, expires_at, forward_state, forward_after, forward_attempts,
	published_once, created_at, updated_at, client_transport_version`

func scanRequest(row interface{ Scan(...any) error }) (Request, error) {
	var (
		req          Request
		kind         int32
		status       string
		errorCode    int32
		forwardState string
	)
	err := row.Scan(
		&req.RequestID,
		&req.HostID,
		&req.DeviceID,
		&req.CommandID,
		&req.PayloadDigest,
		&kind,
		&req.ProtocolVersion,
		&status,
		&req.RequestBody,
		&req.ResultBody,
		&errorCode,
		&req.ErrorMessage,
		&req.ChannelID,
		&req.TraceID,
		&req.ExpiresAt,
		&forwardState,
		&req.ForwardAfter,
		&req.ForwardAttempts,
		&req.PublishedOnce,
		&req.CreatedAt,
		&req.UpdatedAt,
		&req.ClientTransportVersion,
	)
	if err != nil {
		return Request{}, err
	}
	req.Kind = whalereinv1.RequestKind(kind)
	req.Status = parseStatus(status)
	req.ErrorCode = whalereinv1.ErrorCode(errorCode)
	req.ForwardState = ForwardState(forwardState)
	return req, nil
}

func parseStatus(value string) whalereinv1.OnlineStatus {
	number, ok := whalereinv1.OnlineStatus_value[value]
	if !ok {
		return whalereinv1.OnlineStatus_online_status_unspecified
	}
	return whalereinv1.OnlineStatus(number)
}

// validStatus reports whether value is one of the seven wire statuses.
func validStatus(value whalereinv1.OnlineStatus) bool {
	switch value {
	case whalereinv1.OnlineStatus_received,
		whalereinv1.OnlineStatus_host_offline,
		whalereinv1.OnlineStatus_rejected,
		whalereinv1.OnlineStatus_host_accepted,
		whalereinv1.OnlineStatus_succeeded,
		whalereinv1.OnlineStatus_failed,
		whalereinv1.OnlineStatus_unknown:
		return true
	default:
		return false
	}
}

// ValidStatus reports whether value is a storable online status.
func ValidStatus(value whalereinv1.OnlineStatus) bool { return validStatus(value) }

// StatusName returns the proto value name used on the wire.
func StatusName(value whalereinv1.OnlineStatus) string {
	if !validStatus(value) {
		return "online_status_unspecified"
	}
	return whalereinv1.OnlineStatus_name[int32(value)]
}

func isUniqueViolation(err error) bool {
	var pgErr *pgconn.PgError
	return errors.As(err, &pgErr) && pgErr.Code == "23505"
}

func wrapRequestScan(err error) error {
	if errors.Is(err, pgx.ErrNoRows) {
		return ErrNotFound
	}
	return err
}
