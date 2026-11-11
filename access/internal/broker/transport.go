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
	"strings"
	"sync"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

const transportVersion = 2
const transportChunkBytes = 60 * 1024
const transportPacketBytes = 64 * 1024
const transportMessageBytes = 16 * 1024 * 1024

var transportPrefix = []byte("WRMQTT2")
var errTransport = errors.New("logical transport rejected or interrupted")
var ErrPeerTransportUnsupported = errors.New("peer does not support logical MQTT transport")
var ErrMessageBudget = errors.New("logical message exceeds transfer resource budget")

type transportState struct {
	total  uint32
	count  uint32
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
	waitingBytes int
	publish      func(context.Context, string, []byte) error
	deliver      func(string, []byte)
	now          func() time.Time
	ackTimeout   time.Duration
}

func newLogicalTransport(publish func(context.Context, string, []byte) error, deliver func(string, []byte)) *logicalTransport {
	return &logicalTransport{bulkSlots: make(chan struct{}, 2), bulkWaiters: make(chan struct{}, 4), incoming: map[string]*transportState{}, completed: map[string]*transportState{}, pending: map[string]*transportReceipt{}, publish: publish, deliver: deliver, now: time.Now, ackTimeout: time.Second}
}
func encodeTransport(frame *whalereinv1.TransportFrame) []byte {
	body, _ := proto.Marshal(frame)
	return append(append([]byte{}, transportPrefix...), body...)
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
}
func (t *logicalTransport) receive(topic string, payload []byte, ackTopic string) (bool, error) {
	if !bytes.HasPrefix(payload, transportPrefix) {
		return false, nil
	}
	if len(payload) > transportPacketBytes {
		return true, errTransport
	}
	frame := &whalereinv1.TransportFrame{}
	if proto.Unmarshal(payload[len(transportPrefix):], frame) != nil || frame.Version != transportVersion || len(frame.TransferId) != 32 || len(frame.Topic) == 0 || len(frame.Topic) > 512 || len(frame.Digest) != 32 {
		return true, errTransport
	}
	if _, err := hex.DecodeString(frame.TransferId); err != nil {
		return true, errTransport
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
	if frame.Topic != topic || ackTopic == "" || frame.TotalBytes == 0 || frame.TotalBytes > transportMessageBytes || frame.Count != (frame.TotalBytes+transportChunkBytes-1)/transportChunkBytes || frame.Index >= frame.Count {
		t.mu.Unlock()
		return true, errTransport
	}
	expected := min(uint32(transportChunkBytes), frame.TotalBytes-frame.Index*transportChunkBytes)
	if uint32(len(frame.Payload)) != expected {
		t.mu.Unlock()
		return true, errTransport
	}
	now := t.now()
	for _, states := range []map[string]*transportState{t.incoming, t.completed} {
		for key, state := range states {
			if !state.until.After(now) {
				delete(states, key)
			}
		}
	}
	key := frame.Topic + "\x00" + frame.TransferId
	state := t.completed[key]
	done := state != nil
	if state == nil {
		state = t.incoming[key]
	}
	if state == nil {
		var reserved uint32
		for _, item := range t.incoming {
			reserved += item.total
		}
		if len(t.completed) >= 65536 || (frame.TotalBytes > transportChunkBytes && (len(t.incoming) >= 4 || reserved+frame.TotalBytes > 32*1024*1024)) {
			t.mu.Unlock()
			return true, errTransport
		}
		state = &transportState{total: frame.TotalBytes, count: frame.Count, digest: append([]byte{}, frame.Digest...), parts: map[uint32][]byte{}, until: now.Add(30 * time.Second)}
		t.incoming[key] = state
	}
	if state.total != frame.TotalBytes || state.count != frame.Count || !bytes.Equal(state.digest, frame.Digest) {
		t.mu.Unlock()
		return true, errTransport
	}
	if previous, ok := state.parts[frame.Index]; ok && !bytes.Equal(previous, frame.Payload) {
		t.mu.Unlock()
		return true, errTransport
	}
	var logical []byte
	rejected := false
	if !done {
		state.parts[frame.Index] = append([]byte{}, frame.Payload...)
		if uint32(len(state.parts)) == state.count {
			logical = make([]byte, 0, state.total)
			for index := uint32(0); index < state.count; index++ {
				logical = append(logical, state.parts[index]...)
			}
			hash := sha256.Sum256(logical)
			rejected = !bytes.Equal(hash[:], state.digest)
			delete(t.incoming, key)
			if !rejected {
				state.parts = nil
				t.completed[key] = state
			} else {
				logical = nil
			}
		}
	}
	t.mu.Unlock()
	ack := encodeTransport(&whalereinv1.TransportFrame{Version: transportVersion, TransferId: frame.TransferId, Topic: topic, Index: frame.Index, Digest: frame.Digest, Ack: true, Rejected: rejected, Complete: !rejected && (done || logical != nil)})
	// Dispatch and receipts are outside the MQTT reader callback, avoiding a
	// publish/PUBACK deadlock. Receipt means assembled, not executed.
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		_ = t.publish(ctx, ackTopic, ack)
	}()
	if logical != nil {
		go t.deliver(topic, logical)
	}
	if rejected {
		return true, errTransport
	}
	return true, nil
}

// Admission is bounded backpressure, independent of business messages.
func (t *logicalTransport) acquireBulk(ctx context.Context, size int) error {
	select {
	case t.bulkSlots <- struct{}{}:
		return nil
	default:
	}
	t.mu.Lock()
	if t.waitingBytes+size > 32*1024*1024 {
		t.mu.Unlock()
		return errTransport
	}
	select {
	case t.bulkWaiters <- struct{}{}:
	default:
		t.mu.Unlock()
		return errTransport
	}
	t.waitingBytes += size
	t.mu.Unlock()
	defer func() { <-t.bulkWaiters; t.mu.Lock(); t.waitingBytes -= size; t.mu.Unlock() }()
	select {
	case t.bulkSlots <- struct{}{}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (t *logicalTransport) send(ctx context.Context, topic string, payload []byte, ackTopic string, capable bool) error {
	if len(payload) > transportMessageBytes {
		return ErrMessageBudget
	}
	if len(payload) == 0 {
		return errTransport
	}
	if (len(payload) <= transportChunkBytes && !capable) || bytes.HasPrefix(payload, transportPrefix) {
		return t.publish(ctx, topic, payload)
	}
	if !capable {
		return ErrPeerTransportUnsupported
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	if len(payload) > transportChunkBytes {
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
	count := (len(payload) + transportChunkBytes - 1) / transportChunkBytes
	for start := 0; start < count; start += 4 {
		var group sync.WaitGroup
		failures := make(chan error, 4)
		for index := start; index < min(start+4, count); index++ {
			group.Add(1)
			go func(index int) {
				defer group.Done()
				if err := t.sendPart(ctx, topic, payload, ackTopic, id, hash[:], count, index); err != nil {
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
			return nil
		}
		return errTransport
	case <-timer.C:
		return errTransport
	}
}
func (t *logicalTransport) sendPart(ctx context.Context, topic string, payload []byte, ackTopic, id string, hash []byte, count, index int) error {
	key := receiptKey(topic, id, uint32(index))
	pending := &transportReceipt{topic: ackTopic, digest: hash, done: make(chan bool, 1)}
	t.mu.Lock()
	t.pending[key] = pending
	t.mu.Unlock()
	defer func() { t.mu.Lock(); delete(t.pending, key); t.mu.Unlock() }()
	frame := encodeTransport(&whalereinv1.TransportFrame{Version: transportVersion, TransferId: id, Topic: topic, Index: uint32(index), Count: uint32(count), TotalBytes: uint32(len(payload)), Digest: hash, Payload: payload[index*transportChunkBytes : min(len(payload), (index+1)*transportChunkBytes)]})
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
			return errTransport
		case <-timer.C:
		}
	}
	return errTransport
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
