package broker

// Fragment opaque logical envelopes below the broker interface. Business
// handlers never see partial messages; command IDs remain execution identities.
import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"strings"
	"sync"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

const transportVersion = 2

// Retained default geometry, used by package tests and as the zero-value
// fallback. Runtime transport always reads the configured budget.
const transportChunkBytes = 60 * 1024
const transportPacketBytes = 64 * 1024
const transportMessageBytes = 16 * 1024 * 1024

const transportSendGroupWidth = 4
const transportAckTimeout = time.Second
const transportCompletedPerConcurrency = 16384

var transportPrefix = []byte("WRMQTT2")

var errTransport = errors.New("logical transport rejected or interrupted")

// ErrPeerTransportUnsupported means the peer does not speak the framed logical
// transport. ErrMessageBudget is a permanent logical-envelope refusal.
// ErrPeerBudgetMissing is a framed send with no authenticated receive budget.
var (
	ErrPeerTransportUnsupported = errors.New("peer does not support logical MQTT transport")
	ErrMessageBudget            = errors.New("logical message exceeds transfer resource budget")
	ErrPeerBudgetMissing        = fmt.Errorf("%w: peer receive budget is not registered", ErrPeerTransportUnsupported)
	ErrPeerBudgetInvalid        = fmt.Errorf("%w: peer receive budget is invalid", ErrPeerTransportUnsupported)
)

type transportState struct {
	total  uint32
	count  uint32
	width  uint32
	digest []byte
	parts  map[uint32][]byte
	until  time.Time
}
type transportReceipt struct {
	topic  string
	digest []byte
	done   chan bool
}
type logicalTransport struct {
	mu           sync.Mutex
	incoming     map[string]*transportState
	completed    map[string]*transportState
	pending      map[string]*transportReceipt
	bulkSlots    chan struct{}
	bulkWaiters  chan struct{}
	waitingBytes int64
	publish      func(context.Context, string, []byte) error
	deliver      func(string, []byte)
	now          func() time.Time
	ackTimeout   time.Duration

	local   config.TransportEffective
	metrics *diagnostics.Metrics
	logger  *slog.Logger

	stop        chan struct{}
	stopOnce    sync.Once
	sweeping    sync.Once
	interrupted chan struct{}
}

func newLogicalTransport(publish func(context.Context, string, []byte) error, deliver func(string, []byte)) *logicalTransport {
	local := config.DefaultTransportEffective()
	slots := int(local.BulkConcurrency)
	if slots <= 0 {
		slots = 1
	}
	return &logicalTransport{
		bulkSlots:   make(chan struct{}, slots),
		bulkWaiters: make(chan struct{}, slots),
		incoming:    map[string]*transportState{},
		completed:   map[string]*transportState{},
		pending:     map[string]*transportReceipt{},
		publish:     publish,
		deliver:     deliver,
		now:         time.Now,
		ackTimeout:  transportAckTimeout,
		local:       local,
		stop:        make(chan struct{}),
		interrupted: make(chan struct{}),
	}
}

func (t *logicalTransport) budget() config.TransportEffective {
	if t.local.EnvelopeBytes == 0 || t.local.FragmentPayloadBytes == 0 || t.local.PacketBytes == 0 {
		return config.DefaultTransportEffective()
	}
	return t.local
}

func (t *logicalTransport) setLocal(local config.TransportEffective, metrics *diagnostics.Metrics, logger *slog.Logger) {
	if local.EnvelopeBytes > 0 {
		t.local = local
	}
	t.metrics = metrics
	t.logger = logger
}

// startSweeper releases idle reassembly reservations without waiting for the
// next inbound frame. Expiry is bounded by fragment_idle_timeout.
func (t *logicalTransport) startSweeper() {
	t.sweeping.Do(func() {
		interval := t.budget().FragmentIdleTimeout / 2
		if interval <= 0 || interval > time.Second {
			interval = time.Second
		}
		go func() {
			ticker := time.NewTicker(interval)
			defer ticker.Stop()
			for {
				select {
				case <-t.stop:
					return
				case <-ticker.C:
					t.expire()
				}
			}
		}()
	})
}

func (t *logicalTransport) close() {
	t.stopOnce.Do(func() { close(t.stop) })
	t.reset()
}

func encodeTransport(frame *whalereinv1.TransportFrame) []byte {
	body, _ := proto.Marshal(frame)
	return append(append([]byte{}, transportPrefix...), body...)
}
func boolToUint64(value bool) uint64 {
	if value {
		return 1
	}
	return 0
}
func receiptKey(topic, id string, index uint32) string {
	return topic + "\x00" + id + "\x00" + string(rune(index))
}
func (t *logicalTransport) reset() {
	t.mu.Lock()
	defer t.mu.Unlock()
	for _, pending := range t.pending {
		select {
		case pending.done <- false:
		default:
		}
	}
	t.incoming = map[string]*transportState{}
	t.completed = map[string]*transportState{}
	t.publishReservationsLocked()
	// A reset interrupts queued senders and reassembly waits without
	// recreating the transport (the session is reused until reconnect).
	close(t.interrupted)
	t.interrupted = make(chan struct{})
}

func (t *logicalTransport) interruptedChan() <-chan struct{} {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.interrupted
}

func (t *logicalTransport) recordReject(code string, bytes, limit int) {
	if t.metrics != nil {
		t.metrics.TransportRejected(code, int64(bytes), int64(limit))
	}
	if t.logger != nil {
		t.logger.Warn("logical MQTT frame rejected", "code", code, "bytes", bytes, "limit", limit)
	}
}

func (t *logicalTransport) recordFrame(direction string, bytes int) {
	if t.metrics != nil {
		t.metrics.TransportFrame(direction, int64(bytes))
	}
}

// admissionBudget is the configured payload reservation plus the maximum
// per-fragment bookkeeping implied by the configured geometry. It lets a full
// envelope fit while still charging every fragment slot, so a tiny fragment
// width cannot consume unbounded bookkeeping for negligible payload bytes.
func admissionBudget(local config.TransportEffective) int64 {
	overhead := uint64(local.PacketOverheadBytes)
	fragments := uint64(local.BulkConcurrency) * uint64(local.FragmentCount)
	if overhead != 0 && fragments > math.MaxUint64/overhead {
		return math.MaxInt64
	}
	bookkeeping := fragments * overhead
	if local.ReservedBytes > math.MaxUint64-bookkeeping {
		return math.MaxInt64
	}
	total := local.ReservedBytes + bookkeeping
	if total > math.MaxInt64 {
		return math.MaxInt64
	}
	return int64(total)
}

func transferReservation(total, count, overhead uint64) int64 {
	if count != 0 && overhead > math.MaxUint64/count {
		return math.MaxInt64
	}
	reserved := total + count*overhead
	if reserved < total || reserved > math.MaxInt64 {
		return math.MaxInt64
	}
	return int64(reserved)
}

func (t *logicalTransport) reservedLocked() int64 {
	var reserved int64
	for _, state := range t.incoming {
		accounted := transferReservation(uint64(state.total), uint64(state.count), uint64(t.budget().PacketOverheadBytes))
		if accounted >= math.MaxInt64-reserved {
			return math.MaxInt64
		}
		reserved += accounted
	}
	return reserved
}

func (t *logicalTransport) publishReservationsLocked() {
	if t.metrics == nil {
		return
	}
	t.metrics.TransportReservations(len(t.incoming), t.reservedLocked())
}

func (t *logicalTransport) expireLocked(now time.Time) bool {
	expired := false
	for _, states := range []map[string]*transportState{t.incoming, t.completed} {
		for key, state := range states {
			if !state.until.After(now) {
				delete(states, key)
				expired = true
			}
		}
	}
	return expired
}

func (t *logicalTransport) expire() {
	t.mu.Lock()
	if t.expireLocked(t.now()) {
		t.publishReservationsLocked()
	}
	t.mu.Unlock()
}

func (t *logicalTransport) completedLimit() int {
	limit := int64(t.budget().BulkConcurrency) * transportCompletedPerConcurrency
	if limit < 1024 {
		limit = 1024
	}
	if limit > 1<<20 {
		limit = 1 << 20
	}
	return int(limit)
}

func (t *logicalTransport) receive(topic string, payload []byte, ackTopic string) (bool, error) {
	if !bytes.HasPrefix(payload, transportPrefix) {
		return false, nil
	}
	local := t.budget()
	if len(payload) > int(local.PacketBytes) {
		t.recordReject(transportCodePacket, len(payload), int(local.PacketBytes))
		return true, transportReject(transportCodePacket, errTransport)
	}
	frame := &whalereinv1.TransportFrame{}
	if proto.Unmarshal(payload[len(transportPrefix):], frame) != nil || frame.Version != transportVersion ||
		len(frame.TransferId) != 32 || len(frame.Topic) == 0 || len(frame.Topic) > 512 || len(frame.Digest) != 32 {
		t.recordReject(transportCodeHeader, len(payload), 0)
		return true, transportReject(transportCodeHeader, errTransport)
	}
	if _, err := hex.DecodeString(frame.TransferId); err != nil {
		t.recordReject(transportCodeHeader, len(payload), 0)
		return true, transportReject(transportCodeHeader, errTransport)
	}
	if _, ok := whalereinv1.TransportProfile_name[int32(frame.GetProfile())]; !ok {
		t.recordReject(transportCodeHeader, len(payload), 0)
		return true, transportReject(transportCodeHeader, errTransport)
	}
	t.mu.Lock()
	if frame.Ack {
		pending := t.pending[receiptKey(frame.Topic, frame.TransferId, frame.Index)]
		if pending != nil && pending.topic == topic && bytes.Equal(pending.digest, frame.Digest) {
			select {
			case pending.done <- !frame.Rejected:
			default:
			}
		}
		completion := t.pending[frame.Topic+"\x00"+frame.TransferId+"\x00complete"]
		if completion != nil && completion.topic == topic && bytes.Equal(completion.digest, frame.Digest) && (frame.Complete || frame.Rejected) {
			for key, pending := range t.pending {
				if strings.HasPrefix(key, frame.Topic+"\x00"+frame.TransferId+"\x00") && pending.topic == topic && bytes.Equal(pending.digest, frame.Digest) {
					select {
					case pending.done <- !frame.Rejected:
					default:
					}
				}
			}
		}
		t.mu.Unlock()
		return true, nil
	}
	if frame.Topic != topic || ackTopic == "" {
		t.mu.Unlock()
		t.recordReject(transportCodeTopic, len(payload), 0)
		return true, transportReject(transportCodeTopic, errTransport)
	}
	total := uint64(frame.TotalBytes)
	if total == 0 || total > local.EnvelopeBytes {
		t.mu.Unlock()
		t.recordReject(transportCodeEnvelope, len(frame.Payload), int(local.EnvelopeBytes))
		return true, transportReject(transportCodeEnvelope, fmt.Errorf("%w: %w", errTransport, ErrMessageBudget))
	}
	width := uint64(frame.GetFragmentPayloadBytes())
	if width == 0 || width > uint64(local.FragmentPayloadBytes) {
		t.mu.Unlock()
		t.recordReject(transportCodeWidth, len(frame.Payload), int(local.FragmentPayloadBytes))
		return true, transportReject(transportCodeWidth, errTransport)
	}
	count := uint64(frame.Count)
	if count == 0 || count > uint64(local.FragmentCount) {
		t.mu.Unlock()
		t.recordReject(transportCodeFragments, len(frame.Payload), int(local.FragmentCount))
		return true, transportReject(transportCodeFragments, errTransport)
	}
	index := uint64(frame.Index)
	expectedCount := total/width + boolToUint64(total%width != 0)
	if index >= count || count != expectedCount {
		t.mu.Unlock()
		t.recordReject(transportCodeGeometry, len(frame.Payload), int(width))
		return true, transportReject(transportCodeGeometry, errTransport)
	}
	// The index is below ceil(total/width), so offset < total and the
	// subtraction cannot underflow even for a maximal envelope.
	offset := index * width
	expected := min(width, total-offset)
	if uint64(len(frame.Payload)) != expected {
		t.mu.Unlock()
		t.recordReject(transportCodeGeometry, len(frame.Payload), int(expected))
		return true, transportReject(transportCodeGeometry, errTransport)
	}
	now := t.now()
	t.expireLocked(now)
	key := frame.Topic + "\x00" + frame.TransferId
	state := t.completed[key]
	done := state != nil
	if state == nil {
		state = t.incoming[key]
	}
	if state == nil {
		if len(t.completed) >= t.completedLimit() || len(t.incoming) >= int(local.BulkConcurrency) {
			t.mu.Unlock()
			t.recordReject(transportCodeReassembly, int(total), int(local.ReservedBytes))
			return true, transportReject(transportCodeReassembly, errTransport)
		}
		reservation := transferReservation(total, count, uint64(local.PacketOverheadBytes))
		if reservation >= math.MaxInt64-t.reservedLocked() || t.reservedLocked()+reservation > admissionBudget(local) {
			t.mu.Unlock()
			t.recordReject(transportCodeReassembly, int(total), int(local.ReservedBytes))
			return true, transportReject(transportCodeReassembly, errTransport)
		}
		state = &transportState{total: frame.TotalBytes, count: frame.Count, width: frame.GetFragmentPayloadBytes(),
			digest: append([]byte{}, frame.Digest...), parts: map[uint32][]byte{}, until: now.Add(local.FragmentIdleTimeout)}
		t.incoming[key] = state
		t.publishReservationsLocked()
	}
	if state.total != frame.TotalBytes || state.count != frame.Count || state.width != frame.GetFragmentPayloadBytes() || !bytes.Equal(state.digest, frame.Digest) {
		t.mu.Unlock()
		t.recordReject(transportCodeGeometry, len(frame.Payload), int(state.width))
		return true, transportReject(transportCodeGeometry, errTransport)
	}
	if previous, ok := state.parts[frame.Index]; ok && !bytes.Equal(previous, frame.Payload) {
		t.mu.Unlock()
		t.recordReject(transportCodeGeometry, len(frame.Payload), int(len(previous)))
		return true, transportReject(transportCodeGeometry, errTransport)
	}
	var logical []byte
	rejected := false
	if !done {
		state.parts[frame.Index] = append([]byte{}, frame.Payload...)
		if uint32(len(state.parts)) == state.count {
			// The reservation was validated before any part buffer existed.
			logical = make([]byte, 0, state.total)
			for part := uint32(0); part < state.count; part++ {
				logical = append(logical, state.parts[part]...)
			}
			hash := sha256.Sum256(logical)
			rejected = !bytes.Equal(hash[:], state.digest)
			delete(t.incoming, key)
			if !rejected {
				state.parts = nil
				t.completed[key] = state
				t.recordFrame("in", int(state.total))
			} else {
				logical = nil
				t.recordReject(transportCodeIntegrity, int(state.total), 0)
			}
		}
	}
	t.publishReservationsLocked()
	t.mu.Unlock()
	ack := encodeTransport(&whalereinv1.TransportFrame{
		Version: transportVersion, TransferId: frame.TransferId, Topic: topic, Index: frame.Index,
		Digest: frame.Digest, Ack: true, Rejected: rejected, Complete: !rejected && (done || logical != nil),
		FragmentPayloadBytes: frame.GetFragmentPayloadBytes(), Profile: frame.GetProfile(),
	})
	// Dispatch and receipts are outside the MQTT reader callback, avoiding a
	// publish/PUBACK deadlock. Receipt means assembled, not executed.
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), t.budget().TransferTimeout)
		defer cancel()
		_ = t.publish(ctx, ackTopic, ack)
	}()
	if logical != nil {
		go t.deliver(topic, logical)
	}
	if rejected {
		return true, transportReject(transportCodeIntegrity, errTransport)
	}
	return true, nil
}

// acquisition budgets a bulk send in the configured outbound lane. Controls
// and small envelopes never enter this lane, so pressure cannot stop them.
func (t *logicalTransport) acquireBulk(ctx context.Context, size int) error {
	select {
	case t.bulkSlots <- struct{}{}:
		return nil
	default:
	}
	local := t.budget()
	t.mu.Lock()
	if uint64(size) > local.ReservedBytes || t.waitingBytes+int64(size) > int64(local.ReservedBytes) {
		t.mu.Unlock()
		return transportReject(transportCodeAdmission, errTransport)
	}
	select {
	case t.bulkWaiters <- struct{}{}:
	default:
		t.mu.Unlock()
		return transportReject(transportCodeAdmission, errTransport)
	}
	t.waitingBytes += int64(size)
	t.mu.Unlock()
	defer func() { <-t.bulkWaiters; t.mu.Lock(); t.waitingBytes -= int64(size); t.mu.Unlock() }()
	timer := time.NewTimer(local.FragmentIdleTimeout)
	defer timer.Stop()
	select {
	case t.bulkSlots <- struct{}{}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-t.interruptedChan():
		return transportReject(transportCodeAdmission, errTransport)
	case <-timer.C:
		return transportReject(transportCodeAdmission, errTransport)
	}
}

func (t *logicalTransport) send(ctx context.Context, topic string, payload []byte, ackTopic string, capable bool) error {
	local := t.budget()
	return t.sendTo(ctx, topic, payload, ackTopic, capable, &local)
}

// sendTo frames one logical envelope for a peer with a registered receive
// budget. A nil peer means no authenticated advertisement: small legacy
// envelopes still work, but a framed send fails explicitly.
func (t *logicalTransport) sendTo(ctx context.Context, topic string, payload []byte, ackTopic string, capable bool, peer *config.TransportEffective) error {
	local := t.budget()
	budget := local
	if peer != nil && peer.EnvelopeBytes > 0 {
		budget = negotiateBudget(local, *peer)
	}
	if len(payload) > int(budget.EnvelopeBytes) || uint64(len(payload)) > uint64(math.MaxUint32) {
		return fmt.Errorf("%w: %w", errTransport, ErrMessageBudget)
	}
	if len(payload) == 0 {
		return transportReject(transportCodeEnvelope, errTransport)
	}
	if bytes.HasPrefix(payload, transportPrefix) {
		return t.publish(ctx, topic, payload)
	}
	width := int(budget.FragmentPayloadBytes)
	if len(payload) <= width && !capable {
		return t.publish(ctx, topic, payload)
	}
	if !capable {
		return ErrPeerTransportUnsupported
	}
	// A framed send needs the authenticated receive shape; the local profile
	// alone cannot prove the peer's fragment width. Small bootstrap and control
	// envelopes remain legacy publications so readiness and revocation are not
	// blocked by a not-yet-registered advertisement.
	if peer == nil {
		if len(payload) <= width {
			return t.publish(ctx, topic, payload)
		}
		return ErrPeerBudgetMissing
	}
	count := (len(payload) + width - 1) / width
	if count > int(budget.FragmentCount) {
		return fmt.Errorf("%w: %w", errTransport, ErrMessageBudget)
	}
	ctx, cancel := context.WithTimeout(ctx, budget.TransferTimeout)
	defer cancel()
	bulk := len(payload) > width
	if bulk {
		if err := t.acquireBulk(ctx, len(payload)); err != nil {
			return err
		}
		defer func() { <-t.bulkSlots }()
	}

	raw := make([]byte, 16)
	if _, err := rand.Read(raw); err != nil {
		return err
	}
	id := hex.EncodeToString(raw)
	hash := sha256.Sum256(payload)
	completionKey := topic + "\x00" + id + "\x00complete"
	completion := &transportReceipt{topic: ackTopic, digest: hash[:], done: make(chan bool, 1)}
	t.mu.Lock()
	t.pending[completionKey] = completion
	t.mu.Unlock()
	defer func() { t.mu.Lock(); delete(t.pending, completionKey); t.mu.Unlock() }()
	for start := 0; start < count; start += transportSendGroupWidth {
		var group sync.WaitGroup
		failures := make(chan error, transportSendGroupWidth)
		for index := start; index < min(start+transportSendGroupWidth, count); index++ {
			group.Add(1)
			go func(index int) {
				defer group.Done()
				if err := t.sendPart(ctx, topic, payload, ackTopic, id, hash[:], count, index, width, budget.WireProfile); err != nil {
					failures <- err
					cancel()
				}
			}(index)
		}
		group.Wait()
		close(failures)
		for err := range failures {
			return err
		}
	}
	timer := time.NewTimer(t.ackTimeout * 3)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case accepted := <-completion.done:
		if accepted {
			t.recordFrame("out", len(payload))
			return nil
		}
		return transportReject(transportCodeReceipt, errTransport)
	case <-timer.C:
		return transportReject(transportCodeReceipt, errTransport)
	}
}

func (t *logicalTransport) sendPart(ctx context.Context, topic string, payload []byte, ackTopic, id string, hash []byte, count, index, width int, profile whalereinv1.TransportProfile) error {
	key := receiptKey(topic, id, uint32(index))
	pending := &transportReceipt{topic: ackTopic, digest: hash, done: make(chan bool, 1)}
	t.mu.Lock()
	t.pending[key] = pending
	t.mu.Unlock()
	defer func() { t.mu.Lock(); delete(t.pending, key); t.mu.Unlock() }()
	frame := encodeTransport(&whalereinv1.TransportFrame{
		Version: transportVersion, TransferId: id, Topic: topic, Index: uint32(index), Count: uint32(count),
		TotalBytes: uint32(len(payload)), Digest: hash, Payload: payload[index*width : min(len(payload), (index+1)*width)],
		FragmentPayloadBytes: uint32(width), Profile: profile,
	})
	for attempt := 0; attempt < 3; attempt++ {
		if err := t.publish(ctx, topic, frame); err != nil {
			return err
		}
		timer := time.NewTimer(t.ackTimeout)
		select {
		case <-ctx.Done():
			timer.Stop()
			return ctx.Err()
		case accepted := <-pending.done:
			timer.Stop()
			if accepted {
				return nil
			}
			return transportReject(transportCodeReceipt, errTransport)
		case <-timer.C:
		}
	}
	return transportReject(transportCodeReceipt, errTransport)
}
func transportAckTopic(topic string) string {
	parsed, err := ParseTopic(topic)
	if err != nil {
		return ""
	}
	switch parsed.Kind {
	case KindResult, KindEvent:
		return RequestTopic(parsed.HostID)
	case KindRequest:
		return ResultTopic(parsed.HostID)
	case KindDeviceEvent:
		return DeviceAckTopic(parsed.HostID, parsed.DeviceID)
	}
	return ""
}
