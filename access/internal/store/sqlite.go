package store

// SQLite is the only Access runtime storage. The adapter keeps the narrow pgx
// shaped seam used by the rest of the package so the SQL and its callers are
// shared with the historical PostgreSQL fixtures; production code never opens
// a PostgreSQL connection.

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"embed"
	"errors"
	"fmt"
	"math"
	"reflect"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	_ "modernc.org/sqlite"
)

// Open opens one node-local SQLite database. The path is a filesystem path,
// never a network DSN: a node with an unreadable disk must fail startup rather
// than silently reach another node's storage.
func Open(ctx context.Context, path string) (*Store, error) {
	if strings.TrimSpace(path) == "" {
		return nil, errors.New("open database: a local SQLite path is required")
	}
	if strings.Contains(path, "://") {
		return nil, errors.New("open database: database_path is not a local SQLite path")
	}
	dsn := "file:" + path + "?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)&_pragma=synchronous(FULL)&_pragma=foreign_keys(ON)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open database: %w", err)
	}
	// One connection serializes writers, keeps the transaction-scoped session
	// authority setting visible to the pairing trigger, and turns busy errors
	// into a bounded retry instead of a false durable receipt.
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	db.SetConnMaxLifetime(0)
	if err := db.PingContext(ctx); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("ping database: %w", err)
	}
	data := &Store{sqlite: true}
	adapter := &sqliteDatabase{db: db, onBusy: func() { data.busy.Add(1) }}
	data.db = adapter
	data.control = adapter
	return data, nil
}

// OpenPostgres is retained only for explicit PostgreSQL migration fixtures.
// Runtime configuration never selects it: an Access node always opens SQLite.
func OpenPostgres(ctx context.Context, databaseURL string) (*Store, error) {
	return openPostgres(ctx, databaseURL)
}

type sqliteDatabase struct {
	db     *sql.DB
	onBusy func()
}

func (d *sqliteDatabase) normalize(err error) error {
	normalized := normalizeSQLiteError(err)
	if d.onBusy != nil && errors.Is(normalized, ErrUnavailable) {
		d.onBusy()
	}
	return normalized
}

func (d *sqliteDatabase) Ping(ctx context.Context) error { return d.db.PingContext(ctx) }
func (d *sqliteDatabase) Close()                         { _ = d.db.Close() }

func (d *sqliteDatabase) Query(ctx context.Context, query string, args ...any) (pgx.Rows, error) {
	rewritten, values, err := translateSQL(query, args)
	if err != nil {
		return nil, err
	}
	rows, err := d.db.QueryContext(ctx, rewritten, values...)
	if err != nil {
		return nil, fmt.Errorf("%w [%s]", d.normalize(err), rewritten)
	}
	return &sqliteRows{rows: rows}, nil
}

func (d *sqliteDatabase) QueryRow(ctx context.Context, query string, args ...any) pgx.Row {
	rewritten, values, err := translateSQL(query, args)
	if err != nil {
		return errorRow{err: err}
	}
	return &sqliteRow{row: d.db.QueryRowContext(ctx, rewritten, values...)}
}

func (d *sqliteDatabase) Exec(ctx context.Context, query string, args ...any) (pgconn.CommandTag, error) {
	rewritten, values, err := translateSQL(query, args)
	if err != nil {
		return pgconn.CommandTag{}, err
	}
	result, err := d.db.ExecContext(ctx, rewritten, values...)
	if err != nil {
		return pgconn.CommandTag{}, fmt.Errorf("%w [%s]", d.normalize(err), rewritten)
	}
	affected, _ := result.RowsAffected()
	return pgconn.NewCommandTag(fmt.Sprintf("UPDATE %d", affected)), nil
}

func (d *sqliteDatabase) Begin(ctx context.Context) (pgx.Tx, error) {
	// The PostgreSQL implementation used a transaction-local setting. SQLite
	// has no equivalent, so clear the previous value before a new transaction
	// can observe it; Commit and Rollback clear it again.
	_, _ = d.db.ExecContext(ctx, `DELETE FROM app_settings WHERE key='whalerein.session_authority'`)
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, d.normalize(err)
	}
	return &sqliteTx{tx: tx, db: d.db}, nil
}

type sqliteTx struct {
	pgx.Tx
	tx *sql.Tx
	db *sql.DB
}

func (t *sqliteTx) Query(ctx context.Context, query string, args ...any) (pgx.Rows, error) {
	rewritten, values, err := translateSQL(query, args)
	if err != nil {
		return nil, err
	}
	rows, err := t.tx.QueryContext(ctx, rewritten, values...)
	if err != nil {
		return nil, fmt.Errorf("%w [%s]", normalizeSQLiteError(err), rewritten)
	}
	return &sqliteRows{rows: rows}, nil
}

func (t *sqliteTx) QueryRow(ctx context.Context, query string, args ...any) pgx.Row {
	rewritten, values, err := translateSQL(query, args)
	if err != nil {
		return errorRow{err: err}
	}
	return &sqliteRow{row: t.tx.QueryRowContext(ctx, rewritten, values...)}
}

func (t *sqliteTx) Exec(ctx context.Context, query string, args ...any) (pgconn.CommandTag, error) {
	rewritten, values, err := translateSQL(query, args)
	if err != nil {
		return pgconn.CommandTag{}, err
	}
	result, err := t.tx.ExecContext(ctx, rewritten, values...)
	if err != nil {
		return pgconn.CommandTag{}, fmt.Errorf("%w [%s]", normalizeSQLiteError(err), rewritten)
	}
	affected, _ := result.RowsAffected()
	return pgconn.NewCommandTag(fmt.Sprintf("UPDATE %d", affected)), nil
}

func (t *sqliteTx) Commit(ctx context.Context) error {
	err := t.tx.Commit()
	t.clearAuthority()
	return err
}

func (t *sqliteTx) Rollback(ctx context.Context) error {
	err := t.tx.Rollback()
	t.clearAuthority()
	return err
}

func (t *sqliteTx) clearAuthority() {
	if t.db == nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_, _ = t.db.ExecContext(ctx, `DELETE FROM app_settings WHERE key='whalerein.session_authority'`)
}

type sqliteRow struct {
	row *sql.Row
}

func (r *sqliteRow) Scan(dest ...any) error {
	values := make([]any, len(dest))
	pointers := make([]any, len(dest))
	for i := range values {
		pointers[i] = &values[i]
	}
	if err := r.row.Scan(pointers...); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return pgx.ErrNoRows
		}
		return normalizeSQLiteError(err)
	}
	return assignValues(dest, values)
}

type errorRow struct{ err error }

func (r errorRow) Scan(...any) error { return r.err }

type sqliteRows struct {
	pgx.Rows
	rows   *sql.Rows
	values []any
	err    error
}

func (r *sqliteRows) Next() bool {
	if r.err != nil {
		return false
	}
	if !r.rows.Next() {
		r.err = r.rows.Err()
		return false
	}
	columns, err := r.rows.Columns()
	if err != nil {
		r.err = err
		return false
	}
	values := make([]any, len(columns))
	pointers := make([]any, len(columns))
	for i := range values {
		pointers[i] = &values[i]
	}
	if err := r.rows.Scan(pointers...); err != nil {
		r.err = err
		return false
	}
	r.values = values
	return true
}

func (r *sqliteRows) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	return assignValues(dest, r.values)
}

func (r *sqliteRows) Err() error {
	if r.err != nil {
		return r.err
	}
	return r.rows.Err()
}
func (r *sqliteRows) Close() { _ = r.rows.Close() }

// assignValues converts SQLite's dynamic values into the Go destinations the
// store uses. Timestamps are Unix milliseconds and booleans are 0/1.
func assignValues(dest []any, values []any) error {
	if len(dest) != len(values) {
		return fmt.Errorf("scan: %d columns into %d destinations", len(values), len(dest))
	}
	for i, target := range dest {
		if err := assignValue(target, values[i]); err != nil {
			return fmt.Errorf("scan column %d: %w", i, err)
		}
	}
	return nil
}

func assignValue(target, value any) error {
	switch typed := target.(type) {
	case *any:
		*typed = value
		return nil
	case *string:
		*typed = stringValue(value)
		return nil
	case *[]byte:
		switch raw := value.(type) {
		case []byte:
			*typed = raw
		case string:
			*typed = []byte(raw)
		case nil:
			*typed = nil
		default:
			*typed = []byte(fmt.Sprint(raw))
		}
		return nil
	case *time.Time:
		parsed, ok := timeValue(value)
		if !ok {
			return fmt.Errorf("not a timestamp: %T", value)
		}
		*typed = parsed
		return nil
	case **time.Time:
		if value == nil {
			*typed = nil
			return nil
		}
		parsed, ok := timeValue(value)
		if !ok {
			return fmt.Errorf("not a timestamp: %T", value)
		}
		*typed = &parsed
		return nil
	case *bool:
		*typed = boolValue(value)
		return nil
	case *int:
		*typed = int(intValue(value))
		return nil
	case *int32:
		*typed = int32(intValue(value))
		return nil
	case *int64:
		*typed = intValue(value)
		return nil
	case *uint32:
		*typed = uint32(uintValue(value))
		return nil
	case *uint64:
		*typed = uintValue(value)
		return nil
	case *float64:
		*typed = floatValue(value)
		return nil
	case *float32:
		*typed = float32(floatValue(value))
		return nil
	}
	// Fall back to reflection for aliases and pointer-to-pointer shapes.
	reflection := reflect.ValueOf(target)
	if reflection.Kind() == reflect.Ptr && !reflection.IsNil() {
		element := reflection.Elem()
		if element.Kind() == reflect.Ptr {
			if value == nil {
				element.SetZero()
				return nil
			}
			inner := reflect.New(element.Type().Elem())
			if err := assignValue(inner.Interface(), value); err != nil {
				return err
			}
			element.Set(inner)
			return nil
		}
		switch element.Kind() {
		case reflect.String:
			element.SetString(stringValue(value))
			return nil
		case reflect.Bool:
			element.SetBool(boolValue(value))
			return nil
		case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
			element.SetInt(intValue(value))
			return nil
		case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
			element.SetUint(uintValue(value))
			return nil
		case reflect.Float32, reflect.Float64:
			element.SetFloat(floatValue(value))
			return nil
		case reflect.Slice:
			if element.Type().Elem().Kind() == reflect.Uint8 {
				raw, _ := value.([]byte)
				element.SetBytes(raw)
				return nil
			}
		}
	}
	return fmt.Errorf("unsupported scan destination %T", target)
}

func stringValue(value any) string {
	switch raw := value.(type) {
	case nil:
		return ""
	case string:
		return raw
	case []byte:
		return string(raw)
	default:
		return fmt.Sprint(raw)
	}
}

func intValue(value any) int64 {
	switch raw := value.(type) {
	case int64:
		return raw
	case int:
		return int64(raw)
	case int32:
		return int64(raw)
	case uint64:
		return int64(raw)
	case float64:
		return int64(raw)
	case bool:
		if raw {
			return 1
		}
		return 0
	case string:
		parsed, _ := strconv.ParseInt(raw, 10, 64)
		return parsed
	case nil:
		return 0
	default:
		return 0
	}
}

func uintValue(value any) uint64 {
	number := intValue(value)
	if number < 0 {
		return uint64(0)
	}
	return uint64(number)
}

func floatValue(value any) float64 {
	switch raw := value.(type) {
	case float64:
		return raw
	case int64:
		return float64(raw)
	case string:
		parsed, _ := strconv.ParseFloat(raw, 64)
		return parsed
	case nil:
		return 0
	default:
		return float64(intValue(raw))
	}
}

func boolValue(value any) bool {
	switch raw := value.(type) {
	case bool:
		return raw
	case int64:
		return raw != 0
	case int:
		return raw != 0
	case string:
		return raw == "1" || strings.EqualFold(raw, "true")
	case []byte:
		return string(raw) == "1" || strings.EqualFold(string(raw), "true")
	default:
		return false
	}
}

func timeValue(value any) (time.Time, bool) {
	switch raw := value.(type) {
	case time.Time:
		return raw.UTC(), true
	case int64:
		return time.UnixMilli(raw).UTC(), true
	case int:
		return time.UnixMilli(int64(raw)).UTC(), true
	case float64:
		return time.UnixMilli(int64(raw)).UTC(), true
	case []byte:
		return parseTimeString(string(raw))
	case string:
		return parseTimeString(raw)
	case nil:
		return time.Time{}, false
	default:
		return time.Time{}, false
	}
}

func parseTimeString(raw string) (time.Time, bool) {
	if raw == "" {
		return time.Time{}, false
	}
	if millis, err := strconv.ParseInt(raw, 10, 64); err == nil {
		return time.UnixMilli(millis).UTC(), true
	}
	for _, layout := range []string{time.RFC3339Nano, "2006-01-02 15:04:05.999999999-07:00", "2006-01-02 15:04:05"} {
		if parsed, err := time.Parse(layout, raw); err == nil {
			return parsed.UTC(), true
		}
	}
	return time.Time{}, false
}

func normalizeSQLiteError(err error) error {
	if err == nil {
		return nil
	}
	message := strings.ToLower(err.Error())
	switch {
	case strings.Contains(message, "database is locked"), strings.Contains(message, "database table is locked"),
		strings.Contains(message, "busy"):
		return fmt.Errorf("%w: %v", ErrUnavailable, err)
	case strings.Contains(message, "database or disk is full"), strings.Contains(message, "disk i/o error"):
		return fmt.Errorf("%w: %v", ErrStorageFull, err)
	case strings.Contains(message, "database disk image is malformed"), strings.Contains(message, "file is not a database"):
		return fmt.Errorf("%w: %v", ErrCorrupt, err)
	case strings.Contains(message, "unique constraint failed"), strings.Contains(message, "constraint failed"):
		return fmt.Errorf("%w: %v", ErrConflict, err)
	}
	return err
}

// ErrUnavailable reports a retryable local storage outage. A caller must never
// turn it into a successful durable receipt.
var ErrUnavailable = errors.New("storage unavailable")

// ErrStorageFull reports a retryable disk-full condition.
var ErrStorageFull = errors.New("storage full")

// ErrCorrupt reports an unreadable local database.
var ErrCorrupt = errors.New("storage corrupt")

var placeholderPattern = regexp.MustCompile(`\$(\d+)`)
var castPattern = regexp.MustCompile(`::(?:timestamptz|timestamp(?: with(?:out)? time zone)?|double precision|integer|smallint|bigint|int8|int4|int2|float8|float4|boolean|numeric|varchar|jsonb|json|bytea|text|uuid|bool|int)`)
var intervalPattern = regexp.MustCompile(`(?i)interval\s+'([0-9]+)\s+(second|seconds|minute|minutes|hour|hours|day|days)'`)
var anyPattern = regexp.MustCompile(`(?i)ANY\(\s*\$(\d+)\s*\)`)

// translateSQL maps the store's PostgreSQL-shaped SQL onto SQLite. It handles
// the placeholder form, bool/time argument conversion and the small set of
// PostgreSQL expressions used by the store.
func translateSQL(query string, args []any) (string, []any, error) {
	// One documented session setting backs the pairing trigger.
	if strings.HasPrefix(strings.TrimSpace(query), "SELECT set_config('whalerein.session_authority'") {
		if len(args) != 1 {
			return "", nil, errors.New("set_config: one argument is required")
		}
		return "INSERT OR REPLACE INTO app_settings(key,value) VALUES ('whalerein.session_authority',?)", []any{stringValue(args[0])}, nil
	}
	if strings.Contains(strings.ToLower(query), "pg_advisory") {
		return "SELECT 1", nil, nil
	}

	var values []any
	var builder strings.Builder
	for index := 0; index < len(query); {
		if query[index] == '$' {
			end := index + 1
			for end < len(query) && query[end] >= '0' && query[end] <= '9' {
				end++
			}
			if end > index+1 {
				position, err := strconv.Atoi(query[index+1 : end])
				if err != nil || position < 1 || position > len(args) {
					return "", nil, fmt.Errorf("placeholder $%s has no argument", query[index+1:end])
				}
				argument := args[position-1]
				prefix := builder.String()
				if len(prefix) >= 4 && strings.EqualFold(prefix[len(prefix)-4:], "any(") {
					reflection := reflect.ValueOf(argument)
					if reflection.Kind() == reflect.Slice || reflection.Kind() == reflect.Array {
						listPrefix := prefix[:len(prefix)-4]
						// `= ANY(...)` becomes `IN (...)`, not `= IN (...)`.
						trimmed := strings.TrimRight(listPrefix, " ")
						if strings.HasSuffix(trimmed, "=") {
							listPrefix = strings.TrimRight(trimmed[:len(trimmed)-1], " ") + " "
						}
						builder.Reset()
						builder.WriteString(listPrefix)
						builder.WriteString("IN (")
						for i := 0; i < reflection.Len(); i++ {
							if i > 0 {
								builder.WriteByte(',')
							}
							builder.WriteByte('?')
							values = append(values, normalizeArg(reflection.Index(i).Interface()))
						}
						// The query's own closing parenthesis closes the list.
						index = end
						continue
					}
				}
				builder.WriteByte('?')
				values = append(values, normalizeArg(argument))
				index = end
				continue
			}
		}
		builder.WriteByte(query[index])
		index++
	}
	translated := builder.String()

	// SQLite has no row locks; one serialized writer provides the same
	// transaction boundary.
	translated = regexp.MustCompile(`(?is)\s+FOR\s+(UPDATE|SHARE|NO\s+KEY\s+UPDATE)(\s+SKIP\s+LOCKED)?`).ReplaceAllString(translated, "")
	translated = regexp.MustCompile(`(?i)\s+SKIP\s+LOCKED`).ReplaceAllString(translated, "")
	translated = castPattern.ReplaceAllString(translated, "")
	translated = regexp.MustCompile(`(?i)\bUPDATE\s+([a-z_][a-z0-9_]*)\s+([a-z_][a-z0-9_]*)\s+SET\b`).ReplaceAllString(translated, "UPDATE $1 AS $2 SET")
	// SQLite requires AS before a DELETE table alias.
	translated = regexp.MustCompile(`(?i)\bDELETE\s+FROM\s+([a-z_][a-z0-9_]*)\s+([a-z_][a-z0-9_]*)\s+WHERE\b`).ReplaceAllString(translated, "DELETE FROM $1 AS $2 WHERE")
	// SQLite's UPDATE ... FROM does not resolve the target alias inside
	// RETURNING, so strip only that qualified prefix.
	if index := strings.LastIndex(strings.ToUpper(translated), "RETURNING"); index >= 0 {
		translated = translated[:index] + strings.ReplaceAll(translated[index:], "r.", "")
	}
	translated = regexp.MustCompile(`(?i)GREATEST\(`).ReplaceAllString(translated, "max(")
	translated = regexp.MustCompile(`(?i)LEAST\(`).ReplaceAllString(translated, "min(")
	translated = regexp.MustCompile(`(?i)(now|clock_timestamp)\(\)`).ReplaceAllString(translated, "(unixepoch()*1000)")
	translated = intervalPattern.ReplaceAllStringFunc(translated, func(match string) string {
		parts := intervalPattern.FindStringSubmatch(match)
		amount, _ := strconv.ParseInt(parts[1], 10, 64)
		switch strings.ToLower(parts[2]) {
		case "second", "seconds":
			return strconv.FormatInt(amount*1000, 10)
		case "minute", "minutes":
			return strconv.FormatInt(amount*60*1000, 10)
		case "hour", "hours":
			return strconv.FormatInt(amount*3600*1000, 10)
		default:
			return strconv.FormatInt(amount*24*3600*1000, 10)
		}
	})
	translated, err := rewriteExtractEpoch(translated)
	if err != nil {
		return "", nil, err
	}
	return translated, values, nil
}

// rewriteExtractEpoch converts EXTRACT(EPOCH FROM (a - b)) to a millisecond
// difference in seconds. The store only uses this for age metrics.
func rewriteExtractEpoch(query string) (string, error) {
	lower := strings.ToLower(query)
	for {
		start := strings.Index(lower, "extract(epoch from (")
		if start < 0 {
			return query, nil
		}
		open := start + len("extract(epoch from ")
		depth := 0
		closeIndex := -1
		for i := open; i < len(query); i++ {
			switch query[i] {
			case '(':
				depth++
			case ')':
				depth--
				if depth == 0 {
					closeIndex = i
				}
			}
			if closeIndex >= 0 {
				break
			}
		}
		if closeIndex < 0 {
			return "", errors.New("unbalanced EXTRACT expression")
		}
		inner := strings.TrimSpace(query[open+1 : closeIndex])
		end := closeIndex + 1
		if end < len(query) && query[end] == ')' {
			end++
		}
		replacement := "((" + inner + ")/1000.0)"
		query = query[:start] + replacement + query[end:]
		lower = strings.ToLower(query)
	}
}

func normalizeArg(value any) any {
	switch typed := value.(type) {
	case time.Duration:
		return typed.Milliseconds()
	case time.Time:
		return typed.UnixMilli()
	case *time.Time:
		if typed == nil {
			return nil
		}
		return typed.UnixMilli()
	case bool:
		if typed {
			return int64(1)
		}
		return int64(0)
	case uint64:
		if typed > math.MaxInt64 {
			return int64(math.MaxInt64)
		}
		return int64(typed)
	case uint32:
		return int64(typed)
	case float32:
		return float64(typed)
	case []any:
		normalized := make([]any, len(typed))
		for i, item := range typed {
			normalized[i] = normalizeArg(item)
		}
		return normalized
	case []string:
		normalized := make([]any, len(typed))
		for i, item := range typed {
			normalized[i] = item
		}
		return normalized
	default:
		return value
	}
}

//go:embed migrations_sqlite/*.sql
var sqliteMigrationFS embed.FS

// migrateSQLite applies the node-local schema once. A checksum change means an
// older binary is starting against a newer local schema; it refuses instead of
// running partial DDL.
func (s *Store) migrateSQLite(ctx context.Context) error {
	if s == nil || s.db == nil {
		return errors.New("store: no database")
	}
	if _, err := s.db.Exec(ctx, `CREATE TABLE IF NOT EXISTS schema_migrations (
		name TEXT PRIMARY KEY, checksum BLOB NOT NULL, applied_at INTEGER NOT NULL DEFAULT (unixepoch()*1000))`); err != nil {
		return fmt.Errorf("create migration ledger: %w", err)
	}
	entries, err := sqliteMigrationFS.ReadDir("migrations_sqlite")
	if err != nil {
		return fmt.Errorf("read migrations: %w", err)
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		raw, err := sqliteMigrationFS.ReadFile("migrations_sqlite/" + entry.Name())
		if err != nil {
			return fmt.Errorf("read migration %s: %w", entry.Name(), err)
		}
		checksum := sha256.Sum256(raw)
		var applied []byte
		err = s.db.QueryRow(ctx, `SELECT checksum FROM schema_migrations WHERE name=$1`, entry.Name()).Scan(&applied)
		if err == nil {
			if !bytes.Equal(applied, checksum[:]) {
				return fmt.Errorf("migration checksum changed: %s", entry.Name())
			}
			continue
		}
		if !errors.Is(err, pgx.ErrNoRows) {
			return err
		}
		if _, err := s.db.Exec(ctx, string(raw)); err != nil {
			return fmt.Errorf("apply migration %s: %w", entry.Name(), err)
		}
		if _, err := s.db.Exec(ctx, `INSERT INTO schema_migrations(name,checksum) VALUES ($1,$2)`, entry.Name(), checksum[:]); err != nil {
			return fmt.Errorf("record migration %s: %w", entry.Name(), err)
		}
	}
	return nil
}

var _ database = (*sqliteDatabase)(nil)

// Snapshot writes a consistent node-local copy for a Standalone operator. The
// caller must stop Access first; VACUUM INTO is the consistent-copy primitive
// and never mutates the live database.
func (s *Store) Snapshot(ctx context.Context, destination string) error {
	if s == nil || !s.sqlite {
		return errors.New("snapshot is only available for node-local SQLite storage")
	}
	if strings.TrimSpace(destination) == "" {
		return errors.New("snapshot destination is required")
	}
	if _, err := s.db.Exec(ctx, `VACUUM INTO $1`, destination); err != nil {
		return fmt.Errorf("snapshot database: %w", err)
	}
	return nil
}

// NextNodeSequence returns the next durable Central report sequence. It lives
// with the node's SQLite state so a normal process restart continues above the
// previous sequence instead of appearing stale. A replacement disk starts at
// one and must register a fresh incarnation.
func (s *Store) NextNodeSequence(ctx context.Context) (uint64, error) {
	if s == nil || s.db == nil {
		return 0, errors.New("store: no database")
	}
	var sequence int64
	err := s.db.QueryRow(ctx, `INSERT INTO app_settings(key,value) VALUES ('control.sequence','1')
		ON CONFLICT(key) DO UPDATE SET value = CAST(app_settings.value AS INTEGER) + 1
		RETURNING CAST(value AS INTEGER)`).Scan(&sequence)
	if err != nil {
		return 0, fmt.Errorf("next node sequence: %w", err)
	}
	if sequence < 1 {
		return 0, errors.New("next node sequence: invalid sequence")
	}
	return uint64(sequence), nil
}

// BusyErrors reports retryable SQLite contention observed by this store.
func (s *Store) BusyErrors() uint64 {
	if s == nil {
		return 0
	}
	return s.busy.Load()
}
