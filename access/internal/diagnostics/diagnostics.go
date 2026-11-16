package diagnostics

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"regexp"
	"strings"
	"sync"
	"time"
	"unicode"
)

// MaxRedactedLength bounds a redacted diagnostic message so one malformed
// backend error cannot flood a log line.
const MaxRedactedLength = 512

// sqlLiteral matches a quoted SQL string literal. Backend errors sometimes
// embed parameter values that way; the content is replaced before logging.
var sqlLiteral = regexp.MustCompile(`'(?:[^']|'')*'`)

// Masker produces process-local salted identifiers for logs. The salt is
// random on every process start, so identifiers cannot be correlated across
// restarts or diagnostic exports.
type Masker struct {
	key []byte
}

// NewMasker creates a masker with a random per-process key.
func NewMasker() *Masker {
	key := make([]byte, 32)
	if _, err := rand.Read(key); err != nil {
		panic("diagnostics: cannot read random salt: " + err.Error())
	}
	return &Masker{key: key}
}

// NewMaskerWithKey creates a deterministic masker from an explicit key. It is
// intended for tests only.
func NewMaskerWithKey(key []byte) *Masker {
	copied := append([]byte(nil), key...)
	return &Masker{key: copied}
}

func (m *Masker) id(prefix, value string) string {
	if m == nil || value == "" {
		return ""
	}
	mac := hmac.New(sha256.New, m.key)
	_, _ = mac.Write([]byte(value))
	return prefix + "-" + hex.EncodeToString(mac.Sum(nil))[:16]
}

// Host returns the salted diagnostic id for a host identifier.
func (m *Masker) Host(id string) string { return m.id("h", id) }

// Device returns the salted diagnostic id for a device identifier.
func (m *Masker) Device(id string) string { return m.id("d", id) }

// Request returns the salted diagnostic id for a request identifier.
func (m *Masker) Request(id string) string { return m.id("r", id) }

// Redact replaces every known raw identifier in text with its salted form,
// removes control characters (so a crafted value cannot forge log lines) and
// bounds the result. Unknown content is preserved for diagnosis.
func (m *Masker) Redact(text string, replacements map[string]string) string {
	for raw, masked := range replacements {
		if raw == "" || masked == "" {
			continue
		}
		text = strings.ReplaceAll(text, raw, masked)
	}
	text = sqlLiteral.ReplaceAllString(text, "'?'")
	cleaned := strings.Map(func(r rune) rune {
		if r == '\n' || r == '\r' || r == '\t' {
			return ' '
		}
		if unicode.IsControl(r) {
			return -1
		}
		return r
	}, text)
	if len(cleaned) > MaxRedactedLength {
		if runes := []rune(cleaned); len(runes) > MaxRedactedLength {
			cleaned = string(runes[:MaxRedactedLength]) + "…"
		}
	}
	return cleaned
}

// NewLogger returns a JSON structured logger at the configured level.
func NewLogger(level string, w io.Writer) *slog.Logger {
	var lvl slog.Level
	switch strings.ToLower(level) {
	case "debug":
		lvl = slog.LevelDebug
	case "warn":
		lvl = slog.LevelWarn
	case "error":
		lvl = slog.LevelError
	default:
		lvl = slog.LevelInfo
	}
	handler := slog.NewJSONHandler(w, &slog.HandlerOptions{Level: lvl})
	return slog.New(handler).With("service", "whalerein-access")
}

// Readiness aggregates named checks that decide whether Access may accept
// traffic.
type Readiness struct {
	mu     sync.RWMutex
	checks map[string]func(context.Context) error
	order  []string
}

// NewReadiness creates an empty readiness registry.
func NewReadiness() *Readiness {
	return &Readiness{checks: make(map[string]func(context.Context) error)}
}

// Add registers a named check. Duplicate names replace the previous check.
func (r *Readiness) Add(name string, check func(context.Context) error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, exists := r.checks[name]; !exists {
		r.order = append(r.order, name)
	}
	r.checks[name] = check
}

// Check runs every registered check and returns the failures.
func (r *Readiness) Check(ctx context.Context) map[string]string {
	r.mu.RLock()
	order := append([]string(nil), r.order...)
	checks := make(map[string]func(context.Context) error, len(r.checks))
	for name, check := range r.checks {
		checks[name] = check
	}
	r.mu.RUnlock()

	failures := make(map[string]string)
	for _, name := range order {
		check := checks[name]
		if check == nil {
			continue
		}
		if err := check(ctx); err != nil {
			failures[name] = "unavailable"
		}
	}
	return failures
}

// HealthHandler is process liveness and always succeeds while the process can
// serve HTTP.
func HealthHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.WriteHeader(http.StatusOK)
		_, _ = io.WriteString(w, "ok\n")
	})
}

// ReadyHandler reports readiness as JSON. It never exposes backend error
// details, only the failing check names.
func (r *Readiness) ReadyHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		ctx, cancel := context.WithTimeout(req.Context(), 3*time.Second)
		defer cancel()
		failures := r.Check(ctx)
		w.Header().Set("Content-Type", "application/json")
		if len(failures) == 0 {
			w.WriteHeader(http.StatusOK)
			_ = json.NewEncoder(w).Encode(map[string]any{"status": "ready"})
			return
		}
		w.WriteHeader(http.StatusServiceUnavailable)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"status": "unavailable",
			"checks": failures,
		})
	})
}

// DatabaseHealth is the durable-state check used by readiness. Ping alone
// cannot distinguish a writable primary from a read-only replica or an
// exhausted pool, so readiness also requires a bounded write probe.
type DatabaseHealth interface {
	Ping(context.Context) error
	WriteProbe(context.Context) error
}

// DatabaseReadiness returns the readiness check for durable state. A database
// that answers Ping but rejects a write keeps the instance out of rotation.
func DatabaseReadiness(db DatabaseHealth) func(context.Context) error {
	return func(ctx context.Context) error {
		if err := db.Ping(ctx); err != nil {
			return err
		}
		return db.WriteProbe(ctx)
	}
}

// WithTraceID carries only the ephemeral operation ID across internal services.
type traceContextKey struct{}

func WithTraceID(ctx context.Context, id string) context.Context {
	return context.WithValue(ctx, traceContextKey{}, id)
}

func TraceID(ctx context.Context) string {
	id, _ := ctx.Value(traceContextKey{}).(string)
	return id
}
