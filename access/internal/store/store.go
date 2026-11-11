// Package store owns the WhaleRein Access PostgreSQL schema and all SQL.
//
// The schema has exactly five tables: hosts, devices, grants, pairing_tokens
// and requests. Authorization is derived from the live grants rows; no other
// package keeps an in-memory authorization or request copy.
package store

import (
	"context"
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

//go:embed migrations/*.sql
var migrationFS embed.FS

const migrationLockKey int64 = 0x5748524143434553 // "WHRACCES"

// Store is a PostgreSQL-backed state store. It is safe for concurrent use.
type Store struct {
	pool *pgxpool.Pool
}

// Open parses a DSN, opens a pool and verifies connectivity.
func Open(ctx context.Context, databaseURL string) (*Store, error) {
	cfg, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		return nil, fmt.Errorf("parse database_url: %w", err)
	}
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		return nil, fmt.Errorf("open database: %w", err)
	}
	store := &Store{pool: pool}
	if err := store.Ping(ctx); err != nil {
		pool.Close()
		return nil, err
	}
	return store, nil
}

// NewWithPool wraps an existing pool. It is intended for component tests and
// for callers that already own the pool.
func NewWithPool(pool *pgxpool.Pool) *Store { return &Store{pool: pool} }

// Close releases the connection pool.
func (s *Store) Close() {
	if s != nil && s.pool != nil {
		s.pool.Close()
	}
}

// Ping verifies that the database accepts queries.
func (s *Store) Ping(ctx context.Context) error {
	if s == nil || s.pool == nil {
		return errors.New("store: no database pool")
	}
	if err := s.pool.Ping(ctx); err != nil {
		return fmt.Errorf("ping database: %w", err)
	}
	return nil
}

// Pool exposes the underlying pool for component tests.
func (s *Store) Pool() *pgxpool.Pool { return s.pool }

// Migrate applies all embedded idempotent migrations under a transaction-scoped
// advisory lock. There is no migration metadata table because the Access
// schema must contain exactly five tables.
func (s *Store) Migrate(ctx context.Context) error {
	entries, err := migrationFS.ReadDir("migrations")
	if err != nil {
		return fmt.Errorf("read migrations: %w", err)
	}
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return fmt.Errorf("begin migration: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock($1)", migrationLockKey); err != nil {
		return fmt.Errorf("lock migrations: %w", err)
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		raw, err := migrationFS.ReadFile("migrations/" + entry.Name())
		if err != nil {
			return fmt.Errorf("read migration %s: %w", entry.Name(), err)
		}
		for _, statement := range splitStatements(string(raw)) {
			if _, err := tx.Exec(ctx, statement); err != nil {
				return fmt.Errorf("apply migration %s: %w", entry.Name(), err)
			}
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
	RequestID    string
	HostID       string
	DeviceID     string
	Status       whalereinv1.OnlineStatus
	ResultBody   []byte
	ErrorCode    whalereinv1.ErrorCode
	ErrorMessage string
	ChannelID    string
	At           time.Time
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
