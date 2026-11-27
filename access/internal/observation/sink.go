package observation

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// Defaults bound the producer handoff and the on-disk capture. They are
// declared in contracts/latency/observation.json's implementation notes and
// must stay identical across producers.
const (
	DefaultCapacity     = 4096
	DefaultMaxBytes     = 32 << 20
	DefaultCloseTimeout = 5 * time.Second

	// ModeOff and ModeAggregate create no writer. ModeTrace is the only mode
	// that admits typed detail.
	ModeOff       = "off"
	ModeAggregate = "aggregate"
	ModeTrace     = "trace"
)

// Environment variables supplied by the container fixture before it launches a
// timed product process.
const (
	EnvMode       = "WHALEREIN_BENCH_MODE"
	EnvOutlet     = "WHALEREIN_BENCH_OUTLET"
	EnvRunID      = "WHALEREIN_BENCH_RUN_ID"
	EnvCaptureKey = "WHALEREIN_BENCH_CAPTURE_KEY"
	EnvLifetime   = "WHALEREIN_BENCH_LIFETIME"
	// The fixture exports the bounded handoff as OUTLET_CAPACITY/OUTLET_BYTES;
	// early producer revisions named them CAPACITY/BYTE_CAP. Accept either so
	// the fixture remains the single owner of these bounds.
	EnvCapacity    = "WHALEREIN_BENCH_OUTLET_CAPACITY"
	EnvBytes       = "WHALEREIN_BENCH_OUTLET_BYTES"
	EnvCapacityAlt = "WHALEREIN_BENCH_CAPACITY"
	EnvBytesAlt    = "WHALEREIN_BENCH_BYTE_CAP"
)

const producerRole = "access"

// Config describes one process outlet. Empty fields fall back to the fixture
// environment so the production composition root can call Open(Config{})
// while tests set fields directly. Output is an embed/test seam; production
// opens Outlet with owner-only permissions.
type Config struct {
	Mode         string
	Outlet       string
	RunID        string
	CaptureKey   string
	Lifetime     string
	Capacity     int
	MaxBytes     int64
	CloseTimeout time.Duration
	Clock        Clock
	Output       io.Writer
}

// Stats is the loss and volume accounting kept by the bounded outlet. A
// saturated or failing outlet increments Dropped; business handling is never
// blocked.
type Stats struct {
	Emitted     uint64
	Dropped     uint64
	Bytes       uint64
	WriteErrors uint64
	CapReached  bool
	Closed      bool
}

// Envelope is one NDJSON record. Its field names are the manifest header; the
// data object is constructed only from typed, allowlisted fields.
type Envelope struct {
	V        int            `json:"v"`
	Run      string         `json:"run"`
	Producer string         `json:"producer"`
	Lifetime string         `json:"lifetime"`
	Seq      uint64         `json:"seq"`
	TS       uint64         `json:"ts"`
	Event    string         `json:"event"`
	Data     map[string]any `json:"data"`
}

// Sink is the process-wide bounded observation outlet. A nil *Sink is the
// disabled state: every method returns immediately and carries zero cost.
type Sink struct {
	token *Tokenizer
	clock Clock

	runID    string
	producer string
	lifetime string

	capacity     int
	maxBytes     int64
	closeTimeout time.Duration
	output       io.Writer
	ownedFile    *os.File

	sequence uint64

	ch     chan []byte
	done   chan struct{}
	guard  sync.RWMutex
	closed atomic.Bool

	emitted     atomic.Uint64
	dropped     atomic.Uint64
	bytes       atomic.Int64
	writeErrors atomic.Uint64
	capReached  atomic.Bool
	writeFailed atomic.Bool

	closeOnce sync.Once
	closeErr  error
	finalErr  error
}

// Open constructs the process outlet. off and aggregate return (nil, nil) and
// create no writer. trace requires an outlet, run, capture key and a verified
// measurement clock; a missing or unreadable clock fails instead of mixing
// unverified time into a capture.
func Open(config Config) (*Sink, error) {
	config.Mode = strings.ToLower(strings.TrimSpace(firstNonEmpty(config.Mode, os.Getenv(EnvMode))))
	config.Outlet = firstNonEmpty(config.Outlet, os.Getenv(EnvOutlet))
	config.RunID = firstNonEmpty(config.RunID, os.Getenv(EnvRunID))
	config.CaptureKey = firstNonEmpty(config.CaptureKey, os.Getenv(EnvCaptureKey))
	config.Lifetime = firstNonEmpty(config.Lifetime, os.Getenv(EnvLifetime))

	switch config.Mode {
	case "", ModeOff, ModeAggregate:
		return nil, nil
	case ModeTrace:
	default:
		return nil, fmt.Errorf("observation: unsupported mode %q", config.Mode)
	}
	if config.Clock == nil {
		config.Clock = systemClock()
	}
	if _, err := config.Clock.Now(); err != nil {
		return nil, err
	}
	if config.RunID == "" {
		return nil, errors.New("observation: trace mode requires a run id")
	}
	if config.Output == nil && config.Outlet == "" {
		return nil, errors.New("observation: trace mode requires an outlet path")
	}
	tokenizer, err := NewTokenizer([]byte(config.CaptureKey))
	if err != nil {
		return nil, err
	}
	if config.Lifetime == "" {
		config.Lifetime, err = newLifetime()
		if err != nil {
			return nil, err
		}
	}
	if config.Capacity <= 0 {
		config.Capacity = envInt(firstNonEmpty(os.Getenv(EnvCapacity), os.Getenv(EnvCapacityAlt)))
	}
	if config.Capacity <= 0 {
		config.Capacity = DefaultCapacity
	}
	if config.MaxBytes <= 0 {
		config.MaxBytes = int64(envInt(firstNonEmpty(os.Getenv(EnvBytes), os.Getenv(EnvBytesAlt))))
	}
	if config.MaxBytes <= 0 {
		config.MaxBytes = DefaultMaxBytes
	}
	if config.CloseTimeout <= 0 {
		config.CloseTimeout = DefaultCloseTimeout
	}

	sink := &Sink{
		token:        tokenizer,
		clock:        config.Clock,
		runID:        config.RunID,
		producer:     producerRole,
		lifetime:     config.Lifetime,
		capacity:     config.Capacity,
		maxBytes:     config.MaxBytes,
		closeTimeout: config.CloseTimeout,
		output:       config.Output,
		ch:           make(chan []byte, config.Capacity),
		done:         make(chan struct{}),
	}
	if sink.output == nil {
		file, err := os.OpenFile(config.Outlet, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
		if err != nil {
			return nil, fmt.Errorf("observation: open outlet: %w", err)
		}
		sink.ownedFile = file
		sink.output = file
	}
	go sink.drain()
	return sink, nil
}

func (s *Sink) emit(event string, data map[string]any) {
	if s == nil {
		return
	}
	ts, err := s.clock.Now()
	if err != nil {
		s.dropped.Add(1)
		return
	}
	seq := atomic.AddUint64(&s.sequence, 1)
	line, err := json.Marshal(Envelope{
		V: 1, Run: s.runID, Producer: s.producer, Lifetime: s.lifetime,
		Seq: seq, TS: ts, Event: event, Data: data,
	})
	if err != nil {
		s.dropped.Add(1)
		return
	}
	line = append(line, '\n')
	s.guard.RLock()
	defer s.guard.RUnlock()
	if s.closed.Load() {
		s.dropped.Add(1)
		return
	}
	select {
	case s.ch <- line:
		s.emitted.Add(1)
	default:
		s.dropped.Add(1)
	}
}

// run drains the bounded channel. It never blocks a producer: a write error
// switches the outlet to discard mode while the writer keeps draining so
// Close remains prompt.
func (s *Sink) drain() {
	defer close(s.done)
	for line := range s.ch {
		if s.writeFailed.Load() {
			s.dropped.Add(1)
			continue
		}
		if s.maxBytes > 0 && s.bytes.Load()+int64(len(line)) > s.maxBytes {
			s.capReached.Store(true)
			s.dropped.Add(1)
			continue
		}
		written, err := s.output.Write(line)
		if err == nil && written != len(line) {
			err = io.ErrShortWrite
		}
		if err != nil {
			s.writeErrors.Add(1)
			s.writeFailed.Store(true)
			s.dropped.Add(1)
			if s.finalErr == nil {
				s.finalErr = fmt.Errorf("observation: write outlet: %w", err)
			}
			continue
		}
		s.bytes.Add(int64(written))
	}
	if syncer, ok := s.output.(interface{ Sync() error }); ok {
		if err := syncer.Sync(); err != nil {
			s.finalErr = errors.Join(s.finalErr, fmt.Errorf("observation: fsync outlet: %w", err))
		}
	}
	if s.ownedFile != nil {
		if err := s.ownedFile.Close(); err != nil {
			s.finalErr = errors.Join(s.finalErr, fmt.Errorf("observation: close outlet: %w", err))
		}
	}
}

// Close stops admission, drains best-effort, flushes and fsyncs within the
// configured bound. A stalled writer makes Close return an explicit timeout
// instead of blocking product shutdown indefinitely.
func (s *Sink) Close() error {
	if s == nil {
		return nil
	}
	s.closeOnce.Do(func() {
		s.guard.Lock()
		s.closed.Store(true)
		close(s.ch)
		s.guard.Unlock()

		timer := time.NewTimer(s.closeTimeout)
		defer timer.Stop()
		select {
		case <-s.done:
			s.closeErr = s.finalErr
		case <-timer.C:
			s.closeErr = errors.New("observation: outlet shutdown timed out")
		}
	})
	return s.closeErr
}

// Stats returns a snapshot of the outlet accounting.
func (s *Sink) Stats() Stats {
	if s == nil {
		return Stats{}
	}
	return Stats{
		Emitted:     s.emitted.Load(),
		Dropped:     s.dropped.Load(),
		Bytes:       uint64(s.bytes.Load()),
		WriteErrors: s.writeErrors.Load(),
		CapReached:  s.capReached.Load(),
		Closed:      s.closed.Load(),
	}
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
}

// envInt parses a positive bounded handoff setting. An absent or malformed
// value falls back to the package default rather than silently disabling the
// bound.
func envInt(value string) int {
	if value == "" {
		return 0
	}
	parsed, err := strconv.Atoi(value)
	if err != nil || parsed <= 0 {
		return 0
	}
	return parsed
}
