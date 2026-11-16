package broker

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func testTransportFrame(id string, index int) *whalereinv1.TransportFrame {
	hash := sha256.Sum256(make([]byte, transportChunkBytes+1))
	part := make([]byte, transportChunkBytes)
	if index == 1 {
		part = make([]byte, 1)
	}
	return &whalereinv1.TransportFrame{Version: 2, TransferId: id, Topic: "data", Index: uint32(index), Count: 2, TotalBytes: transportChunkBytes + 1, Digest: hash[:], Payload: part}
}

func TestLogicalTransportReceiptsRetransmitAndNoPartialDelivery(t *testing.T) {
	var sender *logicalTransport
	var count atomic.Int32
	var dropped atomic.Bool
	var wirePackets atomic.Int32
	var callbackFailed atomic.Bool
	completed := make(chan []byte, 2)
	receiver := newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		ack := &whalereinv1.TransportFrame{}
		_ = proto.Unmarshal(payload[len(transportPrefix):], ack)
		if ack.Index == 0 && !dropped.Swap(true) {
			return nil
		}
		_, err := sender.receive(topic, payload, "")
		return err
	}, func(_ string, payload []byte) { count.Add(1); completed <- append([]byte(nil), payload...) })
	var first []byte
	var mu sync.Mutex
	sender = newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		if len(payload) > transportPacketBytes {
			callbackFailed.Store(true)
		}
		wirePackets.Add(1)
		mu.Lock()
		if first == nil {
			first = append([]byte{}, payload...)
		}
		mu.Unlock()
		_, err := receiver.receive(topic, payload, "ack")
		return err
	}, func(string, []byte) {})
	sender.ackTimeout = 20 * time.Millisecond
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	want := make([]byte, 2*1024*1024)
	for index := range want {
		want[index] = byte(index % 251)
	}
	if err := sender.send(ctx, "data", want, "ack", true); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(<-completed, want) || callbackFailed.Load() {
		t.Fatal("invalid transport delivery")
	}
	if _, err := receiver.receive("data", first, "ack"); err != nil {
		t.Fatal(err)
	}
	if count.Load() != 1 || wirePackets.Load() < 35 {
		t.Fatal("logical envelope was duplicated or not fragmented")
	}
	if err := sender.send(ctx, "data", make([]byte, transportChunkBytes+1), "ack", false); err == nil {
		t.Fatal("unsupported peer accepted")
	}
	if err := sender.send(ctx, "data", make([]byte, transportMessageBytes+1), "ack", true); err == nil {
		t.Fatal("logical budget bypassed")
	}
	receiver.reset()
	sender.reset()
}

func TestLogicalTransportRejectsMalformedFramesAndQuotaExcessWithoutPartialDelivery(t *testing.T) {
	delivered := 0
	receiver := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) { delivered++ })
	if consumed, err := receiver.receive("data", []byte("legacy"), "ack"); consumed || err != nil {
		t.Fatal("legacy rejected")
	}
	if _, err := receiver.receive("data", append(append([]byte{}, transportPrefix...), make([]byte, transportPacketBytes)...), "ack"); err == nil {
		t.Fatal("large packet accepted")
	}
	if _, err := receiver.receive("data", transportPrefix, "ack"); err == nil {
		t.Fatal("empty frame accepted")
	}
	changes := map[string]func(*whalereinv1.TransportFrame){
		"unsupported_version":          func(f *whalereinv1.TransportFrame) { f.Version = 3 },
		"short_transfer_id":            func(f *whalereinv1.TransportFrame) { f.TransferId = "bad" },
		"non_hex_transfer_id":          func(f *whalereinv1.TransportFrame) { f.TransferId = "z" + f.TransferId[1:] },
		"missing_digest":               func(f *whalereinv1.TransportFrame) { f.Digest = nil },
		"different_topic":              func(f *whalereinv1.TransportFrame) { f.Topic = "other" },
		"empty_envelope":               func(f *whalereinv1.TransportFrame) { f.TotalBytes = 0 },
		"wrong_chunk_count":            func(f *whalereinv1.TransportFrame) { f.Count = 3 },
		"out_of_range_index":           func(f *whalereinv1.TransportFrame) { f.Index = 2 },
		"missing_chunk_payload":        func(f *whalereinv1.TransportFrame) { f.Payload = nil },
		"logical_byte_budget_exceeded": func(f *whalereinv1.TransportFrame) { f.TotalBytes = transportMessageBytes + 1 },
	}
	for name, change := range changes {
		t.Run(name, func(t *testing.T) {
			frame := testTransportFrame("aabbccddeeff00112233445566778899", 0)
			change(frame)
			if _, err := receiver.receive("data", encodeTransport(frame), "ack"); err == nil {
				t.Fatal("invalid frame accepted")
			}
			if delivered != 0 {
				t.Fatal("invalid frame reached application dispatch")
			}
		})
	}
	frame := testTransportFrame("aabbccddeeff00112233445566778899", 0)
	if _, err := receiver.receive("data", encodeTransport(frame), ""); err == nil {
		t.Fatal("missing receipt route accepted")
	}
	if _, err := receiver.receive("data", encodeTransport(frame), "ack"); err != nil {
		t.Fatal(err)
	}
	frame.Payload[0] = 1
	if _, err := receiver.receive("data", encodeTransport(frame), "ack"); err == nil {
		t.Fatal("conflicting duplicate accepted")
	}
	frame.Payload[0] = 0
	frame.Digest[0] ^= 1
	if _, err := receiver.receive("data", encodeTransport(frame), "ack"); err == nil {
		t.Fatal("changed metadata accepted")
	}
	corrupt := testTransportFrame("aabbccddeeff00112233445566778899", 1)
	corrupt.Payload[0] = 1
	if _, err := receiver.receive("data", encodeTransport(corrupt), "ack"); err == nil {
		t.Fatal("corrupt message accepted")
	}
	for index := 0; index < 4; index++ {
		f := testTransportFrame(fmt.Sprintf("%032x", index), 0)
		if _, err := receiver.receive("data", encodeTransport(f), "ack"); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := receiver.receive("data", encodeTransport(testTransportFrame("fabbccddeeff00112233445566778899", 0)), "ack"); err == nil {
		t.Fatal("receive quota bypassed")
	}
	receiver.now = func() time.Time { return time.Now().Add(time.Minute) }
	if _, err := receiver.receive("data", encodeTransport(testTransportFrame("fabbccddeeff00112233445566778899", 0)), "ack"); err != nil {
		t.Fatal(err)
	}
	if delivered != 0 {
		t.Fatal("incomplete message delivered")
	}
}

func TestLogicalTransportCancellationPublishErrorsAndBackpressure(t *testing.T) {
	transport := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	transport.ackTimeout = time.Millisecond
	if err := transport.send(context.Background(), "data", make([]byte, transportChunkBytes+1), "ack", true); err == nil {
		t.Fatal("missing receipts accepted")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := transport.send(ctx, "data", make([]byte, transportChunkBytes+1), "ack", true); err == nil {
		t.Fatal("cancellation ignored")
	}
	transport.publish = func(context.Context, string, []byte) error { return errors.New("offline") }
	if err := transport.send(context.Background(), "data", []byte{0}, "ack", false); err == nil {
		t.Fatal("legacy failure ignored")
	}
	if err := transport.send(context.Background(), "data", make([]byte, transportChunkBytes+1), "ack", true); err == nil {
		t.Fatal("publish failure ignored")
	}
	transport.bulkSlots <- struct{}{}
	transport.bulkSlots <- struct{}{}
	limited, stop := context.WithTimeout(context.Background(), time.Millisecond)
	defer stop()
	if err := transport.send(limited, "data", make([]byte, transportChunkBytes+1), "ack", true); err == nil {
		t.Fatal("send quota bypassed")
	}
	// reset interrupts waiting sends promptly.
	ready := make(chan struct{}, 1)
	<-transport.bulkSlots
	<-transport.bulkSlots
	transport.publish = func(context.Context, string, []byte) error {
		select {
		case ready <- struct{}{}:
		default:
		}
		return nil
	}
	result := make(chan error, 1)
	go func() {
		result <- transport.send(context.Background(), "data", make([]byte, transportChunkBytes+1), "ack", true)
	}()
	<-ready
	transport.reset()
	if err := <-result; err == nil {
		t.Fatal("reset did not interrupt transfer")
	}
	for topic, want := range map[string]string{RequestTopic("h"): ResultTopic("h"), ResultTopic("h"): RequestTopic("h"), EventTopic("h"): RequestTopic("h"), DeviceEventTopic("h", "d"): DeviceAckTopic("h", "d")} {
		if transportAckTopic(topic) != want {
			t.Fatal("missing receipt route")
		}
	}
	if transportAckTopic("bad") != "" || transportAckTopic(DeviceAckTopic("h", "d")) != "" {
		t.Fatal("receipt loop allowed")
	}
}

func TestConnectorLearnsHostAndDeviceTransportV2FromSmallLegacyEnvelopes(t *testing.T) {
	var delivered atomic.Int32
	connector := &pahoConnector{dispatch: func(string, []byte) { delivered.Add(1) }}
	ready, _ := proto.Marshal(&whalereinv1.HostReady{HostId: "h", TransportVersion: 2})
	result, _ := proto.Marshal(&whalereinv1.HostResult{HostId: "h", ResultBody: ready})
	connector.deliver(ResultTopic("h"), result)
	if capable, _ := connector.peers.Load(RequestTopic("h")); capable != true {
		t.Fatal("host capability missing")
	}
	connector.deliver("bad", []byte{0})
	connector.deliver(ResultTopic("h"), []byte{0xff})
	var published []byte
	logical := newLogicalTransport(func(_ context.Context, _ string, payload []byte) error {
		published = append([]byte{}, payload...)
		return nil
	}, func(string, []byte) {})
	session := &pahoSession{client: &fakeMQTTClient{}, transport: logical, peers: &connector.peers}
	// No host capability: small legacy messages still carry device capability.
	connector.peers.Store(RequestTopic("h"), false)
	request, _ := proto.Marshal(&whalereinv1.HostRequest{DeviceId: "d", Kind: whalereinv1.RequestKind_REQUEST_KIND_QUERY, ClientTransportVersion: 2})
	if err := session.Publish(context.Background(), RequestTopic("h"), request, 1); err != nil {
		t.Fatal(err)
	}
	if capable, _ := connector.peers.Load(DeviceEventTopic("h", "d")); capable != true || !bytes.Equal(published, request) {
		t.Fatal("device capability missing")
	}
	if err := session.Close(); err != nil {
		t.Fatal(err)
	}
}

// transportFrames splits one logical payload into the exact wire frames the
// sender produces, so a case can feed them to a receiver in a chosen order.
func transportFrames(t *testing.T, topic string, payload []byte) [][]byte {
	t.Helper()
	hash := sha256.Sum256(payload)
	transfer := fmt.Sprintf("%032x", 0x72657374)
	count := (len(payload) + transportChunkBytes - 1) / transportChunkBytes
	frames := make([][]byte, 0, count)
	for index := 0; index < count; index++ {
		frame := &whalereinv1.TransportFrame{
			Version:    transportVersion,
			TransferId: transfer,
			Topic:      topic,
			Index:      uint32(index),
			Count:      uint32(count),
			TotalBytes: uint32(len(payload)),
			Digest:     hash[:],
			Payload:    payload[index*transportChunkBytes : min(len(payload), (index+1)*transportChunkBytes)],
		}
		frames = append(frames, encodeTransport(frame))
	}
	return frames
}

func TestLogicalTransportLostReassemblyNeverConfirmsDeliveryAndRetryRecovers(t *testing.T) {
	var delivered atomic.Int32
	var mu sync.Mutex
	var acks []*whalereinv1.TransportFrame
	receiver := newLogicalTransport(func(_ context.Context, _ string, payload []byte) error {
		frame := &whalereinv1.TransportFrame{}
		if err := proto.Unmarshal(payload[len(transportPrefix):], frame); err != nil {
			return err
		}
		mu.Lock()
		acks = append(acks, frame)
		mu.Unlock()
		return nil
	}, func(string, []byte) { delivered.Add(1) })
	frames := transportFrames(t, "data", make([]byte, transportChunkBytes*4+1))
	if len(frames) != 5 {
		t.Fatalf("frame count = %d", len(frames))
	}

	// Access receives the first fragment, then restarts: the in-memory
	// reassembly map is dropped. The remaining fragments arrive into a fresh
	// state that can never be completed.
	if _, err := receiver.receive("data", frames[0], "ack"); err != nil {
		t.Fatal(err)
	}
	receiver.reset()
	for _, frame := range frames[1:] {
		if _, err := receiver.receive("data", frame, "ack"); err != nil {
			t.Fatal(err)
		}
	}
	mu.Lock()
	for _, ack := range acks {
		if ack.GetAck() && ack.GetComplete() && !ack.GetRejected() {
			t.Fatal("a lost reassembly acknowledged a completed transfer")
		}
	}
	mu.Unlock()
	if delivered.Load() != 0 {
		t.Fatal("lost reassembly reached application dispatch")
	}
	// The sender must retry the whole logical message; a complete retry is
	// independent of the dropped fragment state and is delivered once.
	for _, frame := range frames {
		if _, err := receiver.receive("data", frame, "ack"); err != nil {
			t.Fatal(err)
		}
	}
	deadline := time.Now().Add(2 * time.Second)
	for delivered.Load() != 1 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	if delivered.Load() != 1 {
		t.Fatalf("retry delivered %d messages", delivered.Load())
	}
}

func TestLogicalTransportPayloadBoundariesAroundTheChunkSize(t *testing.T) {
	for _, size := range []int{1, transportChunkBytes - 1, transportChunkBytes, transportChunkBytes + 1, 2*transportChunkBytes - 1, 2*transportChunkBytes + 1} {
		for _, capable := range []bool{true, false} {
			if !capable && size > transportChunkBytes {
				continue
			}
			var sender *logicalTransport
			var receiver *logicalTransport
			var delivered atomic.Int32
			var frames atomic.Int32
			receiver = newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
				_, err := sender.receive(topic, payload, "")
				return err
			}, func(string, []byte) { delivered.Add(1) })
			sender = newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
				frames.Add(1)
				consumed, err := receiver.receive(topic, payload, "ack")
				if err == nil && !consumed {
					receiver.deliver(topic, payload)
				}
				return err
			}, func(string, []byte) {})
			sender.ackTimeout = 100 * time.Millisecond
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			body := bytes.Repeat([]byte{byte(size % 251)}, size)
			err := sender.send(ctx, "data", body, "ack", capable)
			cancel()
			if err != nil {
				t.Fatalf("size %d capable=%v: %v", size, capable, err)
			}
			deliveryDeadline := time.Now().Add(2 * time.Second)
			for delivered.Load() != 1 && time.Now().Before(deliveryDeadline) {
				time.Sleep(time.Millisecond)
			}
			if delivered.Load() != 1 {
				t.Fatalf("size %d capable=%v delivered %d", size, capable, delivered.Load())
			}
			want := (size + transportChunkBytes - 1) / transportChunkBytes
			if !capable {
				want = 1
			}
			if frames.Load() != int32(want) {
				t.Fatalf("size %d capable=%v frames = %d, want %d", size, capable, frames.Load(), want)
			}
		}
	}
}

func TestTransientBulkSaturationIsRetryableAndNotAPermanentBudgetFailure(t *testing.T) {
	transport := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	transport.ackTimeout = time.Millisecond
	if err := transport.acquireBulk(context.Background(), 1); err != nil {
		t.Fatal(err)
	}
	if err := transport.acquireBulk(context.Background(), 1); err != nil {
		t.Fatal(err)
	}
	// Both bulk slots are held and every waiter slot is occupied, which is a
	// transient admission condition, not an oversized message.
	var waiters []context.CancelFunc
	for index := 0; index < cap(transport.bulkWaiters); index++ {
		ctx, cancel := context.WithCancel(context.Background())
		waiters = append(waiters, cancel)
		go func() { _ = transport.acquireBulk(ctx, 1) }()
	}
	deadline := time.Now().Add(time.Second)
	for len(transport.bulkWaiters) < cap(transport.bulkWaiters) && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	err := transport.acquireBulk(context.Background(), transportChunkBytes+1)
	if err == nil {
		t.Fatal("saturated admission accepted a bulk message")
	}
	if errors.Is(err, ErrMessageBudget) {
		t.Fatal("transient saturation was classified as a terminal payload budget failure")
	}
	for _, cancel := range waiters {
		cancel()
	}
	// After the cancellation releases the waiters, a retry succeeds.
	<-transport.bulkSlots
	<-transport.bulkSlots
	deadline = time.Now().Add(time.Second)
	for len(transport.bulkWaiters) > 0 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	if err := transport.acquireBulk(context.Background(), transportChunkBytes+1); err != nil {
		t.Fatalf("retry after saturation failed: %v", err)
	}
	<-transport.bulkSlots
	// A message larger than the logical transport budget is permanent.
	permanent := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	if err := permanent.send(context.Background(), "data", make([]byte, transportMessageBytes+1), "ack", true); !errors.Is(err, ErrMessageBudget) {
		t.Fatalf("oversized message error = %v", err)
	}
}

func TestChunkReceiptsFromDifferentReceiverStatesDoNotConfirmDelivery(t *testing.T) {
	var sender *logicalTransport
	var delivered, parts atomic.Int32
	receiver := newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		_, err := sender.receive(topic, payload, "")
		return err
	}, func(string, []byte) { delivered.Add(1) })
	sender = newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		_, err := receiver.receive(topic, payload, "ack")
		if parts.Add(1) == 4 {
			receiver.reset()
		}
		return err
	}, func(string, []byte) {})
	sender.ackTimeout = time.Millisecond
	if err := sender.send(context.Background(), "data", make([]byte, transportChunkBytes*4+1), "ack", true); err == nil {
		t.Fatal("partial receiver state reported delivery")
	}
	if delivered.Load() != 0 {
		t.Fatal("partial message dispatched")
	}
}

func TestBulkAdmissionWaitsAndReleasesCancellationBudget(t *testing.T) {
	transport := newLogicalTransport(func(context.Context, string, []byte) error { return nil }, func(string, []byte) {})
	if err := transport.acquireBulk(context.Background(), 1); err != nil {
		t.Fatal(err)
	}
	if err := transport.acquireBulk(context.Background(), 1); err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- transport.acquireBulk(context.Background(), 1) }()
	select {
	case <-done:
		t.Fatal("bulk send bypassed concurrency admission")
	case <-time.After(time.Millisecond):
	}
	<-transport.bulkSlots
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	cancelled, stop := context.WithCancel(context.Background())
	stop()
	if err := transport.acquireBulk(cancelled, 1); !errors.Is(err, context.Canceled) {
		t.Fatal("queued admission ignored cancellation")
	}
	transport.mu.Lock()
	remaining := transport.waitingBytes
	transport.waitingBytes = 32 * 1024 * 1024
	transport.mu.Unlock()
	if remaining != 0 {
		t.Fatal("cancelled admission retained bytes")
	}
	if err := transport.acquireBulk(context.Background(), 1); err == nil {
		t.Fatal("queued byte budget bypassed")
	}
	transport.mu.Lock()
	transport.waitingBytes = 0
	transport.mu.Unlock()
	for i := 0; i < 4; i++ {
		transport.bulkWaiters <- struct{}{}
	}
	if err := transport.acquireBulk(context.Background(), 1); err == nil {
		t.Fatal("queued admission count bypassed")
	}
	for i := 0; i < 4; i++ {
		<-transport.bulkWaiters
	}
	<-transport.bulkSlots
	<-transport.bulkSlots
}

func TestCompletedFramedReplyRestoresOnlyItsAuthenticatedHostCapability(t *testing.T) {
	connector := &pahoConnector{dispatch: func(string, []byte) {}}
	valid, _ := proto.Marshal(&whalereinv1.HostResult{RequestId: "reply", HostId: "host", ProtocolVersion: 1})
	connector.deliverFramed(ResultTopic("other"), valid)
	if _, known := connector.peers.Load(RequestTopic("other")); known {
		t.Fatal("cross-Host result established capability")
	}
	connector.deliverFramed(ResultTopic("host"), []byte{0xff})
	if _, known := connector.peers.Load(RequestTopic("host")); known {
		t.Fatal("malformed result established capability")
	}
	connector.deliverFramed(ResultTopic("host"), valid)
	if capable, _ := connector.peers.Load(RequestTopic("host")); capable != true {
		t.Fatal("valid v2 reply did not restore capability")
	}
}

func TestForwardedEncryptedRequestHonorsLogicalEnvelopeBudget(t *testing.T) {
	var sender *logicalTransport
	var delivered atomic.Int32
	receiver := newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		_, err := sender.receive(topic, payload, "")
		return err
	}, func(string, []byte) { delivered.Add(1) })
	sender = newLogicalTransport(func(_ context.Context, topic string, payload []byte) error {
		_, err := receiver.receive(topic, payload, "ack")
		return err
	}, func(string, []byte) {})
	request := &whalereinv1.HostRequest{
		RequestId: "fixture-request", HostId: "fixture-host", DeviceId: "fixture-device",
		CommandId: "fixture-command", Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND,
		PayloadDigest: make([]byte, 32), RelayTransportVersion: 2,
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	request.Body = make([]byte, transportMessageBytes-4096)
	overhead := proto.Size(request) - len(request.Body)
	atLimit := transportMessageBytes - overhead
	for _, delta := range []int{-1, 0, 1} {
		request.Body = make([]byte, atLimit+delta)
		payload, err := proto.Marshal(request)
		if err != nil || len(payload) != transportMessageBytes+delta {
			t.Fatalf("incorrect encoded size: %d (%v)", len(payload), err)
		}
		err = sender.send(ctx, "data", payload, "ack", true)
		if delta > 0 {
			if !errors.Is(err, ErrMessageBudget) {
				t.Fatalf("oversized envelope error: %v", err)
			}
		} else if err != nil {
			t.Fatalf("valid envelope rejected: %v", err)
		}
	}
	if delivered.Load() != 2 {
		t.Fatalf("received %d envelopes, expected 2", delivered.Load())
	}
}
